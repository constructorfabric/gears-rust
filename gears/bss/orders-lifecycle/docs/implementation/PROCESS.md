# S1-05 — Process and receiver contract baseline

Status: **local contract proposal and executable specification verified**. This package publishes 19 semantic operation contracts, a reconciled alias register and 151 frozen reference-model cases. It does not deliver the absent provider SDKs, authenticated grants, receiver transactions or owner agreement. Those remain requirements for S1-05's production acceptance, with implementation owners below. No business route or readiness gate is enabled. Local files only; no commit/PR.

Read [contracts.json](process/contracts.json) for request/result fields, authority, retry, failure and package ownership. These field descriptions are a proposed semantic profile, not a registered wire schema or callable API. Owner SDKs must freeze concrete types, bounds, schema versions and errors before consumers bind to them. Reuse [S1-02 public catalog](contracts/CONTRACTS.md) and the [S1-04 native commercial codec](COMMERCIAL.md); do not add competing public operations.

## Existing capability and required owner work

| Owner / inspected implementation | Available now | Missing contract and implementation package |
|---|---|---|
| [Account Management client](../../../../system/account-management/account-management-sdk/src/client.rs) | Tenant and metadata reads, service-account management | Authoritative payer currency/region and named-seller relationship evidence; verifiable delegation issuance/revocation. S4-02, PDP S2-03 |
| Contracts | Design requirements; no Contracts runtime SDK in this tree | Scoped status/party eligibility and live acceptance declaration. S4-02 and S4-08/10 |
| [Ledger SDK](../../../ledger/ledger-sdk/src/api.rs) | Postings, payment settlement/return/allocation | Payments authorization and durable intent status, with explicit amount or postpaid basis. S4-09/11; Ledger is not this provider |
| [Approvals SDK](../../../approvals/approvals-sdk/src/lib.rs) | Source inbox/read/vote forwarding | Order-specific requirement policy and version-bound decisions. S4-09/S5-03; Workflow owns orchestration |
| [Workflow PRD](../../../orders-workflow/docs/PRD.md) | Process design, no Workflow runtime SDK | Durable version-bound gates, payment continuation, flat two-wave orchestration, exact result correlation and recovery. S5-02–10 |
| [Subscriptions seams](../../../subscriptions/docs/SEAMS.md) | Receiver design, no Subscriptions runtime SDK | Canonical key/provenance/occupancy S5-06a; intent/status/create settlement S5-05; every-writer admission/fencing S5-06b/07/09 |
| Pricing / Products | Native commercial receipt, hold and revision-reference protections | S1-04 owner gaps remain; future release coordinator/report protocol S6-08. Keep cleanup disabled |
| [ToolKit dead-letter support](../../../../../libs/toolkit-db/src/outbox/dead_letter.rs) | Durable dead-letter management and replay claim support | Orders producer republication and each consumer's business recovery integration. S2-08/S6-05; a claimed DLQ row is not evidence of successful republication |

Account Management is under `gears/system`; other listed business owners are under `gears/bss`. Existing generic APIs cannot be labeled as delivered commercial profiles. Every provider release needs the agreed SDK/schema version, configured implementation, authenticated principal, explicit grants, supported scopes, evidence freshness and an actual conformance run. S1-03's real PDP limitations remain in [CAPABILITIES](CAPABILITIES.md).

## Commercial owner boundaries

Payer market comes from Account Management's authoritative commercial profile, with named seller, evidence version/source and validity. Caller-supplied metadata cannot establish it. PDP verifies delegated authority at each protected boundary, including issuer/audience/scope/expiry/revocation; Orders forwards proof and never implements a replacement local policy evaluator.

Contracts supplies referenced-contract status and party eligibility. Re-read its live acceptance requirement when recording consent and at begin-fulfillment. An unavailable declaration cannot become an optional contract. Previous-version customer consent cannot satisfy a new version. Pricing's commercial receipt, Lifecycle customer consent and Workflow approval are separate evidence.

Workflow is the Payments caller and approval coordinator. Lifecycle receives version-bound outcomes through its authorized Workflow seam, consistent with D-175. The payment request identity survives restarts and carries exact order/version/payer/currency and explicit authorized amount or postpaid policy. TCV and approval measures are not default payment amounts. Read status using the same intent after ambiguity; do not create a second authorization merely because a response timed out. Only a conclusive failed outcome reaches Lifecycle's tolerance decision; pending, missing, denial and outage stay unresolved. Changed basis needs an owner-agreed replacement protocol.

## Identities, ownership and transitions

The manifest separates ten coordinates: committed commercial version, fulfillment attempt, dispatch generation, control ID, control generation, execution ID, worker owner fence, committed audit sequence, wave attempt and event ID. Dispatch/control generations use the aggregate `fulfillment_control_generation` domain; they are not two invented independent counters. None can substitute for commercial version or execution ownership.

