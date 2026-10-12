<!-- CONFLUENCE_TITLE: [BSS]: Products — Lifecycle & Approvals (Design, Slice 3) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Product Catalog team -->

# DESIGN — Lifecycle & Approvals (Slice 3)

- [ ] `p1` - **ID**: `cpt-cf-bss-products-design-slice-03`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Author submits publication](#author-submits-publication)
  - [Reviewer approves a unit](#reviewer-approves-a-unit)
  - [Finance changes a GL code](#finance-changes-a-gl-code)
  - [Administrator retires a SKU](#administrator-retires-a-sku)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [lifecycle-edges](#lifecycle-edges)
  - [Atomic fence and submission](#atomic-fence-and-submission)
  - [sod-excludes-authors](#sod-excludes-authors)
  - [quorum-zero-records-unit](#quorum-zero-records-unit)
  - [stale-refresh-generation](#stale-refresh-generation)
  - [unit-version-contended](#unit-version-contended)
- [4. States (CDSL)](#4-states-cdsl)
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

This slice implements the three Products `ApprovalSubject`s and their approval/policy doors using
Foundation's Store and slice 02's validation/version rules. It details the Approvals component and
GL-change, fenced-retirement and stale-refresh sequences in [DESIGN §3.2 and §3.6](../DESIGN.md#36-interactions--sequences).
The local reference predicates are implemented with [slice 04](04-read-model-events.md); retirement
and type change cannot ship before both sides of that barrier are integrated.

The authority is spec §2.2, §4, §6, §7.2 and §13, meaning
`docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout.
[DECISIONS](../DECISIONS.md) P-D-189–194 settle durable fences, policy, generations and events.
Categories and the approval policy remain direct edits; the fence TTL is the deployment setting
`fence_ttl_minutes`, not a tenant setting (P-D-209). No materiality calculation or cross-gear approval unit exists.

## 2. Actor Flows (CDSL)

### Author submits publication

1. [ ] - `p1` - Authenticate products:submit and resolve optional POST replay; collect the draft SKU and validate publication, including meter resolution - `inst-ap-publish-input`
2. [ ] - `p1` - In one transaction record sku_publish, items, reviewed snapshot/hash, effective date and copied quorum, then acquire pending_unit_id conditionally on the observed SKU version and null ownership - `inst-ap-publish-submit`
3. [ ] - `p1` - If ownership acquisition affects zero rows, return ROW_LOCKED_PENDING and roll back the unit/items; otherwise write submission audit - `inst-ap-publish-lock`
4. [ ] - `p1` - At nonzero quorum commit pending; at zero quorum run the same apply/terminal path immediately, preserving the unit record - `inst-ap-publish-quorum`

### Reviewer approves a unit

1. [ ] - `p1` - Read stored snapshot and live recomputation under products:read, then send the reviewed generation with products:approve - `inst-ap-review-read`
2. [ ] - `p1` - In a scoped transaction conditionally advance the observed unit version; refuse non-pending state, generation mismatch and author/submitter approval before a vote can count - `inst-ap-review-guards`
3. [ ] - `p1` - Re-collect proposed business content; on fingerprint drift commit the refresh and return UNIT_STALE with its new generation without recording the attempted vote - `inst-ap-review-refresh`
4. [ ] - `p1` - Insert one decision per actor/current generation; below quorum commit pending, at quorum revalidate and apply the subject - `inst-ap-review-vote`
5. [ ] - `p1` - On success clear pending ownership, retain approved_by_unit_id, close the unit and write terminal audit, ApprovalUnitDecided and the SKU event in the same transaction - `inst-ap-review-apply`
6. [ ] - `p1` - An environment failure returns APPLY_REFUSED and rolls back the vote/apply transaction, retaining the earlier pending unit and any committed fence - `inst-ap-review-refused`

### Finance changes a GL code

1. [ ] - `p1` - Propose Storage's gl_code from 4010-STOR to 4012-STOR with effective_from October 1 via the changes door on a published or deprecated SKU - `inst-ap-gl-propose`
2. [ ] - `p1` - Validate the proposed date and content, record a sku_change snapshot with before/after/date and acquire pending ownership; only a type change needs the type fence - `inst-ap-gl-submit`
3. [ ] - `p1` - An independent reviewer approves the generation; at quorum append SkuVersion and emit SkuChanged with max(requested date, apply date), retaining the requested date in the unit snapshot, atomically with the head and terminal unit - `inst-ap-gl-apply`
4. [ ] - `p1` - Pricing's period-start version read binds the new GL on or after the later of October 1 and the approval date; earlier bindings keep the old GL without any Pricing unit - `inst-ap-gl-consume`

### Administrator retires a SKU

1. [ ] - `p1` - Authenticate products:submit and check replay, then acquire the guarded retirement fence inside the submission transaction; a live local reference returns SKU_REFERENCED before a unit exists - `inst-ap-retire-fence`
2. [ ] - `p1` - Submit sku_retire in the same transaction using the same fence_op_id; a retry after interruption rechecks the registry and resumes the orphan operation - `inst-ap-retire-submit`
3. [ ] - `p1` - At quorum revalidate zero references and conditionally apply retired; failed apply preserves retiring and the pending unit - `inst-ap-retire-apply`
4. [ ] - `p1` - A reviewer rejects with a note or the submitter withdraws; the terminal transaction clears matching pending/fence ownership and restores the saved prior lifecycle - `inst-ap-retire-abort`

## 3. Processes / Business Logic (CDSL)

### lifecycle-edges

1. [ ] - `p1` - sku_publish accepts draft and installs published with a new immediate snapshot; sku_change accepts published/deprecated content and the published ↔ deprecated edges - `inst-ap-lifecycle-kind`
2. [ ] - `p1` - For a change default the requested effective_from to today and refuse a past date at submit; apply max(requested date, apply date) and reject applied dates before the latest stored version with VERSION_ORDER; equal dates append a higher published_version - `inst-ap-lifecycle-date`
3. [ ] - `p1` - Retirement of a published or deprecated SKU first saves its prior lifecycle and installs retiring, then sku_retire installs retired only after approval and environment validation; a never-published draft is not retired but deleted by its author (DELETE /skus/{id}, P-D-206) - `inst-ap-lifecycle-retire`
4. [ ] - `p1` - Reject unsupported lifecycle edges, edits while pending and any return from retired; rejected/withdrawn publication or ordinary change leaves prior business content intact - `inst-ap-lifecycle-refuse`

### Atomic fence and submission

1. [ ] - `p1` - Check replay before fence work; for an existing unexpired fence with no unit, retain fence_op_id and revalidate for resume rather than acquiring another fence - `inst-ap-fence-resume`
2. [ ] - `p1` - For acquisition, conditionally update the scoped SKU on observed version, null pending ownership and absence of another fence, guarded by NOT EXISTS reserved/confirmed references in the same serializable transaction - `inst-ap-fence-acquire`
3. [ ] - `p1` - If references exist, refuse retire with SKU_REFERENCED or type change with SKU_TYPE_FROZEN; do not set fence metadata or create a unit - `inst-ap-fence-referenced`
4. [ ] - `p1` - Save fence_prior_lifecycle, fenced_at and fence_op_id, set retiring or type_change_pending, increment revision and submit the approval unit before committing the transaction - `inst-ap-fence-commit`
5. [ ] - `p1` - Submit the unit against the owned fence in that same transaction; revalidate the reference environment again at apply without a remote count - `inst-ap-fence-submit`
6. [ ] - `p1` - If no pending unit exists and fenced_at exceeds fence_ttl_minutes, the next SKU request or explicit unfence conditionally restores the prior state and clears metadata; guard version, operation id and still-null ownership - `inst-ap-fence-expire`
7. [ ] - `p1` - Rejection/withdrawal clears fence and pending ownership together, guarded by unit id and fence_op_id; successful apply clears them while installing the result and approved_by_unit_id - `inst-ap-fence-clear`

A zero-row acquisition is not success: re-read under tenant scope to distinguish a reference refusal,
stale revision, pending ownership or resumable fence. Never submit against an unowned barrier. Draft
type edits need no fence: drafts cannot be reserved. Published/deprecated edits use sku_change.
Orphan expiry cannot clear a pending unit's fence, and environment refusal cannot undo an earlier commit.

### sod-excludes-authors

1. [ ] - `p1` - On approve compare the authenticated actor with submitted_by and every current item's created_by - `inst-ap-sod-check`
2. [ ] - `p1` - Any match yields 403 SOD_VIOLATION, regardless of holding both author/submit and approve permissions; no decision or vote count changes - `inst-ap-sod-refuse`
3. [ ] - `p1` - Permit an independent actor with approve alone; preserve creator attribution during collection and stale refresh so changing the submitter cannot bypass SoD - `inst-ap-sod-independent`

### quorum-zero-records-unit

1. [ ] - `p1` - Resolve the tenant's kind-specific quorum or '*' default, using 1 when the default is absent; copy the resolved value into the new unit - `inst-ap-quorum-policy`
2. [ ] - `p1` - Even at zero, insert the unit/items, snapshot/hash and submission audit, acquire ownership and run apply revalidation in the submit transaction - `inst-ap-quorum-zero`
3. [ ] - `p1` - On success store approved with decided_at = submitted_at, no decisions, approval provenance and the ordinary terminal audit/events; on refusal roll back submission/apply while preserving any earlier fence - `inst-ap-quorum-record`

### stale-refresh-generation

1. [ ] - `p1` - For approve/reject require pending state and the reviewed generation; a mismatch returns 400 GENERATION_MISMATCH with the current generation - `inst-ap-stale-generation`
2. [ ] - `p1` - Re-collect the proposed result and fingerprint each item's after business content plus effective date, excluding pending ownership, fence and version metadata - `inst-ap-stale-hash`
3. [ ] - `p1` - On drift rewrite items, snapshot and hash, increment generation, mark prior decisions stale and advance unit version conditionally in the transaction - `inst-ap-stale-refresh`
4. [ ] - `p1` - Commit this refresh and return 400 UNIT_STALE with the new generation; no attempted vote or successful apply event is recorded - `inst-ap-stale-commit`
5. [ ] - `p1` - With unchanged content count only non-stale approvals of this generation, enforcing one vote per actor; duplicates return 409 DUPLICATE_VOTE - `inst-ap-stale-count`

### unit-version-contended

1. [ ] - `p1` - Load the scoped unit and conditionally advance version on every existing-unit mutation, including votes, refresh, withdrawal and terminal writes; use no database row lock - `inst-ap-unit-cas`
2. [ ] - `p1` - Zero affected rows returns 409 UNIT_CONTENDED and rolls back all writes of that attempt; the caller must re-read and retry against the winner's state - `inst-ap-unit-lost`
3. [ ] - `p1` - Reject closes a pending unit with a mandatory note after generation/content checks; withdraw requires the original submitter and pending state - `inst-ap-unit-close`
4. [ ] - `p1` - Every terminal transition unlocks in the same transaction and records audit plus ApprovalUnitDecided; a terminal unit refuses further decisions/withdrawal with UNIT_ALREADY_DECIDED - `inst-ap-unit-terminal`

## 4. States (CDSL)

| Entity | Transition and guard |
| --- | --- |
| SKU | draft → published through sku_publish; published ↔ deprecated and content changes through sku_change. Each successful publish/change appends a snapshot. |
| Retirement fence | published/deprecated → retiring after guarded fence commit; retiring → retired on apply; reject/withdraw or expired orphan recovery → saved prior lifecycle. A draft has no retirement edge: a never-published draft is deleted (P-D-206). |
| Type fence | type_change_pending false → true after guarded commit; pending review retains it; successful change, matching abort or expired orphan recovery clears it. |
| Unit | submit → pending for nonzero quorum, or approved for successful quorum zero; pending → pending below quorum or after refreshed generation; pending → approved/rejected/withdrawn terminally. |
| Ownership | pending_unit_id null → unit id conditionally; approval clears it and records approved_by_unit_id; abort clears it without approving new content. |

1. [ ] - `p1` - APPLY_REFUSED rolls back the current apply transaction; a previously committed fence and pending unit retain their state.
2. [ ] - `p1` - UNIT_STALE commits a new generation in pending state; old decisions remain visible as stale and cannot satisfy the new quorum.
3. [ ] - `p1` - A terminal unit and a retired SKU have no reopening transition.

## 5. API Surface

Paths are relative to `/bss-products/v1`. All POSTs accept optional Idempotency-Key. Errors use
Foundation's RFC-9457 Problem mapping; generation errors include the current/new generation.

| Route | Permission and contract |
| --- | --- |
| `POST /skus/{id}/submit` | products:submit; sku_publish for a draft. An optional body `{ note }`: the submitter's note, at most 2000 characters (400 NOTE_TOO_LONG), any other field 400 (P-D-219). |
| `POST /skus/{id}/changes` | products:submit; proposed content/lifecycle plus effective_from (default today) on published/deprecated SKU; type changes first fence. The optional `note` is the submitter's reason, at most 2000 characters (P-D-213, P-D-219). |
| `POST /skus/{id}/retire` | products:submit; commit guarded fence then submit/resume sku_retire. The optional body `{ note }` as on submit (P-D-219). |
| `POST /skus/{id}/unfence` | products:author; explicit recovery of an expired orphan only, never a pending unit's barrier. |
| `GET /approval-units?state&kind&ref_id&limit&cursor&$orderby` | products:read; one page of the tenant queue (P-D-224), ordered by submitted_at with a stable id tie-break; `limit` 200 by default, clamped at 500, `cursor` from `page_info`, bound to the narrowing. `$orderby=submitted_at desc` pages it newest first, the id breaking a tie the same way; the cursor carries its order and the order is not in the narrowing's hash; `$orderby` beside a cursor is 400 ORDER_WITH_CURSOR (P-D-227). A client merging pricing's and products' pages compares submitted_at as an instant, then the id as lower-case hex. Each unit carries `caller_can_approve`, and a page reads its units, their decisions and their items, three statements (P-D-228). |
| `GET /approval-units/counts?state&kind&ref_id` | products:read; the queue's narrowing counted by state and by kind, each named with 0 when none, and the total, in one grouped statement read outside any transaction; the queue's refusals (a kind products does not record is 400 `VALIDATION` on `kind`, on the queue too), and any other key 400 (P-D-227). |
| `GET /approval-units/{id}` | products:read; stored snapshot, generation, decisions (including stale) and live recomputation; a GET does not replace or refresh the stored snapshot. A unit whose draft was deleted after its rejection or withdrawal answers `impact_live: null` (P-D-206). Both unit reads carry `submit_note` (P-D-219). Every unit read and receipt carries `caller_can_approve`: `bss_approval::approve_eligibility` for the caller over the authors of the unit's stored items and its decisions, Approve only and not the grant (P-D-228). |
| `POST /approval-units/{id}/approve` | products:approve; generation required, SoD enforced. |
| `POST /approval-units/{id}/reject` | products:approve; generation and note required; one rejection closes the unit. A reject judges no separation of duties: the submitter and the SKU's creator may reject (P-D-228). |
| `POST /approval-units/{id}/withdraw` | products:submit plus submitter identity; pending only. |
| `GET /approval-policy`, `PUT /approval-policy` | products:settings for both reads and writes; direct tenant default/per-kind quorum management. The GET answers a strong content `ETag`; the PUT requires it as `If-Match` (missing or malformed 400, stale 409 STALE_REVISION), authorization first (P-D-205). |
| `DELETE /approval-policy/{kind}` | products:settings; removes one kind's override under the policy's `If-Match`, so the kind follows the default again; the default (`*`) is 400 POLICY_DEFAULT_REQUIRED, a kind without an override 404, a stale tag 409 STALE_REVISION; authorization first (P-D-216). |
| `DELETE /skus/{id}` | products:author; a never-published draft only, by its author (403 NOT_DRAFT_AUTHOR), If-Match; 204 with an audit row; SKU_NOT_DRAFT, ROW_LOCKED_PENDING, SKU_REFERENCED (409). A draft is deleted, never retired (P-D-206). |

Submit validation failure returns 400 with its code and no new unit (never 422, pricing D-403); a note over 2000 characters is NOTE_TOO_LONG, judged with the body's other violations (P-D-219). Conflicts include ROW_LOCKED_PENDING,
VERSION_ORDER, SKU_REFERENCED, SKU_TYPE_FROZEN, UNIT_CONTENDED and UNIT_ALREADY_DECIDED (409).
SOD_VIOLATION, NOT_SUBMITTER and USAGE_TYPE_FORBIDDEN (the usage-type catalog, read as the caller, refused
the caller at submit or approve; P-D-207) are 403. UNIT_STALE and GENERATION_MISMATCH are 400 with generation.
An apply environment error is APPLY_REFUSED with the underlying domain reason and no success outcome.

## 6. Data Model

[DESIGN §3.7](../DESIGN.md#37-database-schemas--tables) defines the four approval tables; Foundation
migrates them. This slice owns their domain writes:

| Table/columns | Mutation semantics |
| --- | --- |
| `products_approval_policy (tenant_id, kind, quorum)` | Nonnegative quorum; '*' default plus sku_publish/sku_change/sku_retire overrides; policy edits affect future submissions, not already copied quorum. |
| `products_approval_unit` | kind/ref_type/ref_id, state, common_effective_date, quorum_required, generation, submitted_by/at, decided_at/note, snapshot/hash, version, tenant/id, submit_note. common_effective_date holds effective_from; generation/version start at 1 and serve different purposes. `submit_note` (m20260928_000009, P-D-219) is written once at submit, as sent, and never rewritten: a refresh keeps it; it is not content, so neither the snapshot nor the hash carries it; a unit submitted before the migration reads null. |
| `products_approval_unit_item` | unit_id, item_type/id, created_by, before and after. Before is nullable for creation; after is proposed business content. Snapshot/hash use content and date, not storage locks. |
| `products_approval_decision` | unit_id, actor, generation, decision, note, at, stale; key is unit/actor/generation. Refresh marks existing decisions stale instead of deleting them. |
| SKU ownership | pending_unit_id and approved_by_unit_id are tenant-qualified links. Acquisition guards null pending id and observed version; terminal writes guard the current owner. |
| SKU fence | lifecycle retiring or type_change_pending; fence_prior_lifecycle, fenced_at, fence_op_id survive interrupted submission. Conditional cleanup preserves another operation's ownership. |

Approved content and its SkuVersion are written together. Retirement installs retired without fabricating
a publish/change snapshot. Unit item/decision access goes through the scoped parent. Neither the unit nor
the decision table stores an Idempotency-Key; Foundation's replay store is the only client-key authority.

## 7. Events & Alarms

Submit writes audit; quorum zero additionally writes terminal audit/event and the applicable SKU event.
Every approved/rejected/withdrawn transition emits `ApprovalUnitDecided` with unit_id, kind, state,
generation and actors, together with its audit and unlock. Actors identify actual decision participants;
quorum zero has no reviewer decisions. Successful applies add `SkuPublished`, `SkuChanged` or `SkuRetired`.
Slice 04 owns payload assembly; all records use the act's transaction.

UNIT_STALE emits no successful apply event; APPLY_REFUSED emits no terminal success event and rolls back.
Committed orphan fences are visible on the SKU and recoverable by the documented request paths; no
background auto-unfence or new alarm delivery is required. A reservation timeout never releases a barrier.

## 8. Definitions of Done

- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-lifecycle-edges`
- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-sku-publish-unit`
- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-sku-change-effective-from`
- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-sku-retire-fenced`
- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-type-change-fenced`
- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-sod-excludes-authors`
- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-stale-refresh-generation`
- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-unit-contended`
- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-quorum-zero-records-unit`
- see [features/lifecycle-approvals.md](../features/lifecycle-approvals.md) — `cpt-cf-bss-products-dod-terminal-audit-and-event`

## 9. Acceptance Criteria

Numbered criteria refer to [PRD §9](../PRD.md#9-acceptance-criteria).

| Trace | Given / When / Then |
| --- | --- |
| `cpt-cf-bss-products-fr-sku-lifecycle`; AC #7, #19 | Given eligible drafts and published/deprecated SKUs, when approved publish/change executes, then only declared edges occur and snapshots append; pending ownership rejects concurrent edit with ROW_LOCKED_PENDING. Pricing refuses new entries/items on retiring with SKU_RETIRING and new-plan adoption of deprecated with ITEM_SKU_DEPRECATED. |
| `cpt-cf-bss-products-fr-sku-descriptors`; AC #3–4, #8 | Given GL 4010-STOR, when 4012-STOR effective October 1 is approved, then it appends a dated snapshot and emits SkuChanged; earlier bindings remain unchanged. Locked edits fail ROW_LOCKED_PENDING; backwards dates fail VERSION_ORDER. |
| `cpt-cf-bss-products-fr-sku-retire-fenced`; AC #10–12 | Given live references, when retire is requested, then SKU_REFERENCED leaves the SKU unfenced. Given an orphan, retry resumes or expired recovery restores; a pending fence cannot expire. Defensive apply refusal leaves retiring until reject/withdraw restores the prior lifecycle. |
| `cpt-cf-bss-products-fr-sku-type-frozen`; AC #2, #23 | Given a live reservation or concurrent reserve, when a type fence is attempted, then either SKU_TYPE_FROZEN refuses it or the committed fence excludes reserve with SKU_FENCED; no interleaving accepts both. |
| `cpt-cf-bss-products-fr-approval-units`; AC #14 | Given an item author different from the submitter, when either approves even with both grants, then SOD_VIOLATION contributes no vote. An independent approve-only actor can decide. |
| Same FR; AC #15–16 | Given content drift, when a current-generation vote arrives, then UNIT_STALE commits a new generation and stales earlier votes. A delayed generation gets GENERATION_MISMATCH; a repeat current-generation vote gets DUPLICATE_VOTE. |
| Same FR; AC #17–19 | Given quorum one/two units, when distinct eligible reviewers vote, then only quorum applies; reject needs a note, non-submitter withdrawal fails NOT_SUBMITTER, terminal mutation fails UNIT_ALREADY_DECIDED and a concurrent loser gets UNIT_CONTENDED. |
| Same FR; `cpt-cf-bss-products-fr-events`; AC #20–21 | Given quorum zero, when submitted successfully, then an approved unit with equal submitted/decided times and no decisions remains, with audit and domain/decision events. Every terminal path commits audit/events with state; refused apply leaves no terminal success. |

## 10. Non-Functional Considerations

`cpt-cf-bss-products-nfr-authz` (AC #28) separates submit and approve permissions and preserves SoD
inside the domain. `cpt-cf-bss-products-nfr-tenant-isolation` scopes policy, unit detail, children and
SKU ownership. `cpt-cf-bss-products-nfr-audit` preserves rejected, withdrawn and stale review history.
`cpt-cf-bss-products-nfr-two-backends` (AC #29) requires identical quorum 0/1/2, conditional-write,
fence/recovery and concurrent-decision outcomes on SQLite and Postgres. Existing-unit CAS contention
is a client-visible UNIT_CONTENDED, distinct from Foundation's bounded serialization retry.
