<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Foundation (Design, Slice 1) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Pricing team -->

# DESIGN — Foundation (Slice 1)

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-design-slice-01`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Execute a scoped mutation](#execute-a-scoped-mutation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [conditional-store](#conditional-store)
  - [replay-and-audit](#replay-and-audit)
  - [toolkit-outbox](#toolkit-outbox)
- [4. States (CDSL)](#4-states-cdsl)
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

**Delivery:** phase 2c. Every checkbox is an implementation obligation, not an assertion about the legacy code.

Provide the fresh schema, scoped repositories, conditional approval Store, canonical errors, replay, audit and live toolkit outbox infrastructure.

Requirements: `cpt-cf-bss-pricing-nfr-authz`, `cpt-cf-bss-pricing-nfr-audit`, `cpt-cf-bss-pricing-nfr-tenant-isolation`, `cpt-cf-bss-pricing-nfr-two-backends`, `cpt-cf-bss-pricing-nfr-idempotency-concurrency`. Architecture: `cpt-cf-bss-pricing-component-books`, `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-principle-business-content-fingerprint`, `cpt-cf-bss-pricing-constraint-two-backends`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-constraint-one-replay-store`.
[FEATURE](../features/foundation.md) owns the executable flow/algorithm/DoD identifiers; this slice defines no duplicate DoDs.
Dependencies: shared bss-approval and toolkit infrastructure.
Source: PriceBook spec §2.2, §5–§8, §12–§13 and [DECISIONS](../DECISIONS.md) D-384–D-433.

## 2. Actor Flows (CDSL)

### Execute a scoped mutation

Actors: `cpt-cf-bss-pricing-actor-finance-manager`, `cpt-cf-bss-pricing-actor-auditor`. Feature flow: `cpt-cf-bss-pricing-flow-foundation`.

1. [ ] - `p1` - Authenticate and derive AccessScope from PolicyEnforcer; require correlation and the operation preconditions. - `inst-foundation-flow-1`
2. [ ] - `p1` - Resolve client-key replay before any reservation or approval work; reject payload mismatch. - `inst-foundation-flow-2`
3. [ ] - `p1` - Run the owning domain mutation in a bounded transaction retry, cloning per-attempt inputs and retaining typed database errors. - `inst-foundation-flow-3`
4. [ ] - `p1` - Claim and answer replay with domain state, audit and outbox in the same transaction; return the new ETag after commit. - `inst-foundation-flow-4`
5. [ ] - `p1` - On ambiguous commit, reconcile the stored answer/object before compensating a reference; on definite failure roll back the attempt. - `inst-foundation-flow-5`

## 3. Processes / Business Logic (CDSL)

### conditional-store

Feature algorithm: `cpt-cf-bss-pricing-algo-foundation-conditional-store`.

1. [ ] - `p1` - Load tenant-scoped unit and item versions. - `inst-foundation-conditional-store-1`
2. [ ] - `p1` - Acquire pending ownership only where pending_unit_id is null and the observed version matches; zero affected rows is PRICE_LOCKED_PENDING. - `inst-foundation-conditional-store-2`
3. [ ] - `p1` - Bump unit version conditionally; a lost race returns UNIT_CONTENDED without a second decision or apply. - `inst-foundation-conditional-store-3`
4. [ ] - `p1` - Clear locks only for the owning unit, following unit, entry-id, price-id, revision, promotion order. - `inst-foundation-conditional-store-4`

### replay-and-audit

Feature algorithm: `cpt-cf-bss-pricing-algo-foundation-replay-and-audit`.

1. [ ] - `p1` - Look up tenant, concrete endpoint and client_key within the 24-hour retention and compare payload hash. - `inst-foundation-replay-and-audit-1`
2. [ ] - `p1` - Claim the key inside the mutation transaction; simultaneous claims cannot both mutate. - `inst-foundation-replay-and-audit-2`
3. [ ] - `p1` - Persist immutable attributed audit and response status/body with state; a committed UNIT_STALE refresh has a committed replay receipt too. - `inst-foundation-replay-and-audit-3`
4. [ ] - `p1` - Rollback leaves no partial claim, state, audit or success event. - `inst-foundation-replay-and-audit-4`

### toolkit-outbox

Feature algorithm: `cpt-cf-bss-pricing-algo-foundation-toolkit-outbox`.

1. [ ] - `p1` - Install toolkit outbox migrations under bss_pricing_outbox in DatabaseCapability. - `inst-foundation-toolkit-outbox-1`
2. [ ] - `p1` - Encode domain payloads through TypedEvent using the Products envelope sink pattern. - `inst-foundation-toolkit-outbox-2`
3. [ ] - `p1` - Append through the caller transaction and dispatch only committed outbox rows. - `inst-foundation-toolkit-outbox-3`
4. [ ] - `p1` - Wire lifecycle-managed delivery and retry; failure leaves a durable record rather than an unrelayed pricing_outbox row. - `inst-foundation-toolkit-outbox-4`

