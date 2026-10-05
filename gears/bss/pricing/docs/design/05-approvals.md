<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Approvals (Design, Slice 5) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Pricing team -->

# DESIGN — Approvals (Slice 5)

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-design-slice-05`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Review a selected batch](#review-a-selected-batch)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [submit-unit](#submit-unit)
  - [vote-and-refresh](#vote-and-refresh)
  - [apply-prices](#apply-prices)
  - [withdraw-unit](#withdraw-unit)
- [4. States (CDSL)](#4-states-cdsl)
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

**Delivery:** phase 2c for prices; 3 for the other subjects. Every checkbox is an implementation obligation, not an assertion about the legacy code.

Compose price batches and govern every pricing subject with shared quorum, author separation, generation refresh and atomic terminal outcomes.

Requirements: `cpt-cf-bss-pricing-fr-publish-changes`, `cpt-cf-bss-pricing-fr-approval-units`, `cpt-cf-bss-pricing-fr-events`. Architecture: `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-principle-business-content-fingerprint`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-constraint-one-replay-store`, `cpt-cf-bss-pricing-seq-publish-changes`, `cpt-cf-bss-pricing-seq-temporary-pair`, `cpt-cf-bss-pricing-seq-blocked-revision`.
[FEATURE](../features/approvals.md) owns the executable flow/algorithm/DoD identifiers; this slice defines no duplicate DoDs.
Dependencies: `cpt-cf-bss-pricing-feature-prices-windows-dimension`.
Source: PriceBook spec §2.2, §5–§8, §12–§13 and [DECISIONS](../DECISIONS.md) D-384–D-433.

## 2. Actor Flows (CDSL)

### Review a selected batch

Actors: `cpt-cf-bss-pricing-actor-finance-manager`, `cpt-cf-bss-pricing-actor-finance-reviewer`, `cpt-cf-bss-pricing-actor-auditor`. Feature flow: `cpt-cf-bss-pricing-flow-approvals`.

1. [ ] - `p1` - Finance Manager sees all draft prices of the book with full predecessor, money, chain, dates and impact; all start selected. - `inst-approvals-flow-1`
2. [ ] - `p1` - Untick ordinary prices or select atomic temporary pairs; optionally supply a common effective date. - `inst-approvals-flow-2`
3. [ ] - `p1` - Submit validates one-book membership and shifts dates, then locks items conditionally and copies policy quorum. - `inst-approvals-flow-3`
4. [ ] - `p1` - Finance Reviewer reads the stored snapshot, live impact and generation, then approves or rejects with a required client key. - `inst-approvals-flow-4`
5. [ ] - `p1` - Apply only when the current generation meets quorum and fingerprint/revalidation pass; otherwise retain pending votes, refresh stale content or refuse as specified. - `inst-approvals-flow-5`

## 3. Processes / Business Logic (CDSL)

### submit-unit

Feature algorithm: `cpt-cf-bss-pricing-algo-approvals-submit-unit`.

1. [ ] - `p1` - Resolve replay before any work; collect selected business content with every item author and the common date. - `inst-approvals-submit-unit-1`
2. [ ] - `p1` - Validate submit, pair completeness, each temporary's return against the approved chain on its shifted end (PAIR_RETURN_STALE, D-391), uncrossed temporary windows (PRICE_INSIDE_TEMPORARY, TEMPORARY_SPANS_A_CHANGE, D-406), chain rules and ownership; red checks return 400 with their code and no unit. - `inst-approvals-submit-unit-2`
3. [ ] - `p1` - Read kind quorum or tenant default (missing default is fail-safe one), insert unit/items and acquire ordered conditional ownership. - `inst-approvals-submit-unit-3`
4. [ ] - `p1` - Write submission audit; if quorum is zero, apply and write terminal audit/events in the same transaction with no votes. - `inst-approvals-submit-unit-4`

### vote-and-refresh

Feature algorithm: `cpt-cf-bss-pricing-algo-approvals-vote-and-refresh`.

1. [ ] - `p1` - Conditionally claim the observed unit version, require pending state and compare the requested generation. - `inst-approvals-vote-and-refresh-1`
2. [ ] - `p1` - Reject submitter/item-author approval and duplicate current-generation votes. - `inst-approvals-vote-and-refresh-2`
3. [ ] - `p1` - Recollect business content and compare fingerprint before counting a vote; on drift refresh items/snapshot/hash, increment generation, mark earlier decisions stale and commit UNIT_STALE. - `inst-approvals-vote-and-refresh-3`
4. [ ] - `p1` - Otherwise insert a decision; an approval below quorum stays pending, a rejection with note unlocks and closes the unit. - `inst-approvals-vote-and-refresh-4`
5. [ ] - `p1` - At quorum revalidate and apply; environment failure rolls back, successful apply emits domain plus terminal events. - `inst-approvals-vote-and-refresh-5`

### apply-prices

Feature algorithm: `cpt-cf-bss-pricing-algo-approvals-apply-prices`.

