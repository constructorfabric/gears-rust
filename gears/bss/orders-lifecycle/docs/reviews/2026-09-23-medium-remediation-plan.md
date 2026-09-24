# MEDIUM remediation plan — slices 02–08 (2026-09-23)

Source: `2026-09-23-slices-02-08-lens-review.md`. Every MEDIUM item was re-verified against the
working tree **after** the HIGH fixes (D-106…D-111): **43 / 43 still open**, none fixed as a side
effect, none withdrawn. One new item surfaced during planning (N-1).

Legend: **M** = mechanical (one obvious fix) · **D** = owner decision, recommended option first.
New decisions are numbered from **D-112** in execution order.

---

## Decisions (recommendation in bold)

| Item | Question | Options (recommended first) |
|---|---|---|
| M-1 (02) | Reason for unknown/removed `lineId` | **A: retire `line-not-in-draft`, add `line-not-found` (404)** · B: keep both |
| M-2 | Address line admin fields after draft | **A: line `PATCH` admitted in any non-terminal state, admin fields only after draft** · B: order PATCH carries `lines[]` |
| M-3 | Store for per-tenant date policy + revision | **A: new `orders_date_policy` table on the TTL-policy channel** · B: Settings Service (upstream) |
| M-5 | Admin field inside a draft-mutate request | **A: one request = one trigger; mixed requests refused** · B: draft-mutate writes admin tables too |
| M-6 | Seller change in draft vs seller-unique number | **A: `sellerTenantId` immutable from create** · B: reallocate the number |
| M-7 | Whose catalog frontier the gate reads | **A: upstream `pin_frontier_for(ctx, catalog_tenant)`; PEP deny → `catalog-frontier-unavailable`** · B: seller-scoped service context (impersonation) |
| M-9 | Preview with missing term/cycle | **A: success response, `tcv` absent + `tcvWithheld` annotation** · B: 400 refusal |
| M-10 | Overlap read shape (SUB-O5) | **A: occupancy read `(activeCount, max, provenance)`; amend SUB-O5** · B: boolean + limit, unevaluable when max>1 |
| M-11 | Re-check outcome when a port is down | **A: bounded `defer` retried by Workflow, then abort via `acknowledge-failed`** · B: abort immediately |
| M-12 | Pin composition on gate failure | **A: compose in the parallel step, always** · B: new `not_evaluated` verdict |
| M-14 | Source of a line's region | **A: market scope of the resolved price row at the fixed version** · B: narrow predicate 4 to currency, disclose PRD gap |
| M-17 | "Crosses seller scope" test | **A: identity port's payer↔seller eligibility answer** · B: separate tenant-hierarchy port (10th port) |
| M-19 | Skipped inputs vs engine step 3 | **A: step 3.1 first settles earlier-registered failing guards, then unevaluable** · B: never skip; always resolve full gate |
| M-20 | Bad `amendment_reason` | **A: new `amendment-reason-invalid` (400)** · B: reuse `amendment-empty` + field |
| M-21 | Missing expected version | **A: boundary rejection `expected-version-required` (428), no audit** · B: engine `version-conflict` |
| M-23 | Recording-party bar | **A: local guard, new `acceptance-recording-party-barred` (403)** · B: PDP via resource properties (upstream) |
| M-25 | `pending` authorization outcome | **A: keep as defensive fail-closed branch** · B: two-valued field, delete `authorization-pending` |
| M-26 | Contract acceptance declaration source | **A: widen 03 contract port, read live; add Contracts upstream ask** · B: snapshot onto version at submit |
| M-27 | Who changes acceptance elections | **A: deploy-time promotion only** · B: runtime seller endpoint + PDP action |
| M-28 | Evidence on pre-spawn `/workflow-cancel` | **A: always required** · B: required only after spawn signal |
| M-30 | Denial reason on a denied verdict | **A: `denial_reason` input + column, `denial-reason-missing` (400)** · B: drop it from `OrderRejected` |
| M-32 | Shape of `failure_reason` | **A: closed enumeration** · B: opaque Workflow string |
| M-33 | Resume cap vs PRD "MUST be resumable" | **A: keep cap for every resume (as chosen for H-1), register Q-31 for Product** · B: cap only resumes into TTL states — *this reverses the H-1 choice* |
| M-34 | Seller TTL override (open Q-06) | **A: flag `ttl_seller_override_enabled`, default off** · B: provisional D-nn, narrow Q-06 |
| M-35 | Hold actor/instant/reason | **A: audit entry is the record; correct D-22** · B: three new columns |
| M-37 | Timing leak on the no-row read | **A: always make the PDP call + scoped re-read** · B: withdraw the timing promise |
| M-38 | 403 vs 404 for targeted denials | **A: on deny, a follow-up `order × read`; readable → 403, else 404** · B: always 404 for targeted, 403 only untargeted |
| M-40 | Invalid cursor | **A: register `cursor-invalid` (400), one cursor contract for all four collections** · B: toolkit-odata `INVALID_CURSOR` |
| M-43 | `actor_class` source | **A: closed `system` / `service` / `user` from authenticated context** · B: drop the column |

