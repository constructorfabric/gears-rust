---
status: proposed
date: 2026-10-12
decision-makers: "BSS Rating team"
---

Created:  2026-10-12 by Virtuozzo International GmbH
Updated:  2026-10-16 by Virtuozzo International GmbH

# ADR-0007: Record Usage Source Loss per Source Scope and Block Finality

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Record the loss against the source scope (chosen)](#record-the-loss-against-the-source-scope-chosen)
  - [Record the loss per enumerated tenant](#record-the-loss-per-enumerated-tenant)
  - [Ignore the loss and rely on the restart](#ignore-the-loss-and-rely-on-the-restart)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-rating-adr-source-loss`

## Context and Problem Statement

Rating reads usage through the collector feed (`read_usage_feed`), one reader per canonical feed
source. A source is defined by its usage type set (`gts_type_ids`), Rating's service identity and
its configured tenant scope; it spans many tenants. The collector keeps feed entries for a bounded
retention. A cursor older than that retention returns `CursorBeyondRetention`, and Rating restarts
the source from `Oldest` ([collector ADR-0011](../../../../system/usage-collector/docs/ADR/0011-cpt-cf-usage-collector-adr-feed-aggregate-split.md)).
Dedup absorbs the re-read, but entries that left retention before Rating captured them are gone.

The same gap appears when an entry has no readable id or tenant, or when a source's scope widens to
cover history that is already beyond retention.

The lost entries are exactly the ones that would name the affected tenants and windows, and feed
order is not event-time order. Under the interim `delay_only` evidence mode (R-21), a window
finalizes once its delay elapses, so a loss would be invoiced as less usage without anyone
noticing.

How does Rating record a loss whose extent it cannot see, and what does the loss block?

## Decision Drivers

* Lost usage must never become a silent undercharge.
* The affected tenants cannot be enumerated from the entries that were lost.
* `delay_only` is an explicit business acceptance of missing evidence, not of known loss (R-21).
* A restart, a re-read or a metric reset must not make a loss disappear.
* Accepting a loss is a financial decision and must be audited.

## Considered Options

1. Record the loss against the source scope and block finality inside it (chosen).
2. Record the loss per tenant that Rating can enumerate.
3. Ignore the loss and rely on the restart from `Oldest`.

## Decision Outcome

Chosen option: **record the loss against the source scope** (DECISIONS T-D-62).

- **Detection.** `SourceLossTracker` opens a durable row in `bss_rating__source_loss` for
  `cursor_beyond_retention`, `unidentifiable_entry` (an entry without a readable id or tenant; its
  raw bytes go to `bss_rating__usage_capture_reject`) and `scope_widened_beyond_retention`. The row
  is written in the transaction that detects the loss, which holds the checkpoint row lock and the
  cursor CAS of a page transaction: the **restart transaction** for `cursor_beyond_retention` (it
  also resets the cursor, so the next read starts at `Oldest`), the page transaction for
  `unidentifiable_entry`, and the source-creation transaction for `scope_widened_beyond_retention`.
  A stale reader fails the CAS, so it can neither reset a newer cursor nor open a second loss. The
  row records `last_good_cursor`, `last_good_at` (when that cursor was committed) and
  `restart_cursor` (the cursor the transaction wrote; NULL means `Oldest`).
- **Scope.** The loss is recorded against the **source scope**: the source's configured tenant
  scope times its usage type set. It is not split into rows per tenant seen so far.
- **Taint.** Feed order is not event-time order, but the collector's retention rule bounds where
  removed entries can lie. Retention runs from the end of an entry's covered period and keeps it at
  least the floor `F` (backfill window + replay horizon, 125 days at launch). An entry removed by
  the detection instant `t_d` therefore ended at or before `t_d − F`. It was still held at
  `last_good_at`, so relying on the floor it ended at or after `last_good_at − F`. A
  `cursor_beyond_retention` loss taints every `(usage type, day)` of the scope from
  `day(last_good_at − F)` to `day(t_d − F)`. Later days are still held in full and are re-read from
  `Oldest`, so they are not tainted. An `unidentifiable_entry` loss taints the days the feed still
  holds, `day(t_d − F)` to `day(t_d)`.
- **Gate.** `EvidenceGate` keeps a usage child `pending(source_loss)` when its resource tenant and
  usage type fall inside an open or narrowed loss's scope and its window can hold an entry of a
  tainted day. This holds under every evidence mode, `delay_only` included. A final result records
  the open loss ids it checked; the set must be empty. Final children inside the taint are listed
  as suspect and are not changed automatically.
- **Repair.** Reconciliation compares the collector's per `(tenant, usage type, day)`
  reconciliation figures with Rating's captured entries, only for days the collector still holds in
  full. Matching days leave `tainted_days` (`narrowed`). The figures come from Rating's own read once
  its service identity may call the route, and until then from an operator who submits them
  (`POST /source-losses/{id}:reconciliation`, audited). When every tainted day matches, the loss is
  `resolved` with `repaired`.
- **Past retention.** For a day the collector no longer holds in full, its figures count only what
  survived and so match Rating's captures whether or not something was removed: they prove nothing.
  The backfill route also refuses periods older than its window (`PAST_WINDOW`), and `F` exceeds
  that window. A `cursor_beyond_retention` or `scope_widened_beyond_retention` loss therefore never
  becomes `repaired`; it ends only in an acceptance.
- **Acceptance.** Otherwise an operator holding `source_loss x resolve` records `accepted_loss` with
  an evidence reference, and a second, different approver confirms it before it takes effect. Both
  actors and the evidence are audited. The acceptance is also the explicit decision on the suspect
  final children.
- **Never by itself.** A restart, a re-read or a metric reset never resolves a loss.
- **Final windows.** Windows that were final before the loss are not reopened. Entries that repair
  captures for them (after the collector's operator widens the backfill window and re-imports) are
  late usage and produce ordinary corrections (ADR-0004).

### Consequences

* A loss on a broad source holds finality for every tenant in its scope until reconciliation narrows
  it or two operators accept it, so the blast radius is the source, not the tenant. Narrowing per day
  limits the wait, and the bounded taint keeps it to the windows that can hold a removed entry.
* An open loss pages the operator (`rating_source_loss_open`).
* A retention loss is a known undercharge that Rating cannot repair; it is surfaced and decided by
  people, never closed by evidence that cannot prove anything.
* Narrowing does not depend on the collector granting Rating's service identity: an operator can
  submit the figures (UPSTREAM_REQS J-17). Only a collector operator can start a backfill.
* Accepting a loss needs two operators with the resolve grant.

### Confirmation

* An `InvalidArgument(CursorBeyondRetention)` read opens one loss for the source scope under the
  cursor CAS and restarts from `Oldest`; a stale reader's restart rolls back.
* The loss taints the days from one floor before `last_good_at` to one floor before detection, and
  no later day.
* A `delay_only` child inside the taint stays `pending(source_loss)` after its delay elapses; a child
  of a tenant, usage type or day outside it finalizes.
* An instance restart and a metric reset leave the loss open.
* Reconciliation of a day still held in full narrows the tainted days; reconciliation of a day past
  retention narrows nothing; a retention loss is never `repaired`; `accepted_loss` without a
  second, different approver is refused.

## Pros and Cons of the Options

### Record the loss against the source scope (chosen)

* Good: covers every tenant the lost entries could have named; no undercharge can pass the gate.
* Bad: holds finality for tenants that may have lost nothing, until reconciliation proves it.

### Record the loss per enumerated tenant

* Good: a narrow blast radius.
* Bad: the tenants Rating can enumerate are the ones whose entries it still has. A tenant whose only
  entries were lost is invisible, and its windows would finalize undercharged.

### Ignore the loss and rely on the restart

* Good: no operator work and no held windows.
* Bad: the restart only re-reads what the collector still retains. The lost usage is never billed,
  and nothing records that it was lost.

## More Information

- Failure model and reconciliation: [DESIGN §4.4](../DESIGN.md#44-failure-model),
  [DESIGN §4.6](../DESIGN.md#46-reconciliation); operator surface `POST /source-losses/{id}:resolve`
  in [DESIGN §3.3](../DESIGN.md#33-api-contracts).
- Usage intake behaviour: [`../features/04-usage-intake.md`](../features/04-usage-intake.md).
- Collector cursor semantics: [collector ADR-0011](../../../../system/usage-collector/docs/ADR/0011-cpt-cf-usage-collector-adr-feed-aggregate-split.md).

## Traceability

- **Decisions**: [`../DECISIONS.md`](../DECISIONS.md) T-D-62 (this decision), T-D-52 (finalization
  gate), R-21 (finalization evidence, `delay_only`).
- **Design**: [DESIGN §3.2](../DESIGN.md#32-component-model) (`SourceLossTracker`, `EvidenceGate`),
  [DESIGN §3.7](../DESIGN.md#37-database-schemas--tables) (`bss_rating__source_loss`),
  [DESIGN §4.4](../DESIGN.md#44-failure-model), [DESIGN §4.6](../DESIGN.md#46-reconciliation).
- **Upstream**: [`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.1 (Usage Collector), J-17.
