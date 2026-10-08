# Execution queue

Generated from [queue.json](queue.json). Next assignment: **S2-02**. S5-01 contract reconciliation is locally verified; schema/repositories and commercial port/profile work can proceed. This document dispatches work; it does not launch workers or provision providers.

Start dependencies and completion dependencies are separate. A workstream may have narrower slices that begin earlier; the original package plan remains authoritative. The graph checks both edge types for cycles. The source package census includes all 70 numbered packages; S4-08 and S5-06 each have early/later splits.

| Workstream | Packages / owner | Start after | Finish additionally after | Deliverable / completion gate |
|---|---|---|---|---|
| `receiver_contract_corrections` | S5-01 — Orders / Workflow / Subscriptions | Local S1 baseline | — | Patch named S5-01 normative/peer errata using existing S1-05 fixtures. Closed predecessor successor, audit-sequence force freshness, async clocks, flat topology and exact failure taxonomy; source/fixture consistency. |
| `commercial_port_profiles` | S3-01 — Orders | Local S1 baseline | — | Define typed supported ports/profiles and narrowed quantity scope from S1-04/05. Compile against existing SDKs; no claimed owner implementation or fabricated required facts. |
| `schema` | S2-02 — Orders | Local S1 baseline | receiver_contract_corrections | Start non-grant schema from S1-02 catalog and S2-01 crate. Finalize grant/control DDL only after receiver_contract_corrections; full migration/FK/grant evidence. |
| `authorization` | S2-03 — Orders / Platform | schema | — | Implement shared current/proposed PEP, locked rechecks and bounded capabilities. Actual provider policy and service identities/grants; no fallback local evaluator. |
| `transactional_collaborators` | S2-05 / S2-06 / S2-07 / S2-08 — Orders | schema, authorization | — | Implement independent idempotency, audit, claims and producer collaborators. Atomicity, collision/replay, real producer/runtime/grants and frozen-vector checks; no dependency on completed transition engine. |
| `engine` | S2-04 — Orders | schema, authorization | transactional_collaborators | Assemble registry/transaction engine against S1-02 contribution interfaces. Complete composition only after transactional_collaborators. |
| `capture_and_workers` | S2-09 / S2-10 / S2-11 — Orders | engine, transactional_collaborators | — | Draft authoring then date preparation; generic maintenance can proceed from collaborators independently. Authorized field classification, date policy, bounded recovery/retention and audit verification. |
| `draft_reads` | S6-01 / S6-04 — Orders | schema, authorization | capture_and_workers | Build current-parent read/logging subset alongside draft authoring. Usable draft subset after capture_and_workers; full projections wait for actual producers. |
| `draft_milestone` | S2-12 — Orders | capture_and_workers, draft_reads | — | Join public REST/SDK, reads, logs and throttling. First usable draft milestone only after real authorization, storage, audit, events and throttle checks. |
| `identity_contracts` | S4-02 — Account Management / Contracts / Platform / Orders | commercial_port_profiles | — | Implement authoritative payer/delegation/Contracts providers; co-design scopes with S2-03. Real provider/grant conformance before consuming gated flows. |
| `pricing_reads` | S3-02 — Orders / Pricing / Products / Platform | commercial_port_profiles, authorization | — | Adapt existing public Pricing/Products reads. Real seller service grants and upstream access; buyer authority separate. |
| `pricing_assessment` | S3-03 — Pricing | commercial_port_profiles | — | Build nonbinding assessment and independent coverage profile with shared commercial evaluator. Complete native evidence and no command effects; reuse pricing_reads conformance when available. |
| `terms` | S3-04 — Subscriptions | commercial_port_profiles, pricing_reads | — | Create minimal Subscriptions SDK/provider for pure terms resolution. Exact supported selected context/term/policy provenance; no dependency on receiver mutation or full assessment. |
| `pricing_policy` | S3-05 — Pricing | commercial_port_profiles, pricing_reads | — | Deliver seller policy discovery and exact deadline mapping. Same authoritative policy used by receipt acceptance. |
| `receiver_read_provider` | S5-06a — Subscriptions | receiver_contract_corrections | — | Create canonical key/occupancy SDK/read storage independently of receiver mutation runtime. Authoritative provenance, active plus admitted-pending linkage and scoped grants. |
| `gate_identity_overlap` | S3-06 — Orders | commercial_port_profiles, identity_contracts, receiver_read_provider | — | Adapt existing owner outputs into shared gate predicates. No ancestry-derived market or legacy-count re-bucketing. |
| `rating` | S3-07 — Rating | commercial_port_profiles, pricing_assessment, terms, gate_identity_overlap | — | Implement exact-binding batched evaluation and complete required figures. Owner-calculated totals/TCV and supported incomplete Preview horizons. |
| `tax` | S3-08 — Tax owner / Product | commercial_port_profiles, rating | — | Assign/implement the indicative tax provider. Full Preview line/total tax verified; no silent zero/reduced Preview. |
| `assessment_preview` | S3-09 / S3-10 / S3-11 — Orders | pricing_reads, pricing_assessment, terms, pricing_policy, gate_identity_overlap, rating, tax, engine, capture_and_workers | — | Orchestrate bounded reads, diagnostics/retention and Preview. Actual complete providers for success; no acceptance/hold command during Preview. |
| `amendment_consent_foundation` | S4-01 / S4-07 / S4-08a — Orders | engine, capture_and_workers, identity_contracts | — | Register version/consent guards, policy election and pure submit-consent contribution. Does not depend on completed submit or standalone acceptance/history. |
| `submit` | S3-12 — Orders | assessment_preview, amendment_consent_foundation, capture_and_workers | — | Reserve durable attempt/version, accept remotely and commit exact frozen evidence. Crash/retry/ownership and audit/outbox/consent/claims commit atomically. |
| `amendments_admin` | S4-03 / S4-04 / S4-05 — Orders | submit, amendment_consent_foundation | — | Implement amendment preparation/commit and separate administrative edit contribution. Sparse versions, invalidated prior approval/consent, no remote repricing on replay. |
| `history_standalone_consent` | S4-06 / S4-08b — Orders | amendments_admin, draft_reads | — | Implement history projections then standalone recording and integrated history extension. S4-06 base version read may land before S4-08b; acceptance extension closes together, no package cycle. |
| `approval_payments` | S4-09 — Workflow / approval policy / Payments | identity_contracts | — | Deliver real order-approval policy and Payments intent/status. Does not wait for full Workflow or amendment event integration. |
| `begin_guards` | S4-10 — Orders | amendment_consent_foundation, approval_payments | — | Implement pure live-consent/payment guard contribution against S1 seam. Must not wait on completed begin-fulfillment handler. |
| `workflow_schema` | S5-02 — Orders / Workflow / Subscriptions | receiver_contract_corrections, authorization | — | SDK operations and repositories consume the sole S2 schema. Typed interfaces can start from S1 contracts; final repositories need schema and PEP. |
| `approval_reflection` | S5-03 — Orders / Workflow | workflow_schema, approval_payments | — | Version-specific reflection and gate orchestration. Full amendment supersession evidence joins amendments_admin; policy provider itself does not depend on it. |
| `begin_and_grants` | S5-04 — Orders | workflow_schema, engine, begin_guards, draft_reads, submit | — | Begin fulfillment and append committed exact-roster grants. Extend reads to actual committed-version data; no dispatch before commit. |
| `receiver_intents` | S5-05 — Subscriptions | workflow_schema | — | Build receiver intent/status/create settlement and durable outcomes. Real scoped Subscriptions engine/OSS integration. |
| `receiver_enforcement` | S5-06b / S5-07 — Subscriptions / Pricing | receiver_read_provider, receiver_intents, begin_and_grants, pricing_policy, pricing_reads | — | Enforce capacity/fences and Pricing holds on all receiver writers. Actual intent/confirmation races and fresh exact-binding eligibility. |
| `workflow_process` | S5-08 — Workflow | approval_reflection, begin_and_grants, receiver_enforcement | — | Build durable flat two-wave process and rebuild recovery. Select/prove Q-10 durable infrastructure; real timers, inbox and dispatch recovery. |
| `pending_payment` | S4-11 — Workflow | approval_payments, begin_guards | workflow_process | Implement durable payment continuation using a selected Q-10 execution substrate. Runtime join uses workflow_process infrastructure; do not require the complete process to begin this contribution. |
| `controls_ack` | S5-09 / S5-10 / S5-11 — Orders / Workflow / Subscriptions | workflow_process, capture_and_workers | — | Install staged hold/revoke/compensation, acknowledgement and post-spawn controls. Pre-spawn controls can start at workflow_schema; exact roster barriers precede terminal effects. |
| `ttl_sweeps` | S5-12 / S5-13 — Orders | authorization, capture_and_workers | — | Revisioned policy, auto-void and state-expiry algorithms in existing workers. No automatic expiry from fulfillment/fulfillment hold; selected production TTL requirements. |
| `overdue_forced_exit` | S5-14 / S5-15 — Orders | workflow_process, controls_ack, ttl_sweeps | — | Overdue observation and two-person force-failure. Fresh committed audit coordinate, user-only grants, unknown evidence and retained uncertain claims. |
| `complete_reads` | S6-01 / S6-02 / S6-03 / S6-04 — Orders | history_standalone_consent, controls_ack | — | Complete projections/collections/audit and current-parent access logging. SQL authorization, exact cursors, separate audit permission and logging before disclosure. |
| `consumer_recovery` | S6-05 — Platform / all consumer owners | transactional_collaborators | — | Integrate actual consumers and supported authenticated DLQ republication. Each relevant consumer runs corpus; release join waits for workflow_process and downstream owners. |
| `integrated_milestones` | S3-13 / S3-14 / S4-12 / S5-16 — Orders | submit, amendments_admin, history_standalone_consent, pending_payment, controls_ack, overdue_forced_exit, complete_reads, consumer_recovery | — | Join all actual commercial/receiver/provider flows and crash matrices. Reference cleanup stays disabled; no S6 production signoff prerequisite for running integration tests. |
| `operations_security` | S6-06 / S6-07 — Platform / Orders / provider owners | integrated_milestones | — | Finish capacity/latency, dashboards, authorization, residency and disaster recovery evidence. Instrumentation begins with each earlier producing package; final measured evidence at this join. |
| `reference_release_disposition` | S6-08 — Pricing / Orders / Subscriptions | submit, receiver_enforcement | — | Keep cleanup disabled with evidence, or separately implement full Pricing-owned closure/drain/usage. No zero-count release; retain references if any prerequisite unavailable. |
| `release_review` | S6-09 — Delivery coordinator / domain owners | operations_security, reference_release_disposition | — | Close every applicable requirement and profile in actual deployment ledger. No local-fixture or generic upstream test stands in for production conformance. |

