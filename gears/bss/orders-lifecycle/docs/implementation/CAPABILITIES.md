# S1-03 — Authorization and database capability evidence

Owner: Orders + Platform. Status: **local capability proofs implemented; production policy, identities and deployment grants remain blocked**. Date: 2026-10-05. These are executable experiments against the S2-01 scaffold, not a delivered S2-03 authorization adapter. No business routes or successful lifecycle operations are enabled.

## What was proved

The [capability suite](../../orders-lifecycle/tests/capability_proofs.rs) runs the real PolicyEnforcer, constraint compiler and Secure ORM against isolated PostgreSQL containers. Recorded PDP responses specify the decisions being enforced; they do **not** prove who should receive those decisions. A separate negative test invokes the actual bundled static provider through the resolver SDK.

| Proof | Observed result | Implementation consequence |
|---|---|---|
| Three custom tenant axes on a `no_tenant` entity, plus standard `id` | SQL and insert validation enforce every conjunction within each alternative permission path; cross-branch mixing and same-axis/different-ID access fail | Preserve all returned predicates; independent `contains_uuid` checks are insufficient |
| Complete proposed arrangement | Current permission alone cannot authorize a proposed resource/payer change; proposed permission alone cannot authorize the old row; both permit the intended update | S2-03 must evaluate current and proposed arrangements separately, including independent PayerUse/delegation requirements |
| Raw update helper counterexample | `secure_update_with_scope` restricts the existing row, but can change custom payer/resource properties without validating their proposed values | Lock and compare current facts, validate the complete proposed model through existing `scope_with_model`, then update within the same transaction |
| Partial insert counterexample | `NotSet` fields are skipped by scope validation | Construct a complete proposed model before validation; never treat a successful partial-model check as authority |
| Frozen seller and stale facts | Seller changes fail even if the proposed scope permits them; an intervening payer/version update invalidates prepared authorization, including when the old scope still sees the row | Compare persisted facts/version under the lock; reject or restart authorization without settling the stale request |
| Point reads | Hidden and missing targets both perform one prefetch, one PDP evaluation and a constrained reread; neither returns prefetched data. PDP outage returns unavailable for either target | Production REST/SDK map targeted denial to masked 404 and infrastructure/invalid constraints to sanitized 503; transport adapters remain S2-03/S6-01 |
| Historical children | A joined current-parent scope removes former-payer access after a payer change while preserving authorized access to historical evidence | Use a correlated parent join/scope in the child query; `scope_via_exists` alone is uncorrelated |
| Finite service target sets | A scope with one ID-bounded branch and one unbounded alternative is rejected by the shape check | Require finite explicit UUID IDs in **every** service permission alternative; no actor-name shortcut |
| Discovery and target prototypes | Private fields prevent constructing a persisted result or raw scope, and discovery cannot be passed to insert/update/delete/worker APIs | Port the tested prototype into private runtime modules in S2-03; engine state/deadline rechecks are still required |
| Read-only transaction | PostgreSQL rejects an actual scoped insert with a read-only transaction error | Discovery always uses `TxConfig::read_only`; it does not acquire row locks |
| Advisory lock route | Two independent pools contend on the same real PostgreSQL session lock; release permits acquisition by the other pool | Direct routing is locally proved. No session-pooling proxy or deployed route has been tested |
| Bundled static provider | The real provider returns no usable constraints for the Orders resource declaration; the PEP refuses it | A production Orders policy/provider integration is required; startup wiring alone is insufficient |

The deterministic concurrency proof uses barriers, not sleeps. The [compile-fail fixture](../../orders-lifecycle/tests/ui/discovery_escape.rs) has nine expected compiler rejections, checked against its committed-format local stderr fixture. They exercise the same capability source that the PostgreSQL tests use. No runtime capability API is exported by these tests.

## Database privilege proof and limits

The [test migration](../../orders-lifecycle/tests/capability_support/migration.rs) provisions disposable PostgreSQL roles and three minimal evidence tables. Credentials exist only for the isolated container; these are not production migrations or reusable deployment passwords.

| Fixture role | Positive capability | Enforced restriction |
|---|---|---|
| Business | Scoped order insert/read/update privileges | Cannot read private/audit evidence or mutate immutable evidence |
| Private persistence | Append audit; insert/read/update private records | Cannot update/delete audit or checkpoints |
| Verifier | Read audit and checkpoint | Cannot read private records, update evidence or delete evidence |
| Checkpoint | Append/read checkpoint | Cannot change/delete existing evidence |
| Retention | Read/delete private records | Cannot delete orders or immutable evidence |
| Maintenance | Read/update orders | Cannot change/delete immutable evidence; target capability is separately required |
| Discovery | Read orders | Cannot update orders; discovery transaction also rejects writes |

This demonstrates PostgreSQL privilege separation, not the final 24-table grant inventory. The prototype private table stands in for a privilege class; it is not a tested outbox or idempotency implementation. S2-02/03 must assemble runtime-owned privileges so business mutation, private audit/idempotency and platform-owned outbox writes use **one atomic transaction**. Opening an independent private connection would break that guarantee. Configure a restricted runtime transaction role with the necessary union of grants and expose private operations only through bounded internal repositories; use separate verifier/checkpoint/retention/discovery credentials. The final role composition, per-table bounds, migration ownership, actor attribution and deployment provisioning require real integration evidence. A migration/admin connection is never the production business connection.

## Authenticated identity contract