Commercial candidates are permanently reserved; 4→7 is valid. An immutable grant references an actually committed version and exact line/receipt/receiver/create/activation-key roster. It has an engine-issued ID and unique order/generation; the engine appends it atomically with audit/idempotency and exposes it only after commit. A higher integer supplied by Workflow is not authority. Initial spawn is write-once. Settled post-spawn resume and the D-201 internal rebuild continuation append successor authority; pre-spawn resume grants none. Same-generation paused/revoked authority never reopens.

Controls are operational records, not new public states. Their immutable identity includes execution/fingerprint, expected commercial version, attempt/generation, caller proof and full receiver roster. Progress uses prepared/awaiting/barrier_ready/settled/refused/abandoned and conditional worker-owner fencing. One nonterminal control per order owns the pending pointer. Response-key expiration cannot erase it or allow another execution to adopt it. The existing recovery worker resumes it; no sixth worker is introduced.

## Receiver protocol and capacity

The canonical capacity scope is `(payer, resource, catalog subscription product key, declared extra dimensions)`, with authoritative seller/catalog namespace, schema and policy provenance. A legacy per-payer count cannot be re-bucketed. Report active **plus admitted pending** and enough exact order/version/line/attempt linkage to deduplicate both own and other Orders contributions. Ambiguous linkage retains occupancy.

Every activation writer participates: direct activation, Orders activation, resume, transfer and a key-altering plan change. The receiver locks policy generation and enforces unique live key/slot admission in one transaction. Multi-key operations lock in canonical order. An unadmitted draft consumes no slot. An admitted intent consumes capacity before external provisioning; confirmation converts the same pending claim to active. Timeout never frees it. Limit reductions block further admissions without evicting existing occupants. A handover exception requires a durably linked, actually ended predecessor and exactly one successor; TTL is not proof.

Draft create uses an attempt-specific full command key and committed order provenance. Same key/payload replays; changed payload conflicts. `settle_create` serializes with create and returns either `DraftFound` or durable `NoDraft` with a tombstone, including close-before-open. A missing status read alone proves neither. Aged-out command keys are recovered by intent/status reads and reconciliation, not blind re-dispatch.

Activation has two receiver commits. Intent admission checks both fresh Pricing evidence and committed grant/fence authority, reserves capacity and enqueues provisioning atomically. Applied confirmation checks that same attempt fence again. An approved or OSS-unconfirmed intent is not an activated subscription. Late OSS success after closure remains an external effect to reconcile/compensate, with the uncertain claim retained. Correlation identifies the process; transition ID and full source/wave/kind/attempt identify the exact result.

For post-spawn hold/cancel/failure, Orders first commits control preparation without changing public state, timestamps or emitting its transition event. It then releases SQL locks while Workflow installs receiver fences and settles targets. Hold pauses future confirmation and preserves existing active service/slots; draft TTL continues. Terminal control additionally revokes all applicable generations, settles every create key/intent and compensates active members. Partial fan-out is not a barrier. The proof must match control/version/attempt/generation/roster and durable receiver targets. Finalization reauthorizes and checks current facts and execution ownership before the original transition/audit/outbox/response commit. Until then use the existing still-processing behavior, not a new public state or automatic 202 response.

A final denial/version conflict can abandon the local control, but installed receiver fences stay closed; only its current owner can clear its own pointer. Outage remains pending. Forced failure is the separate two-person break-glass path: durable revocation request, fence superseded local control, retain uncertain receiver claims, record unknown compensation and keep manual reconciliation open. Do not interpret the older generic terminal-claim wording as permission to free uncertain D-198 receiver slots.

