# Implementation review and dispositions

Reviewed scope: Orders Lifecycle PRD, DESIGN, DECISIONS, DECOMPOSITION, eight features, eight ADRs and UPSTREAM_REQS; related Workflow, Subscriptions and Rating contracts, with existing Pricing, Products, Approvals, Account Management, Ledger, coordination and Event Broker code consulted for implementation precedents. This review produces work assignments, not a claim that every contract is already implemented.

S1-01 dispositions are recorded in [BASELINE](BASELINE.md). The findings below describe the reviewed starting state; applied corrections and remaining package gates are distinguished there.

## Corrections before dependent implementation

| Finding | Required disposition | Package |
|---|---|---|
| Old broker TODO prose conflicts with selected D-200 and existing broker runtime | Replace obsolete absence claims with concrete SDK integration, deployment grants and recovery evidence requirements | S1-01, S2-08, S6-05 |
| “Every failure in one transaction” and blanket no persisted fence statements conflict with D-188/D-198 remote protocols | Narrow ordinary transactional guarantees; explicitly document durable commercial attempts, dispatch grants and staged control continuations | S1-01/02, S2-05, S5-04/09 |
| Legacy full pin `items[].chains[]` and unresolved-R12 prose survived reconciliation | Use D-192 schema-2 query/receipt/selected bindings and D-198 chosen receiver contract; distinguish selected design from missing implementation | S1-01/04, S3-01, S6-01 |
| Diagnostic default-slot ordering can collapse absent dimension and literal `default` | Use tagged native selection identities and canonical ordering throughout codec, diagnostics and fixtures | S1-04/06, S3-09 |
| Component headings reuse explicit anchors | Preserve first occurrence, uniquely anchor later headings, update dependent links; coverage records source lines to disambiguate today | S1-01 |
| Audit source citation points to nonexistent Pricing domain file; actual Pricing audit is unsealed | Correct citation and reuse transactional writing only; implement Orders hash v1/v2, verifier/checkpoints and roles explicitly | S1-01, S2-06/11 |
| Nullable platform-scope fields cannot be nullable primary keys in PostgreSQL | Use a surrogate identity and correct business uniqueness for policy tables, with platform-row uniqueness proven on supported databases | S2-02, S4-01/07, S5-12 |
| Amendment prose still presents Q-12 as an undecided algorithm despite D-174 | Preserve selected two-step reapproval; reconcile PRD wording without inventing a direct edge | S1-01, S4-01 |
| Consent lookup can assume dense version 2 | Find actual committed submit version by reason/role; sparse D-188 reservations are not public versions | S4-08 |
| Tax authority is not delivered, and terms/assessment dependencies can form a cycle | Assign and build real tax owner; terms resolves from selected read context before assessment/Rating, not from completed assessment | S3-04/08/09 |
| Old 2.25/2.5-second port budgets omit new calls and do not satisfy the sub-second SLO | Rebuild call graph/budgets, measure full governing path, preserve Q-11/Q-16 until evidence resolves them | S1-04, S6-06 |
| Workflow topology language conflicts with selected flat execution; expired draft replacement can conflict with immutable grant roster | Reconcile flat process and explicit successor grant/dispatch generation before schema/receiver implementation | S5-01/02/08 |
| Force-fail approval compared only by timestamp can admit equal-time stale evidence | Bind second-person approval to exact audit/control generation and preserve unknown receiver outcomes | S5-01/15 |
| Stage-wide ordering hides early payer/Contracts/occupancy providers and creates consent/submit cycles | Start S4-02/09 and S5-06a early; split S4-08a pure consent from later recording; one owner per shared component | README execution order |
| Security plans must prove structural restrictions, not only runtime denial | Add D-184 trybuild escape tests, restricted allow_all lint and read-only transaction write rejection to blocking CI | S2-03, S6-07 |
| Repository guidelines link a missing RUST.md | Record stale guideline link; use existing ToolKit/security/dependency/testing guidance. Do not invent absent instructions | S1-01 |

Detailed stage-specific findings and source references are also retained in the six plans.

## Existing code: what to reuse and where it differs

