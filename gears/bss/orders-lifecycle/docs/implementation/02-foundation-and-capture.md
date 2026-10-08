# Stage 2 — Orders foundation and draft capture

Status: **S2-01–S2-12 implemented, independently gap-fix reviewed and locally verified** ([scaffold ledger](SCAFFOLD.md); per-package ledgers linked from each *Delivery* line; [milestone ledger](MILESTONE.md) with the live HTTP/PostgreSQL E2E, 14 passed; S2-FINAL cleanup 2026-10-07). Draft authoring and draft reads are exposed; submit/Preview, amendments, holds, cancel and fulfillment are not. This implements the shared correctness core and feature 02, including the
feature 08 authorization prerequisite. Read [README](README.md), then the complete
[Foundation](../features/01-foundation.md), [Capture](../features/02-capture.md), and
[authorization contract](../DESIGN.md#contract-08-3-5). Do not expose stub operations.

All source paths below are proposed under `gears/bss/orders-lifecycle/` unless linked to an
existing file. Migrations belong to the engine; later slices contribute schema requirements.

## S2-01 — Create SDK/runtime crates and Gear wiring

**Depends on:** S1-01/02; S1-03 capability work may run alongside. **Owner:** Orders.
**Files:** `orders-lifecycle-sdk/{Cargo.toml,src/}`, `orders-lifecycle/{Cargo.toml,src/}`, `gear.toml`, workspace/runtime registration and test configuration.
**Precedent:** [Products Gear](../../../products/products/src/gear.rs), [ToolKit layout](../../../../../docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md).

- Inspect workspace member/package/feature conventions before naming crates. Read dependency guidance before adding Cargo entries; reuse workspace versions. Runtime and SDK remain separate, public SDK types do not import REST or provider internals.
- Implement config validation, ClientHub dependency resolution, local SDK provider, REST/OpenAPI registration, runtime migration exposure, cooperative lifecycle shutdown and readiness/liveness separation.
- Require positive finite idempotency lease configuration and supported database/lock route. Wire real owner ports or explicitly unavailable adapters; no production success from local development doubles.
- Register the standard domain macros, canonical errors, GTS and permission resources. Respect `bss_orders__` database namespace even though older BSS gears use different physical names.

**Tests/done:** crate builds/lints; lifecycle startup/shutdown and missing mandatory dependency/config tests pass. No permissive placeholder handler is publicly enabled. Record actual cargo package names and commands in README's execution ledger.

**Delivery:** [SCAFFOLD](SCAFFOLD.md) records the two compiling crates, optional host feature, real PostgreSQL boot/shutdown and unavailable local SDK. REST/OpenAPI and migration hooks expose no business operation/schema yet. The full Workflow/authoring SDK, native owner codecs, engine, PEP, event schemas and traffic readiness remain in their named packages; only the typed submit entry point is supplied at this stage.

## S2-02 — Implement the full schema inventory and secure repositories

**Depends on:** S1-02, S2-01; finalize grant/control DDL after S5-01 contract corrections. Other schema work proceeds independently. **Sources:** [all 24 Orders tables](../DESIGN.md#37-database-schemas--tables), [Foundation columns](../DESIGN.md#contract-01-3-7), [D-188](../DESIGN.md#contract-01-commercial-attempt), [D-198](../DESIGN.md#contract-06-activation-admission).
**Files:** `src/infra/storage/{entity,repo,migrations}/`; runtime-owned role/grant migration integration.

- Translate every column, key, FK, CHECK, index, writer grant and mutability rule into a schema manifest. Stage later slice tables in ordered migrations, while keeping one canonical owner and all foreign-key dependencies explicit.
- Implement aggregate/current-version deferred FK, sparse committed history with explicit predecessor, separate reserved line identities and mutable draft/admin content, snapshots/totals, diagnostics, acceptance/reflections/projections, all three operational ledgers and overlap claims.
- Resolve nullable platform policy key representations before DDL (REVIEW); never declare a nullable column inside a PostgreSQL primary key. Preserve business uniqueness for nullable diagnostic selections and policy scopes.
- Implement D-201 audit v3/force_request_observation shape checks while retaining frozen v1/v2 readability; register the internal rebuild audit token separately from public state transitions and link grants to their originating committed audit entry.
- Add append-only defenses, exact allowed updates, restricted refusal-retention DELETE, no destructive order-linked commercial path, audit/checkpoint role separation, checked counter limits and no cascade deletion of evidence.
- Use SecureConn/SecureTx and explicit current-parent scope. Populate every security property on inserts. Private registry/audit/producer scopes do not widen business access. Apply platform producer migrations through their owners.

**Tests/done:** empty-install and forward migration tests, PostgreSQL constraints/role-negative tests and scoped inserts pass; sparse version reads, invalid FKs, duplicate claims, identity reuse, unknown enum values and unauthorized UPDATE/DELETE fail. SQLite smoke tests are supplemental. Rollback deployment uses forward corrections, not historical evidence deletion.

**Delivery:** [STORAGE](STORAGE.md) records the full 24-table schema, generated inventory, secure internal repositories, role/mutation guards and real PostgreSQL validation. S2-02 is implemented, independently gap-fix reviewed and locally verified; the schema now carries the delivered draft authoring and draft reads, while every other public operation stays unavailable.

## S2-03 — Implement the shared PEP and bounded internal authority

**Depends on:** S1-03 evidence, S2-01/02. **Files:** `src/authz.rs`, `src/gts/permissions.rs`, scoped repository adapters and maintenance capability types.
**Sources:** [permission matrix](../DESIGN.md#contract-08-4-3), [denial wrapper](../features/08-read-and-authz.md#32-denials-outages-and-bounded-internal-authority), [service grants](../DESIGN.md#contract-08-commercial-service-authorization).

- Map resource/seller/payer axes and standard resource ID exactly. Enforce all complete permission paths; do not copy Products' single owner-tenant mapping into Orders.
- Authorize before registry probing or commercial reads. For create/axis changes authorize the full proposed arrangement, then compare locked facts before mutation. A stale authorization conflict never settles or changes an existing key.
- Implement the exact hidden/missing target call pattern and 403/404/503 mapping, including proof-specific masking and required follow-up read permission. Missing constraints are integration failures.
- Propagate real caller/proof context in REST and SDK; acceptance party guards remain separate domain rules. Replay rechecks current disclosure authority.
- Encapsulate read-only DiscoveryScope and target-derived TargetScope, private persistence privileges and the allowlisted maintenance entry. Do not hand broad worker scope or a fake user context to the engine.

- Add blocking D-184 CI checks: `trybuild` compile-fail fixtures for DiscoveryScope escaping into write/worker APIs and TargetScope construction from raw IDs; deny `AccessScope::allow_all` outside the explicitly permitted discovery implementation through the repository lint configuration; prove runtime rejection of writes inside `TxConfig::read_only`. These structural checks supplement authorization tests.

**Tests/done:** full operation/action census, actor/axis matrix, old/new payer, stale facts, proof denial/revocation, PDP outage and invalid constraints across REST/SDK pass. Workers retain only their explicit independent capability during PDP outages. No public write ships before this package passes.

**Delivery:** [AUTHZ](AUTHZ.md) records the shared PEP, scoped storage adapters, D-184 capability types and gates, and the configurable rules PDP plugin used for real-provider matrices. Implemented, independently gap-fix reviewed and locally verified. Of the public operations only draft authoring and the draft reads are exposed (S2-09, early S6-01/S6-04, S2-12); the rest stay unavailable.

## S2-04 — Implement the declarative transition engine

**Depends on:** S2-02/03 for the skeleton and S1-02 contribution interfaces; completed integration depends on S2-05/06/07/08.
**Files:** `src/domain/{state_table,guards,contributions,transition}.rs`, transaction adapter.
**Sources:** [complete algorithm](../features/01-foundation.md#contract-01-3-6), [state table](../features/01-foundation.md#contract-01-order-state-machine), [engine contract](../DESIGN.md#contract-01-4-1).

- Register every row/guard/event/versioning rule; validate duplicate expanded keys, unknown reasons and missing field classes at startup. Keep engine orchestration independent of commercial evaluation/provisioning logic.
- Implement distinct create and existing-order paths; aggregate-before-registry lock order on existing orders; no nonexistent aggregate lock on create.
- Preserve exact precedence, including Workflow version-before-admissibility and ordinary admissibility-before-version, client/prepared draft revision, early input failures and precluded vs unavailable inputs.
- Apply ordered contributions, overlap acquisition before version append, state-entry time only on state change, pre-hold state, monotonic counters, irreversible spawn/tolerance facts and one audit/event mapping per row.
- Return business refusals as committed transaction outcomes; infrastructure failures abort everything. Unknown commit acknowledgement is not confirmed rollback. D-188/D-198 preparation are explicit specialized subflows, not blanket exceptions.

**Tests/done:** enumerate every row and forbidden edge; verify row effects, eventless rows, guard precedence, no terminal exits and administrative per-field audit. Failure injection at every persistence boundary proves rollback; handlers/slices cannot write aggregates directly.

**Delivery:** [ENGINE](ENGINE.md) records the startup-validated state table, guard registry and field classification, the pure decision and contribution planner, and the transaction adapter (create, existing-order, private worker entry, explicit D-188/D-198 preparation subflows) composing S2-03/05/06/07/08, with real PostgreSQL tests over every row and guard. Implemented, independently gap-fix reviewed and locally verified. Slice predicates and contributions remain with their owning packages: the engine serves the delivered create and `draft-mutate` rows (S2-09) while every other row stays unreachable until its package ships.

## S2-05 — Implement idempotency and durable execution ownership

**Depends on:** S2-02/03. **Files:** `src/infra/storage/repo/idempotency.rs`, `src/domain/idempotency.rs`, operational execution adapter.
**Sources:** [registry contract](../features/01-foundation.md#contract-01-4-2), [D-188](../DESIGN.md#contract-01-commercial-attempt), [D-198](../DESIGN.md#contract-06-activation-admission).

- Stable `(operation, principal_scope, key)` identity; canonical fingerprint includes target/axes/version/draft revision/contribution, excludes transport, correlation, clock and server IDs.
- Advisory authorized replay precedes fresh input resolution. Authoritative conflict-safe insert/locked re-read distinguishes own insert from a competitor; fingerprints precede lease/settlement interpretation.
- Use fresh database time after lock waits, logical operation-specific (ordinary 24-hour; workflow-class ≥30-day under D-173) expiry, immutable retained responses and no window extension. Ordinary claims settle in one transaction; D-188/D-198 durable executions use immutable execution ID plus owner/fencing generation across remote calls.
- Preserve unresolved attempts/controls through response-key expiry; reused key text never adopts an old operational identity. Resume loads frozen evidence first. Final ownership checks prevent stale worker settlement.

**Tests/done:** same-key concurrency, distinct principals/targets, mismatch against all states, expired/live leases, lost reply, cleanup races, advisory miss followed by winning settlement and failed guard resolution; same effect/response once, with explicit post-retention and cross-principal-create limits.

**Delivery:** [IDEMPOTENCY](IDEMPOTENCY.md) records the registry rules, authoritative gate, advisory probe, PEP replay disclosure and D-188/D-198 execution fencing with real PostgreSQL concurrency/fault tests. Implemented, independently gap-fix reviewed and locally verified. Of the public operations only draft authoring and the draft reads are exposed (S2-09, early S6-01/S6-04, S2-12); the rest stay unavailable.

## S2-06 — Implement transactional audit and canonical integrity encoding

**Depends on:** S2-02/03, S1-06 vectors. **Files:** `src/domain/audit.rs`, audit/checkpoint repositories.
**Source:** [audit byte contract](../DESIGN.md#contract-01-audit).
**Precedent:** [Pricing scoped audit writer](../../../pricing/pricing/src/infra/storage/repo/audit_repo.rs) for transactional persistence only; it currently writes unsealed rows, so its missing hash/verifier is not reusable implementation.

- Construct all minimized persisted fields before encoding; writer emits v3 (D-201 supersedes the earlier v2 election), verifier recognizes v1/v2/v3 and rejects unknown versions. Encode NULL vs empty, UUID bytes, signed timestamp microseconds and exact framed field order.
- Allocate contiguous per-order audit sequences under the aggregate lock, independent of sparse commercial versions. Freeze genesis/audit namespace at creation; retain current-resource and subject-tenant evidence separately.
- Create, normal transition, per-field administrative edit, resolved/unresolved refusal each follow their exact row shape. Early denial gets no target enrichment lookup. Failed audit aborts an admitted mutation.
- Store opaque trusted subject UUID and distinct caller_reason; no profile lookups or history rehash after identity deletion. Validate free-text minimization and no proof credentials in references.

**Tests/done:** independent preimage/digest fixtures, mutation of every field, NULL/empty/boundary/timestamp cases, rollback without sequence gaps, concurrent append without forks, identity removal and DB role tests. Worker verification/checkpoints are S2-11, not implicitly complete here.

**Delivery:** [AUDIT](AUDIT.md) records the canonical v1/v2/v3 encoder and verifier, the sealed transactional writer (contiguous per-order sequences under the aggregate lock, frozen namespace, exact row shapes, minimization), configured actor classes and real PostgreSQL concurrency/rollback/role/identity tests. Implemented, independently gap-fix reviewed and locally verified. Worker verification and checkpoints remain S2-11.

## S2-07 — Implement transactional in-flight overlap claims

**Depends on:** S2-02/03 and S1-02 contribution interfaces; completed S2-04 is not a prerequisite. **Files:** overlap repository and engine contribution adapter.
**Source:** [claim schema/algorithm](../DESIGN.md#contract-01-table-orders_inflight_overlap_claim).

- Use full payer/resource/canonical-key tuples and partial live uniqueness; deduplicate and acquire missing tuples in one deterministic binary order at READ COMMITTED.
- Preserve existing claims, collect exact inserted IDs, release only new IDs on shortfall and verify affected count; settle the diagnostic refusal without phantom version or commercial contributions.
- After full acquisition release superseded tuples; on every terminal release all local claims. D-182 releases Orders claims but never uncertain receiver capacity.
- Integrate authoritative predicate-9 replacement with Stage 3 diagnostics and later receiver provenance/dedup with Stage 5. Orders claims alone do not enforce Subscriptions cardinality.

**Tests/done:** concurrent conflicting/multi-key baskets, reversed caller order, payer amendment, duplicate line keys, shortfall cleanup/error and all terminal transitions. PostgreSQL tests prove no deadlock-dependent correctness or lost pre-existing claim.

**Delivery:** [OVERLAP](OVERLAP.md) records the pure tuple/partition rules, the engine directive adapter, and the transactional repository. The repository uses database-time claims, exact-ID shortfall release and a disclosure-scoped conflicting holder. It refuses any isolation level other than READ COMMITTED before its first claim statement and is backed by real PostgreSQL concurrency, isolation, cleanup-fault, refusal-settlement and terminal tests. Implemented, independently gap-fix reviewed and locally verified. Of the public operations only draft authoring and the draft reads are exposed (S2-09, early S6-01/S6-04, S2-12); the rest stay unavailable.

## S2-08 — Integrate typed events and the managed producer

**Depends on:** S1-02, S2-01/02/06. **Files:** `src/infra/{events,broker}.rs`, GTS event schemas and startup wiring.
**Sources:** [event contract](../DESIGN.md#contract-01-4-4), [D-200](../DESIGN.md#contract-01-event-platform-integration).
**Precedent:** [Pricing TxOutbox](../../../pricing/pricing/src/infra/events.rs).

- Register abstract/subject/topic and all eleven concrete events; event source, root tenancy, order UUID subject, `/subject` partition routing and envelope/payload fields exactly match design.
- Bind DbProducer/Chained mode and the toolkit queue/partition configuration. Enqueue using the same transaction runner; fire Wake after commit and discard it on rollback/retry.
- Readiness requires actual broker, schema preparation, root identity/grants, durable producer registration and worker handle. Do not copy Pricing's interim pending queue into a production-ready Orders fallback.
- Use implemented initial-cursor retry behavior; add missing empty-cache transient regression. Preserve identity on lost acknowledgement and permanent-rejection/republication protocol.

**Tests/done:** atomic rollback, accepted/persisted/duplicate, initial and later transport/rate-limit failures, UnknownProducer/restart, permanent rejection and 200-line/64-KiB limit. Shared operator DLQ recovery and consumer conformance remain S6-05 release work.

**Delivery (2026-10-06):** implemented, independently gap-fix reviewed and locally verified ([EVENTS](EVENTS.md)). Readiness cannot verify the broker's partition count, because no broker API reports it. A mismatch was shown to drop events silently. The explicit `events.broker_partitions` declaration is required and equality is a deployment gate; DESIGN §3.7 is amended accordingly by D-205, with a broker-level partition-count capability requested upstream. The silent loss under a mismatch is a **deferred escalation** ([UPSTREAM_REQS §2.7](../UPSTREAM_REQS.md#27-event-broker)), not yet raised with Event Broker.

## S2-09 — Implement draft authoring and field classification

**Depends on:** S2-03–08. **Files:** capture service, REST DTO/routes, shared classifier, draft/identity/admin repositories.
**Sources:** [Capture full contract](../features/02-capture.md), [authored fields](../DESIGN.md#contract-02-4-3).

- Create authorized empty version 1/draft revision 0 and seller-unique immutable number with trusted actor/sales-path evidence; no catalog/Contract resolution, total or price pin.
- Implement five authoring endpoints. Add/edit/remove draft working membership with server-reserved stable line IDs; removed IDs are never reused. Preserve authored terms, cycle, references and dates.
- Select one trigger from named field classes without reading state. Mixed requests refuse; seller remains fixed; proposed resource/payer changes need complete authorization. Administrative edits route to the separately implemented S4 package; do not silently write them through draft mutation.
- Enforce positive quantity, single currency/payer, configured 200-line baseline and admitted category, sharing structural guards with submit/amendment. Different cycles remain valid.
- Draft mutations increment only draft_revision exactly once. Preserve optional-boundary expected_draft_revision semantics: outside draft inadmissibility wins; inside draft omission conflicts.

**Tests/done:** draft capture during catalog outage but not PDP outage, concurrent revision writes, all structural/field precedence cases, stable retry identities and no external commercial side effects. Expose minimal safe draft inspection using S6-01/04 early; no unlogged cross-tenant read.

**Delivery (2026-10-06, gap review 2026-10-07):** implemented, independently gap-fix reviewed and locally verified ([CAPTURE](CAPTURE.md)). Five authoring routes and SDK methods run one shared service through the engine; wire bindings are [D-206](../DECISIONS.md). Administrative edits answer unavailable until S4-05. By coordinator decision no read is exposed: draft inspection and S6-04 logging belong to the early-S6 package.

## S2-10 — Implement date policy and admission-time date preparation

**Depends on:** S2-02/09; consumed by Stage 3. **Files:** date-policy repository/config promotion and pure date resolver.
**Sources:** [date cascade](../features/02-capture.md#contract-02-4-2), [policy schema](../DESIGN.md#contract-02-table-orders_date_policy).

- Validate and deploy platform default plus resource-tenant override, source scope/revision and explicit required-field switches; no new runtime policy-edit endpoint.
- Preserve authored values; derive optional dates only at Preview/submit/amendment. Defaults never satisfy a required authored service/acceptance date.
- Freeze policy and proposed UTC date before dependent calls; engine checks against one transition timestamp under lock. UTC-day rollover refuses, never recomputes only dates while retaining old totals.
- Persist three resolved dates and policy provenance; later policy changes/read/fulfillment cannot rewrite them. Due date is not assent, quoted activation date is not billing anchor.

**Tests/done:** all switch combinations, absent/default/explicit dates, midnight rollover with injected clock, policy promotion and later reads, exact term preservation and no admission snapshot on refusal.

**Delivery:** [DATES](DATES.md) records the pure cascade and frozen basis, the startup policy channel and default check, the `capture.date-basis` guard proven through the engine's submit row with an injected clock, and admitted-line materialisation ([D-207](../DECISIONS.md#d-207--the-date-policy-channel-is-a-startup-promotion-the-date-basis-is-strict-and-frozen-s2-10)). Implemented, independently gap-fix reviewed (2026-10-07) and locally verified. Preview, submit and amendment remain unavailable until S3-12/S4.

## S2-11 — Deliver maintenance infrastructure, retention and audit verification

**Depends on:** S2-02/05/06/08. **Files:** `src/infra/workers/`, configuration, runtime role and metric wiring.
**Sources:** [five-worker roster](../DESIGN.md#contract-01-3-8), [purge](../features/01-foundation.md#35-purge-bounded-retention-rows), [checkpoints](../DESIGN.md#contract-01-audit).

- Use toolkit Db lock/try_lock, exact namespace/keys, direct/session-pooled lock route, explicit release and cooperative cancellation. The BSS [coord lease](../../../libs/coord/README.md) is useful precedent but is not the selected scheduler primitive or an external-effect fence.
- Implement bounded idempotency maintenance (60 seconds/500 rows baseline), preserving live executions/controls/evidence; integrate D-188/D-198 resumptions without adding a sixth worker or holding SQL locks across remote calls.
- Deliver daily per-store retention batches (5,000 baseline): Preview diagnostics 7 days, refused audit 90 days, read log 90 days; immutable commercial evidence remains. No registry/outbox deletion through this worker.
- Implement read-only rolling audit verification and separately privileged atomic daily checkpoint/header/member append using consistent snapshots, sorted members, counter/prior-prefix reconciliation and conflict-safe sequence uniqueness. Mismatch alerts and never repairs.
- Stage 5 supplies expiry/auto-void bodies on the same infrastructure. Wire integrity/cleanup/backlog metrics immediately; optional external checkpoint anchoring is separately configured in S6-07.

**Tests/done:** multi-replica lock contention/session loss while old writes continue; safe cleanup/reclaim races; expiry predicate rechecks; all tamper/removal cases; checkpoint rollback and no fork; bounded memory/pass and preservation of immutable rows. Demonstrate 24-hour checkpoint and 30-day full scan capacity before production.

**Delivery (2026-10-07):** [MAINTENANCE](MAINTENANCE.md) records the five scheduled workers on the host's toolkit advisory keys, the attested per-class restricted connections, the bounded retention purge and idempotency cleanup with the D-188/D-198 recovery port, and the per-namespace checkpoint and rolling verification phases with streamed members ([D-209](../DECISIONS.md#d-209--worker-class-connections-the-conditional-sweep-lock-and-streamed-checkpoint-publication-s2-11)). Implemented, independently gap-fix reviewed and locally verified ([gap review](../../../../../artifacts/orders-lifecycle-s2-20261006/S2-11-gap-review.md)). Expiry/auto-void bodies (Stage 5) and recovery bodies (S3/S5) report unavailable until delivered; the capacity demonstration at the production baseline remains a release gate.

## S2-12 — Close the foundation integration milestone

**Depends on:** S2-01–11 and early S6-01/04 reads. **Files:** public REST/local SDK adapter, OpenAPI, gateway throttling config, integration fixtures.

- Wire canonical boundary validation and shared service entry to both REST and SDK. Count every engine-entering request in pre-engine throttling; implement approved gateway caller/Workflow zones and the existing documented per-(caller,order) fallback if the gateway key is unavailable. Never audit a throttle rejection through the engine.
- Register all future operation contracts but expose only delivered, authorized implementations. Keep service-only actions inaccessible to human authoring paths. Validate finite explicit service resource constraints.
- Run actual migration + PEP + engine + audit + producer integration for draft create/edit/cancel where delivered. Audit failure or outbox failure rolls back; readiness loss stops traffic without restart loops.
- Attach tests and source IDs to the evidence ledger; finish the complete source contract acceptance lists, not merely the examples above.

**Exit:** authorized draft capture is operable and inspectable; the shared core is ready for gate/amendment/fulfillment contributions. Successful production submit is not claimed until Stage 3 real-provider gates pass.

**Delivery (2026-10-07):** [MILESTONE](MILESTONE.md) records the D-185 gateway zone bindings (`rl_orders_caller_write` on the five delivered writes, `rl_orders_workflow_write` reserved for S5-08), the Q-26 per-(caller, order) edge limiter consulted before boundary validation with a bounded key store, the delivered-route census (`DELIVERED_OPERATIONS`, `validate_delivered` at startup), the real readiness check (store, producer, engine and reads; loss makes the instance not ready without stopping it), the canonical path-parameter refusals, and the self-managed live HTTP/PostgreSQL E2E (`make e2e-orders-lifecycle`: real gateway, AuthN, rules PDP, PostgreSQL under the restricted runtime login, Event Broker) with the toolkit migration-runner probe and Orders migration 10 as owner prerequisites ([D-211](../DECISIONS.md#d-211--the-foundation-integration-milestone-pre-engine-throttling-bindings-the-delivered-route-census-real-readiness-and-the-live-e2e-host-s2-12)). Implemented, independently gap-fix reviewed and locally verified ([gap review](../../../../../artifacts/orders-lifecycle-s2-20261006/S2-12-gap-review.md)). Draft cancel, Preview, submit, amendments, holds and the Workflow seams remain unavailable; Stage 3 real-provider gates and the deployment conditions in MILESTONE stand.
