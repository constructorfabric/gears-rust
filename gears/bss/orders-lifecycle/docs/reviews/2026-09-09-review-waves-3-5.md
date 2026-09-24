# Orders Lifecycle design set — review waves 3–5

**Date**: 2026-09-09
**Scope**: the seventeen design artifacts under `gears/bss/orders-lifecycle/docs/`
**Baseline**: `PRD.md`
**Preceding record**: [`2026-09-08-design-set-review.md`](./2026-09-08-design-set-review.md) (waves 1–2)

<!-- toc -->

- [1. Why this record exists](#1-why-this-record-exists)
- [2. Waves and what each was looking for](#2-waves-and-what-each-was-looking-for)
- [3. The dominant defect class](#3-the-dominant-defect-class)
- [4. Findings that were declined, and why](#4-findings-that-were-declined-and-why)
- [5. What the remediation added beyond edits](#5-what-the-remediation-added-beyond-edits)
- [6. Structural changes these waves produced](#6-structural-changes-these-waves-produced)
- [7. What remains open](#7-what-remains-open)

<!-- /toc -->

## 1. Why this record exists

Waves 1–2 are recorded in the file above. Waves 3–5 are recorded here because three of their
findings were **CRITICAL defects that four prior passes had not caught**, and because several
reviewer claims were **declined**. A reader who sees only the remediated documents cannot tell
which alternatives were considered and rejected, and would re-raise them.

## 2. Waves and what each was looking for

| Wave | Lane | Granularity | Findings | Net after verification |
|------|------|-------------|----------|------------------------|
| 3 | Consistency (`cf-semantic-reviewer-consistency`) | single-pass over 17 targets | 25 | 24 (1 declined) |
| 4 | Design coverage against the PRD + ADR sufficiency | 5 reviewers | 63 | 61 (2 declined) |
| 4b | Re-verification of wave 4's own remediation | self | 48 | 48 |
| 5 | Semantic + consistency, fresh read | 2 lanes | 43 | 37 (6 already fixed mid-read) |

The wave counts are **not** a convergence curve. Wave 5 still produced two CRITICALs
(`Rc3-004`, `Rc3-005`) whose blast radius was larger than anything in wave 3, so the set had not
converged when wave 5 ran, and no claim is made here that it has converged now.

## 3. The dominant defect class

Across all five waves the single most common defect was a **`DECISIONS.md` propagation claim
whose named section was never edited** — the register asserting a fix that had not landed (6 in
wave 3, 5 more in wave 4). The second most common was a **count stated in prose drifting from the
thing it counts**.

Both require explicit cross-document review. Further review requirements identified during
remediation are listed in §5.

## 4. Findings that were declined, and why

These were raised by a reviewer, examined, and **not** acted on. Each is recorded so it is not
re-raised as an open defect.

| Claim | Reviewer | Why declined |
|-------|----------|--------------|
| The gear's ADRs should carry `status: proposed`, per programme convention | ADR-sufficiency reviewer (wave 4) | There is no such convention. Only `subscriptions` uses `proposed`; `pricing`, `rating` and `ledger` all use `accepted`. Following the claim would have made this gear the outlier it was said to be avoiding. |
| Both begin-fulfilment elections lack a specified home | Coverage reviewer (wave 4) | Half wrong: the acceptance-required election does have one (`05 §4.2`, `orders_policy_election`). Only the payment-authorization election needed the fix, which it got. |
| `04 §3.3`'s cross-reference conceals a defect by pointing at `07 §3.3` | Consistency reviewer (wave 5) | The *defect* was real (`Rc3-007`) and is fixed, but the concealment reading is not: `04 §3.3` was accurate about its own slice and stale only about the sibling's. Recorded as a stale citation, not as concealment. |
| The table-inventory, reverse-ADR-traceability, refusal-class and routed-question findings (`Rc3-001`, `Rc3-011`, `Rc3-015`, `Rc3-016`, `Rc3-017`, `Rc3-028`) | Consistency reviewer (wave 5) | Genuine when read, already fixed before the report landed — the reviewer read files that were being edited concurrently. Verified fixed rather than re-fixed. |

## 5. What the remediation added beyond edits

The remediation identified additional review requirements after a wave-5 CRITICAL had evaded
every earlier pass. These are manual review obligations, not a claim of CI enforcement:

| Review requirement | The defect it exists to catch |
|--------|-------------------------------|
| **No pre-engine refusals** | `Rc3-004` — nine registered refusals returned ahead of the engine across five slices, producing exactly the unaudited refusals D-09 and ADR-0005 exist to eliminate. `01 §4.1` had forbidden this for four waves. |
| **Step contiguity** | `Rc3-019` — `02 §3.6` *Author Line* numbered 1, then 4–11. Resolving existing citations alone would miss this because nothing cited that algorithm. |
| **Enum membership** | Resolve each column against its owning table's value set, including columns introduced in the slices' prose `**Schema**:` declarations. |

The pre-engine sweep then found **seven further instances** in `06 §3.6` *Reflect Verdict* and
*Acknowledge Fulfillment* that wave 5 had not reached — the reviewer named five slices and did
not examine the seam.

## 6. Structural changes these waves produced

| Change | Driver |
|--------|--------|
| The **two-step amendment seam**: an amendment from `pending_approval`/`approved` lands in `submitted` unconditionally; the sibling gear reflects onward via rows 7/8 | D-12's verdict guard was unobtainable — no verdict exists for a version not yet created, and no port is declared. D-61, Q-12 |
| `orders_inflight_overlap_claim` introduced | The one-in-flight-order partial unique index had been declared on `orders_order_line` filtering on columns that live on `orders_order` — uncreatable. D-26 |
| Creation became a **versioning row** | D-64's "creation appends version 1" was unsatisfiable while row 1 was state-only |
| The submit budget split from the Preview budget (1.5 s / five ports; 1.75 s / six) | The stated "total submit budget" summed a **Preview-only** tax port. `Rc3-014` |
| Nine slice refusals became **declared engine guards** | `Rc3-004` |
| The gate became the **sole** date-cascade resolver | Resolution had two declared owners and the normative one had no mechanism. `Rc3-006` |
| `rejected` added to the claim-release set | A rejected order otherwise held its overlap claim forever, permanently blocking every future order for that `(payer_tenant_id, overlap_scope_key)`. `Rc3-005` |
| ADRs 0003, 0004, 0005 written | Wave 4 found three decisions carrying full alternatives analysis in no ADR |

## 7. What remains open

**Not defects — routed asks with named external owners.** They are listed here so a reviewer does
not read them as gaps in the design.

- **Twenty-three open questions** routed and unanswered (`DECISIONS.md` Q-01…Q-25; Q-10 is out of scope for this gear, Q-14 closed by a design correction). Six of them — Q-19…Q-25 — ask Product to **amend the PRD**: §9.1's operation set, §6.2's version-reason list, §6.5's `OrderAmended` trigger, and §6.1's state diagram. Until those land, a conformance run against the PRD as written fails on those points, and the design says so rather than papering over it.
- **Fourteen upstream asks** unagreed (`UPSTREAM_REQS.md`), nine at `p1`. Five target gears with no specification in this repository.
- **ADR-0003's consequence stands**: three adopted pricing predicates and half the overlap rule are unevaluable today, so **Phase 1 has no working submit path** until `SUB-O5` and the three pricing lanes exist. This is the designed fail-closed behaviour, not a defect.
