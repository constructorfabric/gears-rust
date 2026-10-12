Created:  2026-07-08 by Constructor Tech
Updated:  2026-07-08 by Constructor Tech
# Feature: Retention Policies & Cleanup (Orphan Reconciliation)

- [x] `p2` - **ID**: `cpt-cf-file-storage-featstatus-retention-cleanup-implemented`



<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [List Retention Rules](#list-retention-rules)
  - [Create Retention Rule](#create-retention-rule)
  - [Delete Retention Rule](#delete-retention-rule)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Run Sweep Cycle](#run-sweep-cycle)
  - [Sweep Abandoned Pending Versions (Orphan Reconciliation)](#sweep-abandoned-pending-versions-orphan-reconciliation)
  - [Sweep Versionless Files (Abandoned Multipart-Create Orphans)](#sweep-versionless-files-abandoned-multipart-create-orphans)
  - [Sweep Expired Multipart Sessions](#sweep-expired-multipart-sessions)
  - [Sweep Retention-Policy Expiry](#sweep-retention-policy-expiry)
  - [Validate Retention Rule on Write](#validate-retention-rule-on-write)
- [4. States (CDSL)](#4-states-cdsl)
  - [Multipart Session (owned by multipart-coordinator, driven here on a timer)](#multipart-session-owned-by-multipart-coordinator-driven-here-on-a-timer)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Retention Rule Domain Types and Administration Endpoints](#retention-rule-domain-types-and-administration-endpoints)
  - [Cleanup Engine and Background Sweep Scheduling](#cleanup-engine-scheduling-not-implemented)
  - [Live-Multipart-Session Guard](#live-multipart-session-guard)
  - [Semantic Validation on Write](#semantic-validation-on-write)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

- [ ] `p2` - `cpt-cf-file-storage-feature-retention-cleanup`

### 1.1 Overview

> **Status: not enforced yet.** The gear runs **no background worker**: nothing calls the cleanup engine
> periodically. Retention rules can be stored but are not enforced, and abandoned `pending` versions, expired
> multipart sessions and expired idempotency keys are not cleaned up until a separate cleanup job exists. The
> engine and its algorithms below are kept as the contract for that job; the timer-based wording (sweep interval,
> background loop, ticks) describes how the engine behaves when invoked.

Two related P2 capabilities sharing one engine (the cleanup engine's sweep, not scheduled yet):
(1) **retention policies** (`cpt-cf-file-storage-fr-retention-policies`) — tenant/user/file-scoped rules that
auto-expire files by age, inactivity, or a custom-metadata match; and (2) **orphan reconciliation**
(`cpt-cf-file-storage-fr-orphan-reconciliation`) — reclaiming `pending` version rows (and, transitively, permanently
orphaned zero-version `files` rows) that were pre-registered but never finalized, aborting multipart sessions
whose TTL expired without a `complete`/`abort` call, and — as a second phase of that same reclamation step —
reclaiming a `files` row that never had *any* version row in the first place (a multipart `POST /files` whose
control plane died between committing the file row and inserting its pending version, or whose synchronous
initiate-failure compensation itself failed; see [Sweep Versionless
Files](#sweep-versionless-files-abandoned-multipart-create-orphans)). The sweep also purges expired
`idempotency_keys` rows (a
housekeeping task riding the same cycle, not part of either named requirement). Once a cleanup job exists it runs the full sweep per
control-plane instance; there is no cross-instance coordination in P2 — see
§4.

### 1.2 Purpose

Two independent problems addressed by one engine because both are "delete files/rows the system decides are no
longer wanted, on a schedule, without a public trigger surface": regulated environments and cost-conscious tenants
need automated lifecycle management (retention), and the control plane's own two-phase upload protocol (pre-register
a `pending` version, then a separate later `bind`/`complete` — not atomic with the initial write) inevitably leaves
abandoned rows behind when a client disappears mid-upload (orphan reconciliation). Both are **best-effort**: one
step's failure is logged and does not abort the rest of the cycle, and every delete is idempotent so redundant
concurrent sweeps across instances are safe, merely wasteful.

**Requirements**: `cpt-cf-file-storage-fr-retention-policies`, `cpt-cf-file-storage-fr-orphan-reconciliation`

**Principles**: `cpt-cf-file-storage-principle-control-no-content`

> **Scope note.** The PRD's `cpt-cf-file-storage-fr-orphan-reconciliation` also describes **backend blob-without-row**
> reconciliation (cross-backend orphan enumeration via a `list_paths` scan) and flagging an `available` version with
> no matching backend object for operator attention. **Neither is implemented in P2** — both require cross-instance
> leader election to run safely (a naive per-instance `list_paths` scan would race and double-report/double-delete
> across replicas) and are explicitly deferred to P3 per `cleanup.rs`'s module doc comment. P2's orphan reconciliation
> is scoped to the two cases enumerated in §3 below: abandoned `pending` versions and expired multipart sessions.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-file-storage-actor-platform-user` | Defines tenant/user/file-scope retention rules; their files are subject to both retention expiry and orphan reconciliation |
| `cpt-cf-file-storage-actor-cf-gears` | Peer gear / service whose files are equally subject to both sweep behaviors; no distinct role from the platform user here |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.6 "Retention Policies", "Orphan Reconciliation"
- **Design**: [DESIGN.md](../DESIGN.md)
- **API contract**: [api.md](../api.md) — `GET/POST /retention-rules`, `DELETE /retention-rules/{rule_id}`
- **Dependencies**: [Multipart Upload Coordinator](multipart-coordinator.md)
  (`cpt-cf-file-storage-feature-multipart-coordinator`) — the sweep's expired-multipart step reuses that feature's
  session state machine (`in_progress -> aborted` via TTL, `cpt-cf-file-storage-state-multipart-session`'s third
  transition) and CAS pattern; this feature does not change that state machine, only drives one of its transitions
  on a timer instead of a client call

## 2. Actor Flows (CDSL)

User-facing interactions that start with an actor. The sweep cycle itself has **no actor-facing flow** — PRD
§5.6 requires it be a control-plane internal scheduled task, not triggerable from any public API surface; it is
documented as a process in §3 instead.

### List Retention Rules

- [x] `p1` - **ID**: `cpt-cf-file-storage-flow-retention-list`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Success Scenarios**:
- A caller holding `ADMIN_POLICY` receives every retention rule (any scope) belonging to their tenant,
  one cursor-paginated page at a time, navigable in either direction (`limit`/`cursor`, default 25, max 200;
  canonical order `created_at desc, rule_id desc`)
- A non-admin caller receives only the rules they may see: all `Tenant`-scope rules, `User`-scope
  rules that target themselves, and `File`-scope rules whose target file they own (owner compared
  as the `(owner_kind, owner_id)` pair, not `owner_id` alone — `user` and `app` are disjoint owner
  spaces that can share a UUID) — this visibility filter is applied **in SQL** (a `WHERE` clause with
  a `File`-scope subquery over the caller's own files), so without it the response would leak every
  other tenant member's retention configuration, and — unlike an application-level filter applied
  after an unconditional fetch — every page but the last is guaranteed full
- A `File`-scope rule is deleted together with its target file (every file-delete path removes it
  in the same transaction, since there is no FK/cascade tying `retention_rules.scope_target_id` to
  `files.file_id` at the DB level), so it no longer outlives the file it targets.
- **Remaining known limitation on the `File`-scope branch.** A `File`-scope rule can be created by
  anyone holding per-file `WRITE` on the target — not only the file's owner — so a subject who
  created such a rule via delegated `WRITE` on someone else's file will not see it in this listing:
  visibility is gated on being the target file's *owner*, not on having created the rule.

**Error Scenarios**:
- Caller lacks `READ` — `403`
- Malformed/mismatched `cursor`, an unsupported query parameter (e.g. the old `offset`), or `limit=0` — `400`

**Steps**:
1. [x] - `p1` - Client: GET /api/file-storage/v1/retention-rules?limit=&cursor= - `inst-retention-list-request`
2. [x] - `p1` - Authorize `READ` on `("", None)` - `inst-retention-list-authz`
3. [x] - `p1` - Probe `ADMIN_POLICY` to decide whether the SQL visibility filter below applies
4. [x] - `p1` - DB: SELECT one page of retention rules for the caller's tenant, `LIMIT limit + 1`,
   keyset-seeking past `cursor`'s decoded position, with the non-admin `WHERE` visibility filter
   applied only when the `ADMIN_POLICY` probe was denied - `inst-retention-list-load`
5. [x] - `p1` - RETURN 200 with `{items, page_info: {next_cursor, prev_cursor, limit}}`; return
   `prev_cursor: null` only when no previous page exists - `inst-retention-list-return`

### Create Retention Rule

- [x] `p1` - **ID**: `cpt-cf-file-storage-flow-retention-create`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Success Scenarios**:
- Rule is created and returned; it becomes eligible for matching on the next sweep cycle

**Error Scenarios**:
- The body specifies none of `age`/`inactivity`/`metadata` — `400` (a rule that can never
  match any file is almost certainly a mistake)
- `age.max_age_days == 0` or `inactivity.inactivity_days == 0` — `400` (would match *every* file in the tenant on
  the very next sweep tick, permanently deleting rows and blobs with no dry-run and no undo)
- `scope ∈ {user, file}` with no `scope_target_id` — `400` (a dead rule that can never resolve to a target)
- `scope = file` and the target file does not exist, or belongs to a different
  tenant, or the caller lacks `WRITE` on it — `404`/`403` (`authorize_retention_scope`'s
  `File` arm resolves the target via `require_file` scoped to the caller's own
  tenant — the same prefetch pattern `read_ops.rs`/`write.rs` use — before
  authorizing, so a foreign tenant's file UUID cannot be distinguished from a
  nonexistent one; both surface identically as `FileNotFound`)
- `scope = user` and the target is a different user, without `ADMIN_POLICY` — `403`
- Caller lacks `ADMIN_POLICY` (tenant scope, no `WRITE` fallback) — `403` (a tenant-scope rule is a
  standing instruction for the background sweep to permanently delete every matching file for every
  subject in the tenant, so ordinary per-file `WRITE` is not enough)

**Steps**:
1. [x] - `p1` - Client: POST /api/file-storage/v1/retention-rules {scope, scope_target_id?, body} - `inst-retention-create-request`
2. [x] - `p1` - Algorithm: `cpt-cf-file-storage-algo-validate-retention-rule` — reject a dead-on-write or immediately-total-expiry body - `inst-retention-create-validate` (runs before authorization: otherwise the same malformed body answers `403` for a non-admin caller and `400` for an admin, since only the admin ever reaches validation)
3. [x] - `p1` - Authorize by scope: `Tenant` → `ADMIN_POLICY` outright, no fallback to `WRITE`; `User` → `ADMIN_POLICY`-first with a `WRITE`-plus-target-match fallback (a missing target is a mismatch, not "no check" — unlike the policy-engine's tenant-scope fallback); `File` → resolve the target file via `require_file`, scoped to the caller's own tenant (a foreign or missing file surfaces as `FileNotFound`, not silently accepted) then require per-file `WRITE` - `inst-retention-create-authz`
4. [x] - `p1` - DB: INSERT the rule row - `inst-retention-create-insert`
5. [x] - `p1` - RETURN 201 with the created rule - `inst-retention-create-return`

### Delete Retention Rule

- [x] `p1` - **ID**: `cpt-cf-file-storage-flow-retention-delete`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Success Scenarios**:
- Rule is deleted
- A `scope = file` rule whose target file has since been deleted (there is no
  FK/cascade between `retention_rules.scope_target_id` and `files.file_id`) is
  still deletable: the caller is re-authorized against a plain tenant-wide
  `WRITE` grant instead of a per-file check that can no longer resolve a
  target, so the orphaned rule does not become permanently stuck and does not
  keep being re-scanned by every sweep

**Error Scenarios**:
- `rule_id` does not exist — `404`
- Caller does not own the rule's scope/target (same rules as create) — `403`
- A foreign-tenant caller's `rule_id` (the rule belongs to a different
  tenant) — `404`, not `403`: step 2's fetch is scoped to the caller's own
  tenant, so a foreign tenant's `rule_id` simply does not resolve there — it
  404s the same way a genuinely nonexistent `rule_id` does, before
  authorization is ever attempted

**Steps**:
1. [x] - `p1` - Client: DELETE /api/file-storage/v1/retention-rules/{rule_id} - `inst-retention-delete-request`
2. [x] - `p1` - DB (fetch-then-reauthorize): SELECT the rule scoped to the caller's own tenant, purely to learn its `(scope, scope_target_id)` — a bare `rule_id` carries no ownership information, so a coarse tenant-wide `DELETE` check alone would let any tenant member delete any other member's rule; scoping this fetch to the caller's own tenant also means a foreign tenant's `rule_id` simply does not resolve, `404`-ing here exactly like a nonexistent one, before authorization is ever consulted - `inst-retention-delete-load`
3. [x] - `p1` - Re-run the same scope-based authorization [Create Retention Rule](#create-retention-rule) uses, against the rule's actual `(scope, scope_target_id)`; for `scope = file`, if the target file no longer exists (`FileNotFound`), fall back to a plain tenant-wide `WRITE` gate — there is no file left to check per-file `WRITE` against, so this is the closest available equivalent, deliberately NOT raised to `ADMIN_POLICY` since a rule whose target file is already gone governs nothing — rather than propagating the 404 — an orphaned file-scope rule must remain deletable - `inst-retention-delete-authz`
4. [x] - `p1` - DB: DELETE the rule row, scoped to the authorized `AccessScope` - `inst-retention-delete-remove`
5. [x] - `p1` - RETURN 204 - `inst-retention-delete-return`

## 3. Processes / Business Logic (CDSL)

Internal system functions with no direct actor interaction; the (future) cleanup job is the only caller.

### Run Sweep Cycle

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-run-sweep`

**Input**: none (reads current time and the `CleanupConfig.orphan_grace_secs` and
time-budget knobs)

**Output**: `SweepResult { abandoned_pending_deleted, abandoned_files_deleted, expired_multipart_aborted,
retention_expired_deleted, idempotency_keys_deleted, budget_exhausted, elapsed_ms }`

**Steps**:
1. [x] - `p1` - Step 1: sweep one batch of abandoned pending versions (+ any now-permanently-orphaned parent
   `files` row) — `cpt-cf-file-storage-algo-sweep-abandoned-pending` — **and**, as a second phase of this same
   step, sweep one batch of versionless `files` rows that never had any version row at all —
   `cpt-cf-file-storage-algo-sweep-versionless-files` - `inst-sweep-step1`
2. [x] - `p1` - Step 2: sweep one batch of expired `in_progress` multipart sessions —
   `cpt-cf-file-storage-algo-sweep-expired-multipart` - `inst-sweep-step2`
3. [x] - `p1` - Step 3: sweep one page of retention-policy expiry across all scopes —
   `cpt-cf-file-storage-algo-sweep-retention-expiry` - `inst-sweep-step3`
4. [x] - `p1` - Step 4: delete one batch of expired `idempotency_keys` rows (`expires_at <= now`); `audit_outbox`/
   `events_outbox` rows are deliberately **not** touched here regardless of age, because `published_at` stays
   `NULL` until the Tier-4 `EventBroker` relay exists — an age-based purge would silently drop undelivered
   events - `inst-sweep-step4`
5. [x] - `p1` - Repeat steps 1-4 as further passes, in the same order, each pass running one more batch per
   not-yet-exhausted step — a step is exhausted once its batch comes back shorter than its page size or its
   query/delete errors — until either every step is exhausted or the tick's time budget
   (a fixed time budget, 15 minutes) runs out. The first pass always completes in full
   regardless of the budget, so a misconfigured budget can never make one tick do less than the historical
   single-batch-per-call behavior; the budget is only checked before a later pass's batch -
   `inst-sweep-passes`
6. [x] - `p1` - Each step is independently best-effort: a store error is logged at `warn` and contributes `0` to
   that step's tally rather than aborting the remaining steps - `inst-sweep-best-effort`
7. [x] - `p1` - RETURN the accumulated `SweepResult`, with `budget_exhausted` set when the tick stopped on its time
   budget rather than because every step ran to exhaustion, and `elapsed_ms` set to the tick's wall-clock
   duration; the gear also exports the five delete/abort tallies as metrics counters at the point they are
   logged - `inst-sweep-return`

> **Batching note.** All five sweep queries are bounded per batch by a named `..._SWEEP_BATCH` constant (500 rows
> each: `RETENTION_SWEEP_BATCH`, `ABANDONED_PENDING_SWEEP_BATCH`, `VERSIONLESS_SWEEP_BATCH`,
> `EXPIRED_MULTIPART_SWEEP_BATCH`, `EXPIRED_IDEMPOTENCY_SWEEP_BATCH`) — none of them is a literal unbounded
> query/statement with no `LIMIT`. Within one tick, every step keeps taking another batch — interleaved with the
> other steps, in the fixed step order above — until it is exhausted or the tick's time budget
> runs out; an unfinished step's remaining work carries over rather than being dropped. The abandoned-pending,
> versionless-files, and expired-multipart steps each keep a keyset cursor for this purpose that is local to one
> tick: a candidate a batch could not actually reclaim (a still-active multipart session, a transient per-row
> error) does not block the rest of that step's backlog within the same tick, and every tick starts that step over
> from its oldest candidate again, so a previously-blocked candidate is retried once whatever was blocking it has
> cleared. The retention-expiry scan (keyset-paginated by `file_id` —
> [Sweep Retention-Policy Expiry](#sweep-retention-policy-expiry)) is the exception: its cursor survives **across**
> ticks, because a full table scan can legitimately take longer than one tick's budget on a large deployment, and
> restarting it from the beginning every tick would starve files later in `file_id` order. The
> expired-idempotency-keys step needs no cursor at all — each batch deletes the rows it selects, so the next call
> naturally sees the next-oldest backlog.

### Sweep Abandoned Pending Versions (Orphan Reconciliation)

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-sweep-abandoned-pending`

**Input**: `grace_cutoff` (`now - orphan_grace_secs`), `now`, `after` (this step's tick-local keyset cursor —
`(created_at, version_id)`, `None` on the first batch of a tick)

**Output**: `(pending_versions_deleted, orphan_files_deleted, exhausted, next_after)` — `exhausted` once a batch
comes back shorter than the page size or the list query errors; `next_after` always advances to the last
candidate the batch saw, regardless of whether that candidate was actually reclaimed

**Steps**:
1. [x] - `p1` - DB: list `pending` version rows with `created_at < grace_cutoff` **and** `(created_at, version_id)
   > after`, **excluding** any version that is still the backing version of a live `in_progress` multipart session
   (`multipart_uploads.expires_at > now`) **or** of any `completing` session regardless of its lease — see
   [Live-Multipart-Session Guard](#live-multipart-session-guard) - `inst-sweep-pending-list`
2. [x] - `p1` - FOR EACH candidate (having already resolved every candidate's parent file in ONE batched round-trip up front — a single "list files by ids" call, not one lookup per candidate — purely to attribute the right `tenant_id` on each audit row): write an `orphan_reconcile` audit row, then delete the version row **status-guarded** (`status = pending` only) -- the same CAS pattern step 2 below uses, so a version a racing `finalize_upload` already flipped to `available` between the list query and this delete is left completely untouched (row, blob, and debit alike) - `inst-sweep-pending-audit-delete`
3. [x] - `p1` - **IF** deleted: debit the reclaimed bytes via the usage reporter (fire-and-forget; `bytes_delta = -size`, `file_count_delta = 0` — `size` is structurally `0` in practice since a version is only ever assigned a nonzero size by `finalize_version`, which a reclaimed-here version never reached) - `inst-sweep-pending-usage`
4. [x] - `p1` - Best-effort: delete the backend blob at the version's `(backend_id, backend_path)` — a failure leaves an unreachable orphan blob, acceptable in P2 - `inst-sweep-pending-blob`
5. [x] - `p1` - **IF** the parent file now has zero versions **AND** `content_id IS NULL` **AND** no `in_progress`, unexpired multipart session, nor any `completing` session, still references it (the same guard as step 1, re-checked because a session that has not yet expired could still legitimately have its backing version reclaimed by an unrelated grace-window aging in the *same* sweep pass): delete the `files` row too — the multipart-session check just described is pre-transactional (see [Live-Multipart-Session Guard](#live-multipart-session-guard)); the transactional delete itself (`Store::delete_orphan_file_with_event`) re-verifies only the other two conditions, zero versions and `content_id IS NULL`, fresh inside the same transaction, so a version inserted in the gap is never lost — write a `file.deleted` event and debit `file_count_delta = -1`, `bytes_delta = 0` - `inst-sweep-pending-orphan-file`
6. [x] - `p1` - Advance `next_after` to the last candidate's `(created_at, version_id)` regardless of whether it was
   reclaimed, so a candidate this batch could not reclaim (blocking session, transient error) does not keep
   blocking the rest of the backlog for the remainder of this tick — see [Run Sweep Cycle](#run-sweep-cycle) -
   `inst-sweep-pending-cursor`
7. [x] - `p1` - RETURN the two counts, `exhausted`, and `next_after` - `inst-sweep-pending-return`

### Sweep Versionless Files (Abandoned Multipart-Create Orphans)

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-sweep-versionless-files`

Runs as the **second phase of step 1** in [Run Sweep Cycle](#run-sweep-cycle) — the sweep is still four steps
overall, not five. Unlike [Sweep Abandoned Pending Versions](#sweep-abandoned-pending-versions-orphan-reconciliation)
above, this phase's candidates never had a pending version to begin with: it is the reaper for a multipart
`POST /files` whose control plane died between `FileService::create_file_bare` committing the versionless `files`
row and `MultipartService::initiate_multipart_upload` inserting the pending version, or one whose synchronous
`compensate_failed_multipart_initiate` compensation (run by the handler on any initiate error) itself failed. Before
this phase existed no sweep step ever picked up such a row: the zero-version cleanup in [Sweep Abandoned Pending
Versions](#sweep-abandoned-pending-versions-orphan-reconciliation) step 5 only ever fires as a side effect of
reclaiming a *pending* version, and [Sweep Expired Multipart Sessions](#sweep-expired-multipart-sessions) only ever
fires as a side effect of aborting a session — a file that got neither had no reaper at all. There is no race
against a live `POST /files`: the gap between committing the file row and inserting its version is milliseconds,
while `orphan_grace_secs` is measured in hours.

**Input**: `grace_cutoff` (`now - orphan_grace_secs`), `after` (this step's tick-local keyset cursor —
`(created_at, file_id)`, `None` on the first batch of a tick)

**Output**: `(versionless_files_deleted, exhausted, next_after)` — same `exhausted`/`next_after` semantics as
[Sweep Abandoned Pending Versions](#sweep-abandoned-pending-versions-orphan-reconciliation)

**Steps**:
1. [x] - `p1` - DB: list `files` rows with `content_id IS NULL` and zero rows in `file_versions`, `created_at <
   grace_cutoff` **and** `(created_at, file_id) > after`, one batch — this list query already returns the full
   `File` row for each candidate, so (unlike the abandoned-pending and expired-multipart phases) no separate
   per-candidate or batched file lookup is needed here to attribute the audit row's `tenant_id` -
   `inst-sweep-versionless-list`
2. [x] - `p1` - FOR EACH candidate: reuse `maybe_delete_orphaned_file` — the same guarded primitive [Sweep Abandoned
   Pending Versions](#sweep-abandoned-pending-versions-orphan-reconciliation) step 5 uses for its own zero-version
   case, so the [Live-Multipart-Session Guard](#live-multipart-session-guard) (`has_blocking_multipart_session`) and
   the transactional re-check of "zero versions AND `content_id IS NULL`" apply identically here: a candidate that
   picked up a version, content, or a blocking `in_progress`/`completing` multipart session in the gap since the
   list query is left untouched. On a successful delete, write an `orphan_reconcile` audit row and a `file.deleted`
   event, and debit `file_count_delta = -1`, `bytes_delta = 0` (no bytes were ever uploaded against a row that never
   had a version) - `inst-sweep-versionless-delete`
3. [x] - `p1` - Advance `next_after` to the last candidate's `(created_at, file_id)` regardless of whether it was
   reclaimed, same reasoning as step 1's cursor above - `inst-sweep-versionless-cursor`
4. [x] - `p1` - RETURN the count, `exhausted`, and `next_after` - `inst-sweep-versionless-return`

### Sweep Expired Multipart Sessions

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-sweep-expired-multipart`

**Input**: `now`, `after` (this step's tick-local keyset cursor — `(expires_at, upload_id)`, `None` on the first
batch of a tick)

**Output**: `(sessions_aborted, orphan_files_reclaimed, exhausted, next_after)` — same `exhausted`/`next_after`
semantics as [Sweep Abandoned Pending Versions](#sweep-abandoned-pending-versions-orphan-reconciliation)

**Steps**:
1. [x] - `p1` - DB: list multipart sessions with `expires_at < now` **and** `(expires_at, upload_id) > after` that
   are either still `in_progress` or left `completing` by a dead completer whose lease has also expired
   (`MultipartRepo::list_expired`; a live lease is never reaped mid-assembly) - `inst-sweep-multipart-list`
2. [x] - `p1` - Resolve every listed session's parent file in ONE batched round-trip up front (a single "list files by ids" call, not one lookup per session), same as step 1's abandoned-pending phase — used only to attribute the right `tenant_id` on each session's audit row - `inst-sweep-multipart-audit-batch`
3. [x] - `p1` - FOR EACH: CAS the session `in_progress -> aborted` **first**, via the same `Store::abort_multipart_upload` the user-driven abort path uses — that call also deletes the session's `multipart_upload_parts` rows in the same transaction as the state flip, so a concurrent `complete_multipart_upload` racing on the same session row can win instead (`in_progress -> completed`); only one side wins - `inst-sweep-multipart-cas`
4. [x] - `p1` - **IF** the sweep won the CAS: best-effort abort the backend upload handle, then delete the pending version row **status-guarded** (`status = pending` only) — a version a racing complete already flipped to `available` via `finalize_version` (ahead of its own session CAS) is left untouched; the DELETE simply matches zero rows - `inst-sweep-multipart-cleanup`

   The backend abort is best-effort and **not** retried: once the CAS has moved the session to `aborted`, no later pass lists it again (step 1 selects only `in_progress` and lease-expired `completing` sessions). A failed abort therefore leaves an incomplete multipart upload on the backend that FileStorage will never touch again — configure an `AbortIncompleteMultipartUpload` bucket lifecycle rule as the backstop reaper (see `concurrency-and-failure-model.md` §5).
5. [x] - `p1` - **IF** the sweep lost the CAS (session already transitioned): skip version cleanup entirely and log — if the winner was `complete`, the version is now `Available` and bound; touching it would be data loss - `inst-sweep-multipart-skip`
6. [x] - `p1` - Advance `next_after` to the last session's `(expires_at, upload_id)` regardless of whether it was
   reaped, same reasoning as the abandoned-pending step's cursor - `inst-sweep-multipart-cursor`
7. [x] - `p1` - RETURN the count of sessions the sweep itself won and aborted, plus `exhausted` and `next_after` -
   `inst-sweep-multipart-return`

### Sweep Retention-Policy Expiry

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-sweep-retention-expiry`

Unlike the other three sweep steps, this step's keyset cursor (`files.file_id`) is **not** tick-local: it is kept
on the `CleanupEngine` itself and survives across ticks, because a full table scan can legitimately take longer
than one time budget on a large deployment. `all_rules` is loaded once per tick by the caller
([Run Sweep Cycle](#run-sweep-cycle)) and handed to every page unchanged, since the rule set is small relative to
the files.

**Input**: `now`, `all_rules` (loaded once for the whole tick), `after` (the persisted cursor — `files.file_id`,
`None` to start from the beginning)

**Output**: `(files_deleted_this_page, exhausted, next_after)`:
- a short page (fewer than 500 rows) means the whole table has now been scanned — `exhausted = true`,
  `next_after = None` (the next scan, whenever one is next needed, starts over from the beginning);
- a full page means more files remain — `exhausted = false`, `next_after` points past this page, so a later pass
  this same tick (if the budget allows) or the next tick resumes from there;
- a query error leaves the cursor untouched (`next_after = after`) and marks the step exhausted for this tick —
  retrying the same page next tick rather than losing the caller's place.

**Steps**:
1. [x] - `p1` - DB: fetch one keyset page of up to 500 files (by `file_id`, `after`-cursor), so the sweep never
   materializes every file across every tenant in memory regardless of deployment size - `inst-sweep-retention-scan`
2. [x] - `p1` - **IF** the page is empty: the scan is complete for now — return `exhausted = true`,
   `next_after = None` - `inst-sweep-retention-empty`
3. [x] - `p1` - FOR EACH file in the page: gather rules applicable by scope (`Tenant` → always; `User` → `rule.scope_target_id == file.owner_id`; `File` → `rule.scope_target_id == file.file_id`), restricted to the file's own tenant - `inst-sweep-retention-applicable`
4. [x] - `p1` - **IF** any applicable rule: fetch the custom metadata of every file on this page that needs it, in ONE batched "list metadata for files" call per page (not one query per file) — a page with no metadata-criterion rules issues zero metadata queries; a fetch failure skips every file on that page that needed metadata (logged) rather than treating it as "no metadata, no match" - `inst-sweep-retention-metadata`
5. [x] - `p1` - Evaluate OR semantics across the file's applicable rules — the first matching criterion (age: `now - created_at > max_age_days`; inactivity: `now - last_modified_at > inactivity_days`, **not** reset by downloads, only by writes; metadata: an exact key/value match) triggers expiry - `inst-sweep-retention-match`
6. [x] - `p1` - **IF** expiring: write a `retention_delete` audit row and a `file.deleted` event on the same transactional-outbox path user-initiated deletes use, delete the file (all versions + the `files` row), debit total bytes and `file_count_delta = -1` via the usage reporter, then best-effort delete each version's backend blob - `inst-sweep-retention-delete`
7. [x] - `p1` - RETURN the count deleted on this page, `exhausted`, and `next_after` -
   `inst-sweep-retention-return`

### Validate Retention Rule on Write

- [x] `p2` - **ID**: `cpt-cf-file-storage-algo-validate-retention-rule`

**Input**: `RetentionScope`, `scope_target_id: Option<Uuid>`, `RetentionRuleBody`

**Output**: `Ok(())`, or `DomainError::Validation`

Same spirit as the policy-engine's write-time validation: reject a body that would be
dangerous or permanently dead rather than silently accept it.

**Steps**:
1. [x] - `p2` - **IF** `age`, `inactivity`, and `metadata` are all absent: reject — the rule could never match any file - `inst-validate-retention-empty`
2. [x] - `p2` - **IF** `age.max_age_days < 1` or `inactivity.inactivity_days < 1` (i.e. `== 0`, both are `u32`): reject — `0` would match every file in the tenant on the very next sweep tick, and there is no dry-run or undo - `inst-validate-retention-zero`
3. [x] - `p2` - **IF** `scope ∈ {User, File}` and `scope_target_id` is absent: reject — a dead rule that can never resolve to a target file (this validation runs *before* authorization in `create_retention_rule`, so this check — not `authorize_retention_scope`'s `File` arm — is what catches a missing target for both `User`- and `File`-scope requests; `authorize_retention_scope` never sees a missing `File`-scope target, since a request that lacks one is already rejected by the time it would run) - `inst-validate-retention-target`
4. [x] - `p2` - RETURN `Ok(())` otherwise - `inst-validate-retention-return`

## 4. States (CDSL)

### Multipart Session (owned by multipart-coordinator, driven here on a timer)

- [x] `p1` - **ID**: `cpt-cf-file-storage-state-retention-cleanup-multipart-touch`

This feature does not define its own state machine for the multipart session — that is
[`cpt-cf-file-storage-state-multipart-session`](multipart-coordinator.md#multipart-session-state-machine), owned by
multipart-coordinator.md. This feature is the sole driver of that state machine's third transition
(`in_progress -> aborted` on TTL expiry) via [Sweep Expired Multipart
Sessions](#sweep-expired-multipart-sessions); it introduces no new state values.

**No cross-instance coordination in P2.** The sweep runs independently, unsynchronized, on every control-plane
instance when invoked. This is deliberately safe rather than merely "not yet a bug": every mutation the sweep
performs is a CAS or a status-guarded delete, so a concurrent redundant sweep on the same row gets `Ok(false)`/zero
rows affected rather than corrupting state or double-reporting usage. Leader election / distributed locking to
eliminate the redundant work (not the small risk of incorrectness, since there is none) is deferred to P3.

## 5. Definitions of Done

### Retention Rule Domain Types and Administration Endpoints

- [x] `p1` - **ID**: `cpt-cf-file-storage-dod-retention-rule-endpoints`

`RetentionScope` (`Tenant`/`User`/`File`) and `RetentionRuleBody` (`age`/`inactivity`/`metadata`, OR
semantics — any one matching criterion triggers expiry) back `GET/POST /retention-rules` and
`DELETE /retention-rules/{rule_id}`. Scope-aware authorization (`Tenant`
= `ADMIN_POLICY` outright, no `WRITE` fallback; `User` = `ADMIN_POLICY`-first with `WRITE`-plus-target-match fallback; `File` = resolve-then-
per-file-`WRITE`) is covered by dedicated authorization tests for each scope, including a foreign-owner denial and a
not-found case for deleting a missing rule.

**Implements**:
- `cpt-cf-file-storage-flow-retention-list`
- `cpt-cf-file-storage-flow-retention-create`
- `cpt-cf-file-storage-flow-retention-delete`

**Touches**:
- API: `GET /api/file-storage/v1/retention-rules`, `POST /api/file-storage/v1/retention-rules`,
  `DELETE /api/file-storage/v1/retention-rules/{rule_id}`
- DB Table: `retention_rules`

### Cleanup Engine (scheduling not implemented)

- [x] `p1` - **ID**: `cpt-cf-file-storage-dod-cleanup-engine`

The cleanup engine's sweep entry point implements all four steps in [Run Sweep
Cycle](#run-sweep-cycle). The gear does **not** schedule it: there is no background loop and no `sweep_interval_secs`/`enable_background_sweep`
configuration. A separate cleanup job is expected to call it.

**Implements**:
- `cpt-cf-file-storage-algo-run-sweep`
- `cpt-cf-file-storage-algo-sweep-abandoned-pending`
- `cpt-cf-file-storage-algo-sweep-versionless-files`
- `cpt-cf-file-storage-algo-sweep-expired-multipart`
- `cpt-cf-file-storage-algo-sweep-retention-expiry`

**Touches**:
- Gears: the cleanup engine module and gear startup wiring
- DB Table: `file_versions`, `files`, `multipart_uploads`, `multipart_upload_parts`, `idempotency_keys`

> **Usage-reporting caveat, mirroring multipart-coordinator.md's own note.** `CleanupEngine::report_usage` is a real,
> wired call at every debit/credit point in the sweep (abandoned-pending reclaim, orphan-file delete,
> retention-expiry delete), but `gear.rs` constructs the engine `.with_usage_reporter(None)` — no Usage Collector
> client is wired in any deployment today, so every sweep-driven usage report is currently a fire-and-forget no-op.
> The reporting code path itself is exercised by unit tests injecting a mock reporter where present; this does not
> block or alter sweep correctness (usage reporting is `cpt-cf-file-storage-fr-usage-reporting`, a separate
> requirement from this FEATURE).

### Live-Multipart-Session Guard

- [x] `p1` - **ID**: `cpt-cf-file-storage-dod-cleanup-live-multipart-guard`

`VersionRepo::list_pending_older_than`
— the query backing [Sweep Abandoned Pending
Versions](#sweep-abandoned-pending-versions-orphan-reconciliation) — filters out any `pending` version row whose
`version_id` appears in an active multipart session: `SELECT version_id FROM multipart_uploads WHERE (state =
'in_progress' AND expires_at > now) OR state = 'completing'`. **Invariant**: a pending version backing a
still-`in_progress`, unexpired multipart session, **or** backing **any** `completing` session (regardless of
`lease_until`), is **never** selected for reclamation by step 1, regardless of how old its `created_at` is — a
long-running upload (large file, generous URL TTL) can legitimately keep its backing version `pending` for longer
than `orphan_grace_secs`, and without this guard the sweep would delete the version out from under the in-progress
upload or the completer currently assembling it. A session whose `expires_at` has **already passed** is deliberately
**not** excluded by this guard while it is still `in_progress` — that version becomes reclaimable, but only after
[Sweep Expired Multipart Sessions](#sweep-expired-multipart-sessions) (step 2, running in the same cycle) has
transitioned the session out of `in_progress`; step 1 and step 2 run in a fixed order within one `run_sweep` call, so
a session that expires exactly between them is reclaimed on the *next* cycle, not silently missed. A `completing`
session, by contrast, is excluded unconditionally — independent of `expires_at`, and regardless of whether its own
completion lease (`lease_until`) has itself lapsed — since reaping its backing version would risk destroying content
a completer is (or was) actively assembling; only step 2's takeover/abort path, once it moves a lease-expired
`completing` session out of that state, makes the version reclaimable again. The same live-session check is repeated,
independently, by `CleanupEngine::has_blocking_multipart_session` before deleting a permanently-orphaned zero-version
`files` row (§3, step 5's `inst-sweep-pending-orphan-file`), for the same reason at the file-deletion granularity: a
`files` row's `ON DELETE CASCADE` would otherwise take a still-`in_progress` `multipart_uploads` row down with it.
This check runs **before** the delete transaction — inside `orphan_candidate_file`'s pre-transaction snapshot — not
inside it. The transactional guard that performs the actual delete, `Store::delete_orphan_file_with_event`,
re-verifies only two conditions fresh inside the transaction: zero remaining versions and `content_id IS NULL`; it
does not re-check for a live multipart session. That is safe because `multipart_uploads.version_id` is `NOT NULL`
and a session's backing version is pre-registered in the same operation that creates the session, so a live session
always implies a row in `file_versions` — meaning the transactional zero-versions check alone already rejects the
delete in exactly the scenario a transactional session re-check would have guarded against. The pre-transaction
`has_blocking_multipart_session` check is still worth doing: it is a cheap early rejection, and it also guards
against a version reclaimed earlier in the very same sweep pass, before this file-delete step runs.
[Sweep Versionless Files](#sweep-versionless-files-abandoned-multipart-create-orphans) (step 1's second phase,
`inst-sweep-versionless-delete`) reuses the very same `maybe_delete_orphaned_file` call and therefore the same
guard, even though its candidates never had a pending version for the version-query-level check to apply to in the
first place.

Directly exercised by a dedicated test (a backdated-
`created_at`, still-live session's version survives the sweep untouched) and its companion
`sweep_reclaims_version_after_session_expires` (once `expires_at` also passes, the session is aborted by step 2 and
its version is reclaimed by step 1 on the same `run_sweep()` call).

**Implements**:
- `cpt-cf-file-storage-algo-sweep-abandoned-pending`

**Touches**:
- Gears: the version repository and the cleanup engine module
- DB Table: `file_versions`, `multipart_uploads`

### Semantic Validation on Write

- [x] `p2` - **ID**: `cpt-cf-file-storage-dod-retention-semantic-validation`

The policy service's retention-rule validation rejects an all-criteria-absent body, a zero-day age/inactivity
criterion, and a `User`/`File`-scope rule with no target, at `POST /retention-rules` write time. This write-time
guard is verified to be a real, load-bearing gate rather than a redundant safety net: attempting to create a
zero-`max_age_days` rule is rejected outright (no row written), and a file that *would* have matched such a rule
survives a subsequent sweep untouched. A companion case inserts a zero-day rule directly at the storage layer,
bypassing this write-time guard on purpose, specifically to exercise the sweep's own matcher mechanics in isolation
from the guard.

**Implements**:
- `cpt-cf-file-storage-algo-validate-retention-rule`

**Touches**:
- Gears: the policy service module

## 6. Acceptance Criteria

- [x] Owners can define retention rules at tenant, user, or file scope, matching on age, inactivity, or a
  custom-metadata key/value, with OR semantics across multiple criteria on one rule
  (`cpt-cf-file-storage-fr-retention-policies`)
- [ ] A file matched by any applicable retention rule is deleted (content + metadata + custom metadata, cascading)
  by the cleanup sweep (**not enforced yet**: no background worker runs it), with an audit record (`retention_delete`) and a `file.deleted` event on the same
  transactional-outbox path user-initiated deletes use
- [x] A retention rule that would match every file immediately (`max_age_days`/`inactivity_days == 0`) or could
  never match any file (all criteria absent) or could never resolve a target (`user`/`file` scope with no target) is
  rejected at write time, not silently accepted
- [ ] (**Not enforced yet**: no background worker.) A `pending` version row past the orphan grace period with no finalize/bind is deleted, along with its backend
  blob (best-effort), and an `orphan_reconcile` audit row is written (`cpt-cf-file-storage-fr-orphan-reconciliation`)
- [x] A file left with zero versions and `content_id IS NULL` after its last pending version is reclaimed is itself
  deleted (not left as a permanent, unreachable-forever `files` row), with a `file.deleted` event
- [ ] (**Not enforced yet**: no background worker.) A `files` row that never had any version at all — a multipart `POST /files` whose control plane crashed
  between committing the file row and inserting its pending version, or whose synchronous initiate-failure
  compensation itself failed — is also reclaimed, once it ages past `orphan_grace_secs`, by step 1's second phase
  (`cpt-cf-file-storage-algo-sweep-versionless-files`); it does not require a pending version to have existed and
  been reclaimed first
- [x] A file that still has another (bound) version is never deleted by the zero-version-orphan check, even while
  one of its other versions is independently reclaimed as abandoned-pending
- [x] A `pending` version still backing a **live** (`in_progress`, unexpired) multipart session, **or** backing
  **any** `completing` session regardless of its lease, is **never** selected for orphan reclamation regardless of
  its age — this invariant is enforced both at the version-query level (`list_pending_older_than`'s `NOT IN`
  subquery against active sessions) and, independently, at the zero-version-orphan-file check
  (`has_blocking_multipart_session`)
- [x] Once an `in_progress` session's `expires_at` has also passed, the session is aborted by the sweep's own step 2
  and its previously-protected version becomes reclaimable by step 1 on a subsequent cycle; a `completing` session's
  version instead becomes reclaimable only once step 2's takeover/abort path moves it out of `completing`,
  independent of `expires_at`
- [x] An expired multipart session is aborted via a CAS (`in_progress -> aborted`) that a concurrent
  `complete_multipart_upload` can win instead; the loser leaves the version completely untouched either way (no
  double-delete, no deleting a since-bound version)
- [x] Expired `idempotency_keys` rows are purged by the same sweep cycle; `audit_outbox`/`events_outbox` rows are
  never touched by any sweep step regardless of age, since `published_at` staying `NULL` cannot yet be distinguished
  from "not yet relayed" until the Tier-4 `EventBroker` relay exists
- [x] The sweep runs independently and without coordination on every control-plane instance; every mutation is a
  CAS or status-guarded delete, so redundant concurrent sweeps are safe (idempotent), merely duplicative of effort
- [ ] Backend blob-without-row reconciliation (a `list_paths` scan for backend objects with no matching version row)
  and flagging an `available` version with no matching backend object for operator attention — both named in the
  PRD's `fr-orphan-reconciliation` — are **not implemented**; deferred to P3 pending cross-instance leader election,
  which a per-instance `list_paths` scan cannot safely do without (see the scope note in §1.2)
- [ ] Non-current-version retention (pruning a superseded version once a newer one is bound, per a
  `keep_last_n`/`max_non_current_age_days`-style policy) is **not implemented** — no such field exists on
  `RetentionRuleBody` today; deferred to P3 pending a versioning-policy schema (see `lifecycle.rs`'s module doc
  comment)
- [ ] Sweep-driven usage reports (bytes/file-count debits on reclaim/expiry) are wired end-to-end in code but are a
  no-op in every real deployment today, since no `UsageReporter` is configured (`cpt-cf-file-storage-fr-usage-
  reporting` is a separate, not-yet-connected requirement — see the caveat under [Cleanup Engine and Background
  Sweep Scheduling](#cleanup-engine-scheduling-not-implemented))
- [ ] The retention-expiry **sweep's own internal read** (`CleanupStore::list_all_retention_rules`, distinct from the
  `GET /retention-rules` REST listing below) still loads every stored retention rule across every tenant and scope
  in one unpaginated read before scanning files (only the file scan itself is keyset-paginated, step 2 above). Safe
  today because the rule set is small relative to the number of files, but it is an unbounded read that would need
  its own pagination if the number of tenants/rules grows large enough to matter; deferred until that is observed
  in practice. The REST-facing `GET /retention-rules` listing is cursor-paginated in either direction
  (`limit`/`cursor`, canonical order `created_at desc, rule_id desc`, non-admin visibility filter
  applied in SQL) — this remaining limitation is scoped to the sweep's own data-plane read, not the public API.