Use the selected [D-194 service authentication contract](../DESIGN.md#r08--authenticated-commercial-service-principals-d-194), not older special `*.system` subject assumptions. Stable means a configured authenticated principal tuple, **not** a hard-coded UUID or an actor display name.

| Service owner | Required identity and grants | Provisioning status |
|---|---|---|
| Orders | Separate ordinary `subject_service` principal per configured seller; authorized Pricing plan/price, Products SKU and receipt reads; acceptance-create only for commit flows; agreed assessment/policy/terms read permissions | Not provisioned or tested against a deployed issuer/provider |
| Subscriptions | Its own seller-scoped ordinary service principal; authorized selected receipt read/hold/check-fulfilment and related reads | Not provisioned; no automatic borrowing of Orders credentials |
| Workflow | Configured authenticated service principal; operation-specific Orders grants, explicit finite order IDs for reads; never the user-only forced-failure grant | Not provisioned; finite-scope shape is locally tested, issuance/enforcement policy is not |

Platform owns issuer registration, trust keys, audience/scope validation, credential delivery and revocation through the supported AuthN Resolver S2S path. Deployment binds seller → configured principal/credential reference. Authentication supplies subject ID/type/tenant; Orders must never construct that context by copying an order's seller. Missing/invalid credentials or absent seller bindings fail closed. Do not persist bearer tokens in an attempt.

Token refresh/key rotation may replay an attempt only when authentication yields the same frozen subject ID/type/tenant and required current authority. A changed principal requires explicit reconciliation of uncertain old commands; it must not reuse old command keys as a new principal. Revocation stops new protected effects and is not bypassed during recovery. Test same-principal rotation, changed-principal refusal, cross-seller calls and revoked grants against the selected real issuer/provider before enabling these flows.

Service catalog permissions do not establish buyer authority. Independently authorize the original caller's resource/seller arrangement, proposed payer use and verifiable delegation before foreign-party reads and again where required at final commit. Account Management supplies relationship/proof evidence; PDP makes the permission decision. Preserve the original actor separately from the commercial service identity in audit/recovery.

## Required follow-up before public writes

| Blocker / owner | Exact work | Completion evidence |
|---|---|---|
| `cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration` / Platform + Orders | Select/configure an actual provider that returns complete three-axis permission paths, standard IDs, finite service target sets, separate PayerUse and verified delegation; use registered snake_case action identities | Real-provider operation/actor/axis matrix, invalid constraints, cross-seller, revocation, outage and grant-removal tests; record provider/policy revisions |
| S2-03 / Orders | Turn complete-model proposed validation, lock/recheck and current-parent reads into the shared runtime adapter; retain masking and caller context across REST/SDK | **Implemented locally ([AUTHZ](AUTHZ.md))**: census, actor/axis, check-both, stale/lost-access, proof, outage and invalid-constraint tests pass; no public route exists yet |
| S2-03 / Orders | Move private DiscoveryScope/TargetScope prototypes into runtime modules and install the D-184 `allow_all` lint/CI gate, preserving workspace lint entries | **Implemented locally ([AUTHZ](AUTHZ.md))**: runtime compile-fail fixtures, crate-local clippy deny, recorded failing lint runs and source-scan backstop |
| S2-02/03 + S2-06/08 / Orders + Platform | Map grants to all actual tables and platform producer storage; provide atomic private persistence via restricted runtime transaction authority; separate maintenance/verifier/retention credentials | Real schema migration and rollback/negative role tests; no superuser business pool; private effects roll back with business failure |
| D-194 + S1-05 / Platform and service owners | Provision authenticated seller identities and owner permissions, including proposed owner APIs; implement supported credential acquisition/rotation | Real S2S issuer validation and per-grant denial tests; no test principal or local policy fallback |
| Deployment / Platform | Verify the selected deployed direct/session-pool advisory-lock route and ownership-loss recovery | Same-key contention through the actual connection route; transaction pooling rejected; no claim that a session lock fences external effects |

**S2-03 provider record (2026-10-06).** A configurable, fail-closed [rules AuthZ plugin](../../../../system/authz-resolver/plugins/rules-authz-plugin/README.md) now runs Orders' operation/actor/axis matrix through the resolver plugin client and real `PolicyEnforcer` (policy revision `s2-03-matrix-1` in the PostgreSQL suite). It is a real platform provider for integration and live E2E, not the production policy service: provider selection, policy provisioning, issuer/delegation-proof verification and revocation propagation remain under `cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration`.

The existing toolkit can express the tested row/insert/lock primitives. No generic toolkit change or local Orders policy evaluator is needed to close those particular gaps. Provider policy, proof propagation, real identities and the runtime enforcement assembly remain separate implementation work. The current provider's negative result is a blocker, not a passing production conformance result.

## Verification and next assignment

Run from the repository root (Docker required; Rust 1.98.1 used for the local run):

```sh
cargo +1.98.1 test -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk
cargo +1.98.1 clippy -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk --all-targets -- -D warnings
cargo +1.98.1 fmt -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk --check
cargo gears lint --dylint -P cf-gears-bss-orders-lifecycle -P cf-gears-bss-orders-lifecycle-sdk
python3 gears/bss/orders-lifecycle/docs/implementation/validate_docs.py
```

Observed local result: seven capability integration tests and one compile-fail test, alongside the eleven scaffold/SDK tests. Compiler rejection snapshots must be checked normally, without `TRYBUILD=overwrite`; updating them requires review of each expected error. No commit, PR, deployment or live credential provisioning was performed.

S1-04 local commercial contracts/codecs are now recorded in [COMMERCIAL](COMMERCIAL.md), with remaining producer-profile/API work explicit. S1-05 local contracts and fixtures are recorded in [PROCESS](PROCESS.md). S1-06 local fixture/harness evidence is in [CONFORMANCE](CONFORMANCE.md). S1-07 local inventory is in [READINESS](READINESS.md). **Next: S2-02 schema/repositories**, using the reconciled [S5-01 contracts](RECEIVER_CONTRACTS.md). Keep public business readiness disabled while the listed authorization prerequisites are incomplete.
