<!-- CONFLUENCE_TITLE: [BSS]: Products — Lifecycle & Approvals (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Product Catalog team -->

# Feature: Lifecycle & Approvals

- [ ] `p1` - **ID**: `cpt-cf-bss-products-featstatus-lifecycle-approvals-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-products-feature-lifecycle-approvals`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
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
  - [Stale refresh](#stale-refresh)
  - [unit-version-contended](#unit-version-contended)
- [4. States (CDSL)](#4-states-cdsl)
  - [Lifecycle & Approvals states](#lifecycle--approvals-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Governed lifecycle edges](#governed-lifecycle-edges)
  - [Publication creates and applies a unit](#publication-creates-and-applies-a-unit)
  - [Changes carry an effective date](#changes-carry-an-effective-date)
  - [Retirement is fenced and resumable](#retirement-is-fenced-and-resumable)
  - [Type changes use the reciprocal fence](#type-changes-use-the-reciprocal-fence)
  - [Authors and submitters cannot approve](#authors-and-submitters-cannot-approve)
  - [Stale content commits a new generation](#stale-content-commits-a-new-generation)
  - [Every unit mutation checks version](#every-unit-mutation-checks-version)
  - [Quorum zero preserves the approval record](#quorum-zero-preserves-the-approval-record)
  - [All terminal paths record audit and decision event](#all-terminal-paths-record-audit-and-decision-event)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This phase 1c feature implements [design slice 03](../design/03-lifecycle-approvals.md).
The implementation order and integration
prerequisites are in [DECOMPOSITION](../DECOMPOSITION.md); [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the architecture and schema authority. Unchecked items describe implementation obligations.

### 1.2 Purpose

Govern publish/change/retire with the shared approval shape, copied policy quorum, author SoD, reviewed generations and recoverable local fences. GL changes reach period bindings through dated versions; all terminal paths preserve audit and approval provenance. The queue counts its units by state and kind and pages newest first on request (P-D-227), and every unit says whether its reader may approve it, by the engine's own rule (P-D-228). The unit reads name the submitter and each decision's actor (`submitted_by_name`, `actor_name`), resolved once per answer through Account Management under the reader's own rights, null when no name is available now (P-D-262). The approvals inbox's source answers the card unnamed and declares this gear's system actors, so the inbox names the card in one lookup (P-D-262).

Requirements: `cpt-cf-bss-products-fr-sku-descriptors`, `cpt-cf-bss-products-fr-sku-lifecycle`, `cpt-cf-bss-products-fr-sku-retire-fenced`, `cpt-cf-bss-products-fr-approval-units`, `cpt-cf-bss-products-fr-sku-type-frozen`, `cpt-cf-bss-products-fr-concurrency-idempotency`, `cpt-cf-bss-products-nfr-authz`, `cpt-cf-bss-products-nfr-audit`, `cpt-cf-bss-products-fr-sku-metering`, `cpt-cf-bss-products-fr-sku-versions`, `cpt-cf-bss-products-fr-reference-registry`, `cpt-cf-bss-products-fr-events`.

Design principles: `cpt-cf-bss-products-principle-fence-before-count`, `cpt-cf-bss-products-principle-business-content-fingerprint`.

### 1.3 Actors

`cpt-cf-bss-products-actor-catalog-admin`, `cpt-cf-bss-products-actor-finance-reviewer`, `cpt-cf-bss-products-actor-auditor`. Authenticated doors enforce the applicable products:read, author, submit,
approve or settings permission and tenant scope; holding multiple grants never bypasses SoD.

### 1.4 References

- [PRD](../PRD.md), especially §9's numbered acceptance criteria cited below.
- [DESIGN](../DESIGN.md), §3's model, API, transactions and schema.
- [Slice 03](../design/03-lifecycle-approvals.md), including API/data details and the matching instruction names.
- [DECISIONS](../DECISIONS.md), P-D-184–194, including the superseding reservation, version and generation decisions.
- Source “spec”: `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout, §2.2, §4, §6, §7.2–§7.3 and §13.
- PRD use cases: `cpt-cf-bss-products-usecase-change-gl-code`, `cpt-cf-bss-products-usecase-retire-sku`.

## 2. Actor Flows (CDSL)

### Author submits publication

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-lifecycle-approvals-author-submits-publication`

1. [ ] - `p1` - Authenticate products:submit and resolve optional POST replay; collect the draft SKU and validate publication, including meter resolution - `inst-ap-publish-input`
2. [ ] - `p1` - In one transaction record sku_publish, items, reviewed snapshot/hash, effective date and copied quorum, then acquire pending_unit_id conditionally on the observed SKU version and null ownership - `inst-ap-publish-submit`
3. [ ] - `p1` - If ownership acquisition affects zero rows, return ROW_LOCKED_PENDING and roll back the unit/items; otherwise write submission audit - `inst-ap-publish-lock`
4. [ ] - `p1` - At nonzero quorum commit pending; at zero quorum run the same apply/terminal path immediately, preserving the unit record - `inst-ap-publish-quorum`

### Reviewer approves a unit

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-lifecycle-approvals-reviewer-approves-a-unit`

1. [ ] - `p1` - Read stored snapshot and live recomputation under products:read, then send the reviewed generation with products:approve - `inst-ap-review-read`
2. [ ] - `p1` - In a scoped transaction conditionally advance the observed unit version; refuse non-pending state, generation mismatch and author/submitter approval before a vote can count - `inst-ap-review-guards`
3. [ ] - `p1` - Re-collect proposed business content; on fingerprint drift commit the refresh and return UNIT_STALE with its new generation without recording the attempted vote - `inst-ap-review-refresh`
4. [ ] - `p1` - Insert one decision per actor/current generation; below quorum commit pending, at quorum revalidate and apply the subject - `inst-ap-review-vote`
5. [ ] - `p1` - On success clear pending ownership, retain approved_by_unit_id, close the unit and write terminal audit, ApprovalUnitDecided and the SKU event in the same transaction - `inst-ap-review-apply`
6. [ ] - `p1` - An environment failure returns APPLY_REFUSED and rolls back the vote/apply transaction, retaining the earlier pending unit and any committed fence - `inst-ap-review-refused`

### Finance changes a GL code

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-lifecycle-approvals-finance-changes-a-gl-code`

1. [ ] - `p1` - Propose Storage's gl_code from 4010-STOR to 4012-STOR with effective_from October 1 via the changes door on a published or deprecated SKU - `inst-ap-gl-propose`
2. [ ] - `p1` - Validate the proposed date and content, record a sku_change snapshot with before/after/date and acquire pending ownership; only a type change needs the type fence - `inst-ap-gl-submit`
3. [ ] - `p1` - An independent reviewer approves the generation; at quorum append SkuVersion and emit SkuChanged with max(requested date, apply date), retaining the requested date in the unit snapshot, atomically with the head and terminal unit - `inst-ap-gl-apply`
4. [ ] - `p1` - Pricing's period-start version read binds the new GL on or after the later of October 1 and the approval date; earlier bindings keep the old GL without any Pricing unit - `inst-ap-gl-consume`

### Administrator retires a SKU

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-lifecycle-approvals-administrator-retires-a-sku`

1. [ ] - `p1` - Authenticate products:submit and check replay, then acquire the guarded retirement fence inside the submission transaction; a live local reference returns SKU_REFERENCED before a unit exists - `inst-ap-retire-fence`
2. [ ] - `p1` - Submit sku_retire in the same transaction using the same fence_op_id; a retry after interruption rechecks the registry and resumes the orphan operation - `inst-ap-retire-submit`
3. [ ] - `p1` - At quorum revalidate zero references and conditionally apply retired; failed apply preserves retiring and the pending unit - `inst-ap-retire-apply`
4. [ ] - `p1` - A reviewer rejects with a note or the submitter withdraws; the terminal transaction clears matching pending/fence ownership and restores the saved prior lifecycle - `inst-ap-retire-abort`

## 3. Processes / Business Logic (CDSL)

### lifecycle-edges

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-lifecycle-approvals-lifecycle-edges`

1. [ ] - `p1` - sku_publish accepts draft and installs published with a new immediate snapshot; sku_change accepts published/deprecated content and the published ↔ deprecated edges - `inst-ap-lifecycle-kind`
2. [ ] - `p1` - For a change default the requested effective_from to today and refuse a past date at submit; apply max(requested date, apply date) and reject applied dates before the latest stored version with VERSION_ORDER; equal dates append a higher published_version - `inst-ap-lifecycle-date`
3. [ ] - `p1` - Retirement of a published or deprecated SKU first saves its prior lifecycle and installs retiring, then sku_retire installs retired only after approval and environment validation; a never-published draft is not retired but deleted by its author (DELETE /skus/{id}, P-D-206) - `inst-ap-lifecycle-retire`
4. [ ] - `p1` - Reject unsupported lifecycle edges, edits while pending and any return from retired; rejected/withdrawn publication or ordinary change leaves prior business content intact - `inst-ap-lifecycle-refuse`

### Atomic fence and submission

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-lifecycle-approvals-fence-commits-first`

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

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-lifecycle-approvals-sod-excludes-authors`

1. [ ] - `p1` - On approve compare the authenticated actor with submitted_by and every current item's created_by - `inst-ap-sod-check`
2. [ ] - `p1` - Any match yields 403 SOD_VIOLATION, regardless of holding both author/submit and approve permissions; no decision or vote count changes - `inst-ap-sod-refuse`
3. [ ] - `p1` - Permit an independent actor with approve alone; preserve creator attribution during collection and stale refresh so changing the submitter cannot bypass SoD - `inst-ap-sod-independent`

### quorum-zero-records-unit

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-lifecycle-approvals-quorum-zero-records-unit`

1. [ ] - `p1` - Resolve the tenant's kind-specific quorum or '*' default, using 1 when the default is absent; copy the resolved value into the new unit - `inst-ap-quorum-policy`
2. [ ] - `p1` - Even at zero, insert the unit/items, snapshot/hash and submission audit, acquire ownership and run apply revalidation in the submit transaction - `inst-ap-quorum-zero`
3. [ ] - `p1` - On success store approved with decided_at = submitted_at, no decisions, approval provenance and the ordinary terminal audit/events; on refusal roll back submission/apply while preserving any earlier fence - `inst-ap-quorum-record`

### Stale refresh

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-lifecycle-approvals-stale-refresh-generation`

1. [ ] - `p1` - For approve/reject require pending state and the reviewed generation; a mismatch returns 400 GENERATION_MISMATCH with the current generation - `inst-ap-stale-generation`
2. [ ] - `p1` - Re-collect the proposed result and fingerprint each item's after business content plus effective date, excluding pending ownership, fence and version metadata - `inst-ap-stale-hash`
3. [ ] - `p1` - On drift rewrite items, snapshot and hash, increment generation, mark prior decisions stale and advance unit version conditionally in the transaction - `inst-ap-stale-refresh`
4. [ ] - `p1` - Commit this refresh and return 400 UNIT_STALE with the new generation; no attempted vote or successful apply event is recorded - `inst-ap-stale-commit`
5. [ ] - `p1` - With unchanged content count only non-stale approvals of this generation, enforcing one vote per actor; duplicates return 409 DUPLICATE_VOTE - `inst-ap-stale-count`

### unit-version-contended

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-lifecycle-approvals-unit-version-contended`

1. [ ] - `p1` - Load the scoped unit and conditionally advance version on every existing-unit mutation, including votes, refresh, withdrawal and terminal writes; use no database row lock - `inst-ap-unit-cas`
2. [ ] - `p1` - Zero affected rows returns 409 UNIT_CONTENDED and rolls back all writes of that attempt; the caller must re-read and retry against the winner's state - `inst-ap-unit-lost`
3. [ ] - `p1` - Reject closes a pending unit with a mandatory note after generation/content checks; withdraw requires the original submitter and pending state - `inst-ap-unit-close`
4. [ ] - `p1` - Every terminal transition unlocks in the same transaction and records audit plus ApprovalUnitDecided; a terminal unit refuses further decisions/withdrawal with UNIT_ALREADY_DECIDED - `inst-ap-unit-terminal`

## 4. States (CDSL)

### Lifecycle & Approvals states

- [ ] `p1` - **ID**: `cpt-cf-bss-products-state-lifecycle-approvals`

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

## 5. Definitions of Done

These definitions own this feature's 10 DoDs; the design slice references them without redefining them.
Design constraints: `cpt-cf-bss-products-constraint-approval-shape`, `cpt-cf-bss-products-constraint-no-row-locks`.

### Governed lifecycle edges

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-lifecycle-edges`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/domain/sku.rs`.

The lifecycle is draft, published, deprecated or retired (P-D-248): sku_publish installs published, sku_change governs published/deprecated content and the reversible published/deprecated edge, and a dated change waits for its date (P-D-249). sku_retire installs retired behind `retire_pending`, which keeps the SKU's lifecycle until apply; a never-published draft is deleted by its author, never retired (P-D-206). Retired has no reopening transition; rejected/withdrawn publication or change preserves prior business content, and a retirement abort restores the saved lifecycle. Pending ownership prevents direct editing or a second unit, and the exposed lifecycle supports Pricing's SKU_RETIRING and ITEM_SKU_DEPRECATED adoption guards (spec §3 item 35, §4, §6, §7.2; DESIGN §3.1).

### Publication creates and applies a unit

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-sku-publish-unit`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/domain/approvals/publish.rs`.

POST submit validates a draft, resolves usage metering and records sku_publish with items, author attribution, snapshot/hash, copied quorum and conditional pending ownership in one audited transaction. Quorum-reaching apply revalidates, publishes, increments published_version, appends the immediate snapshot, clears pending ownership and retains approved_by_unit_id with its terminal records. Invalid submit creates no unit and failed ownership acquisition rolls back its partial writes (spec §2.2, §4, §6, §7.2; DESIGN §3.2).

### Changes carry an effective date

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-sku-change-effective-from`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/domain/approvals/change.rs`.

POST changes accepts proposed content and/or lifecycle for a published or deprecated SKU as sku_change with effective_from defaulting to today. Submit refuses a past requested date. Apply uses max(requested effective_from, apply date) for the version and SkuChanged, retains the requested date in the unit snapshot, revalidates the timeline and updates the latest head in the terminal transaction; an applied date before the latest version is VERSION_ORDER and equal dates advance published_version. Products serves the stored snapshots by as_of (spec §2 decision 14, §2.2, §6, §7.2; DESIGN §3.1, §3.6).

### Retirement is fenced and resumable

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-sku-retire-fenced`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/api/rest/sku_governance.rs`.

Retirement fences and submits sku_retire in ONE transaction: the conditional fence write is guarded by NOT EXISTS (live reserved/confirmed reference), otherwise SKU_REFERENCED refuses it. A fence found without a unit is resumed; an expired orphan is lifted by the next SKU request or explicit unfence. Apply rechecks the environment; APPLY_REFUSED with SKU_REFERENCED rolls back apply while retaining the pending fence. Reject/withdraw restores the prior lifecycle with ownership-guarded cleanup (spec decision 17, §2.2; DESIGN §3.1, §3.6).

### Type changes use the reciprocal fence

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-type-change-fenced`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/api/rest/sku_governance.rs`.

Only published/deprecated SKU type changes acquire type_change_pending with durable fence metadata, guarded by NOT EXISTS (live reference), and submit sku_change in the same transaction. A live reference returns SKU_TYPE_FROZEN; the fence refuses new reservations until approved change or guarded abort clears it. Orphan resume and expiry follow retirement recovery. Draft type edits require no fence because drafts cannot be reserved or priced (spec decision 17, §2.2; DESIGN §3.1, §3.7).

### Authors and submitters cannot approve

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-sod-excludes-authors`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/api/rest/approval_units.rs`.

Approval excludes submitted_by and every current item's created_by with 403 SOD_VIOLATION, including when another actor submitted the author's work. Author attribution survives collection and refresh; holding both grants does not bypass the check, while an independent approve-only reviewer is eligible (spec §6; DESIGN §3.2; P-D-190).

### Stale content commits a new generation

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-stale-refresh-generation`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/api/rest/approval_units.rs`.

Approve/reject names the reviewed generation; a mismatch returns GENERATION_MISMATCH with the current generation, and duplicate current-generation voting returns DUPLICATE_VOTE. Before a vote counts, re-collection fingerprints proposed business content and effective date, excluding lock/fence/version metadata and the submitter's note (P-D-219); drift rewrites items/snapshot/hash, increments generation, keeps the unit's `submit_note`, preserves earlier decisions as stale and commits before returning UNIT_STALE with the new generation. Only current-generation non-stale approvals count, and an environment failure remains a rolled-back APPLY_REFUSED (spec §2.2, §6; DESIGN §3.2, §3.6).

### Every unit mutation checks version

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-unit-contended`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/api/rest/approval_units.rs`.

Every existing-unit mutation, including decisions, refresh and withdrawal, conditionally advances the observed version without database row locks. A lost race returns 409 UNIT_CONTENDED and rolls back that attempt's writes; non-pending units return UNIT_ALREADY_DECIDED, rejection requires a note and withdrawal requires the original submitter. GET detail returns stored snapshot, generation, history and live recomputation without silently refreshing stored content, and both unit reads carry the `submit_note` its submit, change or retire sent (P-D-219) (spec §2.2, §6, §7.2; DESIGN §3.2–§3.3).

### Quorum zero preserves the approval record

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-quorum-zero-records-unit`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/api/rest/approval_policy.rs`.

Policy reads choose the tenant kind override then the default, falling back to quorum one if the default is missing; GET/PUT approval-policy changes future submissions directly with SETTINGS required for both reading and writing; the GET answers a content ETag the PUT requires as If-Match (P-D-205); DELETE approval-policy/{kind} resets one kind's override to the default under the same If-Match, and the default is never deleted (P-D-216). Even at zero quorum, submit records the unit, items, snapshot and submission audit, acquires ownership and applies ordinary validation. Success records approved with decided_at equal to submitted_at, no decisions and the ordinary terminal audit/events; copied quorum never changes with later policy edits (spec §6, §14; DESIGN §3.2–§3.3; P-D-190).

### All terminal paths record audit and decision event

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-terminal-audit-and-event`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/api/rest/governance.rs`.

Approve, reject, withdraw and quorum-zero approval clear pending ownership with state, audit and ApprovalUnitDecided in one transaction, with successful apply also recording its SKU event. Matching fence cleanup restores prior lifecycle on abort or installs the approved result, retaining approved_by_unit_id on approval. Submission alone is audited without an event; rollback and stale refresh emit no successful apply event (spec §6, §7.3; DESIGN §3.2, §3.4).

**Owed by pricing (phase 2).** Pricing must bind descriptors by period start, preserve earlier bindings and perform no book approval or refreeze for SKU descriptor changes.

## 6. Acceptance Criteria

Each criterion below corresponds to exactly one DoD above and cites [PRD §9](../PRD.md#9-acceptance-criteria).
Verify these during phase 1c on SQLite and Postgres, including tenant isolation and denied permissions;
this document does not claim those implementation tests have run. Pricing-side assertions are contract
obligations here and integration checks when its phase 2 caller path exists.

| DoD | PRD trace | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-products-dod-lifecycle-edges` | AC #7, #19; `cpt-cf-bss-products-fr-sku-lifecycle`, `cpt-cf-bss-products-fr-approval-units` | Given a draft and published/deprecated SKUs, when eligible publish/change/retire units apply, then only the declared lifecycle edges occur (a draft is deleted, never retired, P-D-206); locked edits fail with ROW_LOCKED_PENDING and retired cannot reopen, while Pricing contract checks refuse entry/item adoption of retiring with SKU_RETIRING and new-plan adoption of deprecated with ITEM_SKU_DEPRECATED. |
| `cpt-cf-bss-products-dod-sku-publish-unit` | AC #5, #19, #20; `cpt-cf-bss-products-fr-sku-lifecycle`, `cpt-cf-bss-products-fr-approval-units`, `cpt-cf-bss-products-fr-sku-metering` | Given valid drafts under quorum one and two, when eligible reviewers approve the current generation, then one or two votes respectively publish with version/provenance and one vote leaves quorum two pending; incomplete usage content fails with USAGE_NEEDS_METER and competing submission fails with ROW_LOCKED_PENDING without an extra unit. |
| `cpt-cf-bss-products-dod-sku-change-effective-from` | AC #3, #4, #8; `cpt-cf-bss-products-fr-sku-descriptors`, `cpt-cf-bss-products-fr-sku-versions` | Given Storage GL 4010-STOR, when independent review approves 4012-STOR effective October 1, then its dated version and SkuChanged commit and earlier bindings retain the old GL; a backwards date fails with VERSION_ORDER and direct editing of the pending proposal fails with ROW_LOCKED_PENDING. |
| `cpt-cf-bss-products-dod-sku-retire-fenced` | AC #10, #11, #12; `cpt-cf-bss-products-fr-sku-retire-fenced` | Given a live reservation, when retire is requested, then SKU_REFERENCED leaves lifecycle unfenced and creates no unit; given a committed orphan fence, retry resumes or expired recovery restores it, but never clears a pending unit's fence; given defensive apply refusal, APPLY_REFUSED/SKU_REFERENCED preserves retiring until matching rejection/withdrawal restores the saved lifecycle. |
| `cpt-cf-bss-products-dod-type-change-fenced` | AC #2, #12, #23; `cpt-cf-bss-products-fr-sku-type-frozen`, `cpt-cf-bss-products-fr-reference-registry` | Given a reference reservation racing a type change, when one transaction wins, then the other receives SKU_TYPE_FROZEN or SKU_FENCED; given a pending type-change unit, rejection/withdrawal clears its fence and leaves type unchanged, and orphan recovery cannot clear another unit's ownership. |
| `cpt-cf-bss-products-dod-sod-excludes-authors` | AC #14, #28; `cpt-cf-bss-products-fr-approval-units`, `cpt-cf-bss-products-nfr-authz` | Given an item authored by A and submitted by B, when either approves with the correct generation and both grants, then SOD_VIOLATION records no vote; an independent reviewer with approve but no submit can vote, while a caller without approve is denied. |
| `cpt-cf-bss-products-dod-stale-refresh-generation` | AC #15, #16, #21; `cpt-cf-bss-products-fr-approval-units` | Given a pending unit with changed proposed content, when a current-generation reviewer decides, then UNIT_STALE returns the incremented generation after committing refreshed content and stale old decisions without counting that vote; a delayed generation fails with GENERATION_MISMATCH, duplicate current voting fails with DUPLICATE_VOTE, and changing storage metadata alone causes no refresh. |
| `cpt-cf-bss-products-dod-unit-contended` | AC #17, #18; `cpt-cf-bss-products-fr-approval-units`, `cpt-cf-bss-products-fr-concurrency-idempotency` | Given two writers observing the same pending unit version, when one commits, then the other returns UNIT_CONTENDED without overwriting decisions; non-submitter withdrawal fails with NOT_SUBMITTER, terminal mutation fails with UNIT_ALREADY_DECIDED, and a note-free rejection cannot close the unit. |
| `cpt-cf-bss-products-dod-quorum-zero-records-unit` | AC #19, #20, #28; `cpt-cf-bss-products-fr-approval-units`, `cpt-cf-bss-products-fr-events` | Given zero, one and two policy quorums, when units submit, then zero applies with a durable approved unit, equal timestamps and no decisions, while others await their copied quorum; missing default means one, denied settings writes do not change policy, and failed zero-quorum apply rolls back rather than fabricating approval. |
| `cpt-cf-bss-products-dod-terminal-audit-and-event` | AC #20, #21; `cpt-cf-bss-products-fr-events`, `cpt-cf-bss-products-nfr-audit` | Given each terminal path, when it completes, then state, unlock, audit and ApprovalUnitDecided commit together and successful apply includes the corresponding SKU event; when apply or a required audit/outbox write fails, then no terminal success survives, and UNIT_STALE leaves the unit pending. |
