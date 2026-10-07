<!-- CONFLUENCE_TITLE: [BSS]: Products — Foundation (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Product Catalog team -->

# Feature: Foundation

- [ ] `p1` - **ID**: `cpt-cf-bss-products-featstatus-foundation-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-products-feature-foundation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [A caller executes a scoped mutation](#a-caller-executes-a-scoped-mutation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [two-backends](#two-backends)
  - [if-match](#if-match)
  - [idempotency-key](#idempotency-key)
  - [audit-row](#audit-row)
  - [outbox-same-tx](#outbox-same-tx)
  - [Conditional lock](#conditional-lock)
- [4. States (CDSL)](#4-states-cdsl)
  - [Foundation states](#foundation-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Tables on both backends](#tables-on-both-backends)
  - [Immutable SKU version table](#immutable-sku-version-table)
  - [Append-only audit records](#append-only-audit-records)
  - [Single client-key replay store](#single-client-key-replay-store)
  - [If-Match uses the concurrency version](#if-match-uses-the-concurrency-version)
  - [Outbox shares the state transaction](#outbox-shares-the-state-transaction)
  - [Conditional approval Store and pending ownership](#conditional-approval-store-and-pending-ownership)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This phase 1c feature implements [design slice 01](../design/01-foundation.md).
The implementation order and integration
prerequisites are in [DECOMPOSITION](../DECOMPOSITION.md); [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the architecture and schema authority. Unchecked items describe implementation obligations.

### 1.2 Purpose

Provide the fresh migration chain, scoped repositories, shared approval Store, concurrency/replay and atomic audit/outbox infrastructure without adding routes.

Requirements: `cpt-cf-bss-products-fr-concurrency-idempotency`, `cpt-cf-bss-products-nfr-authz`, `cpt-cf-bss-products-nfr-audit`, `cpt-cf-bss-products-nfr-tenant-isolation`, `cpt-cf-bss-products-nfr-two-backends`, `cpt-cf-bss-products-fr-sku-define`, `cpt-cf-bss-products-fr-sku-versions`, `cpt-cf-bss-products-fr-events`, `cpt-cf-bss-products-fr-sku-retire-fenced`, `cpt-cf-bss-products-fr-category-flat`, `cpt-cf-bss-products-fr-approval-units`.

### 1.3 Actors

`cpt-cf-bss-products-actor-catalog-admin`, `cpt-cf-bss-products-actor-finance-reviewer`, `cpt-cf-bss-products-actor-auditor`. Authenticated doors enforce the applicable products:read, author, submit,
approve or settings permission and tenant scope; holding multiple grants never bypasses SoD.

### 1.4 References

- [PRD](../PRD.md), especially §9's numbered acceptance criteria cited below.
- [DESIGN](../DESIGN.md), §3's model, API, transactions and schema.
- [Slice 01](../design/01-foundation.md), including API/data details and the matching instruction names.
- [DECISIONS](../DECISIONS.md), P-D-184–194, including the superseding reservation, version and generation decisions.
- Source “spec”: `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout, §2.2, §4, §6, §7.2–§7.3 and §13.

## 2. Actor Flows (CDSL)

### A caller executes a scoped mutation

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-foundation-a-caller-executes-a-scoped-mutation`

1. [ ] - `p1` - Authenticate the caller and obtain the PolicyEnforcer-derived tenant AccessScope before opening any repository or replay entry - `inst-fnd-authorize`
2. [ ] - `p1` - For a keyed POST, resolve replay before performing fence or approval work; for PATCH, pass the required If-Match version to the conditional repository write - `inst-fnd-preconditions`
3. [ ] - `p1` - Open one scoped transaction for the operation; invoke the owning slice's domain rule and persist state, required audit and outbox records through that transaction - `inst-fnd-mutate`
4. [ ] - `p1` - Store the replay answer when applicable, commit, then return the result and the row's new ETag; only committed outbox rows can dispatch - `inst-fnd-commit`
5. [ ] - `p1` - On a refused or failed transaction, roll back its state, claim, audit and outbox writes; a fence committed earlier remains available for recovery - `inst-fnd-rollback`

## 3. Processes / Business Logic (CDSL)

### two-backends

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-foundation-two-backends`

1. [ ] - `p1` - Run the fresh chain on SQLite and Postgres with the same keys, checks and append-only invariants; do not migrate deployed data - `inst-fnd-migrate`
2. [ ] - `p1` - Execute repositories through SecureConn and scoped transactions; approval child rows are reachable only through a tenant-scoped parent - `inst-fnd-scope`
3. [ ] - `p1` - Use conditional writes instead of FOR UPDATE; reserve and fence transactions use serializable isolation on Postgres and SQLite writer serialization - `inst-fnd-isolation`
4. [ ] - `p1` - Retry a Postgres serialization failure once and SQLite lock-upgrade failure through the same bounded transaction retry loop; re-read all guards on retry - `inst-fnd-retry`

### if-match

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-foundation-if-match`

1. [ ] - `p1` - Obtain the concurrency version from If-Match; use the toolkit precondition response if absent, use SKU revision or category version, never published_version - `inst-fnd-if-match-read`
2. [ ] - `p1` - Apply a tenant/id and concurrency-token conditional SKU or category update, checking pending ownership where relevant and incrementing version atomically - `inst-fnd-if-match-cas`
3. [ ] - `p1` - A stale version returns 409 STALE_REVISION and makes no write; a successful read or write exposes the concurrency version as ETag - `inst-fnd-if-match-result`

### idempotency-key

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-foundation-idempotency-key`

1. [ ] - `p1` - Every POST accepts an optional Idempotency-Key; a keyless POST bypasses the client-key store and a keyed POST addresses tenant_id, concrete endpoint and client_key, checking a retained answer before any fence or unit work - `inst-fnd-replay-lookup`
2. [ ] - `p1` - Compare payload_hash before replay; different content cannot execute or replay another request's answer under the same retained key - `inst-fnd-replay-hash`
3. [ ] - `p1` - Claim the key conditionally within the guarded transaction, with expires_at set for 24-hour retention; concurrent claims cannot both perform the mutation - `inst-fnd-replay-claim`
4. [ ] - `p1` - Answer with response_status and response_body in that same transaction; rollback removes the uncommitted claim, and replay returns the saved answer without new domain writes - `inst-fnd-replay-answer`
5. [ ] - `p1` - Commit fence, submission, claim and answer in one transaction; when an existing orphan fence is resumed, retain its fence_op_id instead of starting an independent operation - `inst-fnd-replay-fence`

A committed `UNIT_STALE` refresh is a domain outcome, not a database rollback. The approval service
commits the refresh and its keyed receipt together. Repeating that key replays UNIT_STALE; a decision
on the refreshed generation uses a new key because a changed body under the retained key conflicts.
There is no approval-unit idempotency column and no second replay store (spec §2.2).

### audit-row

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-foundation-audit-row`

1. [ ] - `p1` - Construct a tenant-scoped row with actor_ref, action, subject_kind, a subject identifier, written_at and available correlation_id; use attempted_key for a refusal without a subject id - `inst-fnd-audit-build`
2. [ ] - `p1` - Insert approval submission audit with the submission; insert every terminal audit and operator force-release audit with the corresponding state transaction, even when it also emits an event - `inst-fnd-audit-write`
3. [ ] - `p1` - Insert as unsealed with null chain metadata; reject deletion and record-field updates, allowing only the reserved one-way sealing transition that preserves the record - `inst-fnd-audit-guard`

### outbox-same-tx

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-foundation-outbox-same-tx`

1. [ ] - `p1` - Accept the caller's scoped transaction in the event writer; never open a second connection to record the event - `inst-fnd-outbox-tx`
2. [ ] - `p1` - Append required domain and approval events to the existing toolkit outbox with the state and audit writes - `inst-fnd-outbox-append`
3. [ ] - `p1` - Commit all or roll back all; dispatch only after commit, without publishing success for APPLY_REFUSED or stale refresh - `inst-fnd-outbox-dispatch`

### Conditional lock

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-foundation-conditional-lock`

1. [ ] - `p1` - Read the scoped SKU and its concurrency version; create the proposed unit and items inside the submit transaction - `inst-fnd-lock-read`
2. [ ] - `p1` - Set pending_unit_id only where tenant/id match, pending_unit_id is null and SKU revision equals the observed value - `inst-fnd-lock-acquire`
3. [ ] - `p1` - If zero rows change, return ROW_LOCKED_PENDING and roll back unit/items/audit; otherwise continue with submission audit and quorum handling - `inst-fnd-lock-result`
4. [ ] - `p1` - Clear ownership only with matching unit and any fence_op_id; no FOR UPDATE or unscoped connection participates - `inst-fnd-lock-clear`

## 4. States (CDSL)

### Foundation states

- [ ] `p1` - **ID**: `cpt-cf-bss-products-state-foundation`

The following are storage states; lifecycle and unit business states belong to slice 03.

1. [ ] - `p1` - Replay: absent → claimed → answered inside a successful mutation transaction; rollback returns to the pre-transaction state. An answered row is replayable until expiry; conditional expiry handling must admit only one replacement claimant.
2. [ ] - `p1` - Audit: insert unsealed → optional sealed, preserving every record field; deletion and all other transitions are refused.
3. [ ] - `p1` - Unit Store: an existing version v → v + 1 only if the conditional write matches v; zero affected rows yields UNIT_CONTENDED and rolls back the attempted unit mutation.
4. [ ] - `p1` - Pending ownership: null → unit id only with the observed SKU version; zero affected rows yields ROW_LOCKED_PENDING and rolls back submission. Terminal clear additionally checks the owning unit and, for fenced operations, fence_op_id.

## 5. Definitions of Done

These definitions own this feature's 7 DoDs; the design slice references them without redefining them.
Design constraints: `cpt-cf-bss-products-constraint-two-backends`, `cpt-cf-bss-products-constraint-no-row-locks`.

### Tables on both backends

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-tables-two-backends`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/infra/storage/migrations.rs`.

Migrations 000001–000006 create category, SKU, versions, four approval tables, audit, replay and reference registry on SQLite and Postgres; the forward migration 000007 makes a SKU's category optional (P-D-196). Tenant isolation uses SecureORM scoping and scoped parent-category reads inside the write transaction; it does not depend on composite tenant foreign keys. Repositories, settings and canonical DomainError mapping use the same semantics on both engines, including bounded serialization retry without row locks (DESIGN §3.7; slice 01 §5).

### Immutable SKU version table

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-sku-version-table`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/infra/storage/migrations/m20260925_000002_create_products_sku.rs`.

The version table stores tenant, SKU, published_version, effective_from and the applied business snapshot under the composite primary key in DESIGN §3.7. Publish/change append and the head increment share one transaction; storage refuses updates/deletes and permits equal effective dates without a date-unique index (spec §2.2, §4).

### Append-only audit records

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-audit-append-only`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/infra/storage/repo/audit_repo.rs`.

The audit table retains the backup column types, nullability and guards documented in DESIGN §3.7 and slice 01 §6. Acts append attributed records, including submission, every terminal unit path and operator force-release; deletion and record-field updates are refused, with only the reserved unsealed-to-sealed metadata transition permitted (spec §3 item 27, §4, §6; P-D-193).

### Single client-key replay store

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-idempotency-key-store`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/infra/idempotency.rs`.

The only replay store is keyed by `(tenant_id, endpoint, client_key)` with payload_hash, claimed/answered state and the retained response shape in DESIGN §3.7. A keyed POST checks its 24-hour replay before fence or unit work; a keyless POST is valid, and a changed payload cannot reuse a retained answer. Approval units carry no idempotency key, and fence_op_id preserves operation identity across interrupted fence/submission transactions (spec §2.2, §7.2; P-D-193).

### If-Match uses the concurrency version

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-if-match-version`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/api/rest/preconditions.rs`.

SKU revision IS its concurrency version for ETag, If-Match and compare-and-swap; published_version identifies published snapshots. Categories use their version field. PATCH requires If-Match, conditionally changes the scoped row and increments its concurrency token; stale comparison returns 409 STALE_REVISION without mutation and missing headers use the toolkit precondition response (DESIGN §3.1, §3.3).

### Outbox shares the state transaction

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-outbox-same-tx`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/infra/events.rs`.

The event writer accepts the act's existing scoped transaction and records required outbox rows beside state and audit. Commit makes all visible together and dispatch follows commit; rollback, APPLY_REFUSED and stale refresh cannot publish successful apply events (spec §6, §7.3; DESIGN §3.4).

### Conditional approval Store and pending ownership

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-unit-store`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/infra/storage/repo/sku_repo.rs`.

The Products Store implements bss-approval persistence through scoped executors and conditional unit version updates, returning 409 UNIT_CONTENDED on a lost race. Submission acquires pending_unit_id only when null and the observed SKU version matches, otherwise ROW_LOCKED_PENDING rolls back the entire submit; terminal clearing checks the owner and any fence_op_id. Item authors and decision generations remain durable under tenant-scoped parent access (spec §2.2, §6; DESIGN §3.2, §3.7).

## 6. Acceptance Criteria

Each criterion below corresponds to exactly one DoD above and cites [PRD §9](../PRD.md#9-acceptance-criteria).
Verify these during phase 1c on SQLite and Postgres, including tenant isolation and denied permissions;
this document does not claim those implementation tests have run. Pricing-side assertions are contract
obligations here and integration checks when its phase 2 caller path exists.

| DoD | PRD trace | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-products-dod-tables-two-backends` | AC #1, #22, #29; `cpt-cf-bss-products-nfr-two-backends`, `cpt-cf-bss-products-nfr-tenant-isolation`, `cpt-cf-bss-products-fr-sku-define` | Given fresh SQLite and Postgres databases and two tenants, when the five migrations and repository scenarios run, then both preserve identical keys and scoped relationships; overlapping codes across tenants succeed, duplicate same-tenant codes fail with SKU_CODE_TAKEN, and cross-tenant links fail. |
| `cpt-cf-bss-products-dod-sku-version-table` | AC #8, #9; `cpt-cf-bss-products-fr-sku-versions` | Given two applied versions with the same effective date, when history is read or mutation of a stored snapshot is attempted, then both snapshots remain immutable and the greater published_version wins the date tie; update/delete fails, and a backwards effective date is refused with VERSION_ORDER. |
| `cpt-cf-bss-products-dod-audit-append-only` | AC #20, #21, #26; `cpt-cf-bss-products-fr-events`, `cpt-cf-bss-products-nfr-audit` | Given submission and approved/rejected/withdrawn/quorum-zero or force-release acts, when their transactions commit, then attributed audit rows persist with their state; direct deletion or record modification fails, and an injected audit-write failure rolls back that act. |
| `cpt-cf-bss-products-dod-idempotency-key-store` | AC #12, #27; `cpt-cf-bss-products-fr-concurrency-idempotency`, `cpt-cf-bss-products-fr-sku-retire-fenced` | Given a retained keyed POST and an interrupted fenced operation, when the same request repeats, then the saved answer returns before any new work or the owned orphan operation resumes without duplication; another payload cannot reuse the answer, while another tenant or endpoint has an independent key. |
| `cpt-cf-bss-products-dod-if-match-version` | AC #27; `cpt-cf-bss-products-fr-concurrency-idempotency`, `cpt-cf-bss-products-fr-category-flat` | Given SKU and category ETags, when a PATCH uses the current token, then its update returns a new ETag; when it uses a stale token, STALE_REVISION preserves the row, and omission of If-Match fails the required precondition. |
| `cpt-cf-bss-products-dod-outbox-same-tx` | AC #20, #21; `cpt-cf-bss-products-fr-events`, `cpt-cf-bss-products-nfr-audit` | Given a terminal act with required audit and outbox writes, when it succeeds, then all commit together; when an outbox failure or APPLY_REFUSED occurs, then none of that transaction's state, audit or success events survive. |
| `cpt-cf-bss-products-dod-unit-store` | AC #18, #19; `cpt-cf-bss-products-fr-approval-units`, `cpt-cf-bss-products-fr-concurrency-idempotency` | Given two writers observing one unit version or two submissions observing one unlocked SKU, when they compete, then only one succeeds; the loser receives UNIT_CONTENDED or ROW_LOCKED_PENDING respectively and leaves no partial unit, item or decision writes. |
