<!-- migration-note: index for the Billing Ledger design set, converted from the legacy Virtuozzo design set at vhp-architecture/docs/bss/design/DESIGN-billing-ledger-balances-202606091200/ (original README + slice docs 00–07). Mirrors the source layout: a foundation design plus per-slice design docs, ordered by implementation phase. -->
<!-- CONFLUENCE_TITLE: [BSS]: Billing Ledger — Design Set -->
<!-- Related: ../DESIGN.md, ../PRD.md, ../ADR/ | Owners: @vstudzinskyi (BSS Billing Platform team) -->

# Billing Ledger & Balances — Design Set

This folder holds the Billing Ledger technical design as a **set of slice designs**: a
shared Repository-Foundation ([`01-repository-foundation.md`](./01-repository-foundation.md))
plus per-slice handler designs. Every slice posts **through** the Foundation — the
double-entry posting engine, schema, universal invariants, total lock order, and in-process
data-access API; the Foundation owns no domain policy, each slice is a handler that builds
balanced lines and calls the Foundation API.

**The canonical index for this set — architecture overview, the phased slice map,
dependency order, cross-cutting normative statements, and traceability — is
[`../DESIGN.md`](../DESIGN.md).** Requirements (WHAT/WHY) live in [`../PRD.md`](../PRD.md);
decision rationale in [`../ADR/`](../ADR/).

## Slice documents

- [`01-repository-foundation.md`](./01-repository-foundation.md) — shared engine (journal, balances, lock order, idempotency, money, data-access API) + the ledger-wide normative statements (§4)
- [`01a-invoice-posting.md`](./01a-invoice-posting.md) — invoice-post handler
- [`02-audit-immutability-observability.md`](./02-audit-immutability-observability.md) — tamper chain, freeze, PII/erasure, alarms
- [`03-payments-allocation.md`](./03-payments-allocation.md) — settlement, allocation, chargebacks, wallet
- [`04-asc606-recognition.md`](./04-asc606-recognition.md) — recognition schedules + runs
- [`05-adjustments-notes-refunds.md`](./05-adjustments-notes-refunds.md) — credit/debit notes, refunds, governance
- [`06-fx-multicurrency.md`](./06-fx-multicurrency.md) — functional currency, realized FX, rate snapshots
- [`07-reconciliation-export.md`](./07-reconciliation-export.md) — reconciliations, ERP export, period-close gate

See [`../DESIGN.md` §1.3](../DESIGN.md#13-architecture-layers) for the phase/PRD-slice mapping and the full dependency order.
