# LOW remediation plan — slices 02–08 (2026-09-23)

Source: `2026-09-23-slices-02-08-lens-review.md` §LOW. Re-verified against the working tree after
all HIGH (D-106…D-111) and MEDIUM (D-112…D-139, Q-31) fixes.

**41 items → 38 still open, 3 already fixed, 0 obsolete.** 34 are one-line wording edits; 4 carry
a small choice (recommendation first).

Already fixed: L-05.2 (`requirement_source` lists `volunteered` — D-107), L-06.2
(`begin-fulfillment-preconditions-unmet` retired — D-134), L-08.2 (List Orders validates filters
before PDP — D-139 reorder).

## Choices

| Item | Question | Options (recommended first) |
|---|---|---|
| L-02.3 | `field-unclassified` is registered as a 400 but only startup can hit it | **A: remove it from 02's list and the 01 registry** · B: keep, mark "startup diagnostic; never returned on a request" |
| L-05.1 | "the guard records whether it read an explicit row or the fallback" — nothing records it (both map to `platform_default`) | **A: drop the claim; an election with no row reads as its safe value** · B: add `platform_fallback` to `requirement_source` |
| L-08.1 | Acceptance read: 05 never cites the 08 wrapper; prior records are unbounded and unpaged | **A: cite the wrapper in 05; page acceptance history as a fifth collection (amend D-139)** · B: GET /acceptance returns only the current version |
| L-08.4 | Page size 50/200 cites D-41 (throughput) | **A: drop the citation ("set here")** · B: register D-140 for the page-size baseline |

## Edits by slice

**02-capture (6)** — L-02.1 *Author Line* input gains `expected_version`, `security_context`,
`idempotency_key`, step 1 names the full engine check order · L-02.2 `date-cascade-invalid` raised by
the gate (predicate 8, step 9) **and** the engine's date-basis check (step 15) · L-02.3 (choice) ·
L-02.4 "only guards on authoring" lists the line cap, membership, one-trigger rule, fixed seller ·
L-02.5 `change` uses the shared `category-not-admitted` · L-02.6 "Two additions" → "two properties
that originate here; `01 §3.7` is normative".

**03-gate-and-pin (4)** — L-03.1 08 §4.2 declared exclusions add "pre-tax, no tax figure" · L-03.2
step 12 rewritten in place as "predicate 8's failure was recorded at step 9 and is not added again"
(keeps numbering) · L-03.3 "predicate 9 above" → "in §4.2 below" · L-03.4 Preview "unauthenticated"
→ "open to every PDP-authorized buyer".

**04-versioning (6)** — L-04.1 linear supersedes is an **engine-enforced invariant** (01 §3.7 says no
DDL can express it), not a constraint · L-04.2 appender contributes four things; the engine moves
the pointer · L-04.3 row 20 "unguarded" → "carries the amendment guards, no verdict guard" · L-04.4
admin edit "never appends" to the version chain (chain ID now defined) · L-04.5 "all nine gate
predicates" → full gate (adopted + nine delta) · L-04.6 DESIGN.md and README drop the deleted
amendment-forbidden guard.

**05-preconditions (3)** — L-05.1 (choice) · L-05.3 unquote the 01 step 3.1 wording, cite step 3.1.2
and D-08 · L-05.4 widen Q-08 to cover refund-as-reversal.

**06-workflow-seam (6)** — L-06.1 §3.3 names the per-operation PDP grants (08 §4.3) instead of a
gear-wide scope claim · L-06.3 four `07 §3.6` *Cancel Order* citations → `07 §4.6` · L-06.4
traceability: 07 exempts by state, reads spawn signal only via the cancel guard · L-06.5
`fr-owf-line-progress` is Workflow PRD §6.3 · L-06.6 `orders_approval_reflection` is specified in 06,
add FK `(order_id, version)` → `orders_order_version`, fix D-73's Propagated address · L-06.7 add
`upreq-overlap-activation-atomicity` to §4.6 and §5.

**07-hold-and-expiry (8)** — L-07.1 hold declares no admissibility guard (engine step 11) · L-07.2
sweep workers: `still-processing` → leave for next pass; `idempotency-mismatch` /
`authorization-context-changed` → record as defect, alert, continue · L-07.3 the
`(state, created_at, order_id)` index exists, now serves draft auto-void only · L-07.4 `Db::try_lock`
with a non-blocking `LockConfig` (`max_wait: Some(Duration::ZERO)`, `max_retries: Some(0)`) · L-07.5
"§3.6 step 15" → "`01 §3.6` *Attempt Transition* step 15" · L-07.6 audit reason names the elapsed
per-state TTL (Layer 1), not "which of two bounds" · L-07.7 `in_fulfillment` TTL row is rejected by
the schema CHECK, not `not-admissible` · L-07.8 "One dwell input" → "One dwell input per sweep".

**08-read-and-authz (5)** — L-08.1 (choice) · L-08.3 Direct Customer's widening derives from its
authoring grant (D-117, D-130), amendment excluded · L-08.4 (choice) · L-08.5 §4.5 page-size
citation → §2.2 / §3.6 · L-08.6 route the not-found deviation for AC-13, AC-16 and AC-21 (08 and
D-68).

## Execution

Three sequential batches, one fresh subagent each, `make design-check` after each:

| # | Items | Files |
|---|---|---|
| 1 | 02, 03 | 02, 03, 01 registry, 08 §4.2 |
| 2 | 04, 05, 06 | 04, 05, 06, 01, DESIGN.md, README, DECISIONS (Q-08, D-73) |
| 3 | 07, 08 | 07, 08, DECISIONS (D-68, D-139 if L-08.1 A) |

## Owner choices (2026-09-23)

All four take the recommended option: L-02.3 remove `field-unclassified` (retire it) · L-05.1 drop
the "records explicit vs fallback" claim · L-08.1 cite the wrapper in 05 and page acceptance history
as a fifth collection (amend D-139) · L-08.4 drop the D-41 citation.
