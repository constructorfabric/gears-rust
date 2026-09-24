# Medium-register disposition — 2026-09-23

Source: [Orders review register](https://artifacts.os.jele.io/orders-design#register).
Scope: documentation corrections, not runtime implementation. Previously corrected Mediums
remain covered below so all 28 are accounted for.

| Finding | Current disposition |
|---------|---------------------|
| OL-5 | Corrected: conflict-safe registry claim; Foundation §3.6 |
| OL-6 | Corrected: explicit infrastructure-error rollback and return; Foundation §3.6 |
| OL-7 | Corrected: aggregate contribution writer mapping; Foundation §3.6 |
| OL-11 | Corrected: idempotency expiry and cleanup contract; Foundation §§3.6–3.8 |
| OL-15 | Corrected: immutable audit grants and refusal settlement classes reconciled; Foundation §§3.7, 4.1, 4.4 |
| OL-16 | Corrected: complete Foundation mutability inventory; Foundation §3.7 |
| OL-18 | Superseded by platform producer; no custom drain double return |
| OL-23 | Corrected: shared PolicyEnforcer integration; Read/Authz §§3.5, 4.3; deployed policy verification remains open |
| OL-24 | Corrected: dependency/worker inventories reconciled; master §§3.4–3.5, Foundation §3.8 |
| OL-25 | Corrected: toolkit session advisory locks explicitly selected, limitations disclosed; Foundation §3.8 |
| OL-28 | Corrected contract: assessment ID, trusted scope and complete atomic outcome vector; Gate §3.7. Foundation §§3.6–3.7 now explicitly persist refusal diagnostics and bind replay to the stored assessment/response; engine-only refusals carry no assessment. Scoped operational diagnostic access remains implementation evidence |
| OL-29 | Corrected seam: begin-fulfillment commits first, Workflow executes owning-SDK rechecks, failure is reachable from in_fulfillment; Gate §3.6, Workflow §4.3. SDK availability remains prerequisite |
| OL-30 | Corrected promise: Lifecycle projection is acknowledgement-only; live progress uses Workflow's specified progress read; Workflow §3.2, UPSTREAM_REQS §2.6 |
| OL-31 | Corrected: full transition/counter graph gives 74 TTL-covered dwell entries, with policy and scheduler qualifications; Hold/Expiry §4.2 and D-90 |
| OL-35 | Live PR confirmed stale; replacement description prepared locally, NOT published; see adjacent PR-description draft |
| OL-41 | Corrected citation: per-principal scope qualifies the idempotency NFR, not AC-4; Foundation §4.2. Product ratification remains open |
| OL-42 | Corrected: GTS category text in both aggregate and version; Foundation §3.7 |
| OL-43 | Corrected: all three state-diagram divergences listed; Foundation §4.3 |
| OL-48 | Corrected: admin expected_version, engine-only version links/pointer, amendment explanation column, reachable version-not-found read outcome; Foundation §3.7, Versioning §3.6/3.7, Read/Authz §3.6 |
| OL-49 | Corrected disclosure: cross-seller refusal narrows the PRD MUST; no conformance claim; Versioning §2.2, master §1.2, Q-28 remains open |
| OL-53 | Corrected: summed operation deadlines are conservative ceilings, not a parallel-latency claim; Gate §2.2 |
| OL-54 | Corrected: market input order, engine-enforced pin presence, required upstream cardinality, one date reason, full passed/failed outcome vector; Gate §§3.3–3.7, 4.2 |
| OL-59 | Corrected: Workflow reference fork/count wording, spawn writer, PDP service authorization, correlation writer, canonical trigger; Workflow §§3.6–4.6 and upstream/index references |
| OL-63 | Corrected: shared category guard, acceptance path writer, policy-input failure route, consolidated currency/payer reasons, UTC dates and identity-membership lifecycle; Foundation §3.6/3.7, Capture §4.2, Preconditions §3.6 |
| OL-66 | Corrected: explicit logging writer for every REST/SDK read; shared Read/Authz §3.6 wrapper |
| OL-67 | Corrected: missing/invalid proof and action denial mappings, line index, all collection bounds, MUST NOT wording; Read/Authz §3.6, Foundation §3.7 |
| OL-71 | Corrected: explicit draft-sweep specialization, key/inputs/cadence/batch and draft missing-TTL observation; Hold/Expiry §4.4 |
| OL-72 | Corrected: seller-only TTL selection, effective pre-hold cancellation state, fallback indexes, unused-reason removal and gauge replacement/reset; Hold/Expiry §§3.6, 4.4, 4.6 |

## Verification and boundaries

Read-only graph enumeration checked caps (amendments,resumes) (0,0), (20,0), (0,5), (20,5),
producing maxima 4, 64, 14, 74 including a final hold. Existing-order state transitions are the
source; draft/fulfillment and scheduling delays are intentionally excluded from that count.
The design requires equivalent implementation regression coverage, not a new production tool.

Validation passed: TOCs for the current design/ADR set, local links across 22 documents,
eleven contiguous event rows, all 28 Medium IDs, superseded-contract searches and
`git diff --check`. Runtime, schema migration and external integration tests are not executed
by this documentation change. PR publication and upstream owner agreement are not implied.

Required implementation regression coverage includes: category changes through every commercial
path; separate immutable amendment explanations; removal/reinsertion without line-ID reuse or
phantom current lines; both acceptance paths and unavailable policy resolution; multi-run and
cross-tenant Preview diagnostics including passed outcomes; cardinality greater than one and
missing limits; pre-activation failure before/after draft creation; all read-route log/page/proof
branches; held-fulfillment cancellation after the spawn fence; seller-only/draft TTL configuration,
gauge reset and restart. Existing platform contracts must be verified before these tests can
establish end-to-end readiness.