| Inspected precedent | Reuse | Difference that must survive adaptation |
|---|---|---|
| [Products Gear](../../../products/products/src/gear.rs), [PEP](../../../products/products/src/authz.rs) | ToolKit registration and scoped provider wiring | Orders has three tenant axes and treats invalid provider constraints as integration faults; do not copy single-owner semantics |
| [Pricing commercial acceptance](../../../pricing/pricing/src/infra/commercial_terms/check.rs) | Typed owner operations, replay before effects | Orders adds a durable reserved commercial attempt before remote acceptance; local rollback cannot undo remote success |
| [Pricing audit repository](../../../pricing/pricing/src/infra/storage/repo/audit_repo.rs) | Same-transaction audit insert | This is not Orders cryptographic integrity, append-role separation or verifier implementation |
| [Pricing events](../../../pricing/pricing/src/infra/events.rs) | Transactional producer/outbox and post-commit wake | Orders production readiness requires real broker/grants; a pending fallback is not delivery evidence |
| [Approvals cursor](../../../approvals/approvals/src/domain/cursor.rs) | Versioned validated cursor technique | Orders needs its own principal/filter/parent bindings and five collection orderings |
| [Account Management client](../../../../system/account-management/account-management-sdk/src/client.rs) | Existing authenticated identity access | Commercial profile/delegation owner contracts remain explicit delivery work |
| [Ledger API](../../../ledger/ledger-sdk/src/api.rs) | Establish existing accounting boundaries | Posting/settlement does not implement payment authorization/status |
| [BSS coordination](../../../libs/coord/README.md) | Understand existing lease semantics | Orders selected scheduler uses toolkit DB session advisory locking, not a new TTL-lease worker fence |
| [Event Broker outbox](../../../../system/event-broker/event-broker-sdk/src/producer/outbox.rs) | Existing managed DbProducer platform | Consumer deduplication, live rereads, deployed grants and recovery are still owner integration obligations |

## Product questions and release boundaries

The source register remains authoritative for full wording and named decision owners: [DECISIONS — Open questions](../DECISIONS.md#open-questions). All 35 rows are routed below. “Selected” means preserve the selected behavior; it does not mean its provider exists.

| Questions | Implementation treatment / destination |
|---|---|
| Q-01 | Future change/renewal classification deferred; S1-02 admits selected new_sale only |
| Q-02 | Closed one-line/one-subscription mapping; S5-10 tests uniqueness |
| Q-03 | Do not invent quantity caps; S3-01/07 maps supported Pricing profiles and records broader product scope |
| Q-04 | Source-qualified semantic alias map delivered in S1-01; SDK details remain S5-01 |
| Q-05, Q-40 | D-198 selected customer dimension; real occupancy/admission implementation S5-06a/b |
| Q-06 | Revisioned provisional TTLs, seller override off; S5-12 |
| Q-07 | Draft provisional 90-day policy; commercial record deletion deferred; S5-12/13 and S6-08 preserve references |
| Q-08 | Authorization-only payment scope, no collection/refund invention; S4-09/11 |
| Q-09 | Workflow approval authority selected; provider S4-09, reflection S5-03 |
| Q-10 | Select/prove durable Workflow infrastructure before S5-08 and S4-11 completion; inspect existing gears before choosing |
| Q-11, Q-16 | Full-path latency evidence S1-04/S6-06; no unsupported 30-second replacement target |
| Q-12 | PRD/feature/live register reconciled to selected two-step D-174 in S1-01; implementation S4-01 |
| Q-13 | Closed canonical version-conflict reason; S1-02 |
| Q-14 | Closed date cascade; S2-10 |
| Q-15 | Real gate providers and supported-profile release gate; S3-14/S6-09 |
| Q-17 | Reconcile staleness language with D-191 actual hold_until and owner policy; do not equate state TTL with accepted receipt deadline; S1-01/S3-05 |
| Q-18 | Preserve conservative Partner Admin hold denial until product changes it; S2-03/S5-11 |
| Q-19 | Closed no Orders redrive API; platform recovery S6-05 |
| Q-20 | Audit-read product/privacy scope remains explicit; separate grant and release disposition S6-03 |
| Q-21 | D-189 nonbinding Preview retains diagnostic-only writes; reconcile PRD wording, S3-10/11 |
| Q-22 | Preserve selected draft auto-void edge; reconcile PRD, S1-01/S5-13 |
| Q-23 | Enumerate all operations despite abbreviated PRD list; S1-02 |
| Q-24 | Preserve separate commercial-version reasons and state audit reasons; S1-02/S4-06 |
| Q-25 | Closed read-before-effects consumer rule; S6-05 |
| Q-26 | Per-caller gateway plus documented fallback; distributed per-order/global limits require explicit capacity evidence; S2-12/S6-06 |
| Q-27 | Closed finite production state TTLs; S5-12 |
| Q-28 | Cross-seller payer transfer unsupported; preserve cancel/reorder limitation; S4-03 |
| Q-29 | Binding quote artifact is outside scope; draft/Preview is not such an artifact; S3-11 |
| Q-30 | Customer credential and acceptance surface is a partner-launch dependency; S4-08/S6-09 |
| Q-31 | Selected resume cap remains, PRD qualification retained; S5-11/12 |
| Q-32 | D-190 accepted price survives ordinary successor with fresh eligibility/admission; S5-07/16 |
| Q-33 | Changes/add-ons on live subscriptions are a later commercial profile; S1-02/S3-01 preserve scope |
| Q-41 | OrderAmended fires on amendment, not every version append; S1-02/S4-04 |

No question above authorizes silently weakening fail-closed behavior. A supported subset can launch only with explicit feature/profile scope and evidence; incomplete providers, partner onboarding and product-sensitive read surfaces remain visible in S6-09's release ledger.
