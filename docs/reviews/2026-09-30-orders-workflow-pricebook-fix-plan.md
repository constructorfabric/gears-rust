# Orders Workflow: the PriceBook mirror, applied

Status: **applied** to `gears/bss/orders-workflow/docs` on 2026-09-30 as decisions D-193–D-199 and
open question Q-14, committed on branch `bss/orders-workflow` (PR #4787) on top of `e0c24ba50`. Plan of record:
`~/.claude/plans/hidden-spinning-reddy.md`. Source baselines: PriceBook code diffora `bss/products` at
`16705a243`; the Orders Lifecycle rewrite for it, uncommitted in the sibling worktree
`bss-orders-design-doc` (ADR-0008, D-150–D-168, `docs/reviews/2026-09-29-*` and
`2026-09-30-orders-lifecycle-existing-seams-fix-plan.md`); the BSS Seam Atlas.

The process order of the Workflow design is unchanged: approve, re-approve on `OrderAmended`,
freeze, begin-fulfillment, wave 1, re-check, spawn signal, wave 2, acknowledge. What changed is
the data on six seams and two decisions the user took before the edit.

## Decisions taken (user, 2026-09-30)

1. **Topology**: acquisition lines are independent; the inter-line dependency graph and its
   "Catalog" source are withdrawn; the two-wave barrier stays (D-196).
2. **Approval**: the Generic Approval service is withdrawn; an approval-policy adapter port whose
   contract is PRD §9.2 replaces it, with the stand-in as today's implementation and
   `cf-gears-bss-approval` as the intended one; the gate-to-unit mapping is Q-14 (D-197).
3. **Change Orders** (`gears/bss/orders-changes`): out of scope; noted only (D-199).

## What was applied, by decision

| Decision | Content | Where |
|---|---|---|
| D-193 | Lifecycle SDK `OrdersLifecycleWorkflowV1` + `get_version` for every fixed-version read; events are bounded projections (Lifecycle D-158), so the thin-events ask is answered with a residual; retired vocabulary gone | `DESIGN.md` §2.2, §3.5; `design/03` §3.4, §3.6; `design/04` §3.4, §3.6; `design/05` §2.2, §3.5; `design/06` §3.3; `design/08` §3.5; `ADR/0013`; `UPSTREAM_REQS.md` §2.4, §2.5; `PRD.md` |
| D-194 | Accepted-binding `activation_deadline`: checked at freeze (`planState = binding-expired`, `activation_deadline_at`), at the re-check (`abort_record`), and in the wave-2 guard (`bindingExpired: true` → `wave2BindingExpired` unwind); Subscriptions is the authority (compare-at-activation); one catalogue reason `order-binding-expired`, mapped to Lifecycle's value of the same name; a hold does not pause it | `design/04`, `05`, `06` §4.8, `07`, `08` §2.1, `10` §3.6 (b), §4.6, `01` §4.9 |
| D-195 | Intent carries the source version tuple as the accepted-binding reference, explicit start, tenant axes, correlation, external-reference snapshot; key unchanged; `activated` = Subscriptions `applied`, `oss_unconfirmed` = failure; `SUB-O5` = counts/limit/provenance over the `SUB-G1` key stored on the line; `SUB-O11` refusal kinds extended | `design/05` §2.1, §2.2, §3.3, §3.6, §3.7, §4.1; `design/06` §3.3; `UPSTREAM_REQS.md` §2.1 |
| D-196 | No `DependencyEdge`, `dependency_graph`, `catalog_topology_revision`, `dependency_rank`; `planState` `invalid-graph`/`topology-unavailable`, reasons `invalid-dependency-graph`/`catalog-topology-unavailable` and `blocked-upstream` withdrawn (the reverse walk over `execution_seq` has no upstream subject); Catalog ask withdrawn; PRD §1.3, §5.1, §6.1, §7.1, criterion 5 and the §15 partial-failure row rewritten in place, recorded as `UPSTREAM_REQS.md` §4 item 15; catalogue 41 reasons | `design/04` throughout; `05`, `06`, `07`, `08`, `10`; `01` §4.9; `ADR/0005`; `DESIGN.md`; `PRD.md`; `UPSTREAM_REQS.md` |
| D-197 | Approval-policy adapter port (`verdict`, `submit`, `lookup_by_key`, `read_decision`, `escalate`), implementations `stand-in` and `library`; ~180 "Generic Approval service" mentions renamed, sentences meaning "an external service will exist" reworded; ask 2.3 is LOCAL and keeps its ID | `PRD.md`, `DESIGN.md`, `UPSTREAM_REQS.md`, `design/*`, `ADR/*`, `design/README.md` |
| D-198 | `owf_approval_request`: `tcv_minor bigint`, `currency`, `currency_minor_digits` from `get_version`; "TCV", never "resolved total (TCV)"; Workflow calls neither Rating nor Pricing | `design/03` §3.2, §3.6, §3.7; `DESIGN.md` §2.2; `PRD.md` |
| D-199 | Payment amount and provenance separate from TCV (Q-06 extended); no Products reservation (Lifecycle D-164); market wording (Lifecycle D-156); Change Orders deferred | `design/04` §3.4, §3.5; `UPSTREAM_REQS.md` §2.2, §2.5 |

Amended entries carry dated notes: D-16 (superseded), D-77 and D-105 (counts), D-92, D-110, Q-05,
Q-06, Q-08. New: Q-14. `DECISIONS.md` index rows and highest numbers updated; TOC regenerated with
`cfs toc`.

## Hand-off to the Orders Lifecycle owner

1. **Topology sentence.** Lifecycle's ask on this gear (`…-upreq-workflow-pricebook-contracts`,
   its `UPSTREAM_REQS.md` §2.6) says "its approved-instance constructor requires a revision-stamped
   complete dependency graph before begin-fulfillment … Unknown topology must not become an empty
   dependency set." D-196 answers it by there being **no** topology under PriceBook — a line is one
   plan revision with its selected items, nothing links two revisions — not by an empty graph.
   Please reword the clause to "no inter-line dependency; the barrier is the ordering" or strike it.
   The same applies to D-157's "topology completeness" wording in your decisions table.