## Important early slices

- S5-02 typed interfaces may start against S1-02/05 before secure repositories.
- S2-11 maintenance can start from collaborators without waiting for all capture UI.
- S6-01/04 draft reads ship alongside S2-09; full projections join later.
- S4-06 base version history precedes S4-08b; acceptance history extension follows together.
- S4-11/Q-10 infrastructure work must not wait on full S5-08 workflow implementation.

## Every numbered package

| Package | Work | Status / evidence | Source plan |
|---|---|---|---|
| S1-01 | Reconcile the implementation baseline | local_verified; [BASELINE.md](../BASELINE.md) | [01-contracts-and-fixtures.md](../01-contracts-and-fixtures.md) |
| S1-02 | Freeze the public operation and storage contract catalog | local_verified; [contracts/CONTRACTS.md](../contracts/CONTRACTS.md) | [01-contracts-and-fixtures.md](../01-contracts-and-fixtures.md) |
| S1-03 | Prove the authorization and database capability assumptions | local_verified; [CAPABILITIES.md](../CAPABILITIES.md) | [01-contracts-and-fixtures.md](../01-contracts-and-fixtures.md) |
| S1-04 | Freeze commercial SDK profiles and lossless codecs | local_verified; [COMMERCIAL.md](../COMMERCIAL.md) | [01-contracts-and-fixtures.md](../01-contracts-and-fixtures.md) |
| S1-05 | Freeze process, receiver and external-owner contracts | local_verified; [PROCESS.md](../PROCESS.md) | [01-contracts-and-fixtures.md](../01-contracts-and-fixtures.md) |
| S1-06 | Build shared conformance fixtures and fault-injection seams | local_verified; [CONFORMANCE.md](../CONFORMANCE.md) | [01-contracts-and-fixtures.md](../01-contracts-and-fixtures.md) |
| S1-07 | Prepare the execution queue and deployment readiness ledger | local_verified; [READINESS.md](../READINESS.md) | [01-contracts-and-fixtures.md](../01-contracts-and-fixtures.md) |
| S2-01 | Create SDK/runtime crates and Gear wiring | local_verified; [SCAFFOLD.md](../SCAFFOLD.md) | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-02 | Implement the full schema inventory and secure repositories | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-03 | Implement the shared PEP and bounded internal authority | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-04 | Implement the declarative transition engine | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-05 | Implement idempotency and durable execution ownership | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-06 | Implement transactional audit and canonical integrity encoding | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-07 | Implement transactional in-flight overlap claims | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-08 | Integrate typed events and the managed producer | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-09 | Implement draft authoring and field classification | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-10 | Implement date policy and admission-time date preparation | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-11 | Deliver maintenance infrastructure, retention and audit verification | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S2-12 | Close the foundation integration milestone | planned; Implementation remains planned | [02-foundation-and-capture.md](../02-foundation-and-capture.md) |
| S3-01 | Freeze the implementable contract/profile inventory | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-02 | Integrate existing Pricing/Products reads and service authorization | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-03 | Implement Pricing's missing assessment and complete diagnostics producer | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-04 | Implement Subscriptions' terms resolver and policy provenance | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-05 | Implement Pricing policy discovery and accepted deadline mapping | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-06 | Integrate authoritative payer/Contracts and key/occupancy answers | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-07 | Implement Rating exact-binding purchase evaluation | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-08 | Specify and implement indicative tax owner/provider | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-09 | Build bounded shared assessment orchestration and predicate mapping | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-10 | Persist complete authorized diagnostics and retention | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-11 | Expose Preview and verify read-only commercial effects | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-12 | Integrate durable acceptance and atomic submit | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-13 | Integrate reference lifetime and activation-recheck handoff | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S3-14 | Observability and real-provider completion evidence | planned; Implementation remains planned | [03-preview-and-submit.md](../03-preview-and-submit.md) |
| S4-01 | Reconcile implementer-facing contracts and register guards | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-02 | Deliver commercial owner contracts needed by amendments and consent | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-03 | Prepare an amendment through the shared assessment and attempt pipeline | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-04 | Commit amendment and supersession atomically | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-05 | Administrative order and line edits | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-06 | Version and acceptance-history read projections | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-07 | Policy election and consent persistence | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-08 | Customer consent on submit and separate recording | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-09 | Workflow approval and Payments owner packages | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-10 | Begin-fulfillment guard contribution | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-11 | Durable pending payment continuation in Workflow | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S4-12 | Integrated evidence, operations and production scope | planned; Implementation remains planned | [04-amendments-and-preconditions.md](../04-amendments-and-preconditions.md) |
| S5-01 | Resolve executable seam contracts before generating SDKs | local_verified; [RECEIVER_CONTRACTS.md](../RECEIVER_CONTRACTS.md) | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-02 | SDK operations, scoped schema and engine contributions | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-03 | Approval reflections and version-specific Workflow gates | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-04 | Begin fulfillment and atomic dispatch grants | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-05 | Implement Subscriptions intent/status/settlement provider | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-06 | Receiver capacity and attempt fence on every writer | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-07 | Receiver Pricing hold and commercial admission adapter | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-08 | Durable Workflow process, two waves and recovery | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-09 | Staged receiver controls and normal compensation | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-10 | Terminal acknowledgement, mapping and read projection | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-11 | Hold, resume and ordinary cancellation | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-12 | Revisioned TTL policy and promotion channel | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-13 | State expiry and draft auto-void workers | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-14 | Overdue observation and operational alerts | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-15 | Two-person forced failure with explicit unknown outcomes | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S5-16 | Real integration, crash matrix and release evidence | planned; Implementation remains planned | [05-fulfillment-and-controls.md](../05-fulfillment-and-controls.md) |
| S6-01 | Implement coherent current/historical read projections | planned; Implementation remains planned | [06-reads-and-production-readiness.md](../06-reads-and-production-readiness.md) |
| S6-02 | Implement bounded SQL-scoped collections and cursor contracts | planned; Implementation remains planned | [06-reads-and-production-readiness.md](../06-reads-and-production-readiness.md) |
| S6-03 | Implement separately authorized audit retrieval | planned; Implementation remains planned | [06-reads-and-production-readiness.md](../06-reads-and-production-readiness.md) |
| S6-04 | Implement read-access logging and disclosure failure semantics | planned; Implementation remains planned | [06-reads-and-production-readiness.md](../06-reads-and-production-readiness.md) |
| S6-05 | Deliver event consumers and supported operator recovery | planned; Implementation remains planned | [06-reads-and-production-readiness.md](../06-reads-and-production-readiness.md) |
| S6-06 | Implement observability, capacity and latency evidence | planned; Implementation remains planned | [06-reads-and-production-readiness.md](../06-reads-and-production-readiness.md) |
| S6-07 | Prove security, durability, residency and disaster recovery | planned; Implementation remains planned | [06-reads-and-production-readiness.md](../06-reads-and-production-readiness.md) |
| S6-08 | Implement safe revision-reference release, or prove it remains disabled | planned; Implementation remains planned | [06-reads-and-production-readiness.md](../06-reads-and-production-readiness.md) |
| S6-09 | Close every requirement with a release ledger | planned; Implementation remains planned | [06-reads-and-production-readiness.md](../06-reads-and-production-readiness.md) |

Local verification is scoped to the linked report; S1 owner gaps and S2-01 disabled business readiness remain explicit. The queue is not an estimate of equal-sized tasks or a production completion percentage.