1. [ ] - `p1` - Order affected entries by id and prices by id after the unit; use conditional guards and serializable Postgres transaction. - `inst-approvals-apply-prices-1`
2. [ ] - `p1` - Shift selected prices to the common date with temporary duration preserved. - `inst-approvals-apply-prices-2`
3. [ ] - `p1` - Re-read each chain, enforce usage pair guard and approved start uniqueness, and recompute effective_to independently per value. - `inst-approvals-apply-prices-3`
4. [ ] - `p1` - Set keep_for_bound on the current predecessor of every new price of each touched chain, including a new price approved earlier that a price of this unit now precedes, and never clear it; preserve approved money and historical ids. - `inst-approvals-apply-prices-4`
5. [ ] - `p1` - Replace owned pending locks with approved_by_unit_id and atomically persist audit, PricesPublished and ApprovalUnitDecided. - `inst-approvals-apply-prices-5`

### withdraw-unit

Feature algorithm: `cpt-cf-bss-pricing-algo-approvals-withdraw-unit`.

1. [ ] - `p1` - Authenticate the original submitter, require pending state and conditionally claim unit version. - `inst-approvals-withdraw-unit-1`
2. [ ] - `p1` - Clear only this unit's pending ownership without publishing money. - `inst-approvals-withdraw-unit-2`
3. [ ] - `p1` - Persist withdrawn state, attributed terminal audit, ApprovalUnitDecided and replay response together. - `inst-approvals-withdraw-unit-3`

## 4. States (CDSL)

Unit states are pending → approved/rejected/withdrawn; every terminal state is final. Quorum zero enters approved in the submit transaction. Pending generation g → g+1 refresh commits with UNIT_STALE and earlier decisions stale. Business-content drift is distinct from a lost version race (UNIT_CONTENDED) and environment refusal (APPLY_REFUSED rollback).

State definition: `cpt-cf-bss-pricing-state-approvals` in the FEATURE.

## 5. API Surface

POST /bss-pricing/v1/prices/{id}/submit; POST /plan-revisions/{id}/submit (phase 3, the plan_revision kind of slice 04); POST /price-books/{id}/publish-changes with price_ids?, common_effective_date? and note? (the plan revision submit takes an optional { note } too; the single price's submit takes none, D-464); GET /approval-units?state&kind&ref_id&limit&cursor (one page in submission order, `{ items, page_info }`, D-458; `$orderby=submitted_at desc` newest first with the id breaking a tie the same way, the cursor carrying its order, and `impact=false` to skip the live impact, D-470), GET /approval-units/counts (the list's narrowing counted by state and kind in one grouped statement read outside any transaction, D-470; the list and the counts take kind prices or plan_revision, else 400 QUERY_INVALID on kind) and /approval-units/{id}; POST /approval-units/{id}/approve, /reject (generation required; reject note required) and /withdraw; GET/PUT /approval-policy, whose PUT sets the default (`*`) or the quorum of one kind, `prices` or `plan_revision` (400 POLICY_KIND_INVALID for any other kind; promotions and migrations are deferred, D-409, D-410); DELETE /approval-policy/{kind} removes a kind's override under If-Match so it follows the default again, and the default is never deleted (400 POLICY_DEFAULT_REQUIRED; 404 without an override; D-435). Every unit door dispatches on the unit's stored kind — the subject, the domain event of its apply and the impact of its card — and a stored kind pricing does not record is a corrupt row (500), never judged as another kind. read, submit, approve and settings permissions are separate. A read that feeds a rule is made as the caller and needs products read (D-416): the plan_revision submitter and its final approver (apply re-runs the checks), and the submitter and final approver of a prices unit on a usage chain (the dated metering read, D-402); an approve-only reviewer votes on every other unit and rejects any unit. A pending plan revision's read shows its unit's vote progress, counts only, under plan read: the approvals that bss_approval::counted_approvals counts over its decisions alone, the count the engine's approve judges by (D-459, D-462). Every unit read and receipt carries caller_can_approve, that predicate's verdict for the caller over the authors of the unit's stored items and its decisions: Approve only, since a reject judges no separation of duties, and not the grant, which the vote door still judges (D-471). All POSTs require client keys; the policy PUT and DELETE require If-Match (409 STALE_REVISION when stale). One half of a pair submitted alone is 400 PAIR_SPLIT, while publish-changes adds a ticked half's partner (D-405); a return that no longer matches the approved chain is 400 PAIR_RETURN_STALE at submit and APPLY_REFUSED at apply (D-391); a lost race on the unit, or contention that outlasts the retries at these doors, is 409 UNIT_CONTENDED (CONTENDED at every other door).

