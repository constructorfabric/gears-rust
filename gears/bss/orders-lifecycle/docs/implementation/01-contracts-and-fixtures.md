# Stage 1 — Contracts, design reconciliation and executable fixtures

Status: S1-01 documentation reconciliation complete ([baseline](BASELINE.md)); S1-02 catalog and boundary fixtures complete ([evidence](contracts/CONTRACTS.md)); S1-03 local capability proofs verified with production blockers ([evidence](CAPABILITIES.md)); S1-04 local codec/contracts/fixtures implemented with owner-profile gaps ([evidence](COMMERCIAL.md)); S1-05 local process contracts/fixtures verified with owner gaps ([evidence](PROCESS.md)); S1-06 local conformance corpus/fault harness verified ([evidence](CONFORMANCE.md)); S1-07 local execution/readiness ledger verified ([evidence](READINESS.md)); no business runtime delivery claimed. Read [README](README.md) before starting.
This stage produces the contracts and fixtures needed by parallel owner implementations;
it is not a substitute for those implementations. Source review is recorded in [REVIEW](REVIEW.md).

## S1-01 — Reconcile the implementation baseline

**Depends on:** nothing. **Owner:** implementing agent with Orders architecture ownership.
**Files:** existing DESIGN, DECISIONS, PRD, UPSTREAM_REQS and the eight feature documents; this plan's coverage index.

- Read each assigned normative contract, its current decision and source implementation. Apply the technical errata in REVIEW before translating contradictory prose into code. Preserve earlier decisions as explicitly superseded history.
- Reconcile the actual API, schema, state/trigger/event and worker inventories. Include D-188 commercial attempts and D-198 grants/controls; do not implement only the pre-reconciliation foundation.
- Follow selected D-187–D-200 contracts where they explicitly supersede old assumptions. Where a genuine product question remains, record its exact scope and affected release path; keep the conservative documented behavior. Do not manufacture policy or label an owner API delivered.
- Repair duplicate explicit anchors without breaking existing references: retain the original first occurrence, give later component headings distinct anchors and update referring links. Correct stale broker, ordinary-transaction and pin-format prose.
- Reconcile the SUB-O numbering register by semantic contract and document a canonical alias map across Subscriptions/Workflow; do not interpret identical numeric labels as identical APIs.

**Acceptance:** every review item has a concrete disposition and package owner; selected decisions are not reopened as questions; no new public state/event/policy is introduced incidentally. Local links and coverage inventory validate. PRD divergences retain their product status where unresolved.

## S1-02 — Freeze the public operation and storage contract catalog