## Mechanical (15)

M-1 (06 part: three missing algorithms, retire unraised `begin-fulfillment-preconditions-unmet`) ·
M-4 (`currency` → commercial) · M-8 (clarify D-89: post-`active` = fulfillment failure) ·
M-13 (party eligibility → contract port only) · M-15 (mark rating / AM clients "unexposed", raise
AM ask to p1) · M-16 (amendment principle: rows 19/20) · M-18 (one complete amendment guard order;
line cap applies) · M-22 (acceptance instant = engine `t`) · M-24 (drop the authorization-port
alert; alert on `acceptance-requirement-unevaluable`) · M-29 (five residual "Lifecycle runs the
re-check" sentences) · M-31 (closed compensation-evidence schema) · M-36 (hold step contributes no
pre-hold value) · M-39 (List Orders refused-log step) · M-41 (fourth Preview prohibition in 03 §4.6)
· M-42 (route consumer read grants to UPSTREAM §2.9).

## New item

**N-1 · Concurrent administrative edits lose updates.** Admin edits are guarded only by
`expected_version`, which an admin edit never bumps (`04:304`), so two concurrent edits both
succeed and the second overwrites. Proposed fix: an `admin_revision` counter on the aggregate
(bumped by row 3, required as `expected_admin_revision`), mirroring `draft_revision`. *Decision.*

---

## Execution batches (sequential, one fresh subagent each, `make design-check` after each)

Grouped so that items editing the same algorithm land together and no two batches race on a file.

| # | Batch | Items | Main files |
|---|---|---|---|
| 1 | Text-only corrections | M-4, M-8, M-16, M-22, M-24, M-29, M-36, M-41, M-42 | 02, 03, 04, 05, 06, 07, 08, DECISIONS, UPSTREAM |
| 2 | Engine-wide rules | M-21, M-19, M-38, M-43 | 01 §3.6/§4.1/§3.7, 04, 08 |
| 3 | Draft authoring | M-1 (02), M-2, M-5, M-6, N-1 | 02 §3.3/§3.6/§4.3, 04 admin edit, 08 matrix |
| 4 | Date policy store | M-3 (+ shared store question from M-27) | 02, 01 §3.7, DESIGN |
| 5 | Gate | M-7, M-12, M-13, M-14, M-15 | 03 §2.2/§3.3/§3.6, UPSTREAM §2.2/§2.4, DESIGN |
| 6 | Fulfilment re-check & Preview | M-9, M-10, M-11 | 03, 06 §4.3/§4.4, 01 registry, UPSTREAM §2.1 |
| 7 | Amendment | M-17, M-18, M-20 | 04 §3.6, 01 registry |
| 8 | Acceptance | M-23, M-25, M-26, M-27 | 05, 03 contract port, UPSTREAM (new Contracts §), 08 |
| 9 | Workflow operations | M-1 (06), M-28, M-30, M-31, M-32 | 06 §3.6/§3.7, 01 §3.7/§4.3/§4.4 |
| 10 | Hold & expiry | M-33, M-34, M-35 | 07, DECISIONS (Q-31, D-22, Q-06) |
| 11 | Read surface | M-37, M-39, M-40 | 08, 01 registry |

Order rationale: batch 2 changes engine rules (boundary validation, guard precedence, refusal
mapping, actor class) that later batches cite; batch 3 precedes 7 because the amendment guard
order references capture's guards; batch 5 precedes 6 and 8 (ports and step numbers).

---

## Owner choices (2026-09-23)

- All 29 decisions: **recommended option** (first listed in the table).
- M-33: **keep the cap for every resume; register Q-31** for Product (consistent with H-1 / D-109).
- M-19: **engine step 3.1 settles earlier-registered failing guards before unevaluable.**
- N-1: **accept last-write-wins** — document that administrative fields are last-write-wins, each
  change audited per field; no `admin_revision`. Lands in batch 3.

## Execution status (2026-09-23)

All 11 batches applied; all 43 MEDIUM items and N-1 addressed. Decisions **D-112…D-139**, open
question **Q-31**. Retired reasons: `line-not-in-draft` (D-116), `begin-fulfillment-preconditions-unmet`
and `workflow-cancel-requires-evidence` (D-134); `preview-term-or-cycle-missing` moved to a non-refusal
annotation (D-125). `make design-check` passes. Nothing committed. LOW findings not yet addressed.