## 4. States (CDSL)

Replay moves absent → claimed → answered within the operation transaction, and rollback restores absence. Audit moves unsealed → sealed only through the record-preserving guard. Conditional version v → v+1 admits one writer. Outbox delivery never precedes commit, and the outbox's sequencer is woken only after it (D-455).

State definition: `cpt-cf-bss-pricing-state-foundation` in the FEATURE.

## 5. API Surface

No business route is introduced by foundation. Door adapters preserve headers + Bytes, preconditions::parse_body, correlation::establish and canonical RFC-9457 errors; later features mount their operations.

[DESIGN §3.3](../DESIGN.md#33-api-contracts) fixes canonical errors and route prefixes.
Each mounted route must appear in all four censuses with authz and precondition expectations.

## 6. Data Model

DESIGN §3.7 is the phase 2 schema: pricing_settings, pricing_dimension_key, pricing_price_book, pricing_price_book_entry, pricing_price, four pricing_approval tables, pricing_audit, pricing_idempotency and pricing_reference_op. Toolkit supplies bss_pricing_outbox tables. Replay has one key per tenant/endpoint/client_key; approval children are accessed through scoped units. Migrations are replay-safe on both engines, with no legacy data conversion.

Tenant-scoped parent validation is required even where foreign keys use entity ids. Never substitute a
cross-gear read for transactional local ownership/version guards. Approved money and historical pins survive.

## 7. Events & Alarms

Foundation supplies durable delivery infrastructure; slice 07 specifies payloads. Failed audit/outbox inserts roll back the owning act. Dispatch/retry exhaustion is observable; no success publication is fabricated for a rollback.

Audit and outbox inserts use the same mutation transaction; retry is lifecycle-managed and observes shutdown.

## 8. Definitions of Done

The sole definitions live in [features/foundation.md](../features/foundation.md):

- `cpt-cf-bss-pricing-dod-tables-two-backends` — Fresh schema on both backends.
- `cpt-cf-bss-pricing-dod-scoped-repositories` — Scoped repositories.
- `cpt-cf-bss-pricing-dod-audit-append-only` — Append-only audit.
- `cpt-cf-bss-pricing-dod-idempotency-key-store` — One replay store.
- `cpt-cf-bss-pricing-dod-if-match-version` — Conditional object versions.
- `cpt-cf-bss-pricing-dod-unit-store` — Conditional approval Store.
- `cpt-cf-bss-pricing-dod-outbox-same-tx` — Atomic event writes.
- `cpt-cf-bss-pricing-dod-outbox-toolkit` — Toolkit delivery integration.

## 9. Acceptance Criteria

1. PRD AC #24 / `cpt-cf-bss-pricing-dod-tables-two-backends`: Given empty SQLite and Postgres databases, when the chain runs twice then both schemas remain correct; duplicate book/entry/approved-start keys are refused.
2. PRD AC #23 / `cpt-cf-bss-pricing-dod-scoped-repositories`: Given two tenants and a foreign child id, when a scoped repository reads or writes then no other tenant data is exposed or changed.
3. PRD AC #22 / `cpt-cf-bss-pricing-dod-audit-append-only`: Given a successful act, when audit is read then actor/subject/correlation are present; an injected audit failure rolls back the act and direct deletion fails.
4. PRD AC #25 / `cpt-cf-bss-pricing-dod-idempotency-key-store`: Given a retained key, when the same request repeats then its response replays without mutation; another payload conflicts and simultaneous claims produce one act.
5. PRD AC #25 / `cpt-cf-bss-pricing-dod-if-match-version`: Given a current ETag, when one update succeeds then its successor token changes; a second update using the old token returns STALE_REVISION and omission fails preconditions.
6. PRD AC #24 / `cpt-cf-bss-pricing-dod-unit-store`: Given two writers with one observed unit version, when they contend then only one applies; the loser cannot leave a partial vote or terminal audit.
7. PRD AC #22 / `cpt-cf-bss-pricing-dod-outbox-same-tx`: Given an approval needing events, when outbox insertion fails then state and audit roll back; success persists all three.
8. PRD AC #22 / `cpt-cf-bss-pricing-dod-outbox-toolkit`: Given a committed event and interrupted dispatch, when delivery resumes then its durable envelope is retried; an uncommitted transaction produces no delivery.

## 10. Non-Functional Considerations

All paths enforce tenant isolation, deny-by-default authz and append-only attribution. Mutations use conditional
versions, required POST replay and PATCH/PUT preconditions; PostgreSQL serializable chain changes and SQLite
writer serialization preserve the same invariants. A failed audit/outbox write cannot leave a committed act.
No timeout releases a live reference. Review and apply retain typed database failures for bounded retry.
Implementation gates cover both backends and route censuses; document gates cover toc, language and identifier ownership.