Ordinary compensation is the exact five-member schema in [DESIGN](../DESIGN.md#contract-01-table-orders_order): both ID arrays, activation-dispatched boolean, at-sale-facts boolean, and no-active-remains **true**. Orders validates structure; Workflow/receivers establish the actual roster barrier. Orders does not query Subscriptions to reconstruct those arrays. Forced evidence is engine-authored with operator attestation and unknown assertions; it cannot pass the ordinary acknowledgement boundary.

## S5-01 reconciliation

These S1-05 targets are now amended normatively by D-201; see [S5-01 delivery and semantic matrix](RECEIVER_CONTRACTS.md). The table records the original target and its implementation handoff. Provider conformance remains open.

| Correction | Fixture targets | Remaining work |
|---|---|---|
| Expired draft rebuild vs immutable roster | Closed predecessor covering every old key/intent; unchanged commercial roster; active members fixed; rebuilt draft keys only; fresh checked successor; no spawn reset; sparse version 7 | D-201 internal continuation is normative and registered in the contract catalog; S5-02/08 implement the guarded writer/SDK |
| Forced request freshness | Exact observed committed audit sequence/state/version, even when timestamps match; distinct authorized operators, approval window and retained uncertain claims | D-201 force_request_observation and audit v3 are normative; S2-02/06 storage/encoding, S5-15 guard integration |
| Async actual start | Quoted instant, intent instant, customer acceptance and applied service instant remain distinct; incompatible terms require reconciliation | D-201 preserves immutable held business activation identity separately from authoritative applied start; supported receiver/Rating profile delivery remains open |
| Flat fulfillment and failure taxonomy | Exact one-line/one-subscription roster, distinct subscription IDs, applied outcomes only; historical graph decoding; explicit `order-binding-expired` | Peer DAG and enumeration prose reconciled; retirement/mismatch stops dispatch for repair or compensated line failure; typed provider conformance remains open |

The forced-freshness oracle covers only the added freshness/retention contribution. Existing state, reason, spawn, overdue-window, request identity and authorization guard ordering still require S5-15 integration tests. Its token fixtures are not SDK wire encodings or actor credentials.

## Event consumers and DLQ recovery

[orders-events.json](process/orders-events.json) freezes all 13 cases required by [C1–C5](../DESIGN.md#contract-01-event-consumer-contract), plus missing-version escalation, historical financial work, crash-before-commit and broker-vs-Orders authority. Each business effect uses durable `event_id` dedup and fresh scoped `get_version(event.version)` plus current-order applicability reads. Root broker access does not grant Orders read access.

Applicability must be declared per event/action. The corpus's `current_version` and `historical_financial` rules are illustrative action policies, not global production handlers: a newer version may obsolete one action while historical financial work still applies. Hold may defer without completing. Unknown state/type is safely completed without effect. Read denial, outage or missing historical data stays durable pending with bounded retry/escalation; it does not mean empty, obsolete or successful. No state is reconstructed from gaps or stream ordering. Effect-free notification/audit can use payload alone with dedup.

Complete the durable processed mark atomically with the effect, or after an independently idempotent effect keyed by event ID. Producer DLQ replay preserves event ID/source/version/payload despite a new broker sequence. Claiming a DLQ record is only a lease step; republication and recovery need separate durable completion. Consumers must handle replay after newer events and restart during pending work. S1-06/S2-08 and the first real Workflow consumer must run the same corpus against their actual storage and broker adapters, including crash after external effect but before processed mark. The local atomic-ledger abstraction does not prove that window.

## Future reference release

The future Pricing-owned coordinator closes admission for **all** holder writers under seller/revision/release generation, drains every potentially committable preparation and uncertain remote outcome, and obtains authoritative same-generation usage. Orders reports nonterminal holders/outstanding preparations; Subscriptions reports pending/active holders. Transfer establishes receiver ownership durably before Orders drops its protection; duplicate protection is safe, a zero-holder gap is not. Key/lease expiry alone does not prove an attempt cannot commit.

Only Pricing writes the Products release after every closure/drain/usage proof succeeds. Missing, denied, unavailable, stale or partial reports retain protection. [Boundary cases](process/boundary-cases.json) include a future eligible proof and negative cases, but `production_release` remains **false**. S6-08 owns durable release/recovery and owner implementations; no count-based cleanup is enabled.

## Verification and next assignment

```sh
python3 gears/bss/orders-lifecycle/docs/implementation/process/check_contracts.py
python3 gears/bss/orders-lifecycle/docs/implementation/contracts/contract_checks.py
python3 gears/bss/orders-lifecycle/docs/implementation/commercial/check_contracts.py
python3 gears/bss/orders-lifecycle/docs/implementation/validate_docs.py
python3 gears/bss/orders-lifecycle/docs/implementation/build_coverage.py --check
```

The process suite checks **25 serialized receiver traces, 17 consumer traces and 109 boundary cases**, plus operation-source references and the required consumer inventory. Expected outcomes are checked-in JSON; the runner never generates them. The model uses symbolic identities and verified-evidence inputs, not real signatures, UUID codecs, authorization or external effects. All-writer fixtures exercise shared capacity after admission authorization is assumed; they do not require direct writers to acquire Orders grants. Serialization enumerates selected race orders; PostgreSQL locking/isolation, multi-key deadlocks, authenticated SDK/provider conformance, forced guard ordering and operational recovery remain separate evidence. No Rust/dependency/runtime files changed in this step.

Observed on 2026-10-06: all 151 process cases passed; the existing 73 boundary cases and eight golden envelopes passed; all 36 commercial cases and the 19-node dependency graph passed. Documentation validation checked 4274 local links and the unchanged 24-table/11-event/29-row/21-trigger/11-state inventory. Coverage verified 27 sources, 2557 units and 44 upstream requirements. `git diff --check` passed; the new process files were checked separately for syntax and whitespace. Rust suites were not rerun for this documentation/fixture-only change.

S1-06 local fixtures and harness are recorded in [CONFORMANCE](CONFORMANCE.md). S1-07 inventory is in [READINESS](READINESS.md). S5-01 normative amendments and updated fixture evidence are in [RECEIVER_CONTRACTS](RECEIVER_CONTRACTS.md). Next: **S2-02 — schema and secure repositories**. Owner SDK and production approval remain separate work.