[DESIGN §3.3](../DESIGN.md#33-api-contracts) fixes canonical errors and route prefixes.
Each mounted route must appear in all four censuses with authz and precondition expectations.

## 6. Data Model

The four pricing_approval tables store policy, unit, items and decisions. Unit generation identifies reviewed content; version is the concurrency token. The unit's submit_note (m20260928_000016, D-445) is the submitter's note of the unit shape products shares: the note a plan revision's submit or publish-changes sent, else null (D-464), in every unit read; it is never content: neither the snapshot nor the fingerprint carries it. Items store authors and before/after business content. Decisions are unique by unit/actor/generation and prior generations stay stale. The prices snapshot includes book, common date, predecessors, after content and informational SKU descriptors; GET returns snapshot plus live impact. The review's snapshot, on the pricing doors and through the approvals inbox, also carries book { id, code, name, currency } beside each book id it names (D-516). That field is not stored and is not part of the fingerprint. A plan_revision unit has one item, the revision (its author is the item author separation of duties excludes); its snapshot carries the plan, the revision number, before (the published revision's content), after, the diff between them and each item SKU's current descriptors outside after, and its impact reads { subscriptions: "unavailable until the Subscriptions integration" }. Phase 2 counted prices and entries; phase 3 adds the plans, every plan revision whose items name an entry of the unit ({ plan_id, code, revision_id, rev_no, state }), on the stored snapshot and on every live read (the card, the queue and the publish-changes listing), while subscriptions read "unavailable until the Subscriptions integration". The prices snapshot carries each entry SKU's current descriptors beside after, gathered in collect, so a descriptor change never refreshes a pending unit (D-408). Both kinds read descriptors best-effort: a registry that cannot answer or refuses the caller records "descriptors": "unavailable" and refuses no submit, vote or reject (D-416).

Tenant-scoped parent validation is required even where foreign keys use entity ids. Never substitute a
cross-gear read for transactional local ownership/version guards. Approved money and historical pins survive.

## 7. Events & Alarms

Submission writes audit only. Every terminal transition, including reject, withdraw and quorum zero, writes ApprovalUnitDecided with unit_id, kind, state, generation and actors. Successful apply adds the appropriate domain event; a stale refresh does not announce publication.

Audit and outbox inserts use the same mutation transaction; retry is lifecycle-managed and observes shutdown.

## 8. Definitions of Done

The sole definitions live in [features/approvals.md](../features/approvals.md):

- `cpt-cf-bss-pricing-dod-publish-changes-selection` — Selected price batch.
- `cpt-cf-bss-pricing-dod-prices-unit` — Transactional price apply.
- `cpt-cf-bss-pricing-dod-sod-excludes-authors` — Separation from every author.
- `cpt-cf-bss-pricing-dod-quorum-policy` — Copied quorum including zero.
- `cpt-cf-bss-pricing-dod-stale-refresh-generation` — Committed refresh of stale content.
- `cpt-cf-bss-pricing-dod-generation-and-duplicate-vote` — Generation-scoped votes.
- `cpt-cf-bss-pricing-dod-unit-contended` — Unit contention and environmental refusal.
- `cpt-cf-bss-pricing-dod-terminal-audit-event` — Every terminal outcome is attributable.

## 9. Acceptance Criteria

1. PRD AC #9 / `cpt-cf-bss-pricing-dod-publish-changes-selection`: Given three drafts and one temporary companion, when an ordinary price is unticked then only the selected atomic set is locked; a ticked pair half brings its partner (added_partner, D-405), and a foreign-book price fails with no unit.
2. PRD AC #10 / `cpt-cf-bss-pricing-dod-prices-unit`: Given two overlapping batches, when approvals race then no overlapping approved windows survive; a valid batch applies all prices and a failed batch applies none.
3. PRD AC #10 / `cpt-cf-bss-pricing-dod-sod-excludes-authors`: Given a price authored by A but submitted by B, when A approves then SOD_VIOLATION refuses it; independent C with approve-only permission votes on any unit whose rules read no SKU, and a rule read (usage metering, plan checks) needs products read too (D-416).
4. PRD AC #10 / `cpt-cf-bss-pricing-dod-quorum-policy`: Given quorum 0, 1 and 2 units, when the valid number of independent votes is supplied then each applies once; changing policy cannot silently lower an existing unit's snapshot.
5. PRD AC #10 / `cpt-cf-bss-pricing-dod-stale-refresh-generation`: Given content drift after a vote, when another vote arrives then the unit refreshes and old votes stop counting; repeated keyed request replays the refresh outcome.
6. PRD AC #10 / `cpt-cf-bss-pricing-dod-generation-and-duplicate-vote`: Given refreshed generation 2, when a generation-1 vote arrives then it counts nothing; two votes by one actor in generation 2 cannot meet quorum 2.
7. PRD AC #10 / `cpt-cf-bss-pricing-dod-unit-contended`: Given two connections observing one version, when both approve then one terminal apply persists; if environment validation fails neither partial prices nor success events survive.
8. PRD AC #14 / `cpt-cf-bss-pricing-dod-terminal-audit-event`: Given a reject or withdraw, when it succeeds then locks clear and a terminal event exists without PricesPublished; missing reject note or foreign withdrawal fails.

## 10. Non-Functional Considerations

All paths enforce tenant isolation, deny-by-default authz and append-only attribution. Mutations use conditional
versions, required POST replay and PATCH/PUT preconditions; PostgreSQL serializable chain changes and SQLite
writer serialization preserve the same invariants. A failed audit/outbox write cannot leave a committed act.
No timeout releases a live reference. Review and apply retain typed database failures for bounded retry.
Implementation gates cover both backends and route censuses; document gates cover toc, language and identifier ownership.