2. **Link paths.** This gear links Lifecycle's slices as `../../../orders-lifecycle/docs/design/…`
   (the copy in this worktree, at `c66b2cafe`); your branch names them `features/…`. Pre-existing,
   not changed here; whichever branch merges first should carry the rename.
3. **ID citation.** Your ask's full `cpt-…` ID is cited here in short form
   (`…-upreq-workflow-pricebook-contracts`) because `cfs validate` cannot see a definition that is
   uncommitted in another worktree. Once your remediation is committed, the full ID can be restored.
4. **`order-binding-expired`** is consumed here exactly as your feature 06 §4.4 defines it; the
   interim `payment-authorization-stale` row of the failure-reason coverage ask remains.
5. **`SUB-O5`, `SUB-O11`, `SUB-O13`** wording here follows your D-126/D-153/D-163/D-165; the
   proposal to Subscriptions (SKU of the paid recurring item as the key derivation) is cited, not
   re-raised.

## Verification (2026-09-30)

- Retired-term sweep over `gears/bss/orders-workflow/docs`: `catalogPricePin`, `dependency_graph`,
  `dependency_rank`, `catalog_topology_revision`, `invalid-dependency-graph`,
  `catalog-topology-unavailable`, `blocked-upstream`, `resolved_total`, the Catalog ask ID and
  "Generic Approval" survive only in `DECISIONS.md` history entries, dated withdrawal notes, and the
  §4 amendment items that name what was replaced; `generic-approval` survives only inside stable
  `cpt-…` IDs.
- Reason catalogue (`design/01-foundation.md` §4.9): 41 = 10 engine + 31 slice (04 seven, 06 four);
  D-77/D-105 amended to say so.
- Relative links and anchors under the gear's docs: 0 broken (own slug check with the project's
  heading rule).
- `cfs validate-toc`: every file unchanged/valid after `cfs toc DECISIONS.md`.
- `cfs validate --artifact --skip-code --local-only`: `PRD.md`, `DESIGN.md`, `UPSTREAM_REQS.md` and
  the seven edited ADRs report 0 errors, equal to HEAD in a baseline worktree.
- `cfs check-language`: 28 files, no findings. `git diff --check`: clean.
- Not verified: no runtime exists; the Lifecycle target is uncommitted; the Subscriptions, Rating and
  Pricing counterparts have not adopted the proposals this design now cites.