**Depends on:** S1-01. **Owner:** Orders.
**Sources:** [API inventory](../DESIGN.md#33-api-contracts), [Workflow SDK](../DESIGN.md#orders-lifecycle-workflow-sdk), [boundary/GTS/errors](../DESIGN.md#contract-01-4-7), [state table](../features/01-foundation.md#contract-01-order-state-machine), [schema inventory](../DESIGN.md#37-database-schemas--tables).
**Proposed files:** `orders-lifecycle-sdk/src/{api,models,errors}.rs`, `orders-lifecycle/src/{domain,api/rest,gts}/`; exact module split follows ToolKit, not a new framework.

- Produce an operation manifest: path/method, SDK method, actor/action, request, result, expected version/draft revision, idempotency, trigger, failure mapping and event declaration. Include every authoring, Preview, lifecycle, approval, fulfillment, acceptance, forced-failure and read operation, not just PRD §9.1's abbreviated list.
- Freeze 11 states, 29 source transition rows, 21 trigger tokens, 11 event types and admitted `new_sale` category; expand multi-state rows into unique runtime keys. Enumerated registries, not copied numeric counts, drive checks.
- Define field classification and the full order/version/line/admin/projection/receipt/total/diagnostic schemas, nullable fields and bounded payloads. Categories use the selected GTS instance model, not a new closed database enum.
- Specify one reason/domain/code per condition and canonical RFC 9457 mapping; retain validation vs authorization vs business refusal vs infrastructure distinctions. Workflow SDK and REST must expose the same semantics.
- Add the typed D-198 spawn contribution/result and staged internal control contract to the manifest. Only selected existing endpoints are extended; no generic arbitrary transition endpoint or silent 202 convention.

**Acceptance:** manifest reconciles every source interface and state row; golden request/result/error/event fixtures cover boundary omissions, unsupported fields and canonical round trips. Unknown guard/reason/operation registration fails startup.

**Delivered:** [catalog, selected binding and verification evidence](contracts/CONTRACTS.md), with 73 contract cases and eight golden JSON envelopes. This satisfies the catalog/fixture baseline; proposed Rust files land in S2-01. Runtime boot failures, native owner codecs, real PostgreSQL and provider enforcement remain the named successor packages’ acceptance requirements. No runtime startup or complete serializer conformance is claimed by the Python oracle.

## S1-03 — Prove the authorization and database capability assumptions

**Depends on:** S1-02; can run alongside S1-04/05/06. **Owner:** Orders + Platform.
**Sources:** [shared adapter](../DESIGN.md#contract-08-3-5), [permission matrix](../DESIGN.md#contract-08-4-3), [commercial identities](../DESIGN.md#contract-08-commercial-service-authorization).
**Existing patterns:** [Products PEP](../../../products/products/src/authz.rs), [Products wiring](../../../products/products/src/gear.rs), [secure ORM guide](../../../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md).

- Build a small PostgreSQL/provider-backed capability fixture proving all three mapped tenant axes, explicit order-ID constraints, complete OR permission paths and AND conditions within each path.
- Demonstrate proposed-value enforcement for create and payer/resource changes, including all populated insert fields and locked authorization-fact rechecks. A successful scope compilation alone is not proof.
- Prove the permitted point-prefetch + constrained re-read flow, indistinguishable hidden/missing target denial paths and separate current-parent access to historical children.
- Define stable authenticated Orders/Subscriptions/Workflow service identities, issuer provisioning, seller grants and rotation behavior. Buyer/delegation authority remains separate. Never construct a privileged actor name to bypass PDP.
- Demonstrate runtime-owned database roles for business, private audit/idempotency/outbox, verifier, checkpoint append, retention and bounded maintenance; discovery scopes cannot escape into writes.

**Acceptance:** actual provider constraints enforced by SQL/insert validation, cross-axis denial and proposed-value negative tests pass. Missing grants/provider stay recorded as production blockers; local permissive PDP fixtures do not close them. If current platform cannot express the contract, add the exact provider/toolkit implementation work before public writes, not a local policy evaluator.

**Local delivery:** [CAPABILITIES](CAPABILITIES.md) records actual PostgreSQL enforcement, the update/partial-insert counterexamples, compile-time barriers and the real bundled provider’s failing constraint result. Live policy, service identities, full-schema grants and deployed connection routing remain named production blockers. Prototype adapters are test-only; S2-03 owns runtime enforcement.

## S1-04 — Freeze commercial SDK profiles and lossless codecs

**Depends on:** S1-01/02. **Owner:** Pricing + Subscriptions + Rating + Orders.
**Sources:** [assessment](../DESIGN.md#contract-03-nonbinding-assessment), [policy/deadline](../DESIGN.md#contract-03-commercial-deadline), [billing terms](../DESIGN.md#contract-03-billing-terms-resolution), [frozen receipt](../DESIGN.md#contract-03-frozen-commercial-snapshot), [Rating](../DESIGN.md#contract-03-rating-purchase-evaluation), [diagnostics](../DESIGN.md#contract-03-diagnostic-mapping).

- Define provider-owned typed contracts, profile/version support, canonical digests, complete selected-binding coverage, full independent diagnostics and exact input/result identity. Mark proposed methods as proposed until the provider package delivers them.
- Inventory native Pricing SDK fields versus required Orders evidence. Model exact, derived and missing facts explicitly; do not substitute REST DTOs or Pricing private wire codecs.
- Define the schema-2 Orders receipt/query/binding codec and size/encoding limits, complete BillingTerms and source provenance, exact month/year period mapping, quantity semantics and UTC instants.
- Define Rating result horizons, uncommitted usage vs zero, discounts, single TCV carrier, mixed cycles and optional Preview withholding. Indicative tax is a separate owner seam with its own failure behavior.
- Build a call dependency graph and revise the old port budgets to include assessment, policy, terms, Rating and receipt commands. Retain the governing latency objective; do not turn a timeout budget into an SLO or silently enlarge it.

**Acceptance:** native round-trip/digest fixtures, incomplete/unknown-profile refusal cases and Preview optional-context cases are executable. Every required fact has an owner and implementation package in Stage 3 or the early provider tracks of Stage 4/5.

**Local delivery:** [COMMERCIAL](COMMERCIAL.md) records the compiled native schema-2 codec, exact calendar conversion, independent digest/round-trip fixtures, proposed owner profile requirements and acyclic call graph. Producer profile IDs/registries, missing APIs, policy choices and new budgets remain explicitly assigned owner work; no producer agreement or real-provider conformance is claimed.

## S1-05 — Freeze process, receiver and external-owner contracts

**Depends on:** S1-02/04. **Owner:** Workflow + Subscriptions + commercial owners.
**Sources:** [owner readiness](../DESIGN.md#contract-05-commercial-owner-readiness), [receiver protocol](../DESIGN.md#contract-06-activation-admission), [consumer contract](../DESIGN.md#contract-01-event-consumer-contract), [reference release](../DESIGN.md#contract-03-revision-reference-protection), [upstream register](../UPSTREAM_REQS.md).

- Specify Account Management commercial profile, verifiable delegation/PDP boundary, Contracts eligibility/live consent declaration, Payments intent/status and Workflow order-specific approval contracts. Ledger postings and the existing approval inbox are not replacements.
- Specify canonical overlap scope/provenance and active/pending occupancy, all receiver write paths, grant/control/fence identities, exact roster and create-key handling, two-phase create/settle, status/outcome reads, compensation evidence and correlation.
- Separate immutable commercial version, fulfillment attempt, dispatch generation, operational control generation, idempotency execution and audit sequence in fixtures. Define legal transitions and ownership for each.
- Define event-consumer durable deduplication, fresh Orders read, missing/read-unavailable recovery and platform DLQ recovery. Freeze the expected integration corpus.
- Specify future Pricing-owned reference-release admission closure/drain/usage proof. Retaining references is the selected safe fallback; no cleanup may be enabled from a simple count.

**Acceptance:** receiver/process contracts are implementable through supported owner SDKs with authenticated grants. Stage 5's expired-draft replacement and force-fail generation corrections are reflected in fixtures before activation implementation. Missing APIs remain explicit build packages, not undocumented stand-ins.

**Local delivery:** [PROCESS](PROCESS.md) publishes 19 proposed operations, semantic aliases and 151 reference-model cases covering receiver traces, the mandatory consumer corpus, owner failure boundaries, compensation, successor/freshness corrections and future release proof. Supported owner SDKs, authenticated grants and real receiver/provider conformance remain the named packages’ work. S5-01 normative amendments are now recorded in [RECEIVER_CONTRACTS](RECEIVER_CONTRACTS.md); peer runtime adoption remains open; this is not production acceptance.

## S1-06 — Build shared conformance fixtures and fault-injection seams

**Depends on:** relevant S1-02–05 contracts. **Owner:** Orders with provider test owners.
**Proposed files:** `orders-lifecycle/tests/fixtures/`, owner-specific SDK/provider fixtures and PostgreSQL integration support. Reuse the repo's harness and existing dependencies.

- Create independent frozen byte/preimage/digest vectors for audit v1/v2/genesis/checkpoints, canonical request fingerprints, receipt/terms/binding digests and cursor bindings. Expected values must not be generated by the code under test.
- Build fixtures for self-service/partner/third-party payer, published/scheduled/superseded/closed/retired catalog, finite/rolling/mixed terms, one-time/usage, 200-line baskets, unavailable owner, and native default dimensions distinct from the literal string `default`.
- Define fault points before/after remote acceptance, local commit, outbox enqueue/ack, receiver admission/confirmation, grant revocation and payment response. Use barriers/injected clocks in PostgreSQL integration tests; avoid sleep-based unit tests.
- Establish test venues: pure domain/codec tests; scoped repository tests; real PostgreSQL locking/constraints; real REST/authentication/provider integration; load/DR/operations evidence. SQLite does not prove PostgreSQL concurrency or grants.
- Each package records commands, fixture IDs and observed result. A release ledger links every applicable source requirement to evidence and records conditional/deferred scope honestly.

**Acceptance:** fixture suites compile in the created crates when S2-01 lands; independent expected data and test venue are reviewed before implementation claims. Failure cases cover facts as well as response status.

**Local delivery:** [CONFORMANCE](CONFORMANCE.md) records 27 frozen byte/hash vectors, four native receipts with 16 digest preimages, 13 commercial scenario recipes and 16 fault points. Twelve new Rust tests cover reference bytes, native codec construction and real PostgreSQL rollback/commit/hash/timestamp probes. Runtime business crash integration and actual provider conformance remain assigned to their implementing packages; the test-only helpers are not a production engine.

## S1-07 — Prepare the execution queue and deployment readiness ledger

**Depends on:** S1-01–06 outputs; update incrementally. **Owner:** Orders coordinator + Platform.

- Use [README](README.md)'s dependencies to dispatch work. SDK/provider packages may start before their numbered stage's end-to-end flow. Never create a cycle where submit waits for an owner implemented only after submit.
- Record exact SDK/schema/profile version, source path, configured provider, principal/grants, supported scope, remaining implementation, conformance command and deployment evidence for every [upstream ID](../UPSTREAM_REQS.md).
- Record optional or deferred scope explicitly: reference cleanup disabled, optional external audit anchoring disabled, phase-two change orders refused. Program commercial retention and other product questions keep their existing limits.
- Agree names of future package/test targets after inspecting workspace Cargo metadata. Do not run invented targets or claim unconfigured tooling is a passing check.

**Exit:** S2-01 is already delivered. S5-01 local contract reconciliation is complete. Next execute S2-02 full schema/repositories; S3-01 remains independently startable. Provider tracks have named dependencies, and public/release gates are distinct from what can be developed locally.

**Local delivery:** [READINESS](READINESS.md) and its generated upstream/queue tables account for all 44 upstream IDs, 70 numbered packages, 41 acyclic dispatch workstreams and 18 actual Cargo packages. Missing provider/schema/grant/deployment values are explicit; conditional and deferred scope is preserved. Ledger/source/metadata checks do not establish actual provider or production acceptance.
