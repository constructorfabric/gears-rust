<!-- migration-note: index for the Billing Ledger design set, converted from the legacy Virtuozzo design set at vhp-architecture/docs/bss/design/DESIGN-billing-ledger-balances-202606091200/ (original README + slice docs 00–07). Mirrors the source layout: a foundation design plus per-slice design docs, ordered by implementation phase. -->
<!-- CONFLUENCE_TITLE: [BSS]: Billing Ledger — Design Set (index) -->
<!-- Related: ../PRD.md, ../ADR/ | Owners: @vstudzinskyi (BSS Billing Platform team) -->

# Billing Ledger & Balances — Design Set

This folder holds the Billing Ledger technical design as a **set of slice designs**, mirroring the layout of the source design set. Every slice posts **through** the shared Repository-Foundation ([`01-repository-foundation.md`](./01-repository-foundation.md)) — the double-entry posting engine, schema, universal invariants, total lock order, and in-process data-access API. The Foundation owns no domain policy; each slice below is a handler that builds balanced lines and calls the Foundation API.

Requirements (WHAT/WHY) live in [`../PRD.md`](../PRD.md); the "why this way" rationale for the book-ownership decision is captured as an ADR in [`../ADR/`](../ADR/).

## Slices (ordered by implementation phase)

The numeric prefix = **implementation order** (the ratified phasing). It is **deliberately not** the canonical PRD slice number: slices are numbered by PRD decomposition but built in dependency order (e.g. ASC 606 recognition is PRD Slice 4 but built in Phase 3 because adjustments depends on its schedules; reconciliation-export is design Slice 7 but PRD S7 = ASC 606 Compliance — the two axes never line up; see [`01-repository-foundation.md` §4.1](./01-repository-foundation.md#41-naming-glossary-discipline-and-module-alignment)).

| Doc | PRD slice # | Phase | What it is |
|-----|-------------|-------|------------|
| [`01-repository-foundation.md`](./01-repository-foundation.md) | 1 (+ naming, was slice 8) | 0/1 | **Foundation**: shared engine — journal, balance caches, commit trigger, lock order, idempotency, money, provisioning, the data-access API. Everything posts through it; built first. Also carries the naming/glossary discipline and the three ledger-wide normative statements (§4). |
| [`01a-invoice-posting.md`](./01a-invoice-posting.md) | 1 | 1 | Invoice-post handler: legs (DR AR / CR Revenue + Contract-liability + Tax), account mapping, suspense routing, AR aging, full reversal. |
| [`02-audit-immutability-observability.md`](./02-audit-immutability-observability.md) | 6 | starts in 1, completed in 6 | Tamper chain, freeze, secured store, PII/erasure, alarm catalog. Tamper-evidence is mandatory in prod from the first post (launch blocker); PII/erasure/audit-packs land in Phase 6. |
| [`03-payments-allocation.md`](./03-payments-allocation.md) | 2 | 2 | Settlement, allocation (Mode A), chargebacks/disputes, wallet. First business value: AR is actually cleared. |
| [`04-asc606-recognition.md`](./04-asc606-recognition.md) | 4 | 3 | Recognition schedules + recognition runs, `ScheduleBuilder`. Built earlier than its canonical number — adjustments depends on its schedules. |
| [`05-adjustments-notes-refunds.md`](./05-adjustments-notes-refunds.md) | 3 | 4 | Credit/debit notes, refunds, manual governance. Needs the Phase-2 counters and Phase-3 schedules. |
| [`06-fx-multicurrency.md`](./06-fx-multicurrency.md) | 5 | 5 | Functional currency, realized FX, rate snapshots. A purely additive layer over 01–05. |
| [`07-reconciliation-export.md`](./07-reconciliation-export.md) | 7 | 6 | Reconciliations, ERP export, period close gate. Only makes sense once all posting flows exist. |

## Dependency order

```text
01-repository-foundation (shared engine, schema, invariants, data-access API)
    │
    ├─→ 01a-invoice-posting            (Phase 1)
    │       ├─→ 03-payments-allocation (Phase 2)
    │       ├─→ 04-asc606-recognition  (Phase 3)
    │       │       └─→ 05-adjustments-notes-refunds (Phase 4, also needs 03)
    │       └─→ 06-fx-multicurrency    (Phase 5, additive over 01a–05)
    │
    ├─→ 02-audit-immutability-observability (starts Phase 1, completes Phase 6)
    └─→ 07-reconciliation-export       (Phase 6, needs all posting flows)
```

- `01a-invoice-posting` needs only the Foundation: it is the first posting flow.
- `03-payments-allocation` needs invoice-posting — there must be AR to settle/allocate.
- `04-asc606-recognition` needs invoice-posting (schedules derive from posted legs); built **before** adjustments precisely because adjustments depends on its schedules.
- `05-adjustments-notes-refunds` needs **both** the Phase-2 wallet/counters (03) and the Phase-3 recognized/deferred split (04).
- `06-fx-multicurrency` is additive over 01a–05: it activates the Foundation's native functional columns and the multi-currency trigger relaxation.
- `02-audit-immutability-observability` starts in Phase 1 (Mode S tamper chain is a launch blocker) and completes in Phase 6; it protects every posting flow.
- `07-reconciliation-export` needs all posting flows: the period-close gate and cross-system reconciliations only make sense once every flow that posts into a period exists (a minimal OPEN→CLOSED close subset ships in MVP).

## Cross-cutting / normative

The three ledger-wide normative statements and the naming discipline live in the Foundation design (§4); the ledger-ownership predicate additionally has an ADR:

- **Naming & glossary discipline** — [`01-repository-foundation.md` §4.1](./01-repository-foundation.md#41-naming-glossary-discipline-and-module-alignment) (`journal_entry`/`journal_line` not `LedgerEntry`; `UNALLOCATED` ≠ `REUSABLE_CREDIT`; `SUSPENSE` = mapping parking only; chargeback holds in `DISPUTE_HOLD`).
- **Foundation schema ownership** — [`01-repository-foundation.md` §4.2](./01-repository-foundation.md#42-foundation-schema-ownership-normative).
- **Call-driven ingestion** — [`01-repository-foundation.md` §4.3](./01-repository-foundation.md#43-call-driven-ingestion-model-normative).
- **Ledger-ownership predicate** — [`01-repository-foundation.md` §4.4](./01-repository-foundation.md#44-ledger-ownership-predicate-normative) · ADR [`0001`](../ADR/0001-cpt-cf-bss-ledger-adr-book-ownership-predicate.md).

## Deferred to future scope (post-MVP)

Cross-currency conversion (rejected in MVP — payments-allocation rejects `ALLOCATION_CURRENCY_MISMATCH`; the conversion-event mechanism is a deferred extension of fx-multicurrency), the statutory allocation registry, contract assets / unbilled, bad-debt / write-off / recovery, the full variable-consideration mechanism, escheatment filing, free-form GL, inter-tenant settlement / reseller payout, `NUMERIC(38,0)` money (`BIGINT` minor units confirmed for MVP), the ledger-side payer re-validation guard against the tenant tree, and historical / as-of temporal balance (reconstructable from `journal_line`). Each slice carries its own deferred markers; the consolidated registry is in [`../PRD.md`](../PRD.md) § "Deferred to future scope".
