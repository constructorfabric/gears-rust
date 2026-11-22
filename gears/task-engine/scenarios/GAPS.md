# Task Engine Scenarios: Gaps

Every place where a scenario had to choose because the Task Engine docs are silent, unclear or contradict each other. Each entry names the docs, the scenarios that depend on it, what those scenarios assume, and the question the doc owner must settle. When a gap is settled, fix the docs, update the scenarios it lists, and delete the entry.

- **Status (2026-11-06):** every entry is decided. Each carries a `Decided` line from the wire review (2026-11-04) or the gap review (2026-11-06); entries that only asked for an accepted proposal's doc edit are settled by that proposal. The scenarios, [CONVENTIONS.md](CONVENTIONS.md), [ERRORS.md](ERRORS.md), the draft OpenAPI ([#5791](https://github.com/constructorfabric/gears-rust/pull/5791)) and the draft reference DDL follow the decisions. What is left in each entry is its `Remaining` work: the PRD, DESIGN, ADR and schema edits, which wait for the proposals PR, plus the library changes named there (G-SEC-03, G-SEC-09). Delete an entry once its doc edit lands. The `Assumed` lines record what the scenarios assumed before the decision.
- **Baseline:** `main` at `6efb0f856` plus the accepted proposals of [PR #5730](https://github.com/constructorfabric/gears-rust/pull/5730) (CONVENTIONS §1). Line numbers refer to that commit.
- **Kinds:** `contradiction` (two docs disagree), `missing` (no rule), `unclear` (a rule that admits two readings), `proposal-open` (depends on an `Open` proposal item), `wire` (a request or response shape the docs do not give).
- **IDs** are stable and grouped by area: `WIRE` shapes and error contract · `CRT` create, idempotency, dedup, supersession · `CLM` claim, ordering, limits · `EXE` sessions, fencing, execution, release, entries · `RTY` retry, timeouts, dead-letter, cancel · `WAI` waits, parent/child, dependencies · `SEC` auth, errors, search, reads, events · `OPS` queues, config, bulk, domains, reassignment, blacklist, retention, flows.

## Summary

| Area | Gaps | Contradiction | Missing | Unclear | Proposal-open | Wire |
|---|---:|---:|---:|---:|---:|---:|
| [WIRE](#wire--wire-shapes-and-error-contract) | 20 | 4 | 13 | 3 | 0 | 0 |
| [CRT](#crt--create-idempotency-dedup-supersession) | 26 | 5 | 13 | 6 | 0 | 2 |
| [CLM](#clm--claim-ordering-limits) | 27 | 5 | 10 | 9 | 1 | 2 |
| [EXE](#exe--sessions-fencing-execution-release-entries) | 24 | 4 | 9 | 8 | 1 | 2 |
| [RTY](#rty--retry-timeouts-deadletter-cancel) | 22 | 4 | 6 | 11 | 1 | 0 |
| [WAI](#wai--waits-parentchild-dependencies) | 42 | 9 | 19 | 12 | 0 | 2 |
| [SEC](#sec--auth-errors-search-reads-events) | 28 | 7 | 8 | 8 | 1 | 4 |
| [OPS](#ops--queues-config-bulk-domains-reassignment-blacklist-retention-flows) | 43 | 5 | 22 | 13 | 1 | 2 |
| **Total** | **232** | **43** | **100** | **70** | **5** | **14** |

## Start here: cross-cutting themes

Several areas hit the same problem from different sides. Settle these first; each one closes a cluster of entries.

1. **The error contract cannot be built as written.** DESIGN puts reason codes in `context.reason` on categories whose platform context has no such key, and asks for field violations on `404`/`403`, which cannot carry them. The library also sends `Retry-After` only on `503`, while the PRD requires it on every `429`, and no doc gives values. → G-WIRE-14, G-WIRE-15, G-SEC-02, G-SEC-03, G-SEC-19, G-CLM-09, G-CLM-10, G-WAI-25, G-WAI-33, G-CRT-07, G-CRT-08, G-CLM-18, G-SEC-21, G-CRT-23.
2. **No API contract.** There is no OpenAPI on `main`; the draft OpenAPI ([#5791](https://github.com/constructorfabric/gears-rust/pull/5791)) is the annotated draft, and branch `docs/task-engine-openapi` carries the same contract without annotations (CONVENTIONS §5), and the DESIGN endpoint table and sequence diagram disagree on the claim and complete shapes. DESIGN §3.1 is an internal domain model with no API projection to the SDK or the wire, and it types GTS references as `Uuid`. → G-WIRE-02 to G-WIRE-13, G-WIRE-16, G-WIRE-17, G-WIRE-19, G-EXE-15, G-EXE-18, plus the `wire` entries of each area.
3. **The docs still describe what the accepted proposals removed.** Consumer-set waits, `update` on the parent, dependency add/remove, `update` covering more than tags, `resolve` under `update`, `authz_scope`, the k-way claim merge, the "no SKIP LOCKED" rule, the exact per-tenant depth counter, and `GET /admin/dlq` are all still on `main`, and the event schemas lack the new states. → G-WAI-01, G-WAI-02, G-WAI-03, G-WAI-04, G-WAI-27, G-CRT-15, G-CLM-01, G-CLM-07, G-CLM-11, G-RTY-01, G-SEC-11.
4. **PDP unreachable: `403` or `503`.** PRD.md:918 says `403`; PRD.md:961 and DESIGN.md:1022, :1215 say `503`. Scenarios assume `503`. → G-SEC-01, G-EXE-06, G-WAI-31.
5. **Consequences of sampled per-tenant depth.** The check moves before the transaction, so a replay can get `429` and a supersession no longer frees its own capacity first; the backlog table goes away and the deferred limit is sampled too; tests cannot see when a sample was taken. → G-CRT-14, G-CLM-24, G-CLM-11, G-CLM-13, G-EXE-23.
6. **Fencing holes.** A second `start` resets the execution deadline; fenced writes ignore the session status between expiry and requeue; replaying a retryable-error completion gets `LEASE_LOST`; result validation runs before the fence; `complete` and `cancelled` from `claimed` are allowed by the fence but not by the state machine; a foreign token on an unreadable task: `404` or `LEASE_LOST`. → G-EXE-10, G-EXE-02, G-EXE-16, G-RTY-05, G-EXE-17, G-EXE-11, G-RTY-21, G-EXE-09.
7. **Dead-letter inspection after `GET /admin/dlq` is removed.** Search has a 31-day window, so old dead-lettered tasks cannot be listed, `dead_lettered_from` is not filterable, and the permission drops from `admin` to `read`. → G-SEC-08, G-RTY-10, G-RTY-11.
8. **Initial wait group on create has no rules.** Precedence over `run_after` and predecessors, deferred admission, events (`initial_state` has no `waiting`), and the attempt count of its first claim. → G-CRT-01, G-WAI-11, G-WAI-19, G-CLM-26.
9. **Cancellation details.** Cancel of a terminal task, repeated cancels, the default mode, the error for `cancellable=false`, whether a worker may still succeed after a cancel request, the result code of cascaded child cancels, and a parent that fails without cancelling its children. → G-RTY-13 to G-RTY-19, G-WAI-23, G-CRT-21, G-WAI-38, G-OPS-16, G-OPS-38.
10. **Contradictions in operations.** Bulk denial is `403` in one table and a per-task outcome in another; how a queue move starts and ends, and whether removing a domain with live tasks stops startup or only fails readiness; an expired hand-off counts toward the blacklist or not; a blacklisted worker's claim is `400` or `204`. → G-OPS-14, G-OPS-25, G-OPS-26, G-OPS-30.
11. **Defaults that break the fixtures.** The shipped default queue's execution timeout (86,400 s) exceeds the tenant-owned cap (3,600 s), so a tenant-owned type may be unable to use it; expired idempotency rows still answer `409` until the daily cleanup. → G-CRT-05, G-CRT-12, G-CRT-13.
12. **Events and history.** No event for a liveness requeue; `taskWaiting`/`waitConditionSet` name one condition of a group; whether `taskQueued` is recorded at create; queue-scoped events have no schema or read path; schema field names differ from DESIGN. → G-EXE-05, G-WAI-12, G-WAI-16, G-SEC-23, G-OPS-36, G-OPS-04, G-OPS-05, G-SEC-11, G-CRT-17.

### Duplicate pairs

These entries describe the same question from two areas. Keep both until it is settled, then close them together: G-CRT-14 / G-CLM-24 · G-SEC-08 / G-RTY-10 · G-SEC-02 / G-CLM-10 / G-WIRE-14 · G-SEC-01 / G-EXE-06 / G-WAI-31 · G-CRT-01 / G-WAI-11 / G-CLM-26 · G-CLM-25 / G-OPS-17 · G-WAI-23 / G-CRT-21 · G-SEC-23 / G-OPS-36 · G-RTY-08 / G-OPS-07 · G-CLM-21 / G-OPS-12 · G-SEC-19 / G-WAI-25 / G-WIRE-15 · G-WAI-04 / G-SEC-18 · G-WAI-41 / G-OPS-24 · G-EXE-16 / G-RTY-05 · G-EXE-11 / G-RTY-21.

## WIRE — Wire shapes and error contract

### G-WIRE-01 — Problem `type` URI form
- **Kind**: contradiction
- **Docs**: DESIGN.md:1186 (`"type": "gts.cf.core.errors.err.v1~…"`); docs/arch/errors/DESIGN.md:525 (`"type": "gts://gts.cf.core.errors.err.v1~…"`)
- **Scenarios**: every negative scenario; errors/
- **Assumed**: the platform form with the `gts://` prefix.
- **Question**: fix the Task Engine DESIGN example to the platform form.

### G-WIRE-02 — No OpenAPI or request/response schemas
- **Kind**: missing
- **Docs**: DESIGN.md:1058-1133 (endpoint tables only); PRD.md:1393-1399 (REST contract named, not specified)
- **Scenarios**: all
- **Assumed**: the shapes in CONVENTIONS §5, each field marked D/P/S.
- **Question**: publish an `openapi.yaml` (as the Event Broker does) and replace CONVENTIONS §5 with links to it. Until then every **S** field below is provisional.

### G-WIRE-03 — Create response body and `Location`
- **Kind**: missing
- **Docs**: DESIGN.md:1062, :1079
- **Scenarios**: tasks/create/*
- **Assumed**: `201 Created`, `Location` header, body = full task resource.
- **Question**: confirm status, header and body.
- **Decided** (wire review 2026-11-04): `201 Created`, `Location`, body = the full task resource. Remaining: the doc edit.

### G-WIRE-04 — Task resource: counters, assignment and tag shape
- **Kind**: missing
- **Docs**: DESIGN.md:280-340 (domain model), :538-539 (`retry_count`, `liveness_requeues` stored), PRD.md:363 (key-value tags)
- **Scenarios**: retry/*, timeouts/*, worker/*, tasks/read/*
- **Assumed**: `retry_count` and `liveness_requeues` are returned; `assignment` is returned without `lease_token`; `tags` is a key→value object with unique keys.
- **Question**: which fields does `GET /tasks/{id}` return, and is a tag key unique per task?
- **Decided** (wire review 2026-11-04): `tags` is an array of `{ key, value }`, each key at most once (repeat → `400`), sorted by key; `retry_count` and `liveness_requeues` are returned. Remaining: the doc edit.

### G-WIRE-05 — Claim request `timeout` unit and claim response fields
- **Kind**: unclear
- **Docs**: DESIGN.md:1064, :1081; ADR-0017:56; PRD.md:550 (resumed worker receives the wait result)
- **Scenarios**: worker/claim/*, waits/framework/*
- **Assumed**: `timeout` in seconds; `lease.lease_secs` returned; the wait result arrives as `claims[].resumed { wait_group_id, outcome, conditions[] }`.
- **Question**: name the unit (or rename to `timeout_secs`), and define where and how the wait result reaches the resumed worker.
- **Decided** (wire review 2026-11-04): claim request `timeout_secs` in seconds; the lease stays nested (`claims[].lease { lease_token, lease_epoch, attempt, lease_secs, domain }`); `lease_secs` is returned. `resumed` as in G-WAI-08. Remaining: the doc edit.

### G-WIRE-06 — Worker-call responses (start, batch heartbeat, progress, checkpoint, complete, release)
- **Kind**: missing
- **Docs**: DESIGN.md:1065-1071, :1081
- **Scenarios**: worker/sessions/*, worker/execution/*, worker/release/*
- **Assumed**: start/complete/release return the task resource; batch heartbeat returns `results[{task_id, outcome: ok|lease_lost, state, cancel_requested}]`; progress returns `200` with no body (DESIGN.md:1314; the update is buffered); checkpoint returns the merged `context`.
- **Question**: confirm each response body (the draft the draft OpenAPI ([#5791](https://github.com/constructorfabric/gears-rust/pull/5791)) follows these assumptions).
- **Decided** (wire review 2026-11-04): `start`, `complete` and `release` return `200` with the full task resource; `checkpoint` returns `{ context }`; `progress` returns `200` empty. The batch heartbeat returns `{ session_id, heartbeat_interval_secs, results: [{ task_id, outcome: ok, state, cancel_requested, cancel_request? }, { task_id, outcome: lease_lost }] }`. Remaining: the doc edit.

### G-WIRE-07 — Reported failure has no endpoint of its own
- **Kind**: unclear
- **Docs**: DESIGN.md:1070 (only `/complete`); DESIGN.md:536-538 ("worker completes (terminal failure)"); Proposals › Heartbeats and leases ("complete, fail, release")
- **Scenarios**: retry/*, worker/execution/*
- **Assumed**: a failure is `POST /tasks/{id}/complete` with `result_code: error`.
- **Question**: confirm, and use one name ("complete with error") in the proposals and DESIGN.
- **Decided** (gap review 2026-11-06): confirmed: no `/fail`; a failure is `POST /tasks/{id}/complete` with `result_code: error` ("complete with error" in the proposals and DESIGN). Remaining: the doc edit.

### G-WIRE-08 — Cancel request body and status for a leased task
- **Kind**: missing
- **Docs**: DESIGN.md:1072; PRD.md:346-352
- **Scenarios**: tasks/cancel/*
- **Assumed**: body `{ mode, reason? }`; `200` with the cancelled task when it ends at once; `202` when only `cancel_requested` is set.
- **Question**: confirm the body and both status codes.
- **Decided** (wire review 2026-11-04): the body is optional (`mode` defaults to `requested`, `reason` optional); a leased task with `requested`/`graceful` → `202` with the task resource (`cancel_requested: true`, `cancel_request`). Remaining: the doc edit.

### G-WIRE-09 — Wait-condition request and response shapes
- **Kind**: missing
- **Docs**: DESIGN.md:360-391, :1095-1098
- **Scenarios**: waits/*
- **Assumed**: `timeout_secs` per condition; response `{ wait_group, task_state }`; park response `{ parked, result? }`; resolve body `{ value }`; condition states `pending|satisfied|timed_out|cancelled`.
- **Question**: define the shapes and the condition state enum (DESIGN names `WaitConditionState` but never lists its values).
- **Decided** (wire review 2026-11-04): condition states `pending`, `satisfied`, `timed_out`, `cancelled`; conditions take relative `timeout_secs` and return the computed `timeout_at`; set-wait returns `{ wait_group: { id, mode, parked, conditions[] }, task_state }`, park returns `{ parked: true, task_state }` or `{ parked: false, result: <WaitResult> }`, resolve returns `{ condition }` (the full stored condition). Remaining: the doc edit.

### G-WIRE-10 — Event history item shape
- **Kind**: missing
- **Docs**: DESIGN.md:402-409 (TaskEvent), :1108; PRD.md:868
- **Scenarios**: events/*, most side-effect sections
- **Assumed**: items `{ id, event_type, actor, payload, timestamp }` in a `JsonPage`; `event_type` is the camelCase name.
- **Question**: confirm field names, and whether the history uses the camelCase name or the GTS event-type ID.
- **Decided** (wire review 2026-11-04): history items are `{ id, event_type, actor, timestamp, payload }`: `event_type` the camelCase name, `actor` the causing subject or `null` for engine-caused events, `payload` the `data` of the matching lifecycle event schema. Remaining: the doc edit.

### G-WIRE-11 — Queue resource shape
- **Kind**: missing
- **Docs**: PRD.md:493; DESIGN.md:1106-1107; Proposals › Queue operations (hold state)
- **Scenarios**: queues/*
- **Assumed**: `{ id, kind, domains, limits, counts, sampled_at, hold }` (`domains` and `hold` are P).
- **Question**: define the `GET /queues` item and the detail shape, including the hold fields.
- **Decided** (wire review 2026-11-04): queue resource `{ id, kind, domains, limits, settings, counts, sampled_at, config_version, hold }`; `GET /queues` returns `{ items }`, unpaged (queues are a bounded, platform-registered set). Remaining: the doc edit.

### G-WIRE-12 — Admin and bulk request bodies
- **Kind**: missing
- **Docs**: DESIGN.md:1110-1131
- **Scenarios**: bulk/*, queues/*, dead-letter/*, worker/reassignment/*
- **Assumed**: bulk `{ task_ids, …op fields }`; `/admin/config` `{ queues, types }` keyed by ID or pattern; `GET /admin/config/{version}` `{ config_version, domains[{domain, applied}] }`.
- **Question**: define these bodies.
- **Decided** (wire review 2026-11-04): bulk bodies as drafted: cancel `{ task_ids, mode?, reason? }` (`mode` defaults to `requested`), reprioritize `{ task_ids, priority?, urgency? }` (at least one), retry `{ task_ids }`, move `{ task_ids, queue }`; response `{ results: [{ task_id, outcome }] }` in request order; queue hold and release return `200` with the queue resource. Remaining: the doc edit.

### G-WIRE-13 — Duration field naming
- **Kind**: unclear
- **Docs**: DESIGN.md:352-358 (`Duration` types); `schemas/types/gts.cf.core.tasks.queue.v1~.schema.json` (`*_secs`); DESIGN.md:1071 (`delay_secs`), :1064 (`timeout`)
- **Scenarios**: all with timeouts
- **Assumed**: integer seconds with a `_secs` suffix on the wire, as in the queue schema.
- **Question**: adopt one naming rule for every duration field in the API.
- **Decided** (gap review 2026-11-06): every duration is an integer number of seconds with a `_secs` suffix; every instant is RFC 3339 UTC with an `_at` suffix (as the draft OpenAPI already does). Remaining: the doc edit.

### G-WIRE-14 — Reason codes in `context.reason` on categories that have no such key
- **Kind**: contradiction
- **Docs**: DESIGN.md:1205-1235 (`context.reason` on `invalid_argument`, `failed_precondition`, `resource_exhausted`); `libs/toolkit-canonical-errors/src/context.rs` (only `permission_denied`, `aborted`, `unauthenticated` carry `reason`); docs/arch/errors/categories/08-resource-exhausted.md
- **Scenarios**: every `400`, `429` and `failed_precondition` scenario; errors/
- **Assumed**: both `context.reason` (as DESIGN says) and the library's required array, with the code repeated in `field_violations[].reason`, `violations[].type` or `violations[].subject` (CONVENTIONS §6).
- **Question**: either the platform adds `reason` to these categories, or DESIGN moves each code into the array and says which field carries it. Same decision for the depth dimension of `QUEUE_DEPTH_LIMIT` and `EVENT_WAIT_LIMIT`.
- **Decided** (gap review 2026-11-06): no platform change; codes go in the slots the library emits: `invalid_argument` → `field_violations[].reason` (every code names a field); `failed_precondition` → `violations[{type: "STATE", subject: <code>}]` with `resource_name` = the task; `resource_exhausted` → `violations[{subject: <quota>}]` from the closed list `queue_depth.queue` | `queue_depth.tenant` | `queue_depth.domain_global` | `queue_depth.deferred` | `concurrency` | `event_waits.<dimension>`. DESIGN drops `context.reason` for these categories. Remaining: the doc edit, CONVENTIONS §6, ERRORS.md and the scenarios that assert `context.reason` on these categories.

### G-WIRE-15 — Field violations asked for on `404` and `403`
- **Kind**: contradiction
- **Docs**: DESIGN.md:1200 (`not_found` with a field violation for a parent or predecessor), :1214 and :1222 (`permission_denied` naming the check and reference or condition index); `libs/toolkit-canonical-errors/src/context.rs` (`NotFoundV1 {}`, `PermissionDeniedV1 { reason }`)
- **Scenarios**: hierarchy/children/*, hierarchy/dependencies/*, auth/1.2x creator-authority, waits/event/* consume denials
- **Assumed**: scenarios show the violation as DESIGN asks and mark it in Notes.
- **Question**: drop the violation (the `404` must not leak which link failed anyway), or extend the library.
- **Decided** (gap review 2026-11-06): `404` carries no field violation (it would reveal which link exists); `403` carries only its reason code, and the check or condition index goes only into `detail`. No library change. Remaining: the doc edit.

### G-WIRE-16 — DESIGN types GTS references and subjects as `Uuid`
- **Kind**: contradiction
- **Docs**: DESIGN.md:282-285 (`type_id (GTS)`, `queue_id (GTS)`, `owner_id`, `created_by` as `Uuid`); GTS IDs are strings (`gts.cf.core.tasks.task.v1~acme.reports._.render.v1~`); `schemas/types/…task_created.v1~` (`task_type`, `queue` as GTS strings, `created_by` as `string`); the Event Broker SDK uses `GtsTypeId` / `GtsInstanceId` (`event-broker-sdk/src/models.rs`)
- **Scenarios**: every scenario (fixtures use full GTS IDs); the draft OpenAPI ([#5791](https://github.com/constructorfabric/gears-rust/pull/5791)) `Task`, `CreateTaskRequest`
- **Assumed**: `type` and `queue` are GTS ID strings on the wire and typed GTS IDs in the SDK; `created_by` and `owner_id` are platform subject IDs (strings).
- **Question**: Retype these fields in DESIGN §3.1 (and the SDK) as GTS IDs and subject IDs.
- **Decided** (wire review 2026-11-04): `type` and `queue` are GTS ID strings on the wire; DESIGN §3.1 is to retype them. Remaining: the doc edit.

### G-WIRE-17 — No API projection from the domain model to the SDK and the wire
- **Kind**: missing
- **Docs**: DESIGN.md:276-450 (§3.1 domain model, internal), DESIGN.md:1040-1240 (§3.3 endpoint tables, names only some body fields); Event Broker DESIGN §3.1 "API projection" table for Topic (the pattern); `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md:60-80` (SDK models vs REST DTOs)
- **Scenarios**: CONVENTIONS §5 (every **S** wire name); the draft OpenAPI ([#5791](https://github.com/constructorfabric/gears-rust/pull/5791))
- **Assumed**: wire name = domain name, except `type`/`queue` (domain `type_id`/`queue_id`), `predecessors[]` + `dependency_mode` (domain `TaskDependency { predecessor_id, mode }` per row), `assignment` without `lease_token`, `session_id`, `handoff_until` or deadlines, and `idempotency_key` (REST header; field of the SDK `CreateTaskRequest` until header binding is supported). Event `data` uses `task_id` / `task_type` because the envelope owns `id` / `type`.
- **Question**: Add an "API projection" table per resource (task, assignment, wait condition, entry, queue) to DESIGN §3.3: domain field → SDK field → wire field, and which fields are hidden, renamed, server-set, input-only or read-only. Decide whether the dependency mode is per task or per edge.
- **Decided** (wire review 2026-11-04): wire names `type`, `queue`; `predecessors: [{ task_id, on_predecessor_failure }]` with one `dependency_mode` per task. Remaining: the doc edit.

### G-WIRE-19 — Stored task data the API does not project
- **Kind**: missing
- **Docs**: DESIGN.md:329-340 (`Assignment`: `assignment_deadline`, `execution_deadline`, `handoff_until`), DESIGN.md:367 (`resolved_payload` on the wait condition), PRD.md:346-353 and `POST /tasks/{id}/cancel` `reason` (cancel request), PRD.md:521-527 (SLA breach), DESIGN.md (re-enqueue restarts the lifetime), Proposals › Domains (task domain fixed at creation)
- **Scenarios**: the draft OpenAPI ([#5791](https://github.com/constructorfabric/gears-rust/pull/5791)) `Task`, `Assignment`, `WaitConditionBase`; CONVENTIONS §5.1
- **Assumed**: as decided below. Internal only: `idempotency_key`, `revision`, `resume_pending`, `lease_token`, `session_id`, `assignment_deadline`, `handoff_until`, the lifetime deadline, dedup and supersede keys.
- **Question**: Add the decided fields to DESIGN's API projection (G-WIRE-17). Storage in the draft reference DDL (`migration.sql`, kept outside this branch): the cancel request columns (`cancel_mode_code`, `cancel_requested_by`, `cancel_requested_at`, `cancel_reason`) back `cancel_request`, `grace_until` comes from the `cancel_grace` timer, `resolved_payload` backs the wait-condition `result`.
- **Decided** (wire review 2026-11-04): the task also returns `domain` and `cancel_request { mode, requested_by, requested_at, reason, grace_until, superseded_by }`; `assignment` adds `execution_deadline` only. Not exposed: `lifetime_deadline`, `sla_breached_at`, a wait-group summary, `started_at`, `assignment_deadline`, `handoff_until` (`started_at` and `sla_breached_at` stay as internal columns in migration.sql, on both `te_task_state` and `te_task_results`: written once, kept for run-time and SLA reporting). The wait-condition `result` is returned (G-WAI-08).

### G-WIRE-20 — Storage choices made by the draft migration.sql
- **Kind**: missing
- **Docs**: DESIGN.md §3.7 (DESIGN.md:1394-1960)
- **Scenarios**: the draft reference DDL (`migration.sql`, kept outside this branch)
- **Assumed**: the draft picks these where DESIGN is silent or names no column, index or query:
  - MySQL/MariaDB times are `TIMESTAMP(6)` with a UTC session time zone, as in OAGW; it ends in 2038 (`2038-01-19 03:14:07` UTC), accepted: a later migration changes the type before then. Until it does, a caller-supplied time past that limit (`run_after`, `sla_deadline`, queue `hold_until`) cannot be stored on MySQL.
  - Enum discriminants other than `urgency` are assigned by the draft; 0 is unused so a defaulted 0 fails the `CHECK`.
  - Reserved words renamed: `te_dedup.window` → `window_code`, `te_events.timestamp` → `occurred_at`, `te_tags.key` / `value` → `tag_key` / `tag_value`.
  - `te_events.actor_identity` is nullable (engine-caused events have no actor; DESIGN has `NOT NULL`).
  - Ordering columns appended for keyset batches: `id` on `ix_te_tasks_parent`, `task_id` on `ix_te_task_state_parent` and `ix_te_deps_predecessor`.
  - Indexes DESIGN does not list: `ix_te_task_results_dlq` (DLQ counts), `ix_te_waits_children_pending` (children re-check sweep), `ix_te_worker_blacklist_until` (node reload, cleanup).
  - New columns: `te_task_state.resume_wait_group_id` (feeds `Claim.resumed`; `resume_pending` alone cannot tell a resume from a release), `te_wait_conditions.timeout_secs` (an armed condition's duration).
  - Wait conditions of terminal tasks move to a ring table `te_wait_condition_history` (+ `_retained`) instead of partitioning the live table; resolve replay by condition id probes every partition (no global index).
  - New ring table `te_task_predecessors` (+ `_retained`) for the `predecessor_id` search over terminal tasks.
  - Sampled depth lives in per-domain tables (`te_queue_depth_samples`, `te_tenant_depth_samples`) polled by every node, not in `te_runtime_config`; the per-tenant table keeps only keys at or above their limit.
  - One `wait` timer per task at the earliest deadline of its pending parked conditions (timeouts and timer resume times), re-armed after each fire.
  - The ring fence is seeded with the N buckets ending at the install bucket.
  - Arming a wait takes the task row lock with a guarded no-op update; parking an already-complete armed group keeps its rows and marks them consumed by setting `parked_at`.
  - Tag patches serialize on the task's state (or results) row and bump `te_task_state.updated_at` of a live task only.
  - Search turns the `created_at` window into a UUID v7 `task_id` range.
  - A worker entry write takes `FOR SHARE` on the task row so a concurrent terminal transition waits.
  - `started_at` (set by start, cleared by claim) and `sla_breached_at` (set by the sla timer) are stored but not projected; nothing reads them yet. Name the metric or report that will (for example a run-time histogram, an SLA-breach search filter), or drop them.
- **Question**: Confirm or replace each; move the confirmed ones into DESIGN §3.7.
- **Decided** (gap review 2026-11-06): confirm the draft's choices, with two settled: MySQL keeps `TIMESTAMP(6)` with a UTC session (like OAGW), so a caller-supplied time past `2038-01-19 03:14:07` UTC (`run_after`, `sla_deadline`, hold `until`) → `400` on MySQL until a later migration; `started_at` is kept to feed a `te_task_run_duration_seconds` histogram and `sla_breached_at` is dropped (the `slaBreached` event records it). Remaining: move the confirmed choices into DESIGN §3.7, drop `sla_breached_at` from migration.sql, add the MySQL `400` to the error table.

### G-WIRE-21 — Reassignment accept response
- **Kind**: missing
- **Docs**: DESIGN.md:1069 (`accept` gives the new assignee "a lease in its session"), DESIGN.md:755 (`accept_reassignment → (Task, Lease)`)
- **Scenarios**: [worker/reassignment/1.02-positive-only-named-identity-accepts.md](worker/reassignment/1.02-positive-only-named-identity-accepts.md); the draft OpenAPI ([#5791](https://github.com/constructorfabric/gears-rust/pull/5791)) `AcceptResponse`
- **Assumed**: as decided below.
- **Question**: Add the shape to DESIGN §3.3.
- **Decided** (wire review 2026-11-04): `200 { session_id, domain, heartbeat_interval_secs, task, lease, authority }`, the same as one claim entry plus its session. Remaining: the doc edit.

## CRT — Create, idempotency, dedup, supersession

### G-CRT-01 — Initial wait group on create: placement, admission and events undefined
- **Kind**: missing
- **Docs**: Proposals › Task mutability (accepted: "The create request can carry an initial wait group, so a task can start in `waiting`"); PRD.md:258 (placement rule names only `blocked`/`scheduled`/`queued`); DESIGN.md:466-513 state diagram and DESIGN.md:523-525 (no `— → waiting` row); `schemas/types/gts.cf.core.events.event.v1~cf.core.tasks.task_created.v1~.schema.json` lines 62-69 (`initial_state` enum is `queued`, `scheduled`, `blocked` only); PRD.md:452 (parking is a deferred admission)
- **Scenarios**: [tasks/create/1.04-positive-initial-wait-group-waiting.md](tasks/create/1.04-positive-initial-wait-group-waiting.md)
- **Assumed**: `201`, `state: waiting`; `taskCreated` with `initial_state: "waiting"`; `wait_timeout` and `lifetime` timers armed, no `queue_timeout`; counts as a deferred admission; wait-condition IDs only via `GET /tasks/{id}/wait-conditions`.
- **Question**: Where does `waiting` sit in the placement order (vs unmet predecessors and a future `run_after`)? Is it a deferred admission (`tenant_max_deferred_tasks`, `429 deferred`)? Which events are recorded at create (`taskCreated` with a new `initial_state` value, plus `taskWaiting`/`waitConditionSet`?) — the `taskCreated` schema enum must change. Does the create response carry the wait group and condition IDs? Which wait kinds are allowed at create (`children` makes no sense without children; `timer` duplicates `run_after`)?
- **Decided** (gap review 2026-11-06): `wait` with predecessors or a future `run_after` → `400 invalid_argument` (field violation on `wait`); allowed kinds are those a worker may set in the phase (p1: `timer` only; `input` from p2, Proposals › Waits) except `children` (`400`); no `park`/`lease_token`; condition timeouts and timer `resume_at` count from the create. Deferred admission (`429` `deferred` when full). Events: `taskCreated` with `initial_state: "waiting"` (enum extended), then the same wait event(s) as a worker set (G-WAI-12). Response: the task resource; condition IDs via `GET /tasks/{id}/wait-conditions`. No `queue_timeout` until the task is `queued`. A `children` condition in the create group is `NOT_ALLOWED` in every phase, checked before the p1 feature refusal. Remaining: the doc edit.

### G-CRT-02 — Create into a `dead_letter` queue has no rule or error
- **Kind**: missing
- **Docs**: PRD.md:801 ("it only holds `dead_lettered` tasks, it is never dispatched (a claim naming it is rejected, `400`)"); PRD.md:995; DESIGN.md:590; DESIGN.md:2432 (claim rejects); task-type schema `default_queue` ("MUST reference a queue of kind 'dispatch'"); DESIGN.md:2529-2538 (create admission has no `kind` check)
- **Scenarios**: [tasks/create/1.12-negative-placement-and-trust-limit-refusals.md](tasks/create/1.12-negative-placement-and-trust-limit-refusals.md)
- **Assumed**: `400` `invalid_argument`, field violation on `queue`.
- **Question**: Confirm that `POST /tasks` (and bulk move) naming a `dead_letter` queue is refused, and with which status and field/reason.
- **Decided** (gap review 2026-11-06): a `dead_letter` queue on create or bulk move → `400 invalid_argument`, `field_violations[{field: "/queue", reason: DEAD_LETTER_QUEUE}]` (same code as a claim); in bulk move a per-task outcome. Clarified: in bulk move the destination is one queue for the whole request, so an unknown or dead-letter destination refuses the whole request (`404` / `400 DEAD_LETTER_QUEUE`); a re-enqueue `queue` (G-RTY-20) gets the same answers on `/queue`. Remaining: the doc edit.

### G-CRT-03 — Unknown (unregistered) queue on create has no error mapping
- **Kind**: missing
- **Docs**: PRD.md:995 ("task creation and claims MUST name registered queues"); DESIGN.md:1195-1236 (error table has no unknown-queue row); DESIGN.md:2533
- **Scenarios**: [tasks/create/1.12-negative-placement-and-trust-limit-refusals.md](tasks/create/1.12-negative-placement-and-trust-limit-refusals.md)
- **Assumed**: `400` `invalid_argument`, field violation on `queue` (by analogy with `UNKNOWN_TASK_TYPE`, DESIGN.md:1212).
- **Question**: `400 invalid_argument` or `404 not_found`? Is there a `context.reason` (for example `UNKNOWN_QUEUE`)? Same answer for a claim naming an unknown queue?
- **Decided** (gap review 2026-11-06): unregistered queue on create or bulk move → `404 not_found` (`resource_type` = queue base type, `resource_name` = the ID), as for claims (G-CLM-15); in bulk move a per-task outcome. Clarified: bulk move with an unknown destination → whole-request `404` (one destination per request); re-enqueue naming an unknown `queue` → `404`. Remaining: the doc edit and the create scenarios that assume `400`.

### G-CRT-04 — Which resolved settings are stored, and how a partial retry policy is filled
- **Kind**: unclear
- **Docs**: PRD.md:980 ("a retry policy is taken whole from the first level that has one"); DESIGN.md:715 (resolved result stored on the task); task-type schema `retry_policy` (required `max_retries`, `strategy`, `base_interval_secs`, `max_interval_secs`; defaults `max_liveness_requeues` 20, `jitter_factor` 0, `non_retryable_codes` []); queue schema `timeouts` description (null `heartbeat_timeout_secs` = engine default lease); DESIGN.md:530 (lease resolved at claim); the draft reference DDL (`migration.sql`, kept outside this branch) (stores the effective values in `te_tasks.retry_policy_json` / `timeouts_json`)
- **Scenarios**: [tasks/create/1.03-positive-settings-resolution.md](tasks/create/1.03-positive-settings-resolution.md)
- **Assumed**: omitted fields of the winning retry policy take the trait schema defaults (not the queue's values); the stored `heartbeat_timeout_secs` is the resolved `default_lease_duration_secs` (30), not `null`.
- **Question**: When a type trait or request gives a partial retry policy, are the omitted fields schema defaults, or is that policy invalid? Is the lease (`heartbeat_timeout_secs`) and `default_execution_timeout_secs` resolved and stored at create, or left null and resolved at claim/start? What does `GET /tasks/{id}` show?
- **Decided** (gap review 2026-11-06): a level's retry policy must carry the schema's required fields (`max_retries`, `strategy`, `base_interval_secs`, `max_interval_secs`), else `400`; omitted optional fields take the schema defaults. All effective settings (retry policy, lease `heartbeat_timeout_secs`, execution timeout) are resolved at create, stored and returned by `GET /tasks/{id}`; DESIGN.md:530 reads the lease from the task. Remaining: the doc edit.

### G-CRT-05 — Tenant-owned type inheriting a queue execution timeout above its cap: clamp or reject
- **Kind**: contradiction
- **Docs**: PRD.md:519 ("every declared value is capped by the trust limits"); PRD.md:1010 ("a queue outside them refuses new tasks; a create request outside them is rejected (`400`)"); DESIGN.md:715 (only "a request above it → 400"); DESIGN.md:2197, :2204 (86400 global vs 3600 tenant-owned); `schemas/instances/gts.cf.core.tasks.queue.v1~cf.core.tasks.default.v1.json` (`execution_timeout_secs: 86400`); queue schema `execution_timeout_secs` ("make the type non-creatable or the queue refuse new tasks")
- **Scenarios**: [tasks/create/1.03-positive-settings-resolution.md](tasks/create/1.03-positive-settings-resolution.md)
- **Assumed**: the inherited 86400 is clamped to 3600 and the create succeeds.
- **Question**: For a tenant-owned type placed in a platform queue whose effective execution timeout is within the global cap but above the tenant-owned cap (the shipped **default** queue), is the value clamped, or is the create refused? If refused, every tenant-owned type without its own `execution_timeout_secs` is unusable in the shipped **default** queue.
- **Decided** (gap review 2026-11-06): trust limits cap the effective value: an inherited or defaulted value above the cap is clamped; only a value the request itself sets above the cap is rejected (`400`). Remaining: the doc edit.

### G-CRT-06 — Tag limits: per-task count limit has no name, default or error; length unit and error unstated
- **Kind**: missing
- **Docs**: Proposals › Task mutability (accepted: "Tags. Set on `POST /tasks`, with a new limit on tags per task"; API changes "Add a limit on tags per task"); PRD.md:363 (keys up to 128, values up to 256 "characters"); DESIGN.md:1876-1877 (`VARCHAR(128)`, `VARCHAR(256)`); DESIGN.md:2057-2248 (no tag-count config key); the draft reference DDL (`migration.sql`, kept outside this branch) (`te_tags` `VARCHAR(128)` / `VARCHAR(256)`; the count is checked when tags are written)
- **Scenarios**: [tasks/create/1.10-negative-field-limits.md](tasks/create/1.10-negative-field-limits.md)
- **Assumed**: config `max_tags_per_task: 16` (name and value invented); over-length or over-count → `400` `invalid_argument`, field violation on `tags` / `tags.<key>`; lengths in Unicode characters.
- **Question**: Name and default of the per-task tag limit; error for too many tags and for over-long keys/values; characters vs bytes (`VARCHAR` length semantics differ between PostgreSQL and MySQL); are empty keys allowed?
- **Decided** (gap review 2026-11-06): config `max_tags_per_task`, default 16; key 1–128 and value 0–256 Unicode characters; `400 invalid_argument` with `TOO_MANY_TAGS` on `/tags`, `TOO_LONG` on `/tags/i/key` or `/tags/i/value`, `REQUIRED` for an empty key. Remaining: the doc edit.

### G-CRT-07 — Field-violation `reason` values and how an error "names the setting" are undefined
- **Kind**: wire
- **Docs**: PRD.md:1010 ("The error names the setting"); DESIGN.md:1204, :1210, :1213, :1228 (field violations mentioned, no `reason` values or field-path format); CONVENTIONS §6 (`field_violations[{field, description, reason}]`)
- **Scenarios**: tasks/create/1.09, 1.13, 1.15, 1.17, 1.18, 1.19, 1.20, 1.21, 1.22, 1.23, 1.24, 1.25, 1.26; tasks/idempotency/1.03, 1.06, 1.07; tasks/dedup/1.06, 1.11, 1.12; tasks/supersede/1.05
- **Assumed** (all **S**): `field` paths `input/<pointer>`, `timeouts.<name>`, `tags`, `tags.<key>`, `queue`, `type`, `priority`, `cancellable`, `idempotency_key`, `resource_refs`; `reason` values `SCHEMA_VIOLATION`, `PAYLOAD_TOO_LARGE`, `OUT_OF_RANGE`, `TRUST_LIMIT`, `PLATFORM_QUEUE`, `TYPE_NOT_ACCEPTED`, `DEAD_LETTER_QUEUE`, `UNKNOWN_QUEUE`, `TOO_LONG`, `TOO_MANY`, `REQUIRED`, `KEY_REUSED`, `INVALID_TYPE_POLICY`, `NOT_ALLOWED`; the violated config key (for example `trust_limits.tenant_owned.max_execution_timeout_secs`) appears in `description`.
- **Question**: Define the field-path syntax (JSON Pointer vs dotted), the set of `reason` codes for create validation, and where the setting name goes, so clients and tests can match on them.
- **Decided** (wire review 2026-11-04): the field-violation `reason` codes used in the scenarios (CONVENTIONS §6, including the former placeholders `TRUST_LIMIT`, `QUEUE_KIND`, `KEY_REUSED`, `INVALID_TYPE_POLICY`, `UNKNOWN_TYPE`, `FEATURE_NOT_AVAILABLE`) are the documented set; clients treat it as open. Remaining: the doc edit.

### G-CRT-08 — Create-path `403`s carry no reason, so they cannot be told apart
- **Kind**: missing
- **Docs**: DESIGN.md:1202 (no `expedite`), :1229 (dedup hit unreadable), :1231 (no `cancel` for supersession) — all `permission_denied` without `context.reason`; DESIGN.md:1214 (creator checks do get `CREATOR_AUTHORITY`)
- **Scenarios**: [tasks/create/1.06-guardrail-critical-urgency-needs-expedite.md](tasks/create/1.06-guardrail-critical-urgency-needs-expedite.md), [tasks/dedup/1.05-guardrail-dedup-hit-on-unreadable-task-403.md](tasks/dedup/1.05-guardrail-dedup-hit-on-unreadable-task-403.md), [tasks/supersede/1.06-negative-caller-lacks-cancel-on-old-task.md](tasks/supersede/1.06-negative-caller-lacks-cancel-on-old-task.md)
- **Assumed**: no `context.reason`; only status and category asserted.
- **Question**: Should these carry reasons (for example `EXPEDITE_REQUIRED`, `DUPLICATE_NOT_READABLE`, `SUPERSEDE_CANCEL_DENIED`), as creator checks do? A client cannot otherwise tell "drop `critical`" from "you may not create this type", and `DUPLICATE_NOT_READABLE` must be weighed against PRD.md:312 (no ID, but existence is disclosed under `tenant` scope anyway).
- **Decided** (gap review 2026-11-06): yes: `EXPEDITE_REQUIRED`, `DUPLICATE_NOT_READABLE`, `SUPERSEDE_CANCEL_DENIED`, and `ACTION_DENIED` for a plain create denial (full list in G-SEC-19). Remaining: the doc edit.

### G-CRT-09 — `Idempotency-Key` format, length and empty value are not defined
- **Kind**: missing
- **Docs**: PRD.md:278; DESIGN.md:718 (SDK `CreateTaskRequest.idempotency_key`; ToolKit primitive #5480); DESIGN.md:2573; DESIGN.md:293 domain model `Option<String> idempotency_key`
- **Scenarios**: [tasks/idempotency/1.03-negative-missing-or-malformed-idempotency-key.md](tasks/idempotency/1.03-negative-missing-or-malformed-idempotency-key.md)
- **Assumed**: an empty header counts as missing (`428`); no length or charset limit asserted.
- **Question**: Maximum length and allowed characters of the key; is an empty or whitespace value `428` or `400`; is the key case-sensitive; is it returned on the task resource?
- **Decided** (gap review 2026-11-06): `idempotency_key` is 1–255 printable ASCII characters without spaces (`!`–`~`), case-sensitive; absent → `428`; present but empty or malformed → `400 INVALID_FORMAT` on field `Idempotency-Key` (the wire carries it as a header; header and query fields are named as sent, JSON Pointers are for the body); not returned on the task resource. Remaining: the doc edit.

### G-CRT-10 — `resource_id` length: 256 in the table, 1024 in event schemas, no error stated
- **Kind**: contradiction
- **Docs**: DESIGN.md:1891 (`te_resource_refs.resource_id VARCHAR(256)`); `schemas/types/…task_created.v1~.schema.json` line 86 (`resource_id` `maxLength: 1024`, same in every task-scoped event schema); PRD.md:372 (count bounded, no length); the draft reference DDL (`migration.sql`, kept outside this branch) (keeps `te_resource_refs.resource_id VARCHAR(256)`)
- **Scenarios**: [tasks/create/1.05-positive-tags-and-resource-refs.md](tasks/create/1.05-positive-tags-and-resource-refs.md)
- **Assumed**: nothing about lengths above 256; the scenario uses a short ID.
- **Question**: What is the maximum `resource_id` length, and what does a create with a longer one get (`400` field violation on `resource_refs[i].resource_id`)? Align the table and the event schemas.
- **Decided** (gap review 2026-11-06): `resource_id` is at most 256 characters (the table's length); the event schemas change `maxLength` to 256; a longer value → `400 TOO_LONG` on `/resource_refs/i/resource_id`. Remaining: the doc edit.

### G-CRT-11 — "Queue as sent": naming the resolved queue on a retry is a different request
- **Kind**: unclear
- **Docs**: PRD.md:282 ("if the request differs in type, queue, input, or context"); DESIGN.md:718 and DESIGN.md:2575-2576 (`queue: request.queue or null`); DESIGN.md:1716
- **Scenarios**: [tasks/idempotency/1.04-negative-same-key-different-request.md](tasks/idempotency/1.04-negative-same-key-different-request.md)
- **Assumed**: DESIGN's literal rule: first request without `queue`, retry naming **default** → `400` on `idempotency_key`.
- **Question**: Is this intended? An SDK that fills in the resolved queue on retry would get `400`. Alternatives: fingerprint the resolved queue at create time (still immune to later moves), or document that clients must resend the request byte-for-byte in these fields.
- **Decided** (gap review 2026-11-06): `queue` is left out of the fingerprint hash; the key row stores the resolved queue at create; a retry without `queue`, or with the stored resolved queue, matches; any other queue → `400` on field `Idempotency-Key`. Type, input and context stay in the hash. Remaining: the doc edit and the idempotency scenario that expects `400` for the resolved queue.

### G-CRT-12 — Expired key and identity rows still block creates until cleanup runs
- **Kind**: contradiction
- **Docs**: PRD.md:283 ("After that, the same key creates a new task"); DESIGN.md:2586 (`INSERT te_dedup … ON CONFLICT DO NOTHING`; conflict → replay, no `expires_at` check); DESIGN.md:1719, :1729, :1731 (RetentionService deletes `expires_at < now`; cites "§3.2 step 4" but the step is 5 at DESIGN.md:859); DESIGN.md:2167 (`cleanup_time: "02:00"`, daily)
- **Scenarios**: [tasks/idempotency/1.02-positive-key-window-release.md](tasks/idempotency/1.02-positive-key-window-release.md), [tasks/dedup/1.02-positive-window-release-and-reuse.md](tasks/dedup/1.02-positive-window-release-and-reuse.md)
- **Assumed**: the PRD outcome: after `expires_at` the create returns `201` immediately.
- **Question**: Must the create path treat a row with `expires_at < now` as free (take it over in the same statement, for example `ON CONFLICT … DO UPDATE … WHERE expires_at < now`), or is a key/identity held until the next cleanup run (up to a day longer)? Fix the "step 4" reference.
- **Decided** (gap review 2026-11-06): on a key/identity conflict the create runs a guarded `DELETE … WHERE key = ? AND expires_at < now` and retries the `INSERT` once in the same transaction (portable); an expired row never blocks a create. Fix the "step 4" reference to step 5. Remaining: the doc edit.

### G-CRT-13 — Key window "capped at the task's retention" mixes two clocks
- **Kind**: unclear
- **Docs**: PRD.md:283 ("configurable up to the task's retention"); DESIGN.md:2152 and :2374 ("capped at the task's effective retention"); DESIGN.md:1729 (`expires_at = created_at + idempotency_key_window_days`); DESIGN.md:2162 (retention measured from `terminal_at`)
- **Scenarios**: [tasks/idempotency/1.02-positive-key-window-release.md](tasks/idempotency/1.02-positive-key-window-release.md)
- **Assumed**: with the 100-day global retention the 7-day window is not capped.
- **Question**: For a type with `retention_days` shorter than the window, is `expires_at = created_at + min(window, retention)` or `min(created_at + window, terminal_at + retention)`? Is the cap applied when config is loaded or per task?
- **Decided** (gap review 2026-11-06): `finalize()` sets `expires_at = min(created_at + window, terminal_at + retention)` per task (`NULL` while live); a row whose time has already passed is deleted at once. Remaining: the doc edit.

### G-CRT-14 — Sampled depth checks run before the duplicate check and cannot net out supersession
- **Kind**: contradiction
- **Docs**: PRD.md:454 ("duplicate checks run first, so a replay (`409`) never uses capacity and is never refused as full. Supersession … frees the replaced task's capacity before the new task is admitted"); DESIGN.md:723 (sampled ceilings checked **before** the transaction); DESIGN.md:2586 (dedup check **inside** it); DESIGN.md:2376 (order authorization → validation → dedup → depth); DESIGN.md:2612-2621, :2667 (net delta per backlog row); Proposals › Limits and counters (accepted: per-tenant depth from a sampled count, `te_tenant_queue_backlog` removed)
- **Scenarios**: [tasks/idempotency/1.06-guardrail-replay-not-refused-as-full.md](tasks/idempotency/1.06-guardrail-replay-not-refused-as-full.md), [tasks/supersede/1.07-guardrail-replacement-not-refused-for-predecessor-capacity.md](tasks/supersede/1.07-guardrail-replacement-not-refused-for-predecessor-capacity.md)
- **Assumed**: replay → `409` even when a sampled limit is full; a supersession at the per-tenant limit → `201`.
- **Question**: With every depth limit now a sampled pre-transaction check, where does the replay check go (an extra `te_dedup` read before the depth check?), and how does a supersession honour "frees capacity first" (skip the check when a live group head exists in the same queue and tenant? only when the old task is `queued`?).
- **Decided** (gap review 2026-11-06): (replay) as G-CLM-24: depth is checked after the dedup lookup. (supersession) inside the create transaction the engine reads the old group head's state; if it is counted in a depth dimension the new task also enters (same queue and tenant while `queued`; the deferred set while `scheduled`/`blocked`/`waiting`) and the supersession ends it in this transaction, that dimension is not checked for this create (net change 0). Otherwise (old task `claimed`/`running`, or another queue) the normal check applies. Remaining: the doc edit.

### G-CRT-15 — Non-creatable types: error shape, and `authz_scope` still in docs and schema
- **Kind**: contradiction
- **Docs**: Proposals › Dedup and supersession (accepted: drop `authz_scope`); PRD.md:301, :1493, :1573, :1608 (still describe or test `authz_scope`); DESIGN.md:715-716 (`invalid_argument`); DESIGN.md:1228 and :2564 (`internal`); DESIGN.md:2378 (2) ("`invalid_argument` on create"); ADR-0012:56, :65, :67; task-type schema `dedup.scope` enum (lines 64-72 still list `authz_scope`); PRD.md:310
- **Scenarios**: [tasks/dedup/1.06-negative-invalid-dedup-policy-or-authz-scope.md](tasks/dedup/1.06-negative-invalid-dedup-policy-or-authz-scope.md)
- **Assumed**: `400` `invalid_argument`, field violation on `type` with reason `INVALID_TYPE_POLICY`.
- **Question**: The error (status, field, reason) for any create of a non-creatable type (invalid `dedup`/`supersede`, `authority.mode = worker` on a tenant-owned type, trust-limit breach in a trait). Remove `authz_scope` from the trait enum (so registration fails) or keep it and make the type non-creatable?
- **Decided** (gap review 2026-11-06): `authz_scope` is removed from the trait enum, so such a type fails registration; any create of a type the engine finds non-creatable (invalid `dedup`/`supersede`, `authority.mode = worker` on a tenant-owned type, a trait outside the trust limits) → `400 invalid_argument` on `/type` with `INVALID_TYPE_POLICY`. Remaining: the doc edit.

### G-CRT-16 — A key-field value over 4 KiB at create has no stated outcome
- **Kind**: missing
- **Docs**: PRD.md:300 ("each value at most 4 KiB"); DESIGN.md:2187-2189 (`dedup.max_key_value_bytes: 4096`); DESIGN.md:2378 (2) (policy validated at type load, "field-count/size caps"); DESIGN.md:2566 (`jcs(input[p])` hashed)
- **Scenarios**: [tasks/dedup/1.07-negative-key-field-value-over-4kib.md](tasks/dedup/1.07-negative-key-field-value-over-4kib.md)
- **Assumed**: `400` `invalid_argument`, field violation on the input field.
- **Question**: Is an over-long value rejected per request (and with which error), and is the size measured on the raw value or its JCS form? Can a type declare a schema `maxLength` above 4 KiB for a key field?
- **Decided** (gap review 2026-11-06): the size is measured on the JCS form (what is hashed); a key-field value over `dedup.max_key_value_bytes` → `400 TOO_LONG` on that input pointer (`/input/...`); a type may declare a larger `maxLength`, the per-request check still applies. Remaining: the doc edit.

### G-CRT-17 — Supersession events: cancel request carries no `superseded_by`; which events are emitted
- **Kind**: missing
- **Docs**: PRD.md:328 ("`taskCancelled` carries `superseded_by`"); PRD.md:351 (cancellation publishes `taskCancelRequested` and `taskCancelled`); DESIGN.md:552 (graceful leased: sets `cancel_requested` + `superseded_by` now); DESIGN.md:2657 (outbox: `TaskCreated`, `TaskCancelled{superseded_by}` only); `schemas/types/…task_cancel_requested.v1~.schema.json` (`data` has `task_id`, `resource_refs`, `queue`, `mode`, `requested_by`; no `superseded_by`); heartbeat control response (DESIGN.md:1067) has no supersession field
- **Scenarios**: [tasks/supersede/1.01-positive-higher-order-supersedes-by-old-task-state.md](tasks/supersede/1.01-positive-higher-order-supersedes-by-old-task-state.md), [tasks/supersede/1.11-flow-graceful-supersede-running-task.md](tasks/supersede/1.11-flow-graceful-supersede-running-task.md)
- **Assumed**: immediate supersession records only `taskCancelled`; graceful supersession of a leased task records `taskCancelRequested` (`mode: graceful`) at create time and `taskCancelled` (`superseded`) at the end.
- **Question**: Is `taskCancelRequested` emitted for supersession (immediate and graceful)? Should it carry `superseded_by`? How does the worker learn on heartbeat that the cancel is a supersession (so it can stop without reporting partial output)?
- **Decided** (gap review 2026-11-06): as G-RTY-15: an immediate or forced supersession records only `taskCancelled { superseded_by }`; a graceful supersession of a leased task records `taskCancelRequested { mode: graceful, superseded_by }` (optional field added) and later `taskCancelled { superseded_by }`. The heartbeat control response carries `superseded_by` when the cancel request is a supersession. Remaining: the doc edit.

### G-CRT-18 — Graceful supersession: what a worker's `success` or retryable `error` does
- **Kind**: unclear
- **Docs**: PRD.md:328 ("ends as `superseded` even if a `graceful`-mode worker reports `cancelled`"); DESIGN.md:1510 ("stamps `result_code = superseded` regardless of the result a `graceful`-mode worker reports"); DESIGN.md:2937 (`superseded` overrides only `cancelled` / `forcibly_cancelled`); DESIGN.md:2378 (5) (holder's writes stay valid until it acknowledges)
- **Scenarios**: [tasks/supersede/1.02-positive-graceful-supersession-endings.md](tasks/supersede/1.02-positive-graceful-supersession-endings.md)
- **Assumed**: a `success` report after the supersession commits ends the old task `completed` / `success`; `superseded_by` stays set.
- **Question**: Does `success`/`warning` stand, or become `cancelled`/`superseded`? Does a retryable `error` schedule a retry of a task that is already superseded, or end it `superseded`? Is `superseded_by` cleared when the old task completes normally?
- **Decided** (gap review 2026-11-06): after a graceful supersession `success`/`warning` still end the old task `completed` (finished work stands) and `superseded_by` stays set; any `error` (retryable or not) and `cancelled` end it `superseded`, no retry. Remaining: the doc edit.

### G-CRT-19 — "Re-check the plan inside, retry from the top" is not specified
- **Kind**: missing
- **Docs**: Proposals › Dedup and supersession (accepted: "Authorize before `BEGIN`, re-check the plan inside, and retry from the top if it changed"); DESIGN.md:2601 and :2378 (4) (still `authorize(cancel)` inside the transaction); DESIGN.md:2640-2643 (guarded cancel, 0 rows → retry); DESIGN.md:2667 ("bounded retries, then `409` `aborted`")
- **Scenarios**: [tasks/supersede/1.08-guardrail-plan-rechecked-when-old-task-gets-leased.md](tasks/supersede/1.08-guardrail-plan-rechecked-when-old-task-gets-leased.md), [tasks/supersede/1.09-guardrail-concurrent-submissions-converge-on-highest.md](tasks/supersede/1.09-guardrail-concurrent-submissions-converge-on-highest.md)
- **Assumed**: a changed plan is redone with the task's new state; the PDP is never called while the transaction is open; a few retries suffice under two-way contention.
- **Question**: What counts as "the plan changed" (old task's state only, or also a different `latest_task_id` after a concurrent supersession)? Is `cancel` re-authorized for a different old task on retry? How many retries before `409` `aborted`, and does the client see `aborted` or a `Retry-After`?
- **Decided** (gap review 2026-11-06): the plan changed when the old task's state or the group's `latest_task_id` differs from what was planned; `cancel` is re-authorized (outside the transaction) only when the target old task changed; at most 3 attempts, then `409 aborted` `STATE_CHANGED` (G-SEC-22), no `Retry-After`. Remaining: the doc edit.

### G-CRT-21 — Result code of children cancelled by a supersession cascade
- **Kind**: missing
- **Docs**: PRD.md:328 ("cancellation reaches its children"); PRD.md:351; DESIGN.md:2951-2955 (`finalize` step 8 cascades children when `state = cancelled`); DESIGN.md:2972-2980 (cascade body, no result code); DESIGN.md:738
- **Scenarios**: [tasks/supersede/1.03-positive-supersession-cancels-children.md](tasks/supersede/1.03-positive-supersession-cancels-children.md)
- **Assumed**: each child ends `cancelled` / `cancelled`, `superseded_by` null, no `actor`.
- **Question**: Which result code does a cascaded child get when its parent was superseded (or forcibly cancelled): `cancelled`, `forcibly_cancelled` (inherit the source mode), or `superseded`? Does a leased child get a graceful cancel request or a forced cancel?
- **Decided** (gap review 2026-11-06): see G-WAI-23: a cascade cancels each unfinished child in the parent's cancel mode (a supersession's mode for a superseded parent); children end `cancelled` (`forcibly_cancelled` when forced), never `superseded`; no actor. Remaining: the doc edit.

### G-CRT-22 — A `run_after` in the past: stored or cleared
- **Kind**: missing
- **Docs**: PRD.md:258 ("`scheduled` if `run_after` is in the future, else `queued`"); PRD.md:504; DESIGN.md:523-524
- **Scenarios**: [tasks/create/1.02-positive-run-after-placement-and-lifetime-bound.md](tasks/create/1.02-positive-run-after-placement-and-lifetime-bound.md)
- **Assumed**: task `queued`, `run_after` echoed as sent.
- **Question**: Is a past `run_after` stored as sent, cleared, or replaced by the creation time? Is there an upper bound on how far in the future it may be (versus the lifetime timeout)?
- **Decided** (gap review 2026-11-06): a past `run_after` is stored as sent and the task is `queued`; a `run_after` later than `created_at` + the task's lifetime timeout → `400` on `/run_after`. Remaining: the doc edit.

### G-CRT-23 — Registry outage `503`: `Retry-After`, and stale-cache behaviour
- **Kind**: missing
- **Docs**: PRD.md:253 ("fail closed (`503`) if the GTS registry cannot be reached and the type is not known yet"); DESIGN.md:1219; DESIGN.md:2249-2261 (caching strategy)
- **Scenarios**: [tasks/create/1.08-negative-registry-outage-uncached-type-503.md](tasks/create/1.08-negative-registry-outage-uncached-type-503.md)
- **Assumed**: `503` `service_unavailable`; `Retry-After` not required.
- **Question**: Is `Retry-After` sent? How long may a cached type (and its validated dedup/supersede verdict) be used while the registry is down — indefinitely, or bounded by a TTL after which creates also fail with `503`?
- **Decided** (gap review 2026-11-06): `503` carries `Retry-After` (`unavailable_retry_after_secs`). Cached type metadata (schemas, trait and trust-limit verdicts, keyed by reference and revision) stays usable for the whole outage; per-tenant visibility entries keep their normal TTL, after which that tenant's creates get `503`. Remaining: the doc edit.

### G-CRT-24 — Order of create checks and which layer answers oversize bodies
- **Kind**: unclear
- **Docs**: DESIGN.md:2532-2538 (type → queue → authorize → expedite → trust limits → domain → schema and size, on one line); DESIGN.md:2573 (`428` inside the identity step); PRD.md:1276-1278 (body limit "before it is read"); DESIGN.md:263
- **Scenarios**: [tasks/create/1.11-negative-payload-and-body-size-limits.md](tasks/create/1.11-negative-payload-and-body-size-limits.md), [tasks/idempotency/1.03-negative-missing-or-malformed-idempotency-key.md](tasks/idempotency/1.03-negative-missing-or-malformed-idempotency-key.md)
- **Assumed**: an unknown type wins over a missing key; schema validation and payload size order unspecified (scenarios avoid depending on it); the 1 MB rule may be answered by the gateway.
- **Question**: Fix the order of create checks (so a request with several faults gets one predictable answer): in particular `428` vs `400 UNKNOWN_TASK_TYPE` vs `403`, and schema vs `413`. Is the 1 MB limit enforced by the API Gateway (whose problem document?) or by the engine?
- **Decided** (gap review 2026-11-06): check order: `413` body limit (engine body-limit layer, before parsing; platform problem document) → JSON syntax `400` → type (`400 UNKNOWN_TASK_TYPE` / `503`) → queue (`404` / `400 DEAD_LETTER_QUEUE`) → PDP (`403` / `503`) → expedite (`403`) → `428` missing key → trust limits and domain (`400`) → schema and 64 KiB payload (`400` / `413`) → dedup/idempotency (`409`) → depth (`429`) → insert. Within the payload step the 64 KiB size check runs before schema validation. A sent `Idempotency-Key` is always validated, also for a type with a derived identity. Remaining: the doc edit.

### G-CRT-25 — Content of the `409 already_exists` and `STALE_ORDER` bodies
- **Kind**: wire
- **Docs**: DESIGN.md:1191 (example uses `resource_type: gts.cf.core.tasks.task.v1~`), :1203 (`resource_name` = existing task ID), :1230 (`STALE_ORDER`, no other context); PRD.md:330
- **Scenarios**: [tasks/idempotency/1.01-positive-replay-returns-409-with-existing-id.md](tasks/idempotency/1.01-positive-replay-returns-409-with-existing-id.md), [tasks/supersede/1.04-negative-order-not-above-latest-does-not-supersede.md](tasks/supersede/1.04-negative-order-not-above-latest-does-not-supersede.md)
- **Assumed**: `context.resource_type` is the task base type; `STALE_ORDER` carries only `context.reason` and names no task.
- **Question**: Is `resource_type` the base type or the task's concrete type? Should `STALE_ORDER` name the group's latest task (it is readable by the caller under its scope) so the caller can read the newer outcome?
- **Decided** (gap review 2026-11-06): `409 already_exists`: `resource_type` = task base type, `resource_name` = the existing task ID; `STALE_ORDER` names no task. Remaining: the doc edit.

### G-CRT-26 — Is `priority` optional on create?
- **Kind**: unclear
- **Docs**: PRD.md:248 (lists "priority" without "optional", unlike its neighbours); `schemas/types/gts.cf.core.tasks.queue.v1~.schema.json` (`default_priority`); PRD.md:398 (range 0–1000)
- **Scenarios**: [tasks/create/1.01-positive-minimal-create-queued.md](tasks/create/1.01-positive-minimal-create-queued.md), [tasks/create/1.03-positive-settings-resolution.md](tasks/create/1.03-positive-settings-resolution.md)
- **Assumed**: optional; an absent priority takes the queue's `default_priority`.
- **Question**: Mark priority optional in PRD.md:248 and add it to the type-defaults resolution (request → queue `default_priority`; does a type trait sit between?).
- **Decided** (wire review 2026-11-04): `priority` is optional; default the queue's `default_priority`. Remaining: the doc edit.

### G-CRT-27 — Default of `cancellable`
- **Kind**: missing
- **Docs**: PRD.md:248 ("a cancellable flag"), PRD.md:346-353; DESIGN.md:294
- **Scenarios**: [tasks/create/1.01-positive-minimal-create-queued.md](tasks/create/1.01-positive-minimal-create-queued.md), [tasks/cancel/1.09-guardrail-cancellable-false-allows-only-forced.md](tasks/cancel/1.09-guardrail-cancellable-false-allows-only-forced.md)
- **Assumed**: `true` when absent.
- **Question**: State the default.
- **Decided** (wire review 2026-11-04): `cancellable` defaults to `true`. Remaining: the doc edit.

## CLM — Claim, ordering, limits

### G-CLM-01 — Cross-queue order on `main` is still the k-way merge
- **Kind**: contradiction
- **Docs**: PRD.md:653 ("best eligible task across all named queues"), PRD.md:657 (most urgent, then oldest, then lowest ID); DESIGN.md:107, :755, :2460, :2504-2506, :2527; ADR-0008:46, :71, :126, :139, :195; vs Proposals › Claiming and ordering › Claims across queues
- **Scenarios**: [worker/ordering/1.09-flow-critical-first-across-queues-oldest-first.md](worker/ordering/1.09-flow-critical-first-across-queues-oldest-first.md), [worker/ordering/1.10-flow-rotation-between-queues-for-normal-work.md](worker/ordering/1.10-flow-rotation-between-queues-for-normal-work.md), [worker/ordering/1.06-guardrail-priority-and-age-not-compared-across-queues.md](worker/ordering/1.06-guardrail-priority-and-age-not-compared-across-queues.md)
- **Assumed**: the accepted rule: `critical` first across the named queues (oldest first), then rotation between queues for `normal` work; age is no longer compared across queues.
- **Question**: apply the accepted doc edits (PRD.md:653, :657; DESIGN.md:107, :755, :2460, :2506, :2508; ADR-0008:46-47, :71, :126, :139). In particular ADR-0008:139 is a test obligation ("the claimed task is the first eligible task in the PRD §5.2 order across all requested queues") that now contradicts the decision.

### G-CLM-02 — "Critical first, oldest first" vs the queue's own tenant fairness
- **Kind**: unclear
- **Docs**: Proposals › Claiming and ordering ("probes each named queue for a queued `critical` task (one indexed read per queue, DESIGN.md:1525) and takes those oldest first"; "Order inside a queue … unchanged"); PRD.md:394-397, :410; DESIGN.md:1525, :2446
- **Scenarios**: [worker/ordering/1.09-flow-critical-first-across-queues-oldest-first.md](worker/ordering/1.09-flow-critical-first-across-queues-oldest-first.md)
- **Assumed**: with one `critical` task per queue, the oldest across queues wins. The scenario avoids several `critical` tasks of several tenants in one `round_robin` queue.
- **Question**: inside one `round_robin` queue, are `critical` tasks served by tenant turn (PRD.md:394-397: urgency, then tenant fairness) or oldest first? The cited probe reads `ix_te_task_state_queue_dispatch`, which has no tenant key and returns the oldest-highest-priority `critical` task of the queue regardless of tenant. And across queues, is the comparison by `created_at` only, or by `(created_at, task_id)`?
- **Decided** (gap review 2026-11-06): each named queue offers its next `critical` task in the queue's own order (`round_robin`: tenant turn via the probe on `ix_te_task_state_tenant_dispatch`; `fifo`: priority, then age); across queues the claim takes the oldest offer by `(created_at, task_id)`. The read on `ix_te_task_state_queue_dispatch` only tells whether a queue has any queued `critical` task. Remaining: the doc edit.

### G-CLM-03 — Batch claims that find fewer `critical` tasks than `max_tasks`
- **Kind**: unclear
- **Docs**: Proposals › Claiming and ordering ("Critical first … Otherwise it starts at the queue after …"); PRD.md:655
- **Scenarios**: [worker/ordering/1.09-flow-critical-first-across-queues-oldest-first.md](worker/ordering/1.09-flow-critical-first-across-queues-oldest-first.md)
- **Assumed**: nothing; the scenario uses `max_tasks: 1`.
- **Question**: does one claim take all `critical` tasks it can (several per queue, beyond the one-probe-per-queue read) and then fill the rest of the batch by rotation, or does "Otherwise" mean a claim either serves `critical` work or rotates? Does a claim that took only `critical` work advance the rotation cursor?
- **Decided** (gap review 2026-11-06): one claim takes every `critical` task it can within its scan budget, then fills the rest of the batch by rotation, in the same plan (one transaction, one reservation pass). The rotation start advances on every claim round, `critical`-only rounds included. Remaining: the doc edit.

### G-CLM-04 — Rotation order, first start queue and cursor advance are not specified
- **Kind**: missing
- **Docs**: Proposals › Claiming and ordering ("starts at the queue after the one the previous claim started at"; "cursor lives in node memory per waiter group"); DESIGN.md:2409 (waiter group uses `sorted(queues)`), :2513; PRD.md:657
- **Scenarios**: [worker/ordering/1.10-flow-rotation-between-queues-for-normal-work.md](worker/ordering/1.10-flow-rotation-between-queues-for-normal-work.md), [worker/ordering/1.06-guardrail-priority-and-age-not-compared-across-queues.md](worker/ordering/1.06-guardrail-priority-and-age-not-compared-across-queues.md), [worker/ordering/1.05-positive-queue-listing-order-does-not-matter.md](worker/ordering/1.05-positive-queue-listing-order-does-not-matter.md), [worker/ordering/1.11-flow-scan-budgets-per-kind-per-claim-round.md](worker/ordering/1.11-flow-scan-budgets-per-kind-per-claim-round.md)
- **Assumed**: queues rotate in the sorted order of the waiter group's queue set (so listing order is irrelevant); the first claim of a group may start anywhere; every claim, including one that returns `204`/`429`, advances the start queue. Tests assert alternation, never which queue comes first.
- **Question**: define "the queue after" (sorted ID? request order?), where a new waiter group starts, whether empty, refused and `critical`-only claims advance the cursor, and whether coalesced waiters share one cursor step.
- **Decided** (gap review 2026-11-06): rotation order is the waiter group's queue set sorted by queue GTS ID; a new waiter group starts at a random position; every claim round (each `try_claim_once`, including rounds that answer `204`/`429` or take only `critical` work) advances the start by one; coalesced waiters share one round and one step. Tests assert alternation, never the first queue. Remaining: the doc edit.

### G-CLM-05 — No fairness bound for queue rotation across nodes
- **Kind**: missing
- **Docs**: PRD.md:410 (tenant rotation: "no active tenant is skipped for more than one cycle per node"); DESIGN.md:2513; Proposals › Claiming and ordering (queue cursor in node memory)
- **Scenarios**: [worker/ordering/1.10-flow-rotation-between-queues-for-normal-work.md](worker/ordering/1.10-flow-rotation-between-queues-for-normal-work.md), [worker/ordering/1.08-guardrail-round-robin-across-nodes-is-approximate.md](worker/ordering/1.08-guardrail-round-robin-across-nodes-is-approximate.md)
- **Assumed**: per node, consecutive claims of one waiter group alternate queues; across nodes, tests assert only that every queue with ready work is served eventually.
- **Question**: state a bound for queues like PRD.md:410 does for tenants (e.g. "no named queue with ready work is skipped for more than one rotation per node and waiter group"), so tests have something to check across nodes and restarts.
- **Decided** (gap review 2026-11-06): per node and waiter group, each named queue is the start queue of one claim round in every n consecutive rounds (n = number of named queues), so a queue with a claimable task within the scan limits is served at least once per n rounds; across nodes and restarts only eventual service is promised. Remaining: the doc edit.

### G-CLM-06 — Scan budgets: DESIGN applies them per queue; long-poll retries undefined
- **Kind**: contradiction
- **Docs**: DESIGN.md:2132, :2134 (config comments: per claim), :2443, :2447, :2509 (applied per stream/queue); ADR-0008:71; DESIGN.md:2411-2418 (long-poll loop); vs Proposals › Claiming and ordering › Scan limits count per claim
- **Scenarios**: [worker/ordering/1.11-flow-scan-budgets-per-kind-per-claim-round.md](worker/ordering/1.11-flow-scan-budgets-per-kind-per-claim-round.md)
- **Assumed**: one `claim_candidate_budget` and one `claim_scan_rows_max` per claim, shared by all queues; a claim that uses its budget returns what it has (`429` if it skipped for capacity, with `timeout_secs: 0`), and the next claim starts at the next queue.
- **Question**: apply the accepted edits; then say (a) whether each long-poll retry inside one request (signal or watcher tick) is a "claim" with a fresh budget that starts at the next queue, (b) whether a `fifo` queue's row scan and a `round_robin` queue's probes share one budget or two, and (c) whether a claim that runs out of budget with `timeout > 0` parks or answers at once.
- **Decided** (gap review 2026-11-06): (a) each long-poll round (signal or watcher tick) is one `try_claim_once` with a fresh budget, starting at the next queue; (b) two budgets: `claim_scan_rows_max` shared by the `fifo` queues of a claim and `claim_candidate_budget` shared by its `round_robin` queues; a walk stops when its kind's budget is spent and the claim continues with queues of the other kind; (c) a round that runs out of budget and takes nothing is an empty round: with time left it parks until the next signal or tick, and the tenant cursors keep their place. Remaining: the doc edit.

### G-CLM-07 — DESIGN and ADR-0008 still forbid `SKIP LOCKED` and use the ascending-`task_id` retry rule
- **Kind**: contradiction
- **Docs**: DESIGN.md:2397 ("There is no `SKIP LOCKED`"), :2479, :2491-2493 (only a greater `task_id`), :2508, :2525; ADR-0008:47, :65, :74, :108, :123; DESIGN.md:2300 (`te_claim_lost_races_total`); vs Proposals › Claiming and ordering › SKIP LOCKED when taking tasks
- **Scenarios**: [worker/claim/1.08-guardrail-two-workers-never-claim-same-task.md](worker/claim/1.08-guardrail-two-workers-never-claim-same-task.md), [worker/ordering/1.07-guardrail-strict-fifo-not-promised-under-concurrency.md](worker/ordering/1.07-guardrail-strict-fifo-not-promised-under-concurrency.md)
- **Assumed**: no double claim and each racing claim gets a task no other claim holds; nothing about lost-race metrics or which claimer wins.
- **Question**: apply the accepted edits, and redefine `te_claim_lost_races_total`: with `SKIP LOCKED` a taken row is skipped, not lost by a guarded update. Is it still counted, and under which name?
- **Decided** (gap review 2026-11-06): keep `te_claim_lost_races_total{queue}`, redefined as planned picks not taken because another transaction holds the row (skipped by `SKIP LOCKED`) or it is no longer `queued` (guard miss, the only case on SQLite). Remaining: the doc edit.

### G-CLM-08 — What ordering guarantee remains after `SKIP LOCKED`
- **Kind**: unclear
- **Docs**: DESIGN.md:2506 ("The order is exact among claimable rows"); ADR-0008:46, :126; PRD.md:653; Proposals › Claiming and ordering (Unchanged: "Strict one-at-a-time FIFO is not offered"; Gives up: "best effort")
- **Scenarios**: [worker/ordering/1.07-guardrail-strict-fifo-not-promised-under-concurrency.md](worker/ordering/1.07-guardrail-strict-fifo-not-promised-under-concurrency.md)
- **Assumed**: with one sequential claimer and no retries, the documented order holds exactly; under concurrency nothing about order is asserted.
- **Question**: write the guarantee down: is order exact for a single uncontended claimer, and "best effort" only under contention? Tests need this line to know which order assertions are legitimate.
- **Decided** (gap review 2026-11-06): inside one queue a claim returns tasks in the queue's documented order among the tasks it may take (scope, blacklist, capacity, including scopes the node saw full in the last `claim_saturation_ttl_ms`), as long as no other transaction is changing that queue's tasks; under concurrent claims order is best effort (a task may go ahead of a better one only while another transaction holds the better one); across queues: `critical` first, then rotation (G-CLM-02 to G-CLM-04). Tests assert exact order only with one sequential claimer, no limit reached and no concurrent writes. Remaining: the doc edit.

### G-CLM-09 — `Retry-After` values are not specified
- **Kind**: missing
- **Docs**: PRD.md:429, :451; DESIGN.md:1226-1227, :2180 (`retry_after_secs: 30 # base Retry-After on QUEUE_DEPTH_LIMIT (jittered)`), :2419; nothing for `CONCURRENCY_LIMIT`
- **Scenarios**: [limits/concurrency/1.05-negative-nothing-assignable-returns-429.md](limits/concurrency/1.05-negative-nothing-assignable-returns-429.md), [limits/depth/1.02-negative-queue-depth-full-by-dimension.md](limits/depth/1.02-negative-queue-depth-full-by-dimension.md)
- **Assumed**: `Retry-After` is present and a positive integer of seconds; `30` shown for depth and `1` for concurrency, both illustrative.
- **Question**: give the `CONCURRENCY_LIMIT` value (fixed? the saturation TTL? the poll interval?) and the jitter range of the depth value (only shorter, like retry jitter, or ±?). Should a long-poll `429` use a shorter value since it already waited?
- **Decided** (gap review 2026-11-06): see G-SEC-03: `CONCURRENCY_LIMIT` = poll interval rounded up (1 s); depth = `retry_after_secs` uniform in [base/2, base]; a long-poll `429` uses the same values. Remaining: the doc edit.

### G-CLM-10 — `429` context: canonical `resource_exhausted` has no `reason`; dimension field not named
- **Kind**: wire
- **Docs**: DESIGN.md:1226-1227 (`context.reason = "CONCURRENCY_LIMIT"` / `"QUEUE_DEPTH_LIMIT"`, "dimension `queue`, `tenant`, `domain_global`, or `deferred`"); PRD.md:452, :455; docs/arch/errors/categories/08-resource-exhausted.md:12-30, :64, :83 (context = `resource_type`, `resource_name`, required `violations[{subject, description}]`, `additionalProperties: false`)
- **Scenarios**: [limits/concurrency/1.05-negative-nothing-assignable-returns-429.md](limits/concurrency/1.05-negative-nothing-assignable-returns-429.md), [worker/ordering/1.11-flow-scan-budgets-per-kind-per-claim-round.md](worker/ordering/1.11-flow-scan-budgets-per-kind-per-claim-round.md), [limits/depth/1.02-negative-queue-depth-full-by-dimension.md](limits/depth/1.02-negative-queue-depth-full-by-dimension.md), [limits/depth/1.03-negative-deferred-limit-refuses-every-deferral.md](limits/depth/1.03-negative-deferred-limit-refuses-every-deferral.md) (and 1.06-1.09), [dead-letter/1.04-negative-reenqueue-refusals.md](dead-letter/1.04-negative-reenqueue-refusals.md)
- **Assumed** (new **S** fields): `context.reason` as DESIGN says; the depth dimension in `context.violations[0].subject` (`tenant` | `queue` | `domain_global` | `deferred`); `resource_type` = queue base type with `resource_name` = the queue for `tenant`/`queue`/`domain_global`, and the task base type without `resource_name` for `deferred`; for `CONCURRENCY_LIMIT` one violation with `subject: "concurrency"` and no dimension.
- **Question**: the platform schema rejects `reason` in this category. Either the errors schema gains `reason` for `resource_exhausted`, or the Task Engine moves its codes into `violations[].subject`. Name the field that carries the dimension, say whether `CONCURRENCY_LIMIT` names a dimension, and what `resource_type`/`resource_name` a claim or a deferred refusal carries.
- **Decided** (gap review 2026-11-06): see G-WIRE-14: the quota `violations[].subject` carries code and dimension (`queue_depth.<dim>`, `concurrency`, `event_waits.<dim>`); `resource_type`/`resource_name` = the queue for `queue`/`tenant`/`domain_global`/`concurrency`, the task base type without a name for `deferred`. A concurrency `429` for a claim naming several queues carries the queue base type as `resource_type` and no `resource_name`. Remaining: the doc edit and the scenarios.

### G-CLM-11 — Per-tenant depth: PRD and DESIGN still require an exact locked counter
- **Kind**: contradiction
- **Docs**: PRD.md:447 ("MUST never be passed at admission"), :1492; DESIGN.md:723, :1602, :1647-1677 (esp. :1669 "The per-tenant limits … stay exact"), :2471, :2476, :2488 (claim decrements the backlog row), :2606-2621; ADR-0008:73, :102; vs Proposals › Limits and counters › Per-tenant depth from periodic counts
- **Scenarios**: [limits/depth/1.02-negative-queue-depth-full-by-dimension.md](limits/depth/1.02-negative-queue-depth-full-by-dimension.md), [limits/depth/1.06-flow-tenant-depth-overshoot-bounded-by-one-sample.md](limits/depth/1.06-flow-tenant-depth-overshoot-bounded-by-one-sample.md)
- **Assumed**: admission is refused while the latest per-tenant sample is at or above the limit; a burst inside one interval may pass it; tests never assert that a create inside the interval is refused.
- **Question**: apply the accepted edits (PRD.md:447, :1492; DESIGN.md:1597-1611, :1627-1677, :2606-2621; ADR-0008:73, :102), including removing the backlog step from the claim algorithm (DESIGN.md:2471, :2476, :2488, :2495-2496).

### G-CLM-13 — Tests cannot see when a depth sample was taken
- **Kind**: missing
- **Docs**: DESIGN.md:1669, :2181 (`sample_secs`); PRD.md:457 (depth shown by Queue Introspection); CONVENTIONS §5.9 (`sampled_at`, S)
- **Scenarios**: [limits/depth/1.02-negative-queue-depth-full-by-dimension.md](limits/depth/1.02-negative-queue-depth-full-by-dimension.md), [limits/depth/1.06-flow-tenant-depth-overshoot-bounded-by-one-sample.md](limits/depth/1.06-flow-tenant-depth-overshoot-bounded-by-one-sample.md)
- **Assumed**: tests wait `sample_secs` plus a margin (and the leader's scheduling) after setup creates.
- **Question**: expose the sample time (per queue and, to its own tenant, per tenant) or a test hook, so tests wait on a condition instead of sleeping. Is the per-tenant sample visible to the tenant at all?
- **Decided** (gap review 2026-11-06): the maintenance leader takes all depth samples (queue, domain, per tenant) and the queue counts in one pass; the queue resource's `sampled_at` is that pass's time, so tests wait until `sampled_at` is later than their setup. Per-tenant samples are not shown to tenants (`429` hides usage). No test hook. Remaining: the doc edit.

### G-CLM-14 — Claim naming a queue outside the worker's policy: `403` or filtered?
- **Kind**: unclear
- **Docs**: PRD.md:943, :1531; DESIGN.md:1009 (claim sends the base type, no queue), :2407; ADR-0016:72
- **Scenarios**: [worker/claim/1.09-guardrail-queue-policy-limits-what-worker-receives.md](worker/claim/1.09-guardrail-queue-policy-limits-what-worker-receives.md)
- **Assumed**: silently filtered: the PDP `queue_id` constraint applies in SQL, so the claim ends `204` after its timeout.
- **Question**: confirm. A `403` (or a `400` naming the queue) would tell a misconfigured worker why it never gets work; silent filtering avoids a probe of the policy.
- **Decided** (gap review 2026-11-06): the engine checks the compiled scope in memory: a named queue is provably excluded when every OR'd constraint has a `queue_id` filter that does not contain it; any provably excluded queue → `403 permission_denied` naming that queue, before a session opens. Everything else is filtered in SQL. Conservative: a constraint without a `queue_id` filter, or with one it cannot evaluate, counts as "may match". Remaining: the doc edit and [worker/claim/1.09](worker/claim/1.09-guardrail-queue-policy-limits-what-worker-receives.md) (now `403`).

### G-CLM-15 — Claim naming an unregistered queue, or no queue
- **Kind**: missing
- **Docs**: PRD.md:995 ("claims MUST name registered queues"); DESIGN.md:2404-2433 (no check listed)
- **Scenarios**: [worker/claim/1.07-negative-claim-request-validation.md](worker/claim/1.07-negative-claim-request-validation.md), [worker/claim/1.11-negative-claim-naming-unknown-queue-not-found.md](worker/claim/1.11-negative-claim-naming-unknown-queue-not-found.md)
- **Assumed**: as decided below.
- **Question**: name the status and reason for an unknown queue and for an empty or missing `queues` array.
- **Decided** (wire review 2026-11-04): a claim naming an unregistered queue → `404 not_found` (`resource_type` = queue base type, `resource_name` = the ID), at once, no session opened. An empty or missing `queues` array → `400 REQUIRED` on `/queues` (gap review 2026-11-06, G-CLM-18). Remaining: the doc edit.

### G-CLM-16 — Session opened by a claim that answers `204` or `429`
- **Kind**: missing
- **Docs**: DESIGN.md:2408 (session opened before the claim loop), :2413, :2419-2420 (no body on `204`; problem body on `429`); ADR-0017:55, :71
- **Scenarios**: [worker/claim/1.03-positive-empty-claim-waits-then-204.md](worker/claim/1.03-positive-empty-claim-waits-then-204.md)
- **Assumed**: nothing about sessions after an empty claim.
- **Question**: is a session row created when nothing is claimed? If so, its ID is never returned, and every idle worker without a session creates an orphan row per poll. Should the session open only when a task is claimed (inside the claim transaction), or should `204`/`429` carry `session_id`?
- **Decided** (gap review 2026-11-06): a session is opened or renewed only inside a claim transaction that takes a task (renewed last, as in the pseudocode); a `204`/`429` touches no session and returns none. Remaining: the doc edit.

### G-CLM-17 — Reusing a session with a different `worker_id`
- **Kind**: missing
- **Docs**: ADR-0017:54-55; DESIGN.md:1918-1920, :2408
- **Scenarios**: [worker/claim/1.06-positive-claim-reuses-live-own-session-else-opens-new.md](worker/claim/1.06-positive-claim-reuses-live-own-session-else-opens-new.md)
- **Assumed**: tests reuse the `worker_id` that opened the session.
- **Question**: a session stores one `worker_id`; is a claim with a valid `session_id` of the same subject but another `worker_id` refused, given a new session, or attached (recording the new `worker_id` on the task but not the session)?
- **Decided** (gap review 2026-11-06): a `session_id` whose stored `worker_id` differs from the claim's counts like an unknown or foreign one: a new session opens. Remaining: the doc edit.

### G-CLM-18 — Claim validation: reason codes and validation order
- **Kind**: wire
- **Docs**: DESIGN.md:2405, :2432-2433, :1223; PRD.md:655, :663
- **Scenarios**: [worker/claim/1.07-negative-claim-request-validation.md](worker/claim/1.07-negative-claim-request-validation.md), 1.07-negative-invalid-worker-id.md, 1.08-negative-max-tasks-above-batch-cap.md, 1.09-negative-claim-from-dead-letter-queue.md, 1.10-negative-too-many-queues.md (also 1.11, 1.21)
- **Assumed** (new **S** values): field-violation `reason` codes `REQUIRED`, `INVALID_FORMAT`, `OUT_OF_RANGE`, `DEAD_LETTER_QUEUE`, `TOO_MANY_QUEUES`, `CROSS_DOMAIN`, `UNKNOWN_QUEUE`; field paths `worker_id`, `max_tasks`, `queues`, `queues[i]`; all checks run before the session is opened and before any wait.
- **Question**: name the codes and paths. DESIGN.md:2432 checks `kind = dead_letter` inside `try_claim_once`, after `open_or_reuse_session` (DESIGN.md:2408); should it move to request validation? Does `claim_max_queues` count the list before or after de-duplication (DESIGN.md:2433)?
- **Decided** (gap review 2026-11-06): JSON Pointer paths `/worker_id`, `/max_tasks`, `/queues`, `/queues/i`; reasons `REQUIRED` (incl. an empty or missing `queues`), `INVALID_FORMAT`, `OUT_OF_RANGE`, `TOO_MANY_QUEUES` (distinct queues, after de-duplication), `DEAD_LETTER_QUEUE`, `CROSS_DOMAIN`; unknown queue → `404` (G-CLM-15). Order: syntax and fields → queue registry (`404` / DLQ / domain, in request validation from cached registry data) → PDP and `QUEUE_NOT_PERMITTED` (`403`/`503`) → claim loop; no refusal touches a session. Remaining: the doc edit.

### G-CLM-19 — `timeout` default and range
- **Kind**: missing
- **Docs**: DESIGN.md:1064, :2404, :2414; PRD.md:659
- **Scenarios**: [worker/claim/1.03-positive-empty-claim-waits-then-204.md](worker/claim/1.03-positive-empty-claim-waits-then-204.md)
- **Assumed**: `timeout_secs: 0` means one attempt, no wait; tests always send `timeout`.
- **Question**: what is the default when `timeout` is omitted (0? the cap?), and is a negative or non-integer value `400`?
- **Decided** (wire review 2026-11-04): the field is `timeout_secs`, default `0` (one attempt, no wait), clamped to `claim_max_wait_secs`; negative → `400`. Remaining: the doc edit.

### G-CLM-20 — `heartbeat_interval_secs` formula
- **Kind**: unclear
- **Docs**: PRD.md:659 ("one third of the lease, at least 1 second"), :1518; DESIGN.md:1081 (`max(1, floor(session lease_secs / 3))`), :2192 (`min_lease_secs: 10`); ADR-0013:58
- **Scenarios**: [worker/claim/1.01-positive-claim-single-task.md](worker/claim/1.01-positive-claim-single-task.md), [worker/sessions/1.02-positive-short-lease-task-lowers-session-lease.md](worker/sessions/1.02-positive-short-lease-task-lowers-session-lease.md)
- **Assumed**: DESIGN's formula on the session lease (30 → 10, 12 → 4).
- **Question**: make PRD say "session lease" and "rounded down"; note that with `min_lease_secs` 10 the 1-second floor is unreachable (keep it only if `min_lease_secs` may be lowered).

### G-CLM-21 — "Most specific pattern wins" is undefined for concurrency overrides
- **Kind**: unclear
- **Docs**: PRD.md:475; DESIGN.md:2182 (stated for `queue_depth.overrides` only), :2212-2215 (no precedence comment); PRD.md:1176 and DESIGN.md:2223 (task domains: first match wins); `schemas/types/gts.cf.core.tasks.queue.v1~.schema.json` description
- **Scenarios**: [limits/concurrency/1.04-positive-override-most-specific-pattern-wins.md](limits/concurrency/1.04-positive-override-most-specific-pattern-wins.md)
- **Assumed**: an exact ID beats a trailing-wildcard pattern; an override beats the queue registration.
- **Question**: define specificity (fewest wildcards? longest literal prefix? exact ID > pattern?), the tie rule, and how a queue override and a type override of the same task combine (both apply, as both dimensions are checked?).
- **Decided** (gap review 2026-11-06): an exact ID beats any pattern; among patterns the longest literal prefix before the first wildcard wins; two patterns of equal specificity matching the same queue are rejected when the config is loaded. A runtime override beats a static one for the same pattern (G-OPS-12). Queue and type overrides are different dimensions and both apply. An equal-specificity overlap is refused `400 invalid_argument` `NOT_ALLOWED` on the later pattern when saved through `POST /admin/config`, and fails startup when it is in the static file. Remaining: the doc edit.

### G-CLM-23 — Wake-up bound after a slot is freed on another node
- **Kind**: unclear
- **Docs**: PRD.md:659 (bound stated only for "a task that becomes ready"); DESIGN.md:952 (`slotReleased` is local), :958, :2136 (`claim_saturation_ttl_ms`, cleared early only by a local `slotReleased`), :2138
- **Scenarios**: [limits/concurrency/1.03-positive-slot-freed-on-every-exit.md](limits/concurrency/1.03-positive-slot-freed-on-every-exit.md)
- **Assumed**: within one jittered poll interval plus the saturation TTL (≈ 1.45 s at defaults).
- **Question**: state the bound for capacity-blocked waiters across nodes, and whether a watcher tick bypasses the node's saturation cache.
- **Decided** (gap review 2026-11-06): a watcher tick does not bypass the saturation cache; bound = one jittered poll interval + `claim_saturation_ttl_ms` (≈ 1.45 s at defaults). Config validation rejects `claim_saturation_ttl_ms ≥ 0.8 × claim_poll_interval_ms`, so an entry made at one tick has expired by the next. Remaining: the doc edit.

### G-CLM-24 — Sampled ceilings are checked before the dedup step: a replay can get `429`
- **Kind**: contradiction
- **Docs**: PRD.md:454 ("a replay (`409`) never uses capacity and is never refused as full"); DESIGN.md:723, :2606-2608 (sampled ceilings "checked before BEGIN") vs :2582-2595 (dedup inside the transaction); Proposals › Limits and counters (per-tenant check now sampled too)
- **Scenarios**: [tasks/idempotency/1.06-guardrail-replay-not-refused-as-full.md](tasks/idempotency/1.06-guardrail-replay-not-refused-as-full.md)
- **Assumed**: the PRD rule: a replay gets `409` with the existing task ID even when the queue is full.
- **Question**: move the sampled checks after the dedup lookup (or re-check: on a sampled refusal, look up the key first), and say so in DESIGN §4.6.
- **Decided** (gap review 2026-11-06): the sampled depth check moves after the dedup/idempotency lookup, inside the create transaction: authorize → validate → dedup lookup → sampled depth check → insert. A replay answers `409` before depth is looked at (see also G-CRT-14). Remaining: the doc edit.

### G-CLM-25 — Bulk move and bulk retry have no outcome for a depth refusal
- **Kind**: missing
- **Docs**: PRD.md:451 (bulk move and re-enqueue are admissions), :1160; DESIGN.md:756, :1119 (`outcome ∈ {succeeded, unauthorized, not_found, invalid_state}`)
- **Scenarios**: [dead-letter/1.04-negative-reenqueue-refusals.md](dead-letter/1.04-negative-reenqueue-refusals.md) (single re-enqueue only; bulk owned by OPS)
- **Assumed**: the single-task re-enqueue answers `429 QUEUE_DEPTH_LIMIT`.
- **Question**: add a per-task outcome (e.g. `limit_exceeded`, without usage figures) for tasks refused by depth admission in `bulk/move` and `bulk/retry`.
- **Decided** (gap review 2026-11-06): new per-task outcome `limit_exceeded` (no usage figures) in `bulk/move` and `bulk/retry`; the other tasks proceed; overall `200`. Remaining: the doc edit.

### G-CLM-26 — Create with an initial wait group: deferred admission not described
- **Kind**: missing
- **Docs**: DESIGN.md:556-558 (admission list), :2606-2610 (create dimensions: `queued`, `scheduled`/`blocked` only); Proposals › Task mutability › Set only in the create request (accepted: optional initial wait group)
- **Scenarios**: [limits/depth/1.03-negative-deferred-limit-refuses-every-deferral.md](limits/depth/1.03-negative-deferred-limit-refuses-every-deferral.md)
- **Assumed**: a create that starts in `waiting` is an admission into the deferred set and gets `429` `deferred` when it is full.
- **Question**: confirm and add `waiting` to the create's depth dimensions (DESIGN.md:2609).
- **Decided** (gap review 2026-11-06): confirmed: a create that starts `waiting` is a deferred admission (`429 QUEUE_DEPTH_LIMIT`, dimension `deferred`); add `waiting` to the create's depth dimensions. Remaining: the doc edit.

### G-CLM-27 — Priority aging is Open; ordering tests assume none
- **Kind**: proposal-open
- **Docs**: PRD.md:535-539; DESIGN.md:2514; Proposals › Claiming and ordering › Open "Drop priority aging …"; the draft reference DDL (`migration.sql`, kept outside this branch) (priority aging can only use `created_at` as queued age; no column records entry into `queued`)
- **Scenarios**: [worker/ordering/1.03-positive-priority-orders-only-within-tenant.md](worker/ordering/1.03-positive-priority-orders-only-within-tenant.md), [worker/ordering/1.04-positive-fifo-queue-orders-by-priority-age-id.md](worker/ordering/1.04-positive-fifo-queue-orders-by-priority-age-id.md)
- **Assumed**: no aging changes a priority within the few seconds a test runs.
- **Question**: decide the item; if aging stays, give its threshold so tests can stay below it, and add a scenario for it.
- **Decided** (gap review 2026-11-06): drop priority aging from PRD and DESIGN; reopen only when an adopter mixes priorities in one overloaded queue. Ordering tests assume no aging. Remaining: the doc edit.

### G-CLM-29 — PRD's `fifo` definition omits urgency
- **Kind**: unclear
- **Docs**: PRD.md:411 ("all tenants' tasks in one order by priority, then age"); PRD.md:394-396; DESIGN.md:1525, :2441 (`ORDER BY (urgency_code, priority, created_at, task_id)`)
- **Scenarios**: [worker/ordering/1.04-positive-fifo-queue-orders-by-priority-age-id.md](worker/ordering/1.04-positive-fifo-queue-orders-by-priority-age-id.md)
- **Assumed**: urgency, then priority, then age, then task ID, as DESIGN says.
- **Question**: add urgency and the task-ID tie-break to PRD.md:411.

### G-CLM-30 — Dimension names collide between concurrency and depth counters
- **Kind**: unclear
- **Docs**: DESIGN.md:556, :723 (queue-wide depth called `queue_global`), :1568 (`queue_global` concurrency), :1582 (`queue_tenant` concurrency), :1656 (`queue_tenant` backlog), :2281 and PRD.md:455 (depth dimensions `queue`/`tenant`/`domain_global`/`deferred`); CONVENTIONS §7 counter vocabulary
- **Scenarios**: [limits/concurrency/1.01-positive-full-dimension-holds-back-only-its-tasks.md](limits/concurrency/1.01-positive-full-dimension-holds-back-only-its-tasks.md) (and every counter predicate in this area)
- **Assumed**: in scenarios `queue_global` / `queue_tenant` always mean the concurrency counters; depth dimensions use the `429` names.
- **Question**: use distinct internal names for depth (`queue_depth`, `tenant_queue_depth`, …), or state that the `dimension_code` namespaces are per table.
- **Decided** (gap review 2026-11-06): depth dimensions always use the `429` names (`queue`, `tenant`, `domain_global`, `deferred`) in docs, metrics and runtime config; `queue_global` / `queue_tenant` / `type_global` / `type_tenant` name concurrency counters only. Remaining: the doc edit.

## EXE — Sessions, fencing, execution, release, entries

### G-EXE-01 — Heartbeat for an unknown, expired or foreign session: what is the answer?
- **Kind**: unclear
- **Docs**: DESIGN.md:759, :1305-1307; ADR-0017:55, :57, :80-81
- **Scenarios**: [worker/sessions/1.04-guardrail-heartbeat-lease-lost-cases.md](worker/sessions/1.04-guardrail-heartbeat-lease-lost-cases.md)
- **Assumed**: `200` with every listed lease `lease_lost`; the session is neither renewed nor revived; the answer does not tell "unknown", "expired" and "another subject's" sessions apart.
- **Question**: when the session update matches 0 rows, does the engine still run the per-lease read (and answer `200`), or answer the whole request with an error (`409` `LEASE_LOST`, `404`)? State that the answer must be the same for the three cases.
- **Decided** (gap review 2026-11-06): `200` with every listed lease `lease_lost`, no per-lease read, the same body for an unknown, expired or foreign session; the session is not revived. Remaining: the doc edit.

### G-EXE-02 — Session marked expired but its tasks not yet requeued: the fence does not check the session
- **Kind**: contradiction
- **Docs**: ADR-0017:59, :80 ("after expiry, every call from the old worker returns `LEASE_LOST`"); DESIGN.md:734 (fence: token, holder, state only), :759 (lease read: token, holder, `session_id`, state — not session status), :2750-2760 (expiry step 1, then step 2 in batches of `session_requeue_batch`)
- **Scenarios**: [worker/fencing/1.04-negative-failed-fence-returns-lease-lost.md](worker/fencing/1.04-negative-failed-fence-returns-lease-lost.md), [worker/sessions/1.04-guardrail-heartbeat-lease-lost-cases.md](worker/sessions/1.04-guardrail-heartbeat-lease-lost-cases.md)
- **Assumed**: any worker call or heartbeat for a task of an `expired` session returns `LEASE_LOST`, even before step 2 has requeued the task.
- **Question**: add `session.status = active` to the worker fence and to the heartbeat lease read (one join or one extra predicate), or accept that the old worker can still complete in the window and document it (then `liveness_failure` finds the task gone, DESIGN.md:2759).
- **Decided** (gap review 2026-11-06): accept the window: the fence stays token, holder and state, because the lease token alone prevents a race (no one else can hold the task before step 2 requeues it). A late write in the window succeeds and the requeue skips the ended task; a heartbeat in the window answers `lease_lost` (G-EXE-01). ADR-0017 Confirmation becomes "every call from the old worker returns `LEASE_LOST` once its tasks are requeued". Remaining: the doc edit and [worker/fencing/1.04](worker/fencing/1.04-negative-failed-fence-returns-lease-lost.md) / [worker/sessions/1.04](worker/sessions/1.04-guardrail-heartbeat-lease-lost-cases.md) (no write assertion inside the window; the heartbeat there answers `lease_lost`).

### G-EXE-03 — Session lease never rises after a short-lease task leaves
- **Kind**: unclear
- **Docs**: DESIGN.md:530, :1921; ADR-0017:56
- **Scenarios**: [worker/sessions/1.02-positive-short-lease-task-lowers-session-lease.md](worker/sessions/1.02-positive-short-lease-task-lowers-session-lease.md)
- **Assumed**: `main`: the claim lowers `lease_secs`, it never rises, and every later heartbeat returns the shorter interval.
- **Question**: Is it intended that a session's lease never rises again after a short-lease task leaves, so every later task of that session runs on the short lease? If not, fix the length when the session opens, or let it rise again.
- **Decided** (gap review 2026-11-06): the engine keeps "never rises"; the SDK starts a new session (claims without `session_id`) when it holds no leases and its session lease is below the default. The empty session expires silently. Remaining: the doc edit.

### G-EXE-04 — Batch heartbeat above `heartbeat_batch_max`
- **Kind**: missing
- **Docs**: DESIGN.md:1067 (cap, default 500); configuration block DESIGN.md:2061-2247 (setting not listed)
- **Scenarios**: [worker/sessions/1.03-negative-batch-heartbeat-over-max.md](worker/sessions/1.03-negative-batch-heartbeat-over-max.md)
- **Assumed**: `400` `invalid_argument` with a field violation on `leases`; the session is not renewed.
- **Question**: confirm the outcome (reject vs renew and check the first N), and add `heartbeat_batch_max` to the configuration block with its bounds.
- **Decided** (gap review 2026-11-06): above `heartbeat_batch_max` (default 500, range 1–1000, added to the config block) → `400 TOO_MANY` on `/leases`; the session is not renewed. Remaining: the doc edit.

### G-EXE-05 — Which event records a liveness requeue?
- **Kind**: missing
- **Docs**: DESIGN.md:539, :2799 ("no taskTimedOut here"); PRD.md:868 (history includes retries and timeouts), :879 (event list); `schemas/types/…task_retrying.v1~` (`error` nullable), `…task_queued.v1~` (`reason` only `released` / `reenqueued`)
- **Scenarios**: [worker/sessions/1.10-flow-session-expiry-requeues-tasks.md](worker/sessions/1.10-flow-session-expiry-requeues-tasks.md)
- **Assumed**: `taskRetrying` with `error: null` and `run_after`.
- **Question**: name the history/broker event for a lease-expiry or assignment-timeout requeue (`taskRetrying` with a cause field, a new `reason` on `taskQueued`, or a new event), so an audit can tell a lost worker from a reported retry.
- **Decided** (gap review 2026-11-06): `taskRetrying` with a new required `cause` (`reported` | `liveness`); for `liveness` also `timeout_type` (`heartbeat` | `assignment`, as G-RTY-02) and the incremented counter; `run_after` as today. Remaining: the doc edit, the `task_retrying.v1` schema and [worker/sessions/1.10](worker/sessions/1.10-flow-session-expiry-requeues-tasks.md).

### G-EXE-06 — PDP unreachable on a worker write: `403` or `503`?
- **Kind**: proposal-open
- **Docs**: PRD.md:918 (`403`), :961 (`503`); DESIGN.md:1022 (`503`); DESIGN.md:1215 (error-table `503` row covers only creator and event-wait checks); Proposals › Heartbeats and leases › PDP outage (Open)
- **Scenarios**: [worker/fencing/1.08-guardrail-worker-write-authorization-outcomes.md](worker/fencing/1.08-guardrail-worker-write-authorization-outcomes.md), [worker/sessions/1.06-guardrail-heartbeat-works-during-pdp-outage.md](worker/sessions/1.06-guardrail-heartbeat-works-during-pdp-outage.md)
- **Assumed**: `503` `service_unavailable`, nothing changes, task stays leased.
- **Question**: settle the status code (and fix PRD.md:918 or :961), add a worker-call row to the error table, and decide whether work in hand may finish from a cached decision (B) or fails closed (L).
- **Decided** (gap review 2026-11-06): see G-SEC-01: `503` + `Retry-After`, nothing changes, task stays leased; fail closed (option A) for p1. A `403` is reserved for real denials (`CLAIM_REVOKED` releases the task). Remaining: the doc edit.

### G-EXE-07 — Admission shedding: no reason code, no setting for the reserved pool
- **Kind**: missing
- **Docs**: DESIGN.md:2021, :2032, :2035, :1226 (`CONCURRENCY_LIMIT` `429`), :1232 (rate-limit `429`); Proposals › Heartbeats and leases › Reserved admission (accepted: "for example 64")
- **Scenarios**: [worker/sessions/1.07-guardrail-reserved-admission-keeps-heartbeats-flowing.md](worker/sessions/1.07-guardrail-reserved-admission-keeps-heartbeats-flowing.md), [worker/sessions/1.08-guardrail-sdk-heartbeat-retries-shed-heartbeat.md](worker/sessions/1.08-guardrail-sdk-heartbeat-retries-shed-heartbeat.md)
- **Assumed**: an overload `429` is `resource_exhausted` with `Retry-After` and an empty `context`; reserved pool of 64 permits.
- **Question**: give the overload `429` a `context.reason` (for example `OVERLOADED`) so clients can tell it from `CONCURRENCY_LIMIT`, `QUEUE_DEPTH_LIMIT` and rate limits; name the reserved-pool setting and its default; list exactly which routes use it (the accepted proposal names heartbeat, complete, fail, release, cancel — not start, progress, checkpoint or waits).
- **Decided** (gap review 2026-11-06): node overload → `503 service_unavailable` with `Retry-After` (`unavailable_retry_after_secs`), not `429` (platform: `resource_exhausted` is for per-caller quotas). Reserved pool `admission.reserved_execution_permits`, default 64, on top of the shared 512, used by heartbeat, complete (incl. failure reports and cancel acknowledgements), release, wait set/park and the API cancel; not by start, progress or checkpoint. Remaining: the doc edit.

### G-EXE-08 — Revocation release: event actor, `403` body, and paths without a fenced UPDATE
- **Kind**: unclear
- **Docs**: DESIGN.md:1022 ("releases the task on the system path"), :1218; `schemas/types/…task_queued.v1~` (`actor` "present when `reason` is set"); DESIGN.md:936 (progress accept is a PK read), :2676, :2691 (payload write starts with a scoped read → `404`); Proposals › Heartbeats and leases › Revocation releases the task (accepted: "a single-task worker call's fenced `UPDATE`")
- **Scenarios**: [worker/fencing/1.08-guardrail-worker-write-authorization-outcomes.md](worker/fencing/1.08-guardrail-worker-write-authorization-outcomes.md)
- **Assumed**: `taskQueued` with `reason: released`; `403` `permission_denied` with `resource_type`/`resource_name` and no reason; progress and payload writes detect scope exclusion the same way as the plain fenced update.
- **Question**: (1) is the actor of a revocation release the worker, the engine, or absent — and does it need its own `reason` (e.g. `revoked`)? (2) give the `403` a `context.reason` so the SDK knows the task was released; (3) say how progress accept and the revision-guarded payload write apply the accepted "re-read without scope" rule.
- **Decided** (gap review 2026-11-06): (1) `taskQueued` with new reason `revoked` and no actor; (2) `403 permission_denied` reason `CLAIM_REVOKED`; (3) every worker call uses one failure path: on a scoped miss, one unscoped read of the fence columns: holder and token match → release + `403 CLAIM_REVOKED`, else `409 LEASE_LOST`, never `404` (G-EXE-09); progress accept and payload writes included. Remaining: the doc edit.

### G-EXE-09 — A lease token presented by a subject that cannot read the task: `404` or `409`?
- **Kind**: contradiction
- **Docs**: PRD.md:726 (wrong subject → `409` `LEASE_LOST`), PRD.md:918 and DESIGN.md:1016 (a caller that may not read the task gets `404`); DESIGN.md:1324; ADR-0011:80 ("also when that subject has `claim` on the task")
- **Scenarios**: [worker/fencing/1.04-negative-failed-fence-returns-lease-lost.md](worker/fencing/1.04-negative-failed-fence-returns-lease-lost.md)
- **Assumed**: for a same-tenant subject that may read the task: `409` `LEASE_LOST`. The cross-tenant case is not asserted.
- **Question**: for a worker call from a subject whose scope hides the task (another tenant), is the answer `404` (no oracle) or `409` `LEASE_LOST`? Both reveal little, but tests need one.
- **Decided** (gap review 2026-11-06): worker-fenced calls never answer `404`: any fence failure where the caller is not the current holder with that token is `409 LEASE_LOST` (unknown task ID, foreign tenant, stale token alike). Exception: holder and token match but the scope no longer covers the task → release + `403 CLAIM_REVOKED`. One unscoped re-read on the failure path decides. Remaining: the doc edit.

### G-EXE-10 — `start` on a task that is already running
- **Kind**: missing
- **Docs**: DESIGN.md:531 (`execution_deadline` "set again on every start"), :734 (fence allows `claimed` and `running`), :1234
- **Scenarios**: [worker/execution/1.06-positive-start-optional-and-repeatable.md](worker/execution/1.06-positive-start-optional-and-repeatable.md)
- **Assumed**: `400` `failed_precondition` (`STATE`), deadline unchanged.
- **Question**: is a repeated `start` refused, or an idempotent no-op `200`? Either way state that it never moves `execution_deadline` (otherwise a worker can extend its own execution timeout forever).
- **Decided** (gap review 2026-11-06): a repeated `start` by the holder on a `running` task is an idempotent `200` that never moves `execution_deadline`. Corrected the same day: the claim sets `execution_deadline` = claim time + effective execution timeout, covering `claimed` and `running` (with optional `start`, a task that is never started still has a bound); `start` never moves it; `assignment_timeout` stays an optional shorter bound on `claimed`. Remaining: the doc edit and [worker/execution/1.06](worker/execution/1.06-positive-start-optional-and-repeatable.md).

### G-EXE-11 — Completion from `claimed` without `start`
- **Kind**: contradiction
- **Docs**: DESIGN.md:481, :486, :534-538 (worker completion only from `running`), :1317 (`state = running`), :734 (fence `state IN (claimed, running)`)
- **Scenarios**: [worker/execution/1.06-positive-start-optional-and-repeatable.md](worker/execution/1.06-positive-start-optional-and-repeatable.md)
- **Assumed**: `400` `failed_precondition` (`STATE`); task stays `claimed`.
- **Question**: is `complete` (and a reported failure) allowed from `claimed`? Make the fence and the state machine agree.
- **Decided** (gap review 2026-11-06): `start` is optional: `complete`, a reported failure and a cancel acknowledgement are allowed from `claimed`; add the `claimed → terminal` / `claimed → scheduled` edges. Progress is allowed from `claimed` too, like the other worker calls the fence already accepts. Remaining: the doc edit and [worker/execution/1.06](worker/execution/1.06-positive-start-optional-and-repeatable.md).

### G-EXE-12 — Checkpoint is p3, but p1 operations carry checkpoints
- **Kind**: contradiction
- **Docs**: PRD.md:381 (`fr-task-checkpoint` p3), :687 (p1 release "MAY carry a checkpoint"); DESIGN.md:532 (set wait applies an optional checkpoint), :546, :739
- **Scenarios**: [worker/execution/1.03-positive-checkpoint-merges-as-json-merge-patch.md](worker/execution/1.03-positive-checkpoint-merges-as-json-merge-patch.md), [worker/execution/1.08-negative-checkpoint-over-payload-limit.md](worker/execution/1.08-negative-checkpoint-over-payload-limit.md), [worker/fencing/1.04-negative-failed-fence-returns-lease-lost.md](worker/fencing/1.04-negative-failed-fence-returns-lease-lost.md), [worker/release/1.01-positive-release-returns-task-to-queue.md](worker/release/1.01-positive-release-returns-task-to-queue.md)
- **Assumed**: the merge rule and `POST /tasks/{id}/checkpoint` exist in p1; the scenarios are tagged p3 where they test the standalone endpoint.
- **Question**: move `fr-task-checkpoint` to p1 (it is already needed by release and waits), or say that p1 release and waits carry no checkpoint.
- **Decided** (gap review 2026-11-06): `fr-task-checkpoint` moves to p1 (release and set-wait already need the merge; the endpoint is a thin wrapper). Remaining: the doc edit and the checkpoint scenarios' phase (p3 → p1) in INDEX.md.

### G-EXE-13 — "Key by key" merge is undefined for nested objects and `null`
- **Kind**: unclear
- **Docs**: PRD.md:383; DESIGN.md:739, :2679
- **Scenarios**: [worker/execution/1.03-positive-checkpoint-merges-as-json-merge-patch.md](worker/execution/1.03-positive-checkpoint-merges-as-json-merge-patch.md), [worker/release/1.01-positive-release-returns-task-to-queue.md](worker/release/1.01-positive-release-returns-task-to-queue.md)
- **Assumed**: shallow merge of top-level keys; a `null` value is stored as `null` (no key deletion).
- **Question**: define the merge: top-level only or deep (RFC 7386 JSON Merge Patch, where `null` deletes a key)? Can a worker remove a context key at all?
- **Decided** (gap review 2026-11-06): RFC 7386 JSON Merge Patch: nested objects merge recursively, `null` deletes a key, arrays are replaced whole; the 64 KiB limit applies to the result. Remaining: the doc edit and the checkpoint scenarios that store `null`.

### G-EXE-14 — Which schema revision validates the output?
- **Kind**: unclear
- **Docs**: DESIGN.md:737 ("resolved from the task's own versioned `type_id`"), :1047 ("validated against `te_tasks.type_revision`"), :2255 (schemas cached by Registry Reference and revision)
- **Scenarios**: [worker/execution/1.04-positive-complete-accepted-results.md](worker/execution/1.04-positive-complete-accepted-results.md)
- **Assumed**: a type can get a new revision under the same GTS ID; a task is validated against the revision recorded at creation.
- **Question**: is a schema change a new GTS ID (`…v2~`) or a new revision of the same ID? If the latter, define `type_revision` (it is not in the `te_tasks` column list) and how the old revision's schema stays resolvable.
- **Decided** (gap review 2026-11-06): pin: at create, store `te_tasks.type_revision` (the registry revision that validated the input); the task's output is validated against that revision, which stays resolvable (registry revisions are never deleted). Entries use their entry types (G-EXE-19); resolve values use the condition's stored `input_schema`. Remaining: the doc edit.

### G-EXE-15 — Complete request body: nested `result` or top-level fields?
- **Kind**: wire
- **Docs**: DESIGN.md:1316 (`{lease_token, result_code, output}`), :1070 (no body); CONVENTIONS §5.4 (`{ lease_token, result: { result_code, output?, error?, warnings? } }`)
- **Scenarios**: every worker/execution/ and worker/fencing/ scenario that completes a task
- **Assumed**: the nested `result` object of CONVENTIONS §5.4.
- **Question**: fix one body shape (and the sequence diagram) in the API contract.
- **Decided** (gap review 2026-11-06): flat body `{ lease_token, result_code, output?, error?, warnings? }`, as in the draft OpenAPI, its published copy, the SDK draft and DESIGN.md:1316. Remaining: CONVENTIONS §5.4, the sequence diagram and every worker scenario that sends a nested `result`.

### G-EXE-16 — Replay of a completion that did not end the task
- **Kind**: missing
- **Docs**: PRD.md:728; DESIGN.md:574 (retry clears the token), :576 (token kept only for a terminal worker completion), :736
- **Scenarios**: [worker/fencing/1.02-positive-idempotent-completion-replay.md](worker/fencing/1.02-positive-idempotent-completion-replay.md)
- **Assumed**: only terminal outcomes are replayable; a replayed retryable `error` (task now `scheduled`) gets `409` `LEASE_LOST`.
- **Question**: is that intended? A worker whose retryable-failure response was lost then sees `LEASE_LOST` for a call that succeeded; harmless for the task, but the SDK reports lease loss. Either keep the last token for replay checks on `scheduled` tasks, or document the behaviour.
- **Decided** (gap review 2026-11-06): keep `409 LEASE_LOST` for a repeated non-terminal report and document it; the SDK treats `LEASE_LOST` after its own retryable failure or release as already applied (debug log, no `lease.lost`). Terminal completions stay replayable. Remaining: the doc edit.

### G-EXE-17 — Result validation runs before the lease fence
- **Kind**: unclear
- **Docs**: DESIGN.md:2677-2678 (validate, then guarded write), :2699 ("the lease check always wins the classification"), :737
- **Scenarios**: [worker/execution/1.10-negative-complete-result-refused.md](worker/execution/1.10-negative-complete-result-refused.md)
- **Assumed**: as written — a zombie with an invalid result gets `400`, not `LEASE_LOST`.
- **Question**: should the fence be checked first (cheap PK read) so a worker that lost its lease always learns `LEASE_LOST`, as the "lease check always wins" rule suggests for `CONCURRENT_MODIFICATION`?
- **Decided** (gap review 2026-11-06): validation stays first; when it fails, one PK read of the fence columns decides: caller not the current holder → `LEASE_LOST` (or `403 CLAIM_REVOKED` under the revocation rule), otherwise `400`. Remaining: the doc edit and [worker/execution/1.10](worker/execution/1.10-negative-complete-result-refused.md) (zombie with an invalid result → `LEASE_LOST`).

### G-EXE-18 — Entry resource and entry list contract
- **Kind**: wire
- **Docs**: DESIGN.md:393-399 (TaskEntry fields), :1087-1089 (endpoints only), :1762 (index; type and creator filters); PRD.md:632
- **Scenarios**: [entries/1.01-positive-create-entry.md](entries/1.01-positive-create-entry.md), [entries/1.02-positive-list-entries-filtered.md](entries/1.02-positive-list-entries-filtered.md), [entries/1.03-positive-get-entry-by-id.md](entries/1.03-positive-get-entry-by-id.md)
- **Assumed**: create body `{ lease_token?, type, payload }`; `201 Created` + `Location`; entry `{ id, task_id, type, creator, payload, created_at }`; list via OData `$filter` on `type`, `creator`, `created_at`, returned as a `JsonPage` in `created_at` order with a page limit. These fields are new S fields not in CONVENTIONS §5.
- **Question**: define the entry resource, the create response, the list filter syntax, order, paging and page-size limit (Proposals › API reads and responses requires bounded lists).
- **Decided** (wire review 2026-11-04): create body `{ lease_token?, type, payload }` (token on the worker path only); `201` + `Location` + `{ id, task_id, type, creator, payload, created_at }`; list cursor-paged, `created_at asc`, `$filter` on `type`, `creator`, `created_at`. Remaining: the doc edit.

### G-EXE-19 — Entry type resolution and its errors
- **Kind**: missing
- **Docs**: DESIGN.md:1212 (`UNKNOWN_TASK_TYPE` for task types only), :2255 (types resolved from `types_registry`, caller-context visibility for creates); `schemas/types/gts.cf.core.tasks.entry.v1~` (validation at creation)
- **Scenarios**: [entries/1.01-positive-create-entry.md](entries/1.01-positive-create-entry.md), [entries/1.04-negative-entry-create-refused.md](entries/1.04-negative-entry-create-refused.md)
- **Assumed**: unknown or non-entry type → `400` `invalid_argument` with a field violation on `type`, no reason code; registry down and type uncached → `503`.
- **Question**: add an error row (and a reason such as `UNKNOWN_ENTRY_TYPE`) for entry types, and say whether entry types are resolved with the caller's context like task types.
- **Decided** (gap review 2026-11-06): entry types resolve with the caller's context (as task types); unknown, hidden or non-entry type → `400 invalid_argument` on `/type` with `UNKNOWN_ENTRY_TYPE`; registry down and type uncached → `503` (G-CRT-23). Remaining: the doc edit.

### G-EXE-21 — When may an entry be added?
- **Kind**: missing
- **Docs**: PRD.md:632 ("what happened during execution"); DESIGN.md:1087, :1439 (ring fence covers writing an entry to an old task); the draft's entry write (no state check on the consumer path)
- **Scenarios**: [entries/1.01-positive-create-entry.md](entries/1.01-positive-create-entry.md)
- **Assumed**: an API consumer may add entries in any state, terminal included; worker entries need a live lease.
- **Question**: state the allowed task states for consumer entries, and whether a per-task entry count limit applies (entries are otherwise unbounded per task).
- **Decided** (gap review 2026-11-06): API consumers may add entries in any state, terminal included, until the task is removed; worker entries need a live lease. `max_entries_per_task` (default 1000; over it → `400 failed_precondition` `{type: STATE, subject: TOO_MANY_ENTRIES}`, since no body field is at fault), enforced with an `entry_count` column on the task row updated in the entry transaction (row lock instead of the draft's `FOR SHARE`). Remaining: the doc edit.

### G-EXE-22 — Status for update/delete on an entry
- **Kind**: unclear
- **Docs**: PRD.md:642; DESIGN.md:988, :1087-1089
- **Scenarios**: [entries/1.06-guardrail-entries-cannot-be-changed-or-deleted.md](entries/1.06-guardrail-entries-cannot-be-changed-or-deleted.md)
- **Assumed**: `405 Method Not Allowed` with `Allow: GET` (a test also accepts `404`).
- **Question**: confirm the platform router's answer for an unsupported method on an existing path, so guardrail tests can pin it.
- **Decided** (gap review 2026-11-06): `405 Method Not Allowed` with an `Allow` header and no problem body (axum method router default); entries stay append-only. Remaining: the doc edit.

### G-EXE-23 — Delayed release refused by the deferred limit: what happens to the lease
- **Kind**: missing
- **Docs**: PRD.md:445, :452, :689; DESIGN.md:546; Proposals › Limits and counters › Per-tenant depth from periodic counts (the deferred limit is sampled too)
- **Scenarios**: [limits/depth/1.03-negative-deferred-limit-refuses-every-deferral.md](limits/depth/1.03-negative-deferred-limit-refuses-every-deferral.md)
- **Assumed**: refused `429` `QUEUE_DEPTH_LIMIT` (dimension `deferred`) when the latest sample is at the limit; the lease is kept and the task stays `running`.
- **Question**: The PRD says a delayed release "may get `429`" but not what happens to the task; confirm the lease is kept. Update PRD.md:1492 ("no tenant ever has more than `tenant_max_deferred_tasks`"), which no longer holds exactly.
- **Decided** (gap review 2026-11-06): refused `429 QUEUE_DEPTH_LIMIT` (dimension `deferred`, with `Retry-After`) with no state change: the lease is kept and the task stays `running`. PRD.md:1492 becomes "by at most one sampling interval of its own admissions". Remaining: the doc edit.

### G-EXE-24 — Event for a release that schedules the task
- **Kind**: unclear
- **Docs**: PRD.md:687 ("A `taskQueued` event with reason `released` is published"); DESIGN.md:732; `schemas/types/…task_queued.v1~` (`queue`: "Queue the task is queued on")
- **Scenarios**: [worker/release/1.01-positive-release-returns-task-to-queue.md](worker/release/1.01-positive-release-returns-task-to-queue.md)
- **Assumed**: `taskQueued` with `reason: released` even when the task goes to `scheduled`.
- **Question**: confirm, or use another event (with `run_after`) for a delayed release, so consumers do not read `taskQueued` as "claimable now".
- **Decided** (gap review 2026-11-06): `taskQueued` means "returned to its queue" (release, re-enqueue) and gains an optional `run_after` (absent = claimable now); a delayed release is `taskQueued { reason: released, run_after }`. Remaining: the doc edit.

### G-EXE-25 — `delay_secs` bounds, and delayed release from `claimed`
- **Kind**: missing
- **Docs**: DESIGN.md:491-493 (diagram: only `running → scheduled` on release with delay), :546 (table: `claimed`/`running` → `scheduled`), :1071
- **Scenarios**: [worker/release/1.01-positive-release-returns-task-to-queue.md](worker/release/1.01-positive-release-returns-task-to-queue.md)
- **Assumed**: any positive `delay_secs` is accepted; a delayed release from `claimed` is allowed (table wins over diagram).
- **Question**: give the bounds of `delay_secs` (0, negative, maximum — for example `max_interval_secs` or the remaining lifetime), and add `claimed → scheduled` to the state diagram.
- **Decided** (gap review 2026-11-06): `0 ≤ delay_secs ≤ release_max_delay_secs` (new, default 86,400); 0 = immediate release; negative or above → `400 OUT_OF_RANGE`; a delayed release from `claimed` is allowed (`claimed → scheduled` added to the diagram). Remaining: the doc edit.

## RTY — Retry, timeouts, dead-letter, cancel

### G-RTY-01 — State diagram omits the timeout, liveness and lifetime transitions
- **Kind**: contradiction
- **Docs**: DESIGN.md:466-513 (diagram) vs DESIGN.md:539-543 (table); DESIGN.md:488-489 label `running → failed/dead_lettered` "worker completes" only; DESIGN.md:484-485 still draw consumer-set waits removed by Proposals › Task mutability
- **Scenarios**: [retry/1.04-positive-liveness-budget-exhausted-timed-out.md](retry/1.04-positive-liveness-budget-exhausted-timed-out.md), [timeouts/1.02-positive-assignment-timeout-requeues-with-backoff.md](timeouts/1.02-positive-assignment-timeout-requeues-with-backoff.md), [timeouts/1.03-positive-execution-timeout-is-terminal.md](timeouts/1.03-positive-execution-timeout-is-terminal.md)
- **Assumed**: the transition table is authoritative: `claimed`/`running` → `scheduled` (liveness requeue), `claimed`/`running` → `dead_lettered`/`failed` (liveness exhausted), `running` → `dead_lettered`/`failed` (execution timeout), any non-terminal → `dead_lettered`/`failed` (lifetime).
- **Question**: Add these edges to the diagram (and drop `claimed --> waiting` / `queued --> waiting` by API consumer) so the diagram and the table list the same transitions?
- **Decided** (gap review 2026-11-06): (doc only) the transition table is authoritative; the diagram gains the liveness, execution, lifetime edges and `claimed → terminal`/`claimed → scheduled` (G-EXE-11), and drops the consumer-set `→ waiting` edges. Remaining: the doc edit.

### G-RTY-02 — Which `timeout_type` a liveness exhaustion reports, and none for an expired hand-off
- **Kind**: unclear
- **Docs**: `task_timed_out.v1` schema (`timeout_type` enum `queue|assignment|execution|heartbeat|lifetime`); DESIGN.md:540, :2802 (finalize is called with no timeout type); DESIGN.md:539 (an unaccepted hand-off past `handoff_until` is also a liveness failure)
- **Scenarios**: [retry/1.04-positive-liveness-budget-exhausted-timed-out.md](retry/1.04-positive-liveness-budget-exhausted-timed-out.md)
- **Assumed**: session expiry → `heartbeat`, assignment deadline → `assignment`.
- **Question**: Confirm the mapping, and say which value an exhausted budget after an expired hand-off uses (add `handoff`?).
- **Decided** (gap review 2026-11-06): session expiry → `heartbeat`; assignment deadline → `assignment`; an expired hand-off → `assignment`. No new enum value. Remaining: the doc edit.

### G-RTY-03 — Where a retried attempt's error can be read
- **Kind**: unclear
- **Docs**: PRD.md:1030 (unfinished tasks have no result code); `task_retrying.v1` schema, `error` description ("read them from the task via the API"); DESIGN.md:538 (the retry row says nothing about the result)
- **Scenarios**: [retry/1.01-positive-retryable-error-schedules-retry.md](retry/1.01-positive-retryable-error-schedules-retry.md)
- **Assumed**: the `scheduled` task has `result: null`; the published `taskRetrying` carries only `domain`, `code`, `fault`, `retryable`.
- **Question**: Does the task expose the last attempt's error (message, context) while it waits for a retry, for example as a `last_error` field or in the history entry? If not, the schema text pointing to the task API is wrong.
- **Decided** (gap review 2026-11-06): the `taskRetrying` history row stores the full `TaskError`, returned by `GET /tasks/{id}/events`; the broker event keeps only the summary; the schema text becomes "read it from the task's history". The task's `result` stays `null` until it ends. Remaining: the doc edit.

### G-RTY-04 — "The first retry waits base" does not hold with jitter
- **Kind**: unclear
- **Docs**: PRD.md:790, :1483; DESIGN.md:2861-2862, :2866; queue schema `retry_policy.jitter_factor`
- **Scenarios**: [retry/1.01-positive-retryable-error-schedules-retry.md](retry/1.01-positive-retryable-error-schedules-retry.md), [retry/1.02-positive-backoff-by-strategy-cap-and-jitter.md](retry/1.02-positive-backoff-by-strategy-cap-and-jitter.md)
- **Assumed**: the first wait is in `[base × (1 − jitter_factor), base]`; with `jitter_factor: 1` it can be 0.
- **Question**: Reword PRD.md:1483 and DESIGN.md:2866 to "at most `base`". Is a zero wait acceptable with full jitter, or should there be a floor?
- **Decided** (gap review 2026-11-06): reword to "at most `base`"; every computed backoff has a floor of 1 s. Remaining: the doc edit.

### G-RTY-05 — Repeating a retryable failure report after a lost response
- **Kind**: missing
- **Docs**: DESIGN.md:574 (token cleared on retry), :576, :736 (replay only for a terminal task); PRD.md:1524 (replay acceptance covers completion only); DESIGN.md:703 (SDK never retries `LEASE_LOST`)
- **Scenarios**: [retry/1.08-guardrail-repeated-retryable-report-is-lease-lost.md](retry/1.08-guardrail-repeated-retryable-report-is-lease-lost.md)
- **Assumed**: the repeat gets `409` `LEASE_LOST`; `retry_count` is not incremented twice.
- **Question**: Should a retryable-failure report be replayable like a terminal completion (`200` with the stored `scheduled` outcome), so the worker can tell "applied" from "lease taken"?
- **Decided** (gap review 2026-11-06): see G-EXE-16: the repeat gets `LEASE_LOST`, no engine replay state; the SDK treats it as applied. Remaining: the doc edit.

### G-RTY-06 — `max_liveness_requeues: 0` against the trust limits and without a lifetime timeout
- **Kind**: contradiction
- **Docs**: PRD.md:786 ("0 means unlimited, bounded only by the lifetime timeout"), PRD.md:1010; DESIGN.md:2196 ("0 only together with a lifetime timeout", under `global` only), DESIGN.md:2201 (`tenant_owned.max_liveness_requeues: 20`); queue schema (`0 means unlimited (bounded only by the lifetime timeout, if any)`)
- **Scenarios**: [retry/1.02-positive-backoff-by-strategy-cap-and-jitter.md](retry/1.02-positive-backoff-by-strategy-cap-and-jitter.md), [retry/1.05-positive-unlimited-liveness-requeues-end-at-lifetime.md](retry/1.05-positive-unlimited-liveness-requeues-end-at-lifetime.md), [retry/1.07-negative-unlimited-liveness-requeues-without-lifetime.md](retry/1.07-negative-unlimited-liveness-requeues-without-lifetime.md)
- **Assumed**: `0` is allowed for a tenant-owned type when a lifetime timeout is in effect; without any lifetime timeout a create request with `0` is rejected `400` naming `retry_policy.max_liveness_requeues`.
- **Question**: Is `0` (unlimited) within a numeric cap of 20 for tenant-owned types? Is "requires a lifetime timeout" a rule for every level (request, type, queue), and what happens to a queue that declares `0` with no lifetime?
- **Decided** (gap review 2026-11-06): unlimited (`0`) is above any numeric cap: an explicit `0` for a tenant-owned type → `400` naming the setting, an inherited `0` is clamped to the cap (G-CRT-05). For platform types an effective `0` without an effective lifetime timeout → `400` at create; a queue or type declaring `0` without a lifetime is rejected at registration/configuration. Remaining: the doc edit.

### G-RTY-07 — Lifetime deadline and liveness failure due at the same time
- **Kind**: unclear
- **Docs**: DESIGN.md:543 ("overrides retry/lease handling"), :2734, :2759, :2784-2799; Proposals › Heartbeats and leases › Same timeout precedence in both scans (accepted, covers the execution deadline only)
- **Scenarios**: [timeouts/1.06-guardrail-concurrent-deadline-detectors-act-once.md](timeouts/1.06-guardrail-concurrent-deadline-detectors-act-once.md)
- **Assumed**: the task ends `timed_out` with `timeout_type: lifetime` in either order; whether a liveness requeue, a `liveness_requeues` increment and a blacklist strike happen first is not asserted.
- **Question**: Should the shared expiry step also check the lifetime deadline before requeueing (no requeue, no strike), as it does for the execution deadline?
- **Decided** (gap review 2026-11-06): the shared expiry step reads the task's lifetime timer; if it is due, the task ends `timed_out` (`lifetime`) with no requeue, no `liveness_requeues` increment and no blacklist strike. Remaining: the doc edit.

### G-RTY-08 — Queue timeout while the queue is held
- **Kind**: proposal-open
- **Docs**: Proposals › Queue operations › Open "Queue timeouts while held" (leaning: held time does not count); Proposals › Queue operations › Admin queue hold (accepted: "Timers keep running"); PRD.md:517
- **Scenarios**: [timeouts/1.01-positive-queue-or-lifetime-timeout-ends-task-timed-out.md](timeouts/1.01-positive-queue-or-lifetime-timeout-ends-task-timed-out.md)
- **Assumed**: only that the lifetime timeout fires on a held queue; the queue-timeout outcome is not asserted.
- **Question**: Decide the Open item. If held time does not count, say how the queue deadline is moved (paused and resumed, or re-armed at release).
- **Decided** (gap review 2026-11-06): see G-OPS-07: held time counts; the queue timeout fires during a hold. Remaining: the doc edit.

### G-RTY-09 — Timer-accuracy load profile and how it is measured
- **Kind**: contradiction
- **Docs**: PRD.md:1262 (99 % within 1 s at 100,000 active tasks) vs ADR-0003:77 (10,000 active timers); DESIGN.md:2269-2300 (no timer-lag metric; only `te_timer_channel_size`, DESIGN.md:2288)
- **Scenarios**: [timeouts/1.07-guardrail-timers-fire-within-one-second.md](timeouts/1.07-guardrail-timers-fire-within-one-second.md)
- **Assumed**: the PRD profile; lag measured from event times against due times.
- **Question**: Align ADR-0003's confirmation with the PRD profile, and add a lag metric (for example a histogram of fire time minus due time per timer type and scan) so the NFR can be tested and alerted on.

### G-RTY-10 — DLQ listing by search: time window and permission
- **Kind**: contradiction
- **Docs**: PRD.md:815 ("so operators never miss parked failures"), PRD.md:1143, DESIGN.md:1161 (terminal tasks: 7-day default, 31-day maximum window); DESIGN.md:1104 (`GET /tasks` needs `read`) vs DESIGN.md:1127 (`GET /admin/dlq` needed `admin`); Proposals › API reads and responses (accepted: `GET /admin/dlq` removed)
- **Scenarios**: [dead-letter/1.02-positive-list-dead-lettered-by-search.md](dead-letter/1.02-positive-list-dead-lettered-by-search.md)
- **Assumed**: the ordinary search: dead-lettered tasks created more than 31 days ago cannot be listed; any caller with `read` can list its own dead-lettered tasks.
- **Question**: Exempt `state eq 'dead_lettered'` from the window (the live DLQ set is bounded by retention), or keep a bounded DLQ listing? Is `read` enough to list parked failures, or should listing the **dead_letter** queue need `admin`?
- **Decided** (gap review 2026-11-06): a search filtered to `state eq 'dead_lettered'` only is exempt from the `created_at` window (the set is bounded by retention); a tenant-scoped listing is keyset-paged on `ix_te_task_results_tenant_state`, an unscoped one per DLQ on `ix_te_task_results_dlq` extended to `(queue_id, task_id)`; listing needs only `read` (the caller sees its own scope); re-enqueue still needs `admin`. Clarified: a search is exempt when its state filter restricts it to `dead_lettered`, whatever other filters it has (`queue`, `dead_lettered_from`, `type`). Remaining: the doc edit.

### G-RTY-11 — No way to filter dead-lettered tasks by source queue
- **Kind**: missing
- **Docs**: PRD.md:815 ("unless it is filtered by a source queue"); DESIGN.md:1132 (the removed endpoint's `dead_lettered_from` filter); DESIGN.md:1138 (filterable fields have no `dead_lettered_from`); DESIGN.md:1165-1170 (no view returns it); DESIGN.md:2835 (a `queue` filter picks "the queue's domain", but a DLQ is not routed, PRD.md:1176)
- **Scenarios**: [dead-letter/1.02-positive-list-dead-lettered-by-search.md](dead-letter/1.02-positive-list-dead-lettered-by-search.md)
- **Assumed**: the listing uses only `state`; `dead_lettered_from` is read per task with `GET /tasks/{id}`.
- **Question**: Add `dead_lettered_from` as a filterable field (routing to the source queue's domain) and to the `short` view? Which domains does `queue eq '<dead_letter queue>'` search?
- **Decided** (gap review 2026-11-06): `dead_lettered_from` becomes a filterable field and part of the `short` view; a filter on it routes to the source queue's domain set; new partial index `(owner_tenant_id, dead_lettered_from, task_id) WHERE state = dead_lettered`. Remaining: the doc edit.

### G-RTY-12 — Task fields after re-enqueue: a passed SLA deadline and `terminal_at`
- **Kind**: unclear
- **Docs**: PRD.md:807 ("keep the SLA deadline only if it is still in the future"); DESIGN.md:545, :733 (only the SLA **timer** is skipped; no word on `terminal_at`)
- **Scenarios**: [dead-letter/1.03-positive-reenqueue-dead-lettered-or-failed-task.md](dead-letter/1.03-positive-reenqueue-dead-lettered-or-failed-task.md)
- **Assumed**: a passed `sla_deadline` reads `null`; `terminal_at` reads `null`; `dead_lettered_from` is cleared (DESIGN.md:545).
- **Question**: Which terminal-only fields are reset by re-enqueue, and is a passed SLA deadline cleared or kept for the record?
- **Decided** (gap review 2026-11-06): re-enqueue clears `terminal_at`, the result fields and `dead_lettered_from`; a passed `sla_deadline` is cleared (PRD.md:807), a future one kept with its timer. Remaining: the doc edit.

### G-RTY-13 — Cancelling a task that has already ended
- **Kind**: missing
- **Docs**: PRD.md:344-356 (silent); DESIGN.md:1234 (invalid state transition → `failed_precondition`); PRD.md:1028
- **Scenarios**: [tasks/cancel/1.05-negative-cancel-of-ended-task-refused-unless-cancelled.md](tasks/cancel/1.05-negative-cancel-of-ended-task-refused-unless-cancelled.md)
- **Assumed**: `400` `failed_precondition`, violation type `STATE`, for every terminal state.
- **Question**: `400`, or `200` with the unchanged task (idempotent, at least for an already `cancelled` task), so a client that retries a cancel after a lost response is not told it failed?
- **Decided** (gap review 2026-11-06): a task already ended in the cancelled family (`cancelled`, `forcibly_cancelled`, `superseded`, `dependency_failed`) → `200` with the unchanged task, no event; any other terminal state → `400 failed_precondition` `{type: STATE, subject: TASK_FINISHED}`. Remaining: the doc edit and the cancel scenarios that assert `400` for every terminal state.

### G-RTY-14 — Error for `requested`/`graceful` on a non-cancellable task
- **Kind**: missing
- **Docs**: PRD.md:354 ("MUST be rejected"); DESIGN.md:738; DESIGN.md:1228 (only "`cancellable=false` on a supersede type", which is about type registration)
- **Scenarios**: [tasks/cancel/1.09-guardrail-cancellable-false-allows-only-forced.md](tasks/cancel/1.09-guardrail-cancellable-false-allows-only-forced.md)
- **Assumed**: `400` `failed_precondition`, violation `{ type: STATE, subject: cancellable }`.
- **Question**: Which category and reason code? Add a row to the error table (DESIGN.md:1197-1236).
- **Decided** (gap review 2026-11-06): `400 failed_precondition` `{type: STATE, subject: NOT_CANCELLABLE}`, `resource_name` = the task; `forced` still works. Add the row to the error table. Remaining: the doc edit.

### G-RTY-15 — Is `taskCancelRequested` published for immediate and forced cancels?
- **Kind**: unclear
- **Docs**: PRD.md:352 ("`taskCancelRequested` and `taskCancelled` events are published"); `task_cancel_requested.v1` schema (`mode` enum includes `forced`); DESIGN.md:549-550 (no event named)
- **Scenarios**: [tasks/cancel/1.01-positive-cancel-of-unleased-task-ends-at-once.md](tasks/cancel/1.01-positive-cancel-of-unleased-task-ends-at-once.md), [tasks/cancel/1.02-positive-cancel-of-leased-task-by-mode.md](tasks/cancel/1.02-positive-cancel-of-leased-task-by-mode.md)
- **Assumed**: every accepted cancel request records `taskCancelRequested` (with `mode`, `requested_by`), followed by `taskCancelled` when the task ends.
- **Question**: Confirm, or publish `taskCancelRequested` only when a leased task is flagged.
- **Decided** (gap review 2026-11-06): `taskCancelRequested` is recorded only when a leased task is flagged (`requested`/`graceful`) or escalated; an immediate or forced cancel records only `taskCancelled` (with `actor` and `mode`). After an immediate or forced cancel `cancel_requested` reads `false` (the task was never flagged). Remaining: the doc edit and the cancel scenarios that expect `taskCancelRequested` for immediate/forced cancels.

### G-RTY-16 — A second cancel request on a task already flagged
- **Kind**: missing
- **Docs**: DESIGN.md:551 (one row for `requested`/`graceful` on a leased task; no rule for repeats)
- **Scenarios**: [tasks/cancel/1.03-positive-escalate-requested-cancel-to-graceful.md](tasks/cancel/1.03-positive-escalate-requested-cancel-to-graceful.md)
- **Assumed**: accepted with `202`; `graceful` after `requested` arms `cancel_grace`; a repeated `graceful` keeps the first deadline; each call records `taskCancelRequested`.
- **Question**: Confirm the escalation rules, including whether a repeat is recorded and whether `requested` after `graceful` does anything.
- **Decided** (gap review 2026-11-06): modes only move forward: `requested → graceful` arms `cancel_grace` and records `taskCancelRequested(graceful)`; a repeat of the same mode or `requested` after `graceful` is a no-op `202` (first deadline kept, no event); `forced` always ends the task at once. Remaining: the doc edit.

### G-RTY-17 — May the worker complete normally after a cancel request?
- **Kind**: unclear
- **Docs**: PRD.md:348 ("ends the task as `cancelled`"); DESIGN.md:737 (any worker code allowed; `cancelled` only while the flag is set)
- **Scenarios**: [tasks/cancel/1.04-positive-worker-finishes-normally-after-cancel-request.md](tasks/cancel/1.04-positive-worker-finishes-normally-after-cancel-request.md)
- **Assumed**: `success` is accepted and the task ends `completed`.
- **Question**: Is completing with `success`/`warning`/`error` after a cancel request allowed, and does an `error` then still retry?
- **Decided** (gap review 2026-11-06): after a cancel request `success`/`warning` end the task `completed`; any `error` (retryable or not) ends it `cancelled` with the error recorded, no retry, no DLQ; a graceful supersession still ends `superseded` (PRD.md:328). Remaining: the doc edit.

### G-RTY-18 — Default and validation of the cancel `mode`
- **Kind**: missing
- **Docs**: PRD.md:346-350; DESIGN.md:745 (`cancel_task(ctx, id, mode)`), :1072
- **Scenarios**: [tasks/cancel/1.07-negative-unknown-cancel-mode.md](tasks/cancel/1.07-negative-unknown-cancel-mode.md)
- **Assumed**: an unknown value → `400` `invalid_argument` on `mode`.
- **Question**: Is `mode` required? If not, which mode applies when it is omitted (`requested` is the natural default)?
- **Decided** (wire review 2026-11-04): `mode` is optional and defaults to `requested`; the body itself is optional; an unknown mode → `400`. Remaining: the doc edit.

### G-RTY-19 — Grace period: one engine setting or configurable per queue or request?
- **Kind**: unclear
- **Docs**: PRD.md:349 ("a configurable grace period"); DESIGN.md:2155 (`cancel_grace_period_secs: 60`, engine-wide only)
- **Scenarios**: [tasks/cancel/1.11-flow-graceful-cancel-forced-after-grace-period.md](tasks/cancel/1.11-flow-graceful-cancel-forced-after-grace-period.md)
- **Assumed**: the engine-wide 60 s.
- **Question**: Is the grace period settable per queue, per type, or in the cancel request (for example `grace_secs`), within limits?
- **Decided** (gap review 2026-11-06): engine-wide `cancel_grace_period_secs` only in p1; a per-queue override may be added later. Remaining: the doc edit.

### G-RTY-20 — Dead-lettered tasks left behind after a queue moves domain
- **Kind**: unclear
- **Docs**: PRD.md:1180 (re-enqueue into a queue that now routes elsewhere → `400` `TASK_DOMAIN_MISMATCH`); PRD.md:801 (the task stays in its source domain); Proposals › Domains › During a move (accepted proposal; "Re-enqueue … rejected in the source once it is done"); DESIGN.md:1224
- **Scenarios**: [dead-letter/1.04-negative-reenqueue-refusals.md](dead-letter/1.04-negative-reenqueue-refusals.md)
- **Assumed**: `400` `invalid_argument`, `reason: TASK_DOMAIN_MISMATCH`.
- **Question**: How does an operator recover such a task (re-create it, re-enqueue into another queue of the source domain)? Should a move be blocked, or warn, while dead-lettered tasks from the queue remain in the source?
- **Decided** (gap review 2026-11-06): re-enqueue takes an optional `queue` (a dispatch queue routed to the task's current domain that accepts its type); without it the source queue is used and a moved queue gives `400 TASK_DOMAIN_MISMATCH`. A move does not wait for dead-lettered tasks (they are terminal). A re-enqueue into another queue re-resolves settings like a move (G-OPS-20). Bulk retry reports any per-task precondition refusal (`TASK_DOMAIN_MISMATCH`, `PARENT_FINISHED`, `STALE_ORDER`) as `invalid_state`. Remaining: the doc edit.

### G-RTY-21 — Ending a `claimed` (not started) task as cancelled
- **Kind**: unclear
- **Docs**: DESIGN.md:535 (only `running → cancelled` by the worker); DESIGN.md:734 (fenced writes accept `state IN (claimed, running)`); DESIGN.md:551 (a `claimed` task can be flagged)
- **Scenarios**: [tasks/cancel/1.02-positive-cancel-of-leased-task-by-mode.md](tasks/cancel/1.02-positive-cancel-of-leased-task-by-mode.md)
- **Assumed**: not asserted; scenarios acknowledge from `running` only.
- **Question**: May a worker that sees the flag right after the claim complete `cancelled` without `start`, or must it start first (or release)?
- **Decided** (gap review 2026-11-06): see G-EXE-11: a worker may end a `claimed` task as `cancelled` without `start`. Remaining: the doc edit.

### G-RTY-22 — Execution timeout per start or per attempt
- **Kind**: unclear
- **Docs**: PRD.md:519 ("one run took too long, measured from its start (time in `waiting` is not counted)"); DESIGN.md:531 (`execution_deadline` set again on every start); queue schema `execution_timeout_secs` ("re-armed on each start")
- **Scenarios**: [timeouts/1.04-positive-waiting-time-not-counted-toward-execution.md](timeouts/1.04-positive-waiting-time-not-counted-toward-execution.md)
- **Assumed**: each start after a resume gets the full timeout again.
- **Question**: Is that intended? A task that parks often then has no bound on total running time other than its lifetime; the alternative is a per-attempt budget that resumes where it stopped.
- **Decided** (gap review 2026-11-06): per claim, documented: each claim (including after a resume) sets the full execution timeout (G-EXE-10: from the claim); the lifetime timeout bounds the total. Remaining: the doc edit and [timeouts/1.03](timeouts/1.03-positive-execution-timeout-is-terminal.md) / [timeouts/1.04](timeouts/1.04-positive-waiting-time-not-counted-toward-execution.md) (measure from the claim).

## WAI — Waits, parent/child, dependencies

### G-WAI-01 — PRD and DESIGN still let an API consumer set a wait; refusal status undefined
- **Kind**: contradiction
- **Docs**: PRD.md:548 ("A worker or an API consumer sets the conditions"), PRD.md:550 ("A wait set by an API consumer on a claimed task ends that lease too"), PRD.md:564 (actor list), PRD.md:925 (`update` covers "setting a wait"); DESIGN.md:532 (`claimed`/`queued` (API consumer) → `waiting`), DESIGN.md:813, DESIGN.md:986, DESIGN.md:1081 ("API-consumer wait set" takes no token), DESIGN.md:1095 (`update` (API consumer, no token; revokes any lease)); Proposals › Task mutability › Set only in the create request
- **Scenarios**: [waits/framework/1.07-negative-set-wait-refusals.md](waits/framework/1.07-negative-set-wait-refusals.md)
- **Assumed**: a set-wait without `lease_token` is `400 invalid_argument` (field violation `lease_token`, `REQUIRED`), checked before any task lookup; a consumer sending a token it does not hold gets `409 LEASE_LOST`.
- **Question**: Which status does the removed consumer path return (`400` missing field, `409 LEASE_LOST`, or `403`)? Apply the accepted edits to the PRD and DESIGN lines above, and drop `claimed`/`queued` from the `→ waiting` row.
- **Decided** (gap review 2026-11-06): `lease_token` is required by the set-wait schema: missing → `400 REQUIRED` on `/lease_token` before any lookup; a token the caller does not hold → `409 LEASE_LOST` (G-EXE-09). Remaining: the accepted doc edits.

### G-WAI-02 — `update` on the parent still allows a child; status for a missing `parent_lease_token`
- **Kind**: contradiction
- **Docs**: PRD.md:855 ("or `update` on the parent"), PRD.md:1505 ("Without the parent's lease or `update`"); DESIGN.md:726, DESIGN.md:986 ("adding a child without the parent's lease"), DESIGN.md:1062, DESIGN.md:2630-2632 (`or authorize(update, p)`, also a PDP call inside the transaction); Proposals › Task mutability › Only the parent's worker adds children
- **Scenarios**: [hierarchy/children/1.01-positive-worker-creates-child-with-parent-lease-token.md](hierarchy/children/1.01-positive-worker-creates-child-with-parent-lease-token.md), [hierarchy/children/1.05-guardrail-child-link-refusals.md](hierarchy/children/1.05-guardrail-child-link-refusals.md)
- **Assumed**: `parent_task_id` without `parent_lease_token` → `400 invalid_argument` (field violation `parent_lease_token`); a wrong, stale or foreign token → the same `404` as a missing parent.
- **Question**: Is a missing token a `400` (syntactic, no oracle) or folded into the uniform `404`? Remove the `update` branch from the lines above.
- **Decided** (gap review 2026-11-06): `parent_task_id` without `parent_lease_token` → `400 REQUIRED` on `/parent_lease_token` (syntactic, before any lookup); a wrong, stale or foreign token → the same `404` as a missing parent. The `update` branch is removed. Remaining: the doc edit.

### G-WAI-03 — Dependency changes are still specified; removed endpoints have no status
- **Kind**: contradiction
- **Docs**: PRD.md:839 ("A creation or change…"), PRD.md:841, PRD.md:845, PRD.md:856, PRD.md:1072, PRD.md:1504, PRD.md:1543-1544; DESIGN.md:528 (`queued`/`scheduled` → `blocked`), DESIGN.md:558, DESIGN.md:727, DESIGN.md:730, DESIGN.md:1076-1077, DESIGN.md:1599 (lock step 0), DESIGN.md:2870-2906 (`add_dependencies`, `CYCLE_DETECTED`); Proposals › Task mutability › Set only in the create request
- **Scenarios**: [hierarchy/dependencies/1.05-guardrail-dependency-endpoints-removed.md](hierarchy/dependencies/1.05-guardrail-dependency-endpoints-removed.md)
- **Assumed**: both routes are gone and answer `404 Not Found`; no `CYCLE_DETECTED` path remains.
- **Question**: `404` or `405` for the removed routes? Remove the change paths, the `→ blocked` transitions from `queued`/`scheduled`, the per-tenant graph lock and the cycle walk (keep `max_predecessors_per_task`).
- **Decided** (gap review 2026-11-06): the dependency-change routes are not registered, so the router answers `404`; the change paths, `queued`/`scheduled → blocked`, the per-tenant graph lock and the cycle walk are removed; `max_predecessors_per_task` stays. Remaining: the doc edit.

### G-WAI-04 — Providing input still needs `update` in PRD, DESIGN and the `task_update` instance
- **Kind**: contradiction
- **Docs**: PRD.md:608 ("Providing input needs the `update` action"), PRD.md:922 (task actions list `update`, no `resolve`), PRD.md:925; DESIGN.md:986, DESIGN.md:995 ("cannot … resolve input waits" via `update`), DESIGN.md:1098 (`update`); `schemas/instances/gts.cf.toolkit.authz.permission.v1~cf.task_engine._.task_update.v1.json` (no `task_resolve.v1`); Proposals › Task mutability › `update` covers only tags; `resolve` gets its own action, Proposals › Waits › `resolve`
- **Scenarios**: [waits/framework/1.10-flow-group-decision-outcomes.md](waits/framework/1.10-flow-group-decision-outcomes.md), [waits/input/1.01-positive-resolve-valid-value-resumes-task.md](waits/input/1.01-positive-resolve-valid-value-resumes-task.md), [waits/input/1.06-guardrail-resolve-permission-checks.md](waits/input/1.06-guardrail-resolve-permission-checks.md), [auth/1.05-negative-read-but-not-act.md](auth/1.05-negative-read-but-not-act.md), [tasks/create/1.13-positive-update-tags-after-create.md](tasks/create/1.13-positive-update-tags-after-create.md)
- **Assumed**: `POST /wait-conditions/{id}/resolve` needs `resolve` on the task; a subject that can read but lacks `resolve` gets `403`; one that cannot read gets `404`; a policy granting only `update` allows tag changes and nothing else.
- **Question**: Apply the accepted edits; add `task_resolve.v1`, keep `task_update.v1` and narrow its description to tags.
- **Partly done** (branch `docs/task-engine-openapi`, 4b51870d8, not on `main`): `task_resolve.v1` is registered, and PRD.md:922 and DESIGN.md:595 list `resolve`. Remaining: PRD.md:608 ("Providing input needs the `update` action"), DESIGN.md:986, :995, :1098, and the `task_update.v1` display name (still "input-wait resolution, API-consumer waits").

### G-WAI-05 — Resolve conflicts: error category and code; resolve of a non-input condition
- **Kind**: unclear
- **Docs**: Proposals › Waits › `resolve` ("same value again returns success … a different value is rejected"); PRD.md:728 (completion replay analog: `400`); DESIGN.md:1234; DESIGN.md:1766 (wait rows are live-only, deleted at the terminal transition); DESIGN.md:815 (resolved data stored on the condition until the next claim)
- **Scenarios**: [waits/input/1.02-positive-repeat-with-same-value-succeeds.md](waits/input/1.02-positive-repeat-with-same-value-succeeds.md), [waits/input/1.04-negative-resolve-state-refusals.md](waits/input/1.04-negative-resolve-state-refusals.md)
- **Assumed**: the accepted rules (same value, same subject → `200` + `Idempotency-Replayed`; different value or other subject → `409`; `timed_out`/`cancelled` → `400 failed_precondition`; kept until retention). Chosen by the scenarios: `409` is category `aborted` with reason `ALREADY_RESOLVED`; a non-input condition → `400 failed_precondition` (`STATE`).
- **Question**: Which category and reason code carry the `409`? What does resolve answer for a timer, event or children condition?
- **Decided** (gap review 2026-11-06): same value from the same subject → `200` with `Idempotency-Replayed: true`; a different value or another subject on a resolved condition → `400 failed_precondition` `{type: STATE, subject: ALREADY_RESOLVED}` (like a completion replay with a different result); a timer, event or children condition → `400 failed_precondition` `{type: STATE, subject: NOT_RESOLVABLE}`. Remaining: the doc edit and the resolve scenarios that expect `409`.

### G-WAI-06 — The limit on conditions per wait group has no name or value
- **Kind**: missing
- **Docs**: Proposals › API reads and responses › Bounded responses ("a limit on wait conditions per group", accepted); PRD.md:552; DESIGN.md:2108-2119 (wait config has no such setting)
- **Scenarios**: [waits/framework/1.07-negative-set-wait-refusals.md](waits/framework/1.07-negative-set-wait-refusals.md)
- **Assumed**: setting `wait_conditions.max_conditions_per_group`, value 16; over it → `400 invalid_argument` on `conditions`; also applies to the create-time group; an empty group is refused.
- **Question**: Name, default and error of the limit; does it apply to the initial group on create; is an empty group allowed?
- **Decided** (gap review 2026-11-06): config `wait.max_conditions_per_group`, default 16, also for the create-time group; empty group → `400 REQUIRED` on `/conditions`; over the limit → `400 TOO_MANY` on `/conditions`. Remaining: the doc edit.

### G-WAI-07 — ADR-0004 speaks of a group timeout; PRD and DESIGN have per-condition timeouts
- **Kind**: contradiction
- **Docs**: ADR-0004:54 ("A group may have a timeout whose `on_timeout` action…"), ADR-0004:102 ("an optional group timeout"); PRD.md:561 ("each condition MAY have a timeout"); DESIGN.md:365-366 (`WaitCondition.timeout`, `on_timeout`), DESIGN.md:823, DESIGN.md:1774
- **Scenarios**: [waits/event/1.05-negative-event-condition-validation.md](waits/event/1.05-negative-event-condition-validation.md), [waits/framework/1.10-flow-group-decision-outcomes.md](waits/framework/1.10-flow-group-decision-outcomes.md)
- **Assumed**: per-condition `timeout_secs` and `on_timeout`, with the group rule of PRD.md:561.
- **Question**: Fix ADR-0004:54 and :102 to the per-condition model (or add a group timeout to PRD and DESIGN).

### G-WAI-08 — Shape of the wait result per kind, of the park result and of the condition list
- **Kind**: wire
- **Docs**: PRD.md:550 ("event header, provided input, child counts, or a timeout note"); DESIGN.md:815, DESIGN.md:1096 ("`200` with resolved data"), DESIGN.md:1097 (list, no body), DESIGN.md:1784 (`resolved_payload` contents)
- **Scenarios**: [waits/children/1.05-flow-parent-resumes-after-last-child-with-counts.md](waits/children/1.05-flow-parent-resumes-after-last-child-with-counts.md), [waits/event/1.01-positive-matching-event-resumes-task-with-header-only.md](waits/event/1.01-positive-matching-event-resumes-task-with-header-only.md), [waits/event/1.12-guardrail-eval-budget-exhaustion-resolves-condition.md](waits/event/1.12-guardrail-eval-budget-exhaustion-resolves-condition.md), [waits/framework/1.02-positive-resume-claims-new-lease-same-attempt.md](waits/framework/1.02-positive-resume-claims-new-lease-same-attempt.md), [waits/framework/1.10-flow-group-decision-outcomes.md](waits/framework/1.10-flow-group-decision-outcomes.md), [waits/framework/1.03-positive-park-returns-result-when-already-satisfied.md](waits/framework/1.03-positive-park-returns-result-when-already-satisfied.md)
- **Assumed**: `resumed.conditions[].result` is `{ "value": … }` (input), `{ "event": { id, type, subject, subject_type, time, topic, partition, offset } }` (event), `{ "counts": { completed, failed, cancelled, dead_lettered } }` (children), `{ "resume_at": … }` (timer, not named by the docs), `{ "timed_out": true, "reason": "timeout" | "eval_budget_exceeded" }` (timed out), `null` (cancelled); the park result reuses the `resumed` shape; `GET /tasks/{id}/wait-conditions` returns `{ "items": [ { id, kind, state, timeout_at } ] }`. (The `resumed` envelope itself is S in CONVENTIONS §5.3 and covered by G-WIRE.)
- **Question**: Define the per-kind result, the park response and the list response.
- **Decided** (wire review 2026-11-04): claim entries carry `resumed { wait_group_id, outcome: satisfied|timed_out, conditions: [{ id, kind, state, result }] }` (`null` for a fresh claim); `result` is typed per kind: input `{ value }`, event `{ event: <header> }`, children `{ counts }`, timer `{ resume_at }`, timed out `{ timed_out: true, reason }`, `null` when cancelled; `GET /tasks/{id}/wait-conditions` returns each condition's `result` too. Remaining: the doc edit.

### G-WAI-09 — Which timer drives a timer condition, and is it under the 1 s bound?
- **Kind**: missing
- **Docs**: DESIGN.md:817 ("auto-satisfy when time reached (via TimerService)"), DESIGN.md:533 ("Delete wait timeout"), DESIGN.md:1808 (`te_timers` types: queue, assignment, execution, heartbeat, lifetime, sla, scheduled, wait, cancel_grace, cascade, children_check — none named for `resume_at`); PRD.md:1260 (timer-accuracy list omits wait timeouts and timer conditions); PRD.md:619 (children resume "within the timer-accuracy bound")
- **Scenarios**: [waits/children/1.02-guardrail-resume-within-bound-when-check-is-lost.md](waits/children/1.02-guardrail-resume-within-bound-when-check-is-lost.md), [waits/framework/1.01-positive-set-wait-parks-task.md](waits/framework/1.01-positive-set-wait-parks-task.md), [waits/timer/1.01-positive-timer-resumes-at-resume-at-past-returns-at-once.md](waits/timer/1.01-positive-timer-resumes-at-resume-at-past-returns-at-once.md), [waits/timer/1.04-guardrail-timer-wait-survives-restart.md](waits/timer/1.04-guardrail-timer-wait-survives-restart.md)
- **Assumed**: a durable timer row (type unnamed) fires at `resume_at`; timer conditions and wait timeouts fire within 1 s like other deadlines.
- **Question**: Is the timer condition a `wait` timer, a new type, or a `te_wait_conditions` scan on `timer_resume_at` (no index is listed for it, DESIGN.md:1793-1795)? Add wait timeouts, timer conditions and children-wait resumes to PRD.md:1260, with the 99 % / 1 s threshold of PRD.md:1262 (the children sweep is `children_check_sweep_secs` = 1 s, DESIGN.md:3009).
- **Decided** (gap review 2026-11-06): (corrected the same day) one `wait` row in `te_timers` per task at the earliest deadline among its pending parked conditions (`timeout_at` and `timer_resume_at`), re-armed after each fire, as the draft migration.sql has it (TimerService path: `bg_slot` sharding, claim leases, stale detection); `ix_te_waits_timeout` is only a safety sweep (G-OPS-46). A timeout on a timer condition → `400`. PRD.md:1260 adds wait timeouts, timer conditions and children-wait resumes (99 % within 1 s). Remaining: the doc edit.

### G-WAI-10 — A timer condition with `resume_at` in the past
- **Kind**: missing
- **Docs**: PRD.md:570; DESIGN.md:817
- **Scenarios**: [waits/timer/1.01-positive-timer-resumes-at-resume-at-past-returns-at-once.md](waits/timer/1.01-positive-timer-resumes-at-resume-at-past-returns-at-once.md)
- **Assumed**: accepted; the task parks and resumes within 1 s (no special case).
- **Question**: Accept and fire at once, reject (`400`), or treat as already satisfied and return the result with the lease kept?
- **Decided** (gap review 2026-11-06): accepted: a past `resume_at` is satisfied at once (G-WAI-13), not an error. Remaining: the doc edit.

### G-WAI-11 — Initial wait group on create: placement, events, admission and checks are not specified
- **Kind**: missing
- **Docs**: Proposals › Task mutability › Set only in the create request (accepted: "The create request can carry an initial wait group"); PRD.md:258 (placement rule has no `waiting`); DESIGN.md:523-525 (create rows: `scheduled`, `queued`, `blocked` only), DESIGN.md:2606-2610 (admission dimensions for `queued`, `scheduled`/`blocked` only), DESIGN.md:827 (event checks next to "the task `claim` / `update` check"); `schemas/types/gts.cf.core.events.event.v1~cf.core.tasks.task_created.v1~.schema.json` (`initial_state` enum `queued`, `scheduled`, `blocked`)
- **Scenarios**: [waits/framework/1.04-positive-initial-wait-group-resumes-created-task.md](waits/framework/1.04-positive-initial-wait-group-resumes-created-task.md)
- **Assumed**: a create with `wait` starts `waiting`, counts in `tenant_deferred`, has no queue timeout, and resumes to `queued` like a parked task; `park`/`lease_token` are not accepted in the create-time group.
- **Question**: Precedence of `wait` against predecessors (`blocked`) and `run_after` (`scheduled`) — reject the combination, or apply in order (blocked → scheduled → waiting)? When do condition timeouts start (create, or when the task would otherwise be queued)? Is a `children` condition allowed (a new task has no children)? Which checks does a create-time event condition run, with whose context and which task action? Add `waiting` to `initial_state` and to the create rows.
- **Decided** (gap review 2026-11-06): see G-CRT-01 (placement: reject the combinations; kinds; timeouts from create; deferred admission; `initial_state: waiting`). Still open for p2: the checks a create-time `event` condition runs (whose context, which action). Remaining: the doc edit; the p2 event-condition checks.

### G-WAI-12 — Lifecycle events name one condition; a group can have many
- **Kind**: unclear
- **Docs**: `schemas/types/…task_waiting.v1~` (`wait_condition_id`, `kind` required), `…wait_condition_set.v1~` (`wait_condition_id`, `kind`, `parked`), `…task_resumed.v1~` (no wait fields); PRD.md:879; DESIGN.md:923, DESIGN.md:1342-1343
- **Scenarios**: [waits/children/1.05-flow-parent-resumes-after-last-child-with-counts.md](waits/children/1.05-flow-parent-resumes-after-last-child-with-counts.md), [waits/event/1.01-positive-matching-event-resumes-task-with-header-only.md](waits/event/1.01-positive-matching-event-resumes-task-with-header-only.md), [waits/framework/1.01-positive-set-wait-parks-task.md](waits/framework/1.01-positive-set-wait-parks-task.md), [waits/framework/1.11-flow-arm-then-park.md](waits/framework/1.11-flow-arm-then-park.md)
- **Assumed**: one `waitConditionSet` per condition; `taskWaiting` once per park; not asserted for multi-condition groups.
- **Question**: One `waitConditionSet` per condition or per group? Which condition does `taskWaiting` name for a group? Is a second `waitConditionSet` (`parked: true`) written at park after an arm?
- **Decided** (gap review 2026-11-06): `waitConditionSet` once per condition when the group is set or armed (`wait_condition_id`, `kind`, `parked`); `taskWaiting` once per park with `mode` and `conditions[{wait_condition_id, kind}]` (replacing the single required fields); a park after an arm writes only `taskWaiting`. Remaining: the doc edit.

### G-WAI-13 — One-step set with an already-satisfied group, and park without an armed group
- **Kind**: missing
- **Docs**: PRD.md:557 (park of a satisfied armed group returns the result, lease kept), PRD.md:619 (children: "if there are none when it is parked, it is satisfied at once"); DESIGN.md:822, DESIGN.md:1096, DESIGN.md:3008
- **Scenarios**: [waits/children/1.01-positive-no-unfinished-children-satisfied-at-park.md](waits/children/1.01-positive-no-unfinished-children-satisfied-at-park.md), [waits/children/1.06-flow-armed-children-condition-evaluated-only-at-park.md](waits/children/1.06-flow-armed-children-condition-evaluated-only-at-park.md), [waits/framework/1.08-negative-park-without-armed-group.md](waits/framework/1.08-negative-park-without-armed-group.md), [waits/timer/1.01-positive-timer-resumes-at-resume-at-past-returns-at-once.md](waits/timer/1.01-positive-timer-resumes-at-resume-at-past-returns-at-once.md)
- **Assumed**: a one-step set (`park: true`) whose group is satisfied at once still parks (lease ends) and the task resumes at once with the result on the next claim; park without an armed group → `400 failed_precondition` (`STATE`).
- **Question**: Should a one-step set that is satisfied at once behave like park (return the result, keep the lease) instead? What does park answer when nothing is armed?
- **Decided** (gap review 2026-11-06): a set whose group resolves at once (under its `all`/`any` mode) behaves like parking a satisfied armed group: `200` with the result, lease kept, task stays `running`, no deferred admission; the satisfied group is consumed in that call: its conditions stay stored and listed as `satisfied` (G-WAI-14) but it is not current, so no claim returns it. Park with nothing armed → `400 failed_precondition` `{type: STATE, subject: NO_ARMED_WAIT}`, lease kept. Remaining: the doc edit and [waits/children/1.01](waits/children/1.01-positive-no-unfinished-children-satisfied-at-park.md), [waits/timer/1.01](waits/timer/1.01-positive-timer-resumes-at-resume-at-past-returns-at-once.md) (one-step set satisfied at once returns the result).

### G-WAI-14 — When does a resolved or dropped group stop being the task's wait group?
- **Kind**: missing
- **Docs**: DESIGN.md:815 (resolved data "stored on the wait condition and returned with the next claim"), DESIGN.md:822 ("A task has at most one wait group (armed or parked)"; park deletes a satisfied armed group), DESIGN.md:1766 (rows deleted at the terminal transition), DESIGN.md:1797 ("rows of one task are never added separately"); PRD.md:559 ("A task has at most one wait group")
- **Scenarios**: [waits/children/1.04-guardrail-child-reenqueued-after-parent-resumed.md](waits/children/1.04-guardrail-child-reenqueued-after-parent-resumed.md), [waits/framework/1.02-positive-resume-claims-new-lease-same-attempt.md](waits/framework/1.02-positive-resume-claims-new-lease-same-attempt.md), [waits/framework/1.03-positive-park-returns-result-when-already-satisfied.md](waits/framework/1.03-positive-park-returns-result-when-already-satisfied.md), [waits/framework/1.09-guardrail-armed-group-dropped-when-run-ends.md](waits/framework/1.09-guardrail-armed-group-dropped-when-run-ends.md)
- **Assumed**: a satisfied, timed-out or cancelled group no longer counts; after a resume the new holder may set a new group; the old rows stay until the claim reads them (or until the terminal transition).
- **Question**: Are resolved rows deleted at the claim that reads them, kept until terminal, or kept as history? Does "at most one wait group" mean one at a time or one per task lifetime?
- **Decided** (gap review 2026-11-06): one group at a time: a parked group's rows stay after it resolves, until a new group replaces them (set/arm) or the task ends; every claim of a task whose current group is a resolved parked group returns it in `claims[].resumed` (a lost claim response does not lose the result). A replaced group's rows stay stored for listing and resolve replay (Proposals › Waits keeps every condition until retention) but are no longer current. Remaining: the doc edit.

### G-WAI-15 — Does a checkpoint saved by set-wait record `taskCheckpointed`?
- **Kind**: unclear
- **Docs**: PRD.md:550, PRD.md:868 (history includes checkpoints); DESIGN.md:532 ("apply optional checkpoint")
- **Scenarios**: [waits/framework/1.01-positive-set-wait-parks-task.md](waits/framework/1.01-positive-set-wait-parks-task.md)
- **Assumed**: `context` is updated; the history event is not asserted.
- **Question**: Is `taskCheckpointed` written for a checkpoint carried by set-wait, park or release?
- **Decided** (gap review 2026-11-06): every write that saves a checkpoint records `taskCheckpointed` in the same transaction, whichever call carried it (checkpoint, set-wait, park, release). Remaining: the doc edit.

### G-WAI-16 — Is `taskTimedOut` recorded when a wait timeout fails the task?
- **Kind**: unclear
- **Docs**: PRD.md:561 (`waitTimedOut` is published), PRD.md:879 (`taskTimedOut` "when a task ends because of a timeout (§5.3)"); DESIGN.md:544 (extra event `waitTimedOut` only), DESIGN.md:2958
- **Scenarios**: [waits/framework/1.10-flow-group-decision-outcomes.md](waits/framework/1.10-flow-group-decision-outcomes.md)
- **Assumed**: `waitTimedOut` then the terminal event (`taskDeadLettered` / `taskFailed`); `taskTimedOut` not asserted.
- **Question**: Does a wait-group failure (member timeout or `eval_budget_exceeded`) also record and publish `taskTimedOut`?
- **Decided** (gap review 2026-11-06): when a wait timeout ends the task, `taskTimedOut { timeout_type: wait }` is recorded (new enum value) after `waitTimedOut`; an `eval_budget_exceeded` resolution records no `taskTimedOut`. Remaining: the doc edit.

### G-WAI-17 — Arm-then-park is p1, but only the event kind (p2) needs it; arming other kinds is undefined
- **Kind**: unclear
- **Docs**: PRD.md:546 (framework p1), PRD.md:554-559, PRD.md:577 (event p2), PRD.md:606, PRD.md:617 (input, children p2); DESIGN.md:822, DESIGN.md:3010 (armed children not evaluated); ADR-0004:58; Proposals › Waits › Event waits after the others
- **Scenarios**: [waits/framework/1.11-flow-arm-then-park.md](waits/framework/1.11-flow-arm-then-park.md)
- **Assumed**: arm/park is available for every kind; scenarios use the event kind.
- **Question**: May timer, input and children conditions be armed? Does an armed timer fire, and can an armed input be resolved, before park? Does arm ship in p1 when no p1 kind needs it?
- **Decided** (gap review 2026-11-06): arm is part of the p1 framework for every kind: an armed timer is satisfied once `resume_at` passes (checked at park, nothing fires while armed); an armed input can be resolved (G-WAI-21); an armed children condition is evaluated at park (DESIGN.md:3010); an armed event records matches. Remaining: the doc edit.

### G-WAI-19 — A task that starts `waiting` would be claimed as attempt 0
- **Kind**: contradiction
- **Docs**: DESIGN.md:533 (resume sets `resume_pending = true`), DESIGN.md:731, DESIGN.md:1491, DESIGN.md:2484 (`attempt + if resume_pending {0} else {1}`); PRD.md:661 ("`attempt` counts claims"); Proposals › Task mutability (initial wait group, accepted)
- **Scenarios**: [waits/framework/1.04-positive-initial-wait-group-resumes-created-task.md](waits/framework/1.04-positive-initial-wait-group-resumes-created-task.md)
- **Assumed**: the first claim of a task created with a wait group has `attempt` 1.
- **Question**: Do not set `resume_pending` when the waiting task was never claimed (or set it only on park)?
- **Decided** (gap review 2026-11-06): `resume_pending` is set only when the resumed group was set by a worker (park); a task resumed from a create-time group is claimed with `attempt` 1. The wait result (`claims[].resumed`) is delivered whenever the task's current group is a resolved parked group (G-WAI-14: kept until replaced or terminal), independent of `resume_pending`. Release is unaffected: it still sets `resume_pending` so the next claim keeps the attempt (DESIGN.md:732). Remaining: the doc edit.

### G-WAI-20 — May a worker set a wait on a `claimed` (not started) task?
- **Kind**: unclear
- **Docs**: PRD.md:548, PRD.md:550 (no state restriction for the worker); DESIGN.md:532 (`running` for the worker), DESIGN.md:813 ("on running tasks"); Proposals › Claiming and ordering, open question "Optional `start`"
- **Scenarios**: [waits/framework/1.07-negative-set-wait-refusals.md](waits/framework/1.07-negative-set-wait-refusals.md)
- **Assumed**: refused, `400 failed_precondition` (`STATE`), lease kept.
- **Question**: Is `claimed` a valid source state for a worker set-wait, especially if `start` becomes optional?
- **Decided** (gap review 2026-11-06): yes: `claimed` is a valid source for a worker set-wait (start is optional, G-EXE-11); parking clears the execution deadline. Remaining: the doc edit and [waits/framework/1.07](waits/framework/1.07-negative-set-wait-refusals.md).

### G-WAI-21 — Resolving an input condition of an armed (unparked) group
- **Kind**: unclear
- **Docs**: PRD.md:556 (matches while armed are recorded), DESIGN.md:822 (described for events), DESIGN.md:3010 (armed children condition not evaluated)
- **Scenarios**: [waits/input/1.01-positive-resolve-valid-value-resumes-task.md](waits/input/1.01-positive-resolve-valid-value-resumes-task.md)
- **Assumed**: resolve is specified only on a parked group; the armed case is mentioned in Notes and not asserted.
- **Question**: May an input condition be armed? If so, does a resolve while armed mark it `satisfied` without changing the task state, so that the later park returns the value with `parked: false`? (See also G-WAI-17.)
- **Decided** (gap review 2026-11-06): yes: resolving an armed input marks it `satisfied` without changing the task state; the later park returns the value with `parked: false`. Remaining: the doc edit.

### G-WAI-22 — How the provided input value counts against the payload limit
- **Kind**: unclear
- **Docs**: PRD.md:610 ("subject to the payload size limit (§5.1)"), PRD.md:260 (the limit is the aggregate `input + context + output`; resolve is not in its list of payload-changing writes), DESIGN.md:724 (same list, no resolve)
- **Scenarios**: [waits/input/1.03-negative-resolve-value-refusals.md](waits/input/1.03-negative-resolve-value-refusals.md)
- **Assumed**: the value alone is checked against `max_payload_size_bytes` (64 KiB); over it → `413 invalid_argument`, nothing changes.
- **Question**: Is the value measured alone or added to the task's aggregate payload? Is the resolve write revision-guarded like the other payload writes?
- **Decided** (gap review 2026-11-06): the value is measured alone against `max_payload_size_bytes` (over → `413`); it is stored on the condition, not in the task's aggregate payload; the write is guarded by the condition state (`pending → satisfied`), not the task revision. Remaining: the doc edit.

### G-WAI-23 — Mode and result code of the children cancel cascade
- **Kind**: unclear
- **Docs**: PRD.md:352, PRD.md:826, PRD.md:828; DESIGN.md:549-551, DESIGN.md:2955 ("cancel through cascade(t, children)"), DESIGN.md:2972-2985
- **Scenarios**: [hierarchy/children/1.03-positive-parent-cancel-cascade-to-children.md](hierarchy/children/1.03-positive-parent-cancel-cascade-to-children.md)
- **Assumed**: unleased children end `cancelled` / `cancelled`; a leased child of a `requested` cancel only gets `cancel_requested: true`; on a forced parent cancel every child ends `cancelled` exactly once (result code not asserted).
- **Question**: Does the cascade apply the parent's cancel mode to each child? Does a leased child get `cancel_requested` and a `cancel_grace` timer, or is it ended at once? Which result code do cascaded children get (`cancelled` or `forcibly_cancelled`)?
- **Decided** (gap review 2026-11-06): the cascade applies the parent's cancel mode to each unfinished child: unleased children end `cancelled` at once; leased children get `cancel_requested` (plus a `cancel_grace` timer for `graceful`); `forced` ends every child `forcibly_cancelled`. Remaining: the doc edit.

### G-WAI-24 — Re-enqueueing a child whose parent already resumed or ended
- **Kind**: missing
- **Docs**: DESIGN.md:545 (admin re-enqueue), DESIGN.md:3003 (counts read once at resolve), PRD.md:813 (successors not revived; nothing similar for children), PRD.md:826 (`CHILDREN_PENDING`), PRD.md:828 ("no orphans")
- **Scenarios**: [waits/children/1.04-guardrail-child-reenqueued-after-parent-resumed.md](waits/children/1.04-guardrail-child-reenqueued-after-parent-resumed.md)
- **Assumed**: re-enqueue is allowed; the parent's resume counts stay as they were; a `wait_for_children` parent's success is then refused `400 CHILDREN_PENDING` until the child ends again.
- **Question**: May a child be re-enqueued while its parent is `waiting` (does it reopen the condition?) or terminal (a live child no parent cancel can reach)? Refuse the re-enqueue, or notify the parent?
- **Decided** (gap review 2026-11-06): re-enqueue of a child is allowed while its parent is non-terminal and does not reopen a resolved children condition (a `wait_for_children` parent's success is then refused `CHILDREN_PENDING` until the child ends); refused when the parent is terminal: `400 failed_precondition` `{type: STATE, subject: PARENT_FINISHED}`. Remaining: the doc edit.

### G-WAI-25 — permission_denied cannot carry the field violation DESIGN asks for
- **Kind**: contradiction
- **Docs**: DESIGN.md:832, DESIGN.md:1222, libs/toolkit-canonical-errors/src/context.rs (`PermissionDeniedV1` has only `reason`)
- **Scenarios**: [waits/event/1.06-guardrail-event-wait-authorization-refusals.md](waits/event/1.06-guardrail-event-wait-authorization-refusals.md)
- **Assumed**: `context: { reason }` only; the condition index and the failing check (`topic` / `event_type` / `subject_type`) go in `detail`.
- **Question**: Should the canonical permission_denied context gain field or precondition violations, or should DESIGN drop the "field violation names the condition index" wording and say where the index goes?
- **Decided** (gap review 2026-11-06): see G-WIRE-15: no field violation on `permission_denied`; the condition index goes only into `detail`. Remaining: the doc edit.

### G-WAI-26 — Two worker 403s on the wait endpoint with opposite lease outcomes
- **Kind**: unclear
- **Docs**: DESIGN.md:835, DESIGN.md:1218, Proposals › Heartbeats and leases › Revocation releases the task
- **Scenarios**: [waits/event/1.06-guardrail-event-wait-authorization-refusals.md](waits/event/1.06-guardrail-event-wait-authorization-refusals.md)
- **Assumed**: `EVENT_CONSUME_DENIED` / `EVENT_CONSUME_UNVERIFIABLE` keep the lease; a 403 from a revoked task scope releases the task; the worker tells them apart by `context.reason`.
- **Question**: Should the docs state that a worker must branch on `context.reason` for a 403, and give the revocation 403 its own reason code (none is named today)?
- **Decided** (gap review 2026-11-06): settled by G-SEC-19 and G-EXE-08: the revocation `403` has reason `CLAIM_REVOKED` (release), the consume denials keep the lease; workers branch on the reason. Remaining: the doc edit.

### G-WAI-27 — Event-wait checks still say "claim / update"
- **Kind**: contradiction
- **Docs**: DESIGN.md:827 (also DESIGN.md:986, :1095), Proposals › Task mutability
- **Scenarios**: [waits/event/1.06-guardrail-event-wait-authorization-refusals.md](waits/event/1.06-guardrail-event-wait-authorization-refusals.md)
- **Assumed**: only the worker `claim` check, plus the three `consume` checks.
- **Question**: Edit DESIGN.md:827 to drop `update` (consumer-set waits are gone, see G-WAI-01)?

### G-WAI-28 — Every constraint dropped by the tenant filter: denied or unverifiable?
- **Kind**: missing
- **Docs**: DESIGN.md:830-832
- **Scenarios**: [waits/event/1.06-guardrail-event-wait-authorization-refusals.md](waits/event/1.06-guardrail-event-wait-authorization-refusals.md)
- **Assumed**: not asserted; the scenario uses a group predicate (`EVENT_CONSUME_UNVERIFIABLE`).
- **Question**: When every ORed constraint excludes the task's owner tenant (so none is left), is the answer `EVENT_CONSUME_DENIED` or `EVENT_CONSUME_UNVERIFIABLE`?
- **Decided** (gap review 2026-11-06): `EVENT_CONSUME_DENIED`: when the tenant filter leaves no constraint, nothing allows the consume. Remaining: the doc edit.

### G-WAI-29 — Event type resolution: whose visibility, and is UNKNOWN_EVENT_TYPE an oracle?
- **Kind**: missing
- **Docs**: DESIGN.md:913, DESIGN.md:1220 (compare task types at DESIGN.md:1047, "resolved with the caller's context")
- **Scenarios**: [waits/event/1.05-negative-event-condition-validation.md](waits/event/1.05-negative-event-condition-validation.md)
- **Assumed**: an unregistered type gets `400 UNKNOWN_EVENT_TYPE` before any PDP call.
- **Question**: Is the event type resolved with the caller's GTS visibility? Does a type that exists but is hidden from the caller get the same `400`?
- **Decided** (gap review 2026-11-06): the event type resolves with the caller's GTS visibility; a type hidden from the caller gets the same `400 UNKNOWN_EVENT_TYPE` as an unregistered one. Remaining: the doc edit.

### G-WAI-30 — Request field name for the subject type, and wildcard `allowed_subject_types`
- **Kind**: wire
- **Docs**: DESIGN.md:381, DESIGN.md:826, DESIGN.md:1221, gears/system/event-broker/docs/PRD.md:284
- **Scenarios**: [waits/event/1.05-negative-event-condition-validation.md](waits/event/1.05-negative-event-condition-validation.md)
- **Assumed**: the request field is `subject_type` (CONVENTIONS §5.8); field violations name `conditions[N].subject_type`.
- **Question**: Is the field `subject_type` or `event_subject_type`? If the event type's single `allowed_subject_types` entry is a wildcard pattern, is it still "the single entry", or is an explicit value required?
- **Decided** (gap review 2026-11-06): the request field is `subject_type` (as in the draft OpenAPI); a single `allowed_subject_types` entry that is a wildcard pattern does not count as "the single entry": an explicit `subject_type` is required (`400 EVENT_SUBJECT_TYPE_REQUIRED`). Remaining: the doc edit.

### G-WAI-31 — 503 for an unreachable PDP on set or arm: Retry-After, and the PRD conflict
- **Kind**: contradiction
- **Docs**: PRD.md:582, PRD.md:918, DESIGN.md:833, DESIGN.md:1215
- **Scenarios**: [waits/event/1.06-guardrail-event-wait-authorization-refusals.md](waits/event/1.06-guardrail-event-wait-authorization-refusals.md)
- **Assumed**: `503 service_unavailable`, no `Retry-After` asserted; nothing stored; lease kept.
- **Question**: Is `Retry-After` sent? Does PRD.md:918 (an unreachable PDP gives `403`) yield to the event-wait-specific `503`? Ties to the Open PDP-outage item in Proposals › Heartbeats and leases.
- **Decided** (gap review 2026-11-06): see G-SEC-01: `503` with `Retry-After`; PRD.md:918 yields (changes to `503`). Remaining: the doc edit.

### G-WAI-32 — Default `on_timeout` when a condition omits it
- **Kind**: missing
- **Docs**: PRD.md:561, PRD.md:583, DESIGN.md:366, DESIGN.md:836; the draft reference DDL (`migration.sql`, kept outside this branch) (`te_wait_conditions.on_timeout_code` NULL = no timeout)
- **Scenarios**: [waits/event/1.03-positive-no-timeout-gets-max-wait-with-fail.md](waits/event/1.03-positive-no-timeout-gets-max-wait-with-fail.md)
- **Assumed**: an event condition with neither a timeout nor an action gets `max_wait_secs` and `on_timeout = fail` (DESIGN.md:836); nothing else asserted.
- **Question**: What is the default `on_timeout` for a condition of any kind that sets a timeout but no action? Is `on_timeout` without a timeout allowed?
- **Decided** (gap review 2026-11-06): a condition with a timeout and no `on_timeout` gets `fail`; `on_timeout` without a timeout → `400` on that field. For the `event` kind an omitted timeout means `event_waits.max_wait_secs` (DESIGN.md:836), so `on_timeout` alone is valid there; the `400` applies to the other kinds. Remaining: the doc edit.

### G-WAI-33 — Retry-After on `EVENT_WAIT_LIMIT`
- **Kind**: missing
- **Docs**: PRD.md:585, DESIGN.md:814, DESIGN.md:1216
- **Scenarios**: [waits/event/1.08-guardrail-event-wait-limits-keep-lease.md](waits/event/1.08-guardrail-event-wait-limits-keep-lease.md)
- **Assumed**: `429` with `Retry-After: 30` per CONVENTIONS §6, `context.reason = EVENT_WAIT_LIMIT`, quota subject `unkeyed`.
- **Question**: Does `EVENT_WAIT_LIMIT` carry `Retry-After`, and how is it computed (places free only when other conditions resolve)?
- **Decided** (gap review 2026-11-06): see G-SEC-03: `EVENT_WAIT_LIMIT` carries `Retry-After` = `retry_after_secs` jittered uniformly in [base/2, base]. Remaining: the doc edit.

### G-WAI-34 — Match-expression rejection: reason code and check order
- **Kind**: missing
- **Docs**: PRD.md:594, DESIGN.md:1340, DESIGN.md:2391, ADR-0007:58
- **Scenarios**: [waits/event/1.05-negative-event-condition-validation.md](waits/event/1.05-negative-event-condition-validation.md)
- **Assumed**: `400 invalid_argument`, field violation on `conditions[0].match_expression` with an invented reason `INVALID_MATCH_EXPRESSION`; no PDP call.
- **Question**: Which reason code(s) cover parse errors and the `cel.*` limits? Does expression validation run before the consume PDP calls (DESIGN.md:1340 lists only the waitable, subject-type and timeout checks before them)?
- **Decided** (gap review 2026-11-06): one reason `INVALID_MATCH_EXPRESSION` for parse errors and `cel.*` limit breaches (the description names the rule); expression validation runs before the consume PDP calls. Remaining: the doc edit.

### G-WAI-35 — "Consecutive" evaluation failures, and no reason on `waitTimedOut`
- **Kind**: unclear
- **Docs**: PRD.md:599, DESIGN.md:917, DESIGN.md:1790, ADR-0007:68, schemas/types/gts.cf.core.events.event.v1~cf.core.tasks.wait_timed_out.v1~.schema.json (`wait_condition_id`, `on_timeout` only)
- **Scenarios**: [waits/event/1.12-guardrail-eval-budget-exhaustion-resolves-condition.md](waits/event/1.12-guardrail-eval-budget-exhaustion-resolves-condition.md)
- **Assumed**: ten overruns in a row resolve the condition; only a match resets the counter; `waitTimedOut` carries no reason; the claim's `resumed` result carries `reason: eval_budget_exceeded`.
- **Question**: Does a `false` result or a run-time error between overruns break the run? Should `waitTimedOut` (and history) carry `reason: timeout | eval_budget_exceeded`?
- **Decided** (gap review 2026-11-06): an overrun increments the counter; any in-budget evaluation (true or false) resets it; a run-time error is no match and neither counts nor resets (ADR-0007:67, :89); `waitTimedOut` and its history row carry `reason: timeout | eval_budget_exceeded`. Remaining: the doc edit.

### G-WAI-36 — No reason code (or settled category) for an unsatisfiable dependency set
- **Kind**: missing
- **Docs**: PRD.md:839, DESIGN.md:730, DESIGN.md:1234, DESIGN.md:2637 ("400"), DESIGN.md:2902 (`failed_precondition`); the error table DESIGN.md:1195-1236 has no row for it
- **Scenarios**: [hierarchy/dependencies/1.01-positive-create-with-predecessors-initial-state.md](hierarchy/dependencies/1.01-positive-create-with-predecessors-initial-state.md)
- **Assumed**: `400 failed_precondition` with `violations[{type: "STATE", subject: "predecessors[0]", …}]`, no `context.reason`.
- **Question**: Which reason code (for example `DEPENDENCY_UNSATISFIABLE`)? Is the category `failed_precondition` or `invalid_argument`? Add a row to the error table.
- **Decided** (gap review 2026-11-06): `400 failed_precondition` `{type: STATE, subject: DEPENDENCY_UNSATISFIABLE}`; add the row to the error table. Remaining: the doc edit.

### G-WAI-37 — `on_predecessor_failure: ignore` has no storage and no cascade branch
- **Kind**: missing
- **Docs**: DESIGN.md:1556 (`te_task_results.predecessors`), DESIGN.md:1858-1866 (`te_dependencies` has no column for it), DESIGN.md:2656, DESIGN.md:2911-2916 (`on_predecessor_terminal` has no `ignore` branch); Proposals › Task mutability › Set only in the create request; the draft reference DDL (`migration.sql`, kept outside this branch) (`te_dependencies.on_predecessor_failure_code`; `outcome_code` 0 pending / 1 succeeded / 2 failed_ignored / 3 failed replaces `satisfied`)
- **Scenarios**: [hierarchy/dependencies/1.01-positive-create-with-predecessors-initial-state.md](hierarchy/dependencies/1.01-positive-create-with-predecessors-initial-state.md), [hierarchy/dependencies/1.02-positive-predecessor-outcome-moves-successor.md](hierarchy/dependencies/1.02-positive-predecessor-outcome-moves-successor.md), [hierarchy/dependencies/1.10-flow-fan-out-then-join.md](hierarchy/dependencies/1.10-flow-fan-out-then-join.md)
- **Assumed**: the setting is per link, stored on the link and returned in `predecessors[].on_predecessor_failure`; a failed, cancelled or dead-lettered predecessor on an `ignore` link sets `satisfied = true`, at create and in the cascade; in `any` mode an ignored failure counts as a success and unblocks the successor.
- **Question**: Which column holds it? How does `on_predecessor_terminal` treat `ignore` in `all` and `any` mode — does `any` + `ignore` unblock on a failure? Should the task resource show each link's outcome?
- **Decided** (gap review 2026-11-06): confirm the draft: `te_dependencies.on_predecessor_failure_code` per link and `outcome_code` (pending / succeeded / failed_ignored / failed); an `ignore` link counts a failed, cancelled or dead-lettered predecessor as satisfied, at create and in the cascade, in `all` and `any` mode alike; the task resource shows `predecessors[].outcome`. Remaining: the doc edit.

### G-WAI-38 — A parent that ends failed, dead-lettered or timed out does not reach its children
- **Kind**: unclear
- **Docs**: PRD.md:826 (only "Cancelling a parent cancels its unfinished children"), PRD.md:1546 ("reaches children and successors" on every terminal path), DESIGN.md:2955 (children cancelled only when state = `cancelled`)
- **Scenarios**: [hierarchy/children/1.04-positive-parent-failure-cancels-children-gracefully.md](hierarchy/children/1.04-positive-parent-failure-cancels-children-gracefully.md)
- **Assumed**: the children keep running.
- **Question**: Is this intended? Should a failed parent with `propagate_cancel: true` cancel its unfinished children?
- **Decided** (gap review 2026-11-06): a parent with `propagate_cancel: true` that ends in any non-success terminal state (failed, dead-lettered, timed out, cancelled) cancels its unfinished children through the cascade in `graceful` mode (forced parent cancel stays forced); a successful parent cascades nothing. Remaining: the doc edit.

### G-WAI-39 — No maximum hierarchy depth
- **Kind**: missing
- **Docs**: PRD.md:1606 (open question), DESIGN.md:2989; no config key in DESIGN.md:2100-2160
- **Scenarios**: [hierarchy/children/1.02-positive-nested-children-up-to-max-depth.md](hierarchy/children/1.02-positive-nested-children-up-to-max-depth.md)
- **Assumed**: grandchildren are accepted; no depth limit is enforced.
- **Question**: What is the limit and its error? Should the depth also bound a cascade's total work?
- **Decided** (gap review 2026-11-06): config `max_hierarchy_depth`, default 10; a create past it → `400 TOO_DEEP` on `/parent_task_id`; depth is stored on the task (`hierarchy_depth` = parent's + 1). Cascade work stays bounded by the batched durable cascade, not by depth. A root task has depth 0; the depth check runs after the parent lookup, so a missing parent is still `404`. Remaining: the doc edit.

### G-WAI-40 — Default `dependency_mode` is not stated
- **Kind**: missing
- **Docs**: PRD.md:837, DESIGN.md:415, DESIGN.md:1863
- **Scenarios**: [hierarchy/dependencies/1.01-positive-create-with-predecessors-initial-state.md](hierarchy/dependencies/1.01-positive-create-with-predecessors-initial-state.md)
- **Assumed**: omitted → `all`.
- **Question**: What is the default? Is `dependency_mode` without `predecessors` rejected or ignored?
- **Decided** (wire review 2026-11-04): `dependency_mode` defaults to `all`. Remaining: the doc edit.

### G-WAI-41 — No reason code for a cross-domain parent/child or dependency
- **Kind**: missing
- **Docs**: PRD.md:1182, PRD.md:1506, DESIGN.md:1223, DESIGN.md:1224
- **Scenarios**: [hierarchy/children/1.08-negative-child-in-another-domain-parent-not-found.md](hierarchy/children/1.08-negative-child-in-another-domain-parent-not-found.md)
- **Assumed**: `400 invalid_argument`, `context.reason = "TASK_DOMAIN_MISMATCH"` (borrowed from DESIGN.md:1224), field violation on `queue`.
- **Question**: Reuse `TASK_DOMAIN_MISMATCH` or a new code? Which field is named?
- **Decided** (gap review 2026-11-06): no separate code: a create looks up linked tasks only in the new task's domain, so a parent or predecessor in another domain is not found there and gets the uniform `404` (no cross-domain probe on the create path). PRD.md:1182 changes from `400` to `404`. Remaining: the doc edit.

### G-WAI-42 — The same predecessor listed twice in one create
- **Kind**: missing
- **Docs**: PRD.md:837, DESIGN.md:1866 (PK `(task_id, predecessor_id)`), DESIGN.md:2633-2636
- **Scenarios**: [hierarchy/dependencies/1.06-guardrail-no-cycle-possible-at-create.md](hierarchy/dependencies/1.06-guardrail-no-cycle-possible-at-create.md)
- **Assumed**: not asserted; mentioned in Notes.
- **Question**: Reject with `400`, or merge (and which `on_predecessor_failure` wins)? As written, the PK conflict would surface as an internal error.
- **Decided** (gap review 2026-11-06): `400 invalid_argument` `DUPLICATE` on the second occurrence (`/predecessors/i`). Remaining: the doc edit.

### G-WAI-43 — p1 refusal of p2 wait features: reason code, event kind, `wait_for_children`
- **Kind**: missing
- **Docs**: Proposals › Waits › Input and children waits stay p2 ("setting an input or children condition, and `parent_task_id` on create, return `400` naming the feature"); DESIGN.md:715 (the `authz_scope` precedent); PRD.md:577 (event waits p2)
- **Scenarios**: [waits/framework/1.12-guardrail-p2-wait-kinds-and-parent-refused-in-p1.md](waits/framework/1.12-guardrail-p2-wait-kinds-and-parent-refused-in-p1.md)
- **Assumed**: `400 invalid_argument`, field violation on the offending field, `reason: FEATURE_NOT_AVAILABLE`; an `event` condition is refused the same way in p1; `wait_for_children: true` without a parent is accepted and stored.
- **Question**: Name the reason code. Is the event kind refused the same way? What does p1 do with `wait_for_children` and `propagate_cancel` on create?
- **Decided** (gap review 2026-11-06): `400 invalid_argument` with `FEATURE_NOT_AVAILABLE` on the exact field (`/wait/conditions/i/kind`, `/parent_task_id`); the `event` kind is refused the same way in p1; `wait_for_children` and `propagate_cancel` are accepted and stored (inert without children). Remaining: the doc edit.

## SEC — Auth, errors, search, reads, events

### G-SEC-01 — PDP unreachable on an ordinary request: 403 or 503
- **Kind**: proposal-open
- **Docs**: PRD.md:918, DESIGN.md:245 (`403`); PRD.md:961, DESIGN.md:1022 (`503` for worker calls); DESIGN.md:1215 (`503` for creator and event-wait checks); Proposals › Heartbeats and leases › Open "PDP outage: which operations must keep working, and is `503` the code?"-192)
- **Scenarios**: [auth/1.09-negative-pdp-unreachable-fails-closed.md](auth/1.09-negative-pdp-unreachable-fails-closed.md), [ERRORS.md](ERRORS.md)
- **Assumed**: `503 service_unavailable`, nothing returned, nothing written; tests pin only "fail closed, not 2xx" until the item is decided.
- **Question**: Is an unreachable PDP `403` or `503` on create, read, search, cancel and admin calls? Align PRD.md:918 and DESIGN.md:245 with DESIGN.md:1022/:1215, or the reverse.
- **Decided** (gap review 2026-11-06): `503 service_unavailable` with `Retry-After` (`unavailable_retry_after_secs`) on every endpoint; nothing written, leases kept. Option A (fail closed) for p1; a cached-decision path (B) waits for the platform's decision-caching protocol. PRD.md:918 and DESIGN.md:245 change to `503`. Remaining: the doc edit and auth/1.09 (pin `503`).

### G-SEC-02 — Reason codes in `context.reason` cannot be produced for three categories
- **Kind**: contradiction
- **Docs**: DESIGN.md:1212, :1216, :1217, :1224, :1226, :1227, :1230, :1233 (`context.reason = …` on `invalid_argument`, `resource_exhausted`, `failed_precondition`); DESIGN.md:1200 (field violation on `not_found`); `docs/arch/errors/categories/03-invalid-argument.md:103-135`, `08-resource-exhausted.md:64-83`, `09-failed-precondition.md:66-85`, `05-not-found.md:51-66` (closed contexts, no `reason`); `libs/toolkit-canonical-errors/src/context.rs:242-270` (`ResourceExhaustedV1`/`FailedPreconditionV1` carry only `violations`); CONVENTIONS.md:380
- **Scenarios**: ERRORS.md, 1.02, 1.05, 1.08, 1.11; every scenario of other areas that asserts `QUEUE_DEPTH_LIMIT`, `CONCURRENCY_LIMIT`, `EVENT_WAIT_LIMIT`, `STALE_ORDER`, `CHILDREN_PENDING`, `UNKNOWN_TASK_TYPE`, `TASK_DOMAIN_MISMATCH`, `DEPENDENCY_GRAPH_TOO_LARGE`
- **Assumed**: scenarios write `context.reason` as CONVENTIONS §6 says, and add the platform's required arrays (`violations[]` with the dimension as `subject`; `violations[{type: STATE}]`; `field_violations[]`).
- **Question**: Where does a Task-Engine code go for these categories: `field_violations[].reason` / `violations[].type` / `violations[].subject` (what the library can emit today), or does the platform add `reason` to these contexts? A parent/predecessor "field violation" on `not_found` has the same problem.
- **Decided** (gap review 2026-11-06): see G-WIRE-14 (codes in `field_violations[].reason`, `violations[{type: STATE, subject}]`, quota `violations[].subject`); a `not_found` carries no field violation (G-WIRE-15). Remaining: the doc edit and the scenarios.

### G-SEC-03 — Retry-After on 429 and the retry hint on 503
- **Kind**: contradiction
- **Docs**: PRD.md:451, :1492, DESIGN.md:1226-1227, :1232 (`429` with `Retry-After`); `libs/toolkit-canonical-errors/src/context.rs:36-42` and `src/problem.rs:715-745` (header only for `service_unavailable`; `429` hint per violation); `docs/arch/errors/categories/14-service-unavailable.md:47` (`retry_after_seconds` required); no `Retry-After` value anywhere in the Task Engine docs
- **Scenarios**: [ERRORS.md](ERRORS.md), [auth/1.09-negative-pdp-unreachable-fails-closed.md](auth/1.09-negative-pdp-unreachable-fails-closed.md)
- **Assumed**: `Retry-After: 5` header plus `violations[].retry_after_seconds: 5` on `429` (one depth sampling interval); `retry_after_seconds: 5` on `503`.
- **Question**: How is the `429` header produced (override, library change)? What value per reason (`QUEUE_DEPTH_LIMIT`, `CONCURRENCY_LIMIT`, `EVENT_WAIT_LIMIT`) and for each `503` cause?
- **Decided** (gap review 2026-11-06): library change: `IntoResponse` also emits `Retry-After` for `resource_exhausted`, set to the largest `violations[].retry_after_seconds`; platform doc 08 lists that field. Values: `CONCURRENCY_LIMIT` = `claim_poll_interval_ms` rounded up to seconds; `QUEUE_DEPTH_LIMIT` and `EVENT_WAIT_LIMIT` = `retry_after_secs` (30) jittered uniformly in [base/2, base]; a long-poll `429` uses the same; `503` (PDP or registry unreachable) = new `unavailable_retry_after_secs`, default 5. Remaining: the library change and the doc edit.

### G-SEC-04 — Search view fields do not match the task resource
- **Kind**: unclear
- **Docs**: DESIGN.md:1165-1170 (views name `assignee`, `output`); DESIGN.md:280-311 / CONVENTIONS §5.1 (resource has `assignment`, `result`); PRD.md:374 (search shows `verified`, but no view lists `resource_refs`); DESIGN.md:1145 (`terminal_at` sortable but in no view); DESIGN.md:1012 (`assignee_subject`)
- **Scenarios**: search/1.01, 1.04, 1.06, 1.11, 1.14; auth/1.02
- **Assumed**: `assignee` = assignee subject or `null`; `long` returns `output`; `resource_refs` appear from `long` up.
- **Question**: Give each view's exact member list against the task resource: `assignee` (subject, worker, object?), `output` vs `result` (result code and error?), and where `resource_refs`, `terminal_at`, `owner_tenant_id`, `parent_task_id`, `cancel_requested` appear.
- **Decided** (wire review 2026-11-04): search items use the task resource's names: `short` adds `assignment`, `long` adds `result` and `resource_refs` (not DESIGN's `assignee`/`output`). Remaining: the doc edit.

### G-SEC-05 — OData syntax of the tag, resource-ref, dependency and new link filters
- **Kind**: wire
- **Docs**: DESIGN.md:1138 (`tags` key/value, `resource_refs`, `dependencies` "via dedicated filterable projections"); Proposals › API reads and responses › Bounded responses (`parent_task_id`, `predecessor_id` filters)
- **Scenarios**: search/1.02, 1.04, 1.09; auth/1.16, 1.26
- **Assumed**: `tags/any(t: t/key eq '<key>' and t/value eq '<value>')`, `resource_refs/any(r: r/resource_type eq '…' and r/resource_id eq '…')`, `parent_task_id eq <uuid>`, `predecessor_id eq <uuid>`; the window applies to the link filters.
- **Question**: Which expressions does `ODataFilterable` accept for these fields (lambda operators are not described in `docs/toolkit_unified_system/07_odata_pagination_select_filter.md`)? Is the window applied to `parent_task_id`/`predecessor_id` searches, and which index serves them?
- **Decided** (gap review 2026-11-06): toolkit-odata has no lambda operators, so the engine adds filter functions the existing grammar parses as `Function(name, args)`: `has_tag('key')`, `has_tag('key', 'value')`, `has_resource_ref('type', 'id')`, turned into `EXISTS` semi-joins on `te_tags` / `te_resource_refs`; `parent_task_id eq <uuid>` and `predecessor_id eq <uuid>` are plain fields. The `created_at` window applies to every search (indexes `ix_te_task_state_parent`, `ix_te_deps_predecessor`, ring table `te_task_predecessors`). Clarified: "the window applies to every search" means the designed window (DESIGN.md:2824 still exempts unfinished-only state filters) plus the dead-lettered exemption of G-RTY-10. Remaining: the doc edit, both OpenAPI files and the search scenarios that use `any()`.

### G-SEC-06 — Members and caps of the lists embedded at `view=full`
- **Kind**: wire
- **Docs**: DESIGN.md:1170, :1172, :2146; PRD.md:1136, :1147; Proposals › API reads and responses › Bounded responses ("every list embedded at detail `full` is capped"); the draft reference DDL (`migration.sql`, kept outside this branch) (reuses `search_full_event_cap` for embedded wait conditions and entries)
- **Scenarios**: [search/1.04-positive-views.md](search/1.04-positive-views.md)
- **Assumed**: members `events` (50 most recent, newest first), `wait_conditions`, `entries`, `dependencies`.
- **Question**: Name the members, the order of the capped events, and the caps (config keys and defaults) for entries and wait conditions at `full`.
- **Decided** (wire review 2026-11-04): `full` embeds `events` (newest 50, `events_truncated`), the current group's `wait_conditions`, `entries` (newest 20, `entries_truncated`) and all `dependencies` (≤ 64). Remaining: the doc edit.

### G-SEC-07 — Event history paging: names, limits and order
- **Kind**: missing
- **Docs**: DESIGN.md:1108, :1837; PRD.md:868, :1075; Proposals › API reads and responses › Bounded responses; the draft reference DDL (`migration.sql`, kept outside this branch) (`id asc` serves `timestamp asc, id asc`: UUID v7 ids, `occurred_at` = id time)
- **Scenarios**: events/1.01, 1.02, 1.03
- **Assumed**: the REST paging guideline through the ToolKit OData extractor: `limit`/`cursor`, cap 100 (clamped), `limit=0` → `400`; `$filter` on `event_type` (`eq`, `in`) and `timestamp` (range); order oldest first by event ID, no `$orderby`. Default page size is G-SEC-25.
- **Question**: Confirm the filter set and the order of the history, and the cap.
- **Decided** (gap review 2026-11-06): history reads (`/tasks/{id}/events`, `/queues/{id}/events`) follow the REST paging guideline through the ToolKit extractor: `limit`/`cursor`, default 25 (the 2026-11-04 decision in G-SEC-25 stands; the prompt's "20" was a slip), clamped to 100; `$filter` on `event_type` (`eq`, `in`) and `timestamp` (range); oldest first by event ID, no `$orderby`. Remaining: the doc edit.

### G-SEC-08 — DLQ inspection through search
- **Kind**: unclear
- **Docs**: Proposals › API reads and responses › Bounded responses (`GET /admin/dlq` removed); DESIGN.md:1127, :1132 (DLQ listing fanned out across domains, needed `admin`); DESIGN.md:1161, PRD.md:1143 (window applies to terminal states); DESIGN.md:2223, :2835 (dead-letter queues not routed; queue filter picks one domain)
- **Scenarios**: [dead-letter/1.02-positive-list-dead-lettered-by-search.md](dead-letter/1.02-positive-list-dead-lettered-by-search.md)
- **Assumed**: `state eq 'dead_lettered'` search with the normal window and `read`; a `queue` filter on a dead-letter queue searches all domains.
- **Question**: Is it acceptable that a task created more than 31 days ago cannot be found in the DLQ by search (or should `dead_lettered` be exempt, or windowed by `terminal_at`)? Is DLQ inspection now a `read` right? How does a dead-letter `queue` filter choose domains?
- **Decided** (gap review 2026-11-06): see G-RTY-10 (dead-lettered-only searches skip the window; `read` lists) and G-OPS-23 (a dead-letter `queue` filter searches every domain). Remaining: the doc edit.

### G-SEC-09 — Published event ID and subject type cannot follow the PRD with today's SDK
- **Kind**: contradiction
- **Docs**: PRD.md:883, :885; DESIGN.md:875, :1839 (published `id` = `te_events.id`, concrete `subject_type`); DESIGN.md:892-903 (TODO #5491: SDK stamps its own ID and a fixed subject type; options A/B/C)
- **Scenarios**: events/1.03, 1.10
- **Assumed**: option (A): published `id` equals the history ID and `subject_type` is the task's concrete type.
- **Question**: Which option ships in p1? Under (B), consumers dedup on `data.event_id` and the schemas gain `event_id` and `task_type`; the scenarios would change.
- **Decided** (gap review 2026-11-06): option (A) ships in p1: `TypedEvent::event_id()` and `TypedEvent::subject_type()` with backward-compatible defaults; the Task Engine team writes the #5491 SDK PR for Event Broker review. No fallback (B). Remaining: the #5491 PR and the doc edit (remove the TODO).

### G-SEC-10 — Lifecycle events of barrier tenants are refused by the broker today
- **Kind**: contradiction
- **Docs**: PRD.md:889, :1542; DESIGN.md:887, :890 (TODO #5492: broker publish tenant check always `Respect`); ADR-0016:93
- **Scenarios**: [events/1.03-positive-lifecycle-event-published-under-publishing-subject.md](events/1.03-positive-lifecycle-event-published-under-publishing-subject.md)
- **Assumed**: once #5492 is done, events of tenant C are published with `tenant_id = C`.
- **Question**: Is #5492 a p1 blocker, or does p1 ship with tenant C getting no lifecycle events (and PRD.md:1542 deferred)?
- **Decided** (gap review 2026-11-06): #5492 is not a p1 blocker: until it ships, the engine does not enqueue broker events for tenants behind a barrier (barrier status from the cached tenant resolver); their `te_events` history stays complete and readable; PRD.md:1542 is marked deferred. Remaining: the doc edit and [events/1.03](events/1.03-positive-lifecycle-event-published-under-publishing-subject.md) (barrier case pending #5492).

### G-SEC-11 — Event schemas disagree with DESIGN on `actor`, `queue` and the create state
- **Kind**: contradiction
- **Docs**: DESIGN.md:875 ("task-scoped events also carry the task's current `queue` and … `actor`"); `schemas/types/gts.cf.core.events.event.v1~cf.core.tasks.task_created.v1~.schema.json` (no `actor`, has `created_by`; `initial_state` enum `queued|scheduled|blocked`, lines 62-70); `…task_cancel_requested.v1~` (`requested_by`, no `actor`); `…task_dead_lettered.v1~` (no `queue`, has `from_queue`); Proposals › Task mutability, create can start a task in `waiting`)
- **Scenarios**: events/1.01, 1.13
- **Assumed**: history items carry `actor` for every subject-caused event (`{creator_a}` for `taskCreated`); engine-caused events have none.
- **Question**: Rename `created_by`/`requested_by` to `actor` (or document the exceptions), add `queue` to `taskDeadLettered`, and add `waiting` to `taskCreated.initial_state` now that create may carry a wait group.
- **Decided** (gap review 2026-11-06): (doc only) follow DESIGN: rename `created_by`/`requested_by` to `actor`, add `queue` to `taskDeadLettered`, add `waiting` to `taskCreated.initial_state` (G-CRT-01). Remaining: the schema and doc edits.

### G-SEC-12 — Publishing policy: transitions, precedence, runtime change, and p1 default
- **Kind**: unclear
- **Docs**: PRD.md:877 (publishing is p1), PRD.md:896-898 (configuration and the `lifecycle` preset are p2; policies may name "the state transitions that trigger events"); DESIGN.md:2217-2221 (only `events` lists), DESIGN.md:2047
- **Scenarios**: events/1.04, 1.06
- **Assumed**: p1 publishes the `lifecycle` preset; a matching `per_type` entry replaces the default list; config is static.
- **Question**: What does p1 publish before the p2 configuration exists? Is the per-policy transition filter dropped or designed? Which pattern wins when several match? Can `event_policies` change through `POST /admin/config`?
- **Decided** (gap review 2026-11-06): p1 publishes the `lifecycle` preset for every type with no configuration; the per-policy transition filter is dropped (event-type lists suffice); in p2 `per_type` patterns use the G-CLM-21 specificity rule and `event_policies` change through `POST /admin/config`. Remaining: the doc edit.

### G-SEC-13 — No way to name the owner tenant on create
- **Kind**: missing
- **Docs**: DESIGN.md:1007 (`owner_tenant_id` sent to the PDP on create), DESIGN.md:2378 ("the *target* tenant, not necessarily the caller's"), DESIGN.md:2548 (creator checks use `owner_tenant_id`); PRD.md:934; CONVENTIONS §5.2 (no such request field)
- **Scenarios**: [auth/1.03-positive-platform-worker-crosses-barrier-only-on-platform-queues.md](auth/1.03-positive-platform-worker-crosses-barrier-only-on-platform-queues.md)
- **Assumed**: a platform gear can create a task owned by tenant C (mechanism unspecified).
- **Question**: Is the owner tenant always the caller's tenant, or can a create name another tenant (field name, PDP check, barrier rule)? Platform-owned work for tenants such as **cleanup** needs one of them.
- **Decided** (gap review 2026-11-06): optional request field `owner_tenant_id`, default the caller's `subject_tenant_id`; the create PDP check carries it and decides (subtree, barrier `Respect`); barrier `Ignore` is sent only when the task's queue is `operated_by: platform`. Remaining: the doc edit, both OpenAPI files and [auth/1.03](auth/1.03-positive-platform-worker-crosses-barrier-only-on-platform-queues.md).

### G-SEC-14 — Tenant hierarchy: default subtree visibility and the Tenant Resolver cap
- **Kind**: unclear
- **Docs**: PRD.md:934 ("limited to the caller's accessible tenants, resolved through the Tenant Resolver, regardless of what the PDP returns"); DESIGN.md:978, :2348 (only PDP constraints compiled to `AccessScope`)
- **Scenarios**: [auth/1.11-guardrail-tenant-scope-of-reads.md](auth/1.11-guardrail-tenant-scope-of-reads.md)
- **Assumed**: a parent-tenant subject sees child tasks only when the PDP returns a subtree constraint; the engine intersects the PDP scope with the Tenant Resolver's accessible tenants.
- **Question**: Does DESIGN implement the "regardless of the PDP" cap, and how (intersect with the resolver's tenant set on every query)? Is subtree visibility a policy choice only?
- **Decided** (gap review 2026-11-06): follow the platform model: tenant scoping comes from the PDP's tenant constraints (built by the AuthZ resolver from the Tenant Resolver); subtree visibility is a policy choice; no second engine-side resolver call. PRD.md:934 is reworded. Remaining: the doc edit.

### G-SEC-15 — No operator read or admin path into a tenant behind a barrier
- **Kind**: missing
- **Docs**: PRD.md:954-955; DESIGN.md:1020 (only claims on `operated_by: platform` queues send `ignore`), ADR-0016:74, :87
- **Scenarios**: [auth/1.11-guardrail-tenant-scope-of-reads.md](auth/1.11-guardrail-tenant-scope-of-reads.md)
- **Assumed**: root `{admin}` cannot read, re-enqueue or reassign tenant C's tasks.
- **Question**: Is that intended (C's own operators only), or do admin and DLQ operations need a `barrier_mode: ignore` path with PDP consent?
- **Decided** (gap review 2026-11-06): operations on tasks whose queue is `operated_by: platform` (for a dead-lettered task: its `dead_lettered_from` queue) send barrier `Ignore` and let the PDP decide: read, search filtered to such queues only, cancel, re-enqueue, reassign. A single-task call reads with the `Ignore` scope and keeps the result only if the task's queue is platform-operated; otherwise the `Respect` scope applies. Tenant-operated queues get no bypass. Remaining: the doc edit and [auth/1.11](auth/1.11-guardrail-tenant-scope-of-reads.md).

### G-SEC-16 — Claim that names only queues the policy excludes
- **Kind**: unclear
- **Docs**: PRD.md:1531; DESIGN.md:1009 (claim authorized on the base type), :2407
- **Scenarios**: [auth/1.12-negative-claim-names-queue-outside-policy.md](auth/1.12-negative-claim-names-queue-outside-policy.md)
- **Assumed**: the excluded queue just yields nothing (`204` after the long poll).
- **Question**: Should a claim whose named queues are all outside the `queue_id` constraint fail fast with `403` instead of long-polling forever?
- **Decided** (gap review 2026-11-06): see G-CLM-14: a claim naming a queue the scope provably excludes gets `403` at once (not only when all are excluded). Remaining: the doc edit and [auth/1.12](auth/1.12-negative-claim-names-queue-outside-policy.md).

### G-SEC-17 — Token scopes as a ceiling are undefined
- **Kind**: missing
- **Docs**: PRD.md:943 ("Token scopes MUST act as a ceiling"), PRD.md:909; DESIGN.md:2350
- **Scenarios**: [auth/1.05-negative-read-but-not-act.md](auth/1.05-negative-read-but-not-act.md)
- **Assumed**: a scope not covering an action turns the decision into a denial (`403` when the task is readable).
- **Question**: Which scope values exist for the Task Engine, how do they map to actions, and is the ceiling enforced by the PDP or by the engine?
- **Decided** (gap review 2026-11-06): the PDP enforces the token-scope ceiling (the PEP passes `token_scopes`; platform authorization DESIGN "Token Scopes"); the engine has no scope logic and documents suggested scope names (`tasks:read`, `tasks:write`, `tasks:work`, `tasks:admin`) for policy authors. Remaining: the doc edit.

### G-SEC-18 — `resolve` without `read`: does the wait condition resolve or look missing?
- **Kind**: contradiction
- **Docs**: Proposals › Task mutability, "a service that answers a question gets no other right on the task"); DESIGN.md:1199 (wait condition `404` when the caller may not `read` it), DESIGN.md:1016
- **Scenarios**: [auth/1.14-guardrail-resolve-grants-nothing-else.md](auth/1.14-guardrail-resolve-grants-nothing-else.md)
- **Assumed**: `resolve` alone finds and answers the condition; `GET` and `cancel` give `404`.
- **Question**: Does resolving need `read` on the task too (then the "no other right" goal is not met), or does the `404` rule exempt `resolve`?
- **Decided** (gap review 2026-11-06): the `404` rule exempts resolve: a caller with `resolve` finds and resolves the condition by its ID without `read`; `GET` and `cancel` still give `404`. Remaining: the doc edit.

### G-SEC-19 — Reason codes and field violations on `403`
- **Kind**: wire
- **Docs**: DESIGN.md:1040, :1214 (field violation naming check and reference index); `docs/arch/errors/categories/07-permission-denied.md:53-68` (context `resource_type` + `reason`, closed, no `field_violations`); DESIGN.md:1202, :1218, :1229, :1231, :1236 (no reason codes named)
- **Scenarios**: auth/1.05, 1.21, 1.22; ERRORS.md
- **Assumed**: `context.reason` = `CREATOR_AUTHORITY`/`CREATOR_AUTHORITY_UNVERIFIABLE` plus `field_violations[{ field: "authority.checks[0]", … }]`; other denials carry an unspecified reason.
- **Question**: How is "check and reference index" carried in a `permission_denied` context? Name reason codes for plain denials (action denied, `expedite`, revoked `claim`, dedup hit not readable, supersession without `cancel`).
- **Decided** (gap review 2026-11-06): closed `403` reason list: `ACTION_DENIED`, `EXPEDITE_REQUIRED`, `QUEUE_NOT_PERMITTED` (G-CLM-14), `CREATOR_AUTHORITY`, `CREATOR_AUTHORITY_UNVERIFIABLE`, `EVENT_CONSUME_DENIED`, `EVENT_CONSUME_UNVERIFIABLE`, `DUPLICATE_NOT_READABLE`, `SUPERSEDE_CANCEL_DENIED`, `CLAIM_REVOKED`. The check/reference index goes only into `detail` (G-WIRE-15). Remaining: the doc edit.

### G-SEC-20 — A search with no `read` grant: 403 or an empty page
- **Kind**: unclear
- **Docs**: PRD.md:918 (denial → `403`); DESIGN.md:1009 (search authorized on the base type)
- **Scenarios**: [auth/1.15-guardrail-queue-read-grants-no-task-data.md](auth/1.15-guardrail-queue-read-grants-no-task-data.md)
- **Assumed**: `403`.
- **Question**: When the PDP denies `read` outright, does `GET /tasks` (and `/tasks/count`) answer `403` or `200` with no items?
- **Decided** (gap review 2026-11-06): an outright PDP denial → `403 permission_denied` (`ACTION_DENIED`); constraints that match nothing → `200` with an empty page. Remaining: the doc edit.

### G-SEC-21 — Field-violation format and value echo in validation errors
- **Kind**: unclear
- **Docs**: PRD.md:1360 (no payload contents in error messages), PRD.md:1364; DESIGN.md:1210-1211 (schema violations), DESIGN.md:2344; `docs/arch/errors/DESIGN.md:974` (gears must minimise data in `field`/`resource_name`)
- **Scenarios**: [ERRORS.md](ERRORS.md)
- **Assumed**: `field` is a JSON Pointer path such as `input/format`; `description` names the rule, never the value.
- **Question**: Fix the path format and the `reason` codes for schema violations, and state that the engine strips values from validator messages (input, output, entry, resolve value).
- **Decided** (gap review 2026-11-06): `field` is an RFC 6901 JSON Pointer from the request body root (`/queues/1`, `/input/items/2/name`, `/authority/checks/0`); a header or query parameter is named as sent (`Idempotency-Key`, `cursor`); every user-schema failure (input, output, entry, resolve value) has reason `SCHEMA_VIOLATION` and a description naming the failed keyword, never the value; validator messages are never forwarded. Remaining: the doc edit, CONVENTIONS §6 and every scenario that shows a dotted field path.

### G-SEC-22 — No reason code for a lost race on `aborted`
- **Kind**: missing
- **Docs**: DESIGN.md:1206 ("Concurrent-claim race / lost guarded update", no reason); `docs/arch/errors/categories/10-aborted.md:52` (`reason` required)
- **Scenarios**: [ERRORS.md](ERRORS.md)
- **Assumed**: some reason is present; not asserted.
- **Question**: Name the code (for example a generic `CONFLICT`) and list which endpoints can return it.
- **Decided** (gap review 2026-11-06): `STATE_CHANGED`: a non-worker single-task write whose state guard matched 0 rows (cancel, resolve, re-enqueue, move, reprioritize, reassign, a supersession out of retries). Worker writes keep `LEASE_LOST`; payload revision races keep `CONCURRENT_MODIFICATION`. Remaining: the doc edit.

### G-SEC-23 — Is `taskQueued` recorded when a task is created straight into `queued`?
- **Kind**: unclear
- **Docs**: PRD.md:879; `schemas/types/…task_queued.v1~.schema.json` ("when a task enters the 'queued' state"); `…task_created.v1~` (`initial_state`); DESIGN.md:2046-2047 (≈ 8 history rows per lifecycle)
- **Scenarios**: [events/1.01-positive-history-complete-and-ordered.md](events/1.01-positive-history-complete-and-ordered.md)
- **Assumed**: no; `taskCreated` with `initial_state: queued` only.
- **Question**: Which events does a plain create → claim → start → complete record, exactly?
- **Decided** (gap review 2026-11-06): a create records only `taskCreated` with `initial_state` (`queued` | `scheduled` | `blocked` | `waiting`), plus the wait events for `waiting` (G-WAI-12); no `taskQueued`. Create → claim → start → complete records `taskCreated`, `taskClaimed`, `taskStarted`, `taskCompleted`. An engine-driven entry into `queued` (a `scheduled` task whose `run_after` passed, a `blocked` task whose predecessors finished) records no event; the preceding `taskRetrying` / `taskQueued { run_after }` / predecessor events explain it. Remaining: the doc edit.

### G-SEC-24 — Shape of `authority.checked` in the claim response
- **Kind**: wire
- **Docs**: DESIGN.md:1043 ("mode, checked sources, `checked_at`"); CONVENTIONS §5.3 (`authority` D, `checked: []`)
- **Scenarios**: [auth/1.01-positive-creator-check-verified-and-shown-on-claim.md](auth/1.01-positive-creator-check-verified-and-shown-on-claim.md)
- **Assumed**: entries `{ source, resource_type, action, verdict }`.
- **Question**: Define the entry shape (and whether input-pointer checks list the resource ID that was checked).
- **Decided** (wire review 2026-11-04): `authority { mode, checked: [{ source, resource_type, action, verdict }], checked_at }`, verdicts as stored at creation. Remaining: the doc edit.

### G-SEC-25 — Default and maximum page size of each paged list
- **Kind**: missing
- **Docs**: `guidelines/DNA/REST/QUERYING.md` (default 25, min 1, max 200; out of range → `422 INVALID_LIMIT`); DESIGN.md:1161, :2150-2151 (`search_max_top`, no default); `toolkit-db` `LimitCfg { default, max }` (clamps; `$top=0` → `400`)
- **Scenarios**: [search/1.01-positive-default-window-newest-first.md](search/1.01-positive-default-window-newest-first.md), [search/1.08-guardrail-page-size-clamped.md](search/1.08-guardrail-page-size-clamped.md), [events/1.02-positive-history-cursor-paged.md](events/1.02-positive-history-cursor-paged.md), [waits/framework/1.10-flow-group-decision-outcomes.md](waits/framework/1.10-flow-group-decision-outcomes.md), [waits/timer/1.04-guardrail-timer-wait-survives-restart.md](waits/timer/1.04-guardrail-timer-wait-survives-restart.md)
- **Assumed**: default 25 everywhere; caps: search 100 (20 at `view=full`), history, entries and wait conditions 100; over the cap is clamped; `limit=0` → `400` (ToolKit), not `422` (guideline). `GET /tasks/{id}/wait-conditions` is paged because resolved conditions are kept until retention.
- **Question**: Add a `search_default_top` (and defaults and caps for history, entries and wait conditions) to DESIGN §4.1. Record the ToolKit-vs-guideline difference (`400` vs `422`) at platform level.
- **Decided** (wire review 2026-11-04): the default page size is 25 on every paged list. Still open: the caps of history, entries and wait conditions (assumed 100). Gap review 2026-11-06: the cap is 100 for history, entries and wait conditions (G-SEC-07). Remaining: the doc edit.

### G-SEC-26 — Does a search cursor bind `view` and `$select`?
- **Kind**: missing
- **Docs**: `guidelines/DNA/REST/QUERYING.md` (cursor encodes `$select`; change → `400 FIELD_SELECTION_MISMATCH`); DESIGN.md:1163-1176 (`view` is engine-owned; cursor carries `$orderby` values and per-domain positions); `libs/toolkit-odata` (no `$select` check)
- **Scenarios**: [search/1.06-negative-unsupported-query.md](search/1.06-negative-unsupported-query.md)
- **Assumed**: the cursor carries `view` and `$select`; a different one with a cursor → `400`, violation on `cursor`, `reason: FIELD_SELECTION_MISMATCH`.
- **Question**: Should the engine bind them (the ToolKit does not check `$select` today), and with which reason code?
- **Decided** (gap review 2026-11-06): yes: the engine-owned cursor encodes `view` and `$select`; a different one with a cursor → `400 invalid_argument`, field `cursor`, reason `FIELD_SELECTION_MISMATCH`. Other search refusals: an unsupported `$orderby` key, a `$select` field outside the view or an unknown `view` → `NOT_ALLOWED` on that parameter; a `created_at` range over the window → `OUT_OF_RANGE` on `$filter`. Remaining: the doc edit.

### G-SEC-27 — Search filter allowlist departs from the REST guideline
- **Kind**: contradiction
- **Docs**: `guidelines/DNA/REST/QUERYING.md` ("filtering and ordering are allowed only on fields backed by suitable database indexes"; recommended cap of 10 fields per endpoint); DESIGN.md:1138, :1150-1158 (`assignee`, `urgency`, `priority`, `dependencies` applied inside another index's range); Proposals › API reads and responses (adds `parent_task_id`, `predecessor_id`: 12 fields)
- **Scenarios**: [search/1.02-positive-filter-by-field.md](search/1.02-positive-filter-by-field.md)
- **Assumed**: all 12 fields are accepted, as DESIGN says; the OpenAPI publishes them in `x-odata-filter`.
- **Question**: Add these to the "Task-Engine-specific rules on top of the ToolKit surface" list (DESIGN.md:1178) as a deliberate exception, or require an indexed filter alongside the unindexed ones, or trim the list.
- **Decided** (gap review 2026-11-06): keep the 12 filter fields as a deliberate exception in DESIGN's "Task-Engine-specific rules" list: every search is bounded by the tenant scope and the ≤ 31-day `created_at` window (a UUID v7 `task_id` range), so unindexed fields are residual filters inside an indexed range. The window is as designed (unfinished-only and dead-lettered-only searches are exempt). With `dead_lettered_from` (G-RTY-11) the list has 13 fields; the exception covers them all. Remaining: the doc edit.

### G-SEC-28 — No history event for a tag change
- **Kind**: missing
- **Docs**: PRD.md:879 (event list has no tag event); PRD.md:868 (history includes user actions); Proposals › Task mutability (tags are the only field an API consumer may still change)
- **Scenarios**: the draft reference DDL (`migration.sql`, kept outside this branch)
- **Assumed**: `PATCH /tasks/{id}/tags` records no event; only the tag rows change.
- **Question**: Add an event (e.g. `taskTagsUpdated` with the changed keys) or state that tag changes are not audited.
- **Decided** (gap review 2026-11-06): add `taskTagsUpdated` with `actor`, the keys set (with their new values) and the keys removed; one history row per `PATCH /tasks/{id}/tags`. A tag PATCH bumps `updated_at` of a live task only. Remaining: the schema, the doc edit and a scenario.

## OPS — Queues, config, bulk, domains, reassignment, blacklist, retention, flows

### G-OPS-01 — How queue per-state counts are computed
- **Kind**: missing
- **Docs**: PRD.md:493, DESIGN.md:763, :1106-1107; the draft reference DDL (`migration.sql`, kept outside this branch) (leader samples in `te_queue_depth_samples`; the dead-lettered count scans every history partition per sample, cadence unsettled)
- **Scenarios**: [queues/1.01-positive-queue-introspection.md](queues/1.01-positive-queue-introspection.md)
- **Assumed**: `GET /queues` returns a count per live state with `sampled_at`; exact values are not asserted for a sample older than `queue_depth.sample_secs`. A `dead_letter` queue reports a `dead_lettered` count and no limits.
- **Question**: Are per-state counts exact queries or leader samples, how stale may they be, are they capped, and what does a dead-letter queue report?
- **Decided** (wire review 2026-11-04): `counts` are the maintenance leader's samples (every `queue_depth.sample_secs`) with `sampled_at`, not exact reads. Remaining: the doc edit.

### G-OPS-02 — What a tenant subject with queue `read` sees in queue counts
- **Kind**: missing
- **Docs**: PRD.md:923 ("seeing queues never grants access to task data"), PRD.md:455 (`429` shows no usage figures), DESIGN.md:1106-1107; the draft reference DDL (`migration.sql`, kept outside this branch) (`te_queue_depth_samples` holds all-tenant counts only)
- **Scenarios**: [queues/1.01-positive-queue-introspection.md](queues/1.01-positive-queue-introspection.md)
- **Assumed**: Introspection scenarios use a root operator, who sees all-tenant counts. Nothing is asserted for a tenant caller.
- **Question**: Do queue counts of a shared queue cover all tenants for any caller with queue `read` (which reveals other tenants' volume, while `429` deliberately hides usage), or are they narrowed to the caller's tenant scope?
- **Decided** (gap review 2026-11-06): queue `counts` (and `sampled_at`) are returned only to callers that also hold `configure` on the queue (queues have no `admin` action); other callers get the definition and hold state with `counts` omitted. Remaining: the doc edit.

### G-OPS-03 — Where a hold is stored and how fast it takes effect
- **Kind**: unclear
- **Docs**: Proposals › Queue operations › Admin queue hold ("stored in the queue's domain … within a short refresh interval"); DESIGN.md:2154 (`runtime_config_poll_secs`); DESIGN §3.7 (DESIGN.md:1394-1960) has no table for it; the draft reference DDL (`migration.sql`, kept outside this branch) (`te_queue_holds`, one row per held queue, written to every domain of the queue during a move)
- **Scenarios**: [flows/1.07-flow-incident-hold-and-release.md](flows/1.07-flow-incident-hold-and-release.md), [queues/2.01-positive-hold-queue.md](queues/2.01-positive-hold-queue.md)
- **Assumed**: The hold reaches every node within `runtime_config_poll_secs` (5 s); claims started in that window may still take a task from the held queue.
- **Question**: Name the table/columns for the hold, the refresh interval, and whether a hold response guarantees no claim starts after it (e.g. claims re-check the hold row inside their transaction).
- **Decided** (gap review 2026-11-06): holds live in `te_queue_holds` in the queue's domain (as drafted); each node caches them and refreshes every `runtime_config_poll_secs` (5 s), the serving node at once. Guarantee: no claim starts on any node more than `runtime_config_poll_secs` after the hold returns. No per-claim read. Remaining: the doc edit.

### G-OPS-04 — No schema for `queueHeld` / `queueReleased`
- **Kind**: wire
- **Docs**: Proposals › Queue operations › API changes ("for example `queueHeld` and `queueReleased`"); `schemas/types/` (no such files); DESIGN.md:875 (event list)
- **Scenarios**: [flows/1.07-flow-incident-hold-and-release.md](flows/1.07-flow-incident-hold-and-release.md), [queues/2.01-positive-hold-queue.md](queues/2.01-positive-hold-queue.md), [queues/2.03-positive-release-resumes-claims.md](queues/2.03-positive-release-resumes-claims.md)
- **Assumed**: Queue-scoped events like `configUpdated`: `subject` = queue ID, `subject_type` = `gts.cf.core.tasks.queue.v1~`; `queueHeld.data = { queue, actor, reason, until }`; `queueReleased.data = { queue, actor? }` with `actor` absent for an automatic release; enqueued in the queue's domain.
- **Question**: Add both event types (names, payload, required keys, domain/outbox, tenant attribution) to `schemas/types/`, DESIGN.md:875 and PRD.md:879.
- **Decided** (gap review 2026-11-06): `queueHeld { queue, actor, reason, until? }`; `queueReleased { queue, actor?, automatic }` (`actor` absent for an automatic release); `subject` = queue ID, `subject_type` = queue base type, tenant = root, enqueued in the queue's domain. Remaining: the schemas and the doc edit.

### G-OPS-05 — Queue-scoped events cannot be read through the API, and no publishing policy covers them
- **Kind**: missing
- **Docs**: DESIGN.md:1829 (`task_id` NULL for `configUpdated`, `workerBlacklisted`), DESIGN.md:1108 (history only per task), PRD.md:868 (configuration changes are part of the history), PRD.md:898 and DESIGN.md:2217-2221 (policies are per task type pattern)
- **Scenarios**: [queues/2.01-positive-hold-queue.md](queues/2.01-positive-hold-queue.md), [queues/3.01-positive-runtime-config-save-is-complete-version.md](queues/3.01-positive-runtime-config-save-is-complete-version.md)
- **Assumed**: `configUpdated`, `workerBlacklisted`, `queueHeld`, `queueReleased` are recorded in `te_events` and published under the default policy; side effects assert publication only.
- **Question**: Add a read path for queue-scoped history (e.g. `GET /queues/{id}/events`, needs queue `read`), and say which publishing policy applies to events that have no task type.
- **Decided** (gap review 2026-11-06): add `GET /queues/{id}/events` (paged like task history, queue `read`) for queue-scoped events; events without a task type use the engine-wide default publishing policy. Still open: where engine-wide (non-queue) `configUpdated` events are read. Remaining: the doc edit, both OpenAPI files and a scenario.

### G-OPS-06 — Claim answer and wake-up for a held queue
- **Kind**: unclear
- **Docs**: Proposals › Queue operations ("no queued task of that queue is claimed"); PRD.md:429; DESIGN.md:758 (wake signals), :2420, :2433
- **Scenarios**: [queues/2.02-positive-held-queue-stops-claims-only.md](queues/2.02-positive-held-queue-stops-claims-only.md)
- **Assumed**: A claim that names only held queues long-polls and returns `204`, never `429`; after a release, parked claims notice at the next queue-watcher tick (`claim_poll_interval_ms`).
- **Question**: Confirm `204`, and whether a release emits a `newTask`-style signal so parked claims wake at once.
- **Decided** (gap review 2026-11-06): a claim naming only held queues long-polls and answers `204`; the queue watcher skips held queues; on release the releasing node signals at once, other nodes resume within one refresh plus one poll interval. Remaining: the doc edit.

### G-OPS-07 — Queue timeout while a queue is held
- **Kind**: proposal-open
- **Docs**: Proposals › Queue operations › Open questions (queue timeouts while held; leaning: do not fire); PRD.md:517; DESIGN.md:541; the draft reference DDL (`migration.sql`, kept outside this branch) (nothing stored to suspend a held queue's queue timeout; it keeps firing)
- **Scenarios**: [flows/1.07-flow-incident-hold-and-release.md](flows/1.07-flow-incident-hold-and-release.md), [queues/2.02-positive-held-queue-stops-claims-only.md](queues/2.02-positive-held-queue-stops-claims-only.md)
- **Assumed**: Scenarios assert only what holds either way: the lifetime timeout still ends a held task. On main + accepted, a queue timeout would also fire during a hold.
- **Question**: Decide whether held time counts toward the queue timeout, and how a timer that is due during a hold is re-armed on release.
- **Decided** (gap review 2026-11-06): held time counts: queue timers keep firing during a hold (a queue timeout measures staleness, PRD.md:517); documented, no mechanism. The proposal's leaning is reversed. Remaining: the doc edit.

### G-OPS-08 — Mechanism of `until` auto-release
- **Kind**: missing
- **Docs**: Proposals › Queue operations › Admin queue hold (`until?` "for automatic release"); the draft reference DDL (`migration.sql`, kept outside this branch) (no timer; a maintenance sweep on `te_queue_holds.hold_until`)
- **Scenarios**: [queues/2.03-positive-release-resumes-claims.md](queues/2.03-positive-release-resumes-claims.md)
- **Assumed**: At `until` the queue is released within `runtime_config_poll_secs`; exactly one `queueReleased` is published, with no `actor`.
- **Question**: Is the release a durable timer (`te_timers` type) or a clock comparison on each node? Who writes the single `queueReleased` event? What about an `until` in the past (assumed `400`)?
- **Decided** (gap review 2026-11-06): each node treats a hold as over once `now ≥ until` on its cached row; the maintenance leader's sweep on `hold_until` deletes the row and writes the single `queueReleased { automatic: true }` without `actor`. An `until` not in the future → `400`. The reason for `until` not in the future is `OUT_OF_RANGE` on `/until`. Remaining: the doc edit.

### G-OPS-09 — Repeated hold and release of a queue that is not held
- **Kind**: missing
- **Docs**: Proposals › Queue operations › Admin queue hold; the draft reference DDL (`migration.sql`, kept outside this branch) (a repeated hold overwrites `reason` / `hold_until`, keeps `held_since` / `held_by`)
- **Scenarios**: [queues/2.03-positive-release-resumes-claims.md](queues/2.03-positive-release-resumes-claims.md), [queues/2.04-guardrail-hold-and-release-edge-cases.md](queues/2.04-guardrail-hold-and-release-edge-cases.md)
- **Assumed**: Holding a held queue returns `200`, replaces `reason` and `until`, keeps `since`/`by`, and publishes another `queueHeld`. Releasing a queue that is not held returns `200` with `hold: null` and publishes nothing.
- **Question**: Define both cases (update vs. no-op vs. `409`), and whether `by` changes when another admin re-holds.
- **Decided** (gap review 2026-11-06): hold of a held queue → `200`, replaces `reason` and `until`, keeps `since`, sets `by` to the latest actor, records another `queueHeld`; release of an unheld queue → `200` with `hold: null`, no event. Remaining: the doc edit.

### G-OPS-10 — Holding a dead-letter queue
- **Kind**: missing
- **Docs**: PRD.md:801 (DLQ never dispatched), PRD.md:997; DESIGN.md:2432; Proposals › Queue operations
- **Scenarios**: [queues/2.04-guardrail-hold-and-release-edge-cases.md](queues/2.04-guardrail-hold-and-release-edge-cases.md)
- **Assumed**: `400 invalid_argument` with a field violation naming the queue kind.
- **Question**: Reject (which category), or accept as a no-op? Should a hold on a DLQ block re-enqueue from it?
- **Decided** (gap review 2026-11-06): `400 failed_precondition` `{type: STATE, subject: DEAD_LETTER_QUEUE}`, `resource_name` = the queue; a hold never affects re-enqueue from a DLQ. Remaining: the doc edit and [queues/2.04](queues/2.04-guardrail-hold-and-release-edge-cases.md) (was `invalid_argument`).

### G-OPS-11 — Runtime config: body semantics, storage, validation and apply lag
- **Kind**: missing
- **Docs**: PRD.md:482-487; DESIGN.md:208 and :1129-1130 (`te_runtime_config` named, no table in §3.7), DESIGN.md:860 (step 6), :862 (single leader); the draft reference DDL (`migration.sql`, kept outside this branch) (`te_runtime_config` columns `config_version`, `overrides`, `saved_by`, `saved_at`; singleton `te_runtime_config_applied`)
- **Scenarios**: [queues/3.01-positive-runtime-config-save-is-complete-version.md](queues/3.01-positive-runtime-config-save-is-complete-version.md), [queues/3.03-positive-config-version-apply-status.md](queues/3.03-positive-config-version-apply-status.md), [queues/3.05-negative-queue-outside-trust-limits-refuses-tasks.md](queues/3.05-negative-queue-outside-trust-limits-refuses-tasks.md)
- **Assumed**: A save merges into the previous version (only changed keys appear in `configUpdated`); each domain applies at its next maintenance pass; a value outside the trust limits is rejected at save with `400`; `GET /admin/config/{n}` for an unknown version is `404`; versions are cumulative.
- **Question**: Define the `te_runtime_config` table, merge vs. replace, validation at save (trust limits, unknown keys, unknown queue IDs), how removal of an override is expressed, the maintenance interval that bounds apply lag, and the unknown-version answer.
- **Decided** (wire review 2026-11-04): a runtime config version is the complete override set `{ queues: { <id or pattern>: { … } }, types: { … } }`; a key not present reverts to the registration or default. Remaining: the doc edit.

### G-OPS-12 — "Most specific pattern wins" and runtime vs. static overrides
- **Kind**: unclear
- **Docs**: PRD.md:445, :475; DESIGN.md:756, :2182 ("most specific pattern wins; overrides GTS registration; runtime-updatable")
- **Scenarios**: [queues/3.02-positive-config-override-precedence.md](queues/3.02-positive-config-override-precedence.md)
- **Assumed**: An exact ID beats any wildcard; a longer literal prefix beats a shorter one; a runtime (`/admin/config`) override beats a static YAML override for the same pattern.
- **Question**: Define specificity (and ties), and the precedence between the YAML file and runtime versions.
- **Decided** (gap review 2026-11-06): see G-CLM-21 (exact > longest literal prefix; equal-specificity overlap rejected at load); for the same pattern a runtime (`/admin/config`) override beats the static YAML. Remaining: the doc edit.

### G-OPS-13 — Do runtime timeout / retry / priority changes reach existing tasks?
- **Kind**: unclear
- **Docs**: PRD.md:484 (timeouts, retry policy and default priority are runtime-changeable); DESIGN.md:715 (resolved values stored on the task)
- **Scenarios**: [queues/3.04-positive-timeout-change-applies-to-new-tasks-only.md](queues/3.04-positive-timeout-change-applies-to-new-tasks-only.md)
- **Assumed**: They apply only to tasks created after the domain applies the version; existing tasks keep their stored values. Limits apply to the next claim/admission.
- **Question**: State this in PRD §5.2, or say which settings are re-resolved for live tasks.
- **Decided** (gap review 2026-11-06): see G-CRT-04: settings are resolved and stored at create, so runtime changes do not reach existing tasks. Remaining: the doc edit.

### G-OPS-14 — Bulk authorization: per-task outcome vs. a `403` row
- **Kind**: contradiction
- **Docs**: DESIGN.md:1119 (overall `200`, per-task `unauthorized`) vs. DESIGN.md:1236 ("Bulk per-task authorization denied → `permission_denied` 403"); PRD.md:1160
- **Scenarios**: [bulk/1.01-positive-bulk-cancel-per-task-outcomes.md](bulk/1.01-positive-bulk-cancel-per-task-outcomes.md), [bulk/1.02-positive-bulk-retry-per-task-outcomes.md](bulk/1.02-positive-bulk-retry-per-task-outcomes.md)
- **Assumed**: Overall `200`; a task the caller may read but not act on is `unauthorized`, one it may not read is `not_found`.
- **Question**: Remove or reword DESIGN.md:1236; say whether a caller with no right at all (e.g. no `admin` anywhere) gets `403` for the whole request.
- **Decided** (gap review 2026-11-06): DESIGN.md:1236 is removed: per-task outcomes (`unauthorized` / `not_found`) as DESIGN.md:1119 says; an outright PDP denial of the action on the base type → `403 ACTION_DENIED` for the whole request (as G-SEC-20). Remaining: the doc edit.

### G-OPS-15 — `BATCH_TOO_LARGE` has no error-table row and no config key
- **Kind**: missing
- **Docs**: PRD.md:1161, :1498; DESIGN.md:1195-1236 (no row); DESIGN §4.1 (DESIGN.md:2057-2247, no key)
- **Scenarios**: [bulk/1.05-negative-bulk-too-large.md](bulk/1.05-negative-bulk-too-large.md)
- **Assumed**: `400 invalid_argument`, `context.reason = "BATCH_TOO_LARGE"`, field violation on `task_ids`; limit 1,000.
- **Question**: Add the row and a config key (e.g. `bulk_max_ids`).
- **Decided** (gap review 2026-11-06): config `bulk_max_ids`, default 1000; over it → `400 invalid_argument` `BATCH_TOO_LARGE` on `/task_ids`; add the error-table row. Remaining: the doc edit.

### G-OPS-16 — Bulk cancel outcome for a leased task
- **Kind**: unclear
- **Docs**: PRD.md:1160 (four outcomes), PRD.md:348-352; DESIGN.md:551
- **Scenarios**: [bulk/1.01-positive-bulk-cancel-per-task-outcomes.md](bulk/1.01-positive-bulk-cancel-per-task-outcomes.md)
- **Assumed**: `succeeded` when the cancel request was accepted, even though the task only has `cancel_requested` set.
- **Question**: Should the outcome distinguish "cancelled" from "cancel requested" (single cancel uses `200` vs `202`)?
- **Decided** (gap review 2026-11-06): new per-task outcome `cancel_requested` for a leased task that was only flagged; `succeeded` means the task ended. Remaining: the doc edit.

### G-OPS-17 — Bulk retry / move refused by a depth limit has no outcome
- **Kind**: missing
- **Docs**: PRD.md:1160 (only `succeeded`/`unauthorized`/`not_found`/`invalid_state`), PRD.md:451, :810; DESIGN.md:756, :1227
- **Scenarios**: [bulk/1.02-positive-bulk-retry-per-task-outcomes.md](bulk/1.02-positive-bulk-retry-per-task-outcomes.md)
- **Assumed**: The refused task is reported `invalid_state`; the others proceed.
- **Question**: Add an outcome (e.g. `limit_exceeded` with the dimension), or reject the whole request with `429` and `Retry-After`.
- **Decided** (gap review 2026-11-06): see G-CLM-25: per-task `limit_exceeded`; the request is never refused as a whole for depth. Remaining: the doc edit.

### G-OPS-19 — No history event for priority, urgency or queue changes
- **Kind**: missing
- **Docs**: PRD.md:868 (history includes user actions), PRD.md:879 (event list has no such type); DESIGN.md:875; the draft's bulk move (writes no event)
- **Scenarios**: [bulk/1.03-positive-bulk-reprioritize.md](bulk/1.03-positive-bulk-reprioritize.md), [bulk/1.04-positive-bulk-move-per-task-outcomes.md](bulk/1.04-positive-bulk-move-per-task-outcomes.md)
- **Assumed**: A reprioritize or move records nothing in the history; the task's fields change silently.
- **Question**: Add an event (e.g. `taskUpdated` with changed fields, or `taskMoved`/`taskReprioritized`), or state that these admin actions are not audited per task.
- **Decided** (gap review 2026-11-06): new history event `taskUpdated { actor, changes: { priority?, urgency?, queue? } }` with old and new values for each admin change. Remaining: the doc edit.

### G-OPS-20 — Moved task: destination settings vs. stored resolved settings
- **Kind**: contradiction
- **Docs**: DESIGN.md:1121 ("immediately adopts the destination queue's retry policy and concurrency limits") vs. DESIGN.md:715 (resolved timeouts and retry policy stored on the task); PRD.md:1162
- **Scenarios**: [bulk/1.04-positive-bulk-move-per-task-outcomes.md](bulk/1.04-positive-bulk-move-per-task-outcomes.md)
- **Assumed**: The destination's retry policy, DLQ and limits apply; request-level values are assumed to be overwritten too.
- **Question**: Which values are re-resolved on move (retry policy, timeouts, DLQ, priority default), and do request-level overrides survive? Is the armed queue timeout re-armed?
- **Decided** (gap review 2026-11-06): a move re-runs settings resolution with the destination queue, keeping request-level values (stored as sent, `te_tasks.request_overrides`) and type-level values above the queue level; the queue timeout is re-armed from the move time. Remaining: the doc edit.

### G-OPS-21 — How a cross-domain bulk move is reported
- **Kind**: unclear
- **Docs**: PRD.md:1162 (same domain MUST), PRD.md:1180; DESIGN.md:1121, :1224
- **Scenarios**: [bulk/1.04-positive-bulk-move-per-task-outcomes.md](bulk/1.04-positive-bulk-move-per-task-outcomes.md)
- **Assumed**: Per-task `invalid_state`.
- **Question**: Per-task outcome or whole-request `400 TASK_DOMAIN_MISMATCH`?
- **Decided** (gap review 2026-11-06): per-task outcome `domain_mismatch` (tasks of one request may sit in different domains). Remaining: the doc edit.

### G-OPS-22 — Probe order for reads by ID, and domains that no longer route any queue
- **Kind**: missing
- **Docs**: Proposals › Domains › p1 lays the foundation ("a lookup by task ID probes the configured domains in a fixed order"); PRD.md:1180, :1186; ADR-0010:70
- **Scenarios**: [dead-letter/1.04-negative-reenqueue-refusals.md](dead-letter/1.04-negative-reenqueue-refusals.md), [domains/1.04-guardrail-task-domain-fixed-at-creation.md](domains/1.04-guardrail-task-domain-fixed-at-creation.md), [domains/1.07-positive-placement-and-claims-during-a-move.md](domains/1.07-positive-placement-and-claims-during-a-move.md)
- **Assumed**: `GET /tasks/{id}` and re-enqueue find a task in whichever configured domain holds it; a domain whose queues were all moved away stays configured and readable.
- **Question**: Which fixed order (config order, `default` last)? How is a domain with no routed queue configured (reads and retention only)?
- **Decided** (gap review 2026-11-06): probe order = the order of domains in the config, `default` last; a domain with no routed queue stays configured for reads, retention and maintenance. Remaining: the doc edit.

### G-OPS-23 — DLQ queue filters across domains
- **Kind**: unclear
- **Docs**: PRD.md:1186; DESIGN.md:2835; Proposals › Domains › During a move (search by queue covers the queue's domain set and any domain it left that still holds its rows)
- **Scenarios**: [domains/1.02-positive-one-dead-letter-queue-serves-two-domains.md](domains/1.02-positive-one-dead-letter-queue-serves-two-domains.md)
- **Assumed**: a `queue eq '<dead_letter>'` filter fans out to every domain, like a state filter (dead-letter queues are not routed, so they have no domain set).
- **Question**: Say that a DLQ filter is not "filtered by queue" for routing, or give dead-letter queues a domain set.
- **Decided** (gap review 2026-11-06): dead-letter queues have no domain set: a `queue eq '<dead_letter queue>'` filter searches every configured domain, like a state filter. Remaining: the doc edit.

### G-OPS-24 — Cross-domain parent/child and predecessor rejections: reason code and lookup
- **Kind**: missing
- **Docs**: DESIGN.md:1223 (`invalid_argument`, no reason), :1200 (missing parent/predecessor → `404`); PRD.md:1182, :1505
- **Scenarios**: [domains/1.05-negative-create-across-domain-boundary.md](domains/1.05-negative-create-across-domain-boundary.md)
- **Assumed**: `400 invalid_argument` with a field violation (`reason: CROSS_DOMAIN`, placeholder); `404` still wins when the caller may not read the linked task.
- **Question**: Name a reason code, and say how the engine learns that a linked task exists in another domain (it must look outside the new task's domain to tell `400` from `404`).
- **Decided** (gap review 2026-11-06): see G-WAI-41: links are looked up only in the new task's domain; another domain's task → `404`. Remaining: the doc edit.

### G-OPS-25 — How a queue move starts and ends; removing a domain that still holds live tasks
- **Kind**: missing
- **Docs**: Proposals › Domains › Queue-move model ("The move itself is built with domains in p3"); PRD.md:1180 and ADR-0010:69 ("MUST stop startup"); DESIGN.md:1053 (readiness), :1999 ("startup fails")
- **Scenarios**: [domains/1.06-positive-routing-change-with-live-tasks-starts-a-move.md](domains/1.06-positive-routing-change-with-live-tasks-starts-a-move.md), [domains/1.07-positive-placement-and-claims-during-a-move.md](domains/1.07-positive-placement-and-claims-during-a-move.md)
- **Assumed**: a routing change alone starts a move when the old domain stays configured; the source leaves the queue's domain set once it holds no unfinished task of the queue; removing the source domain entirely while it holds live tasks leaves the instance not ready (`/readyz` `503`).
- **Question**: What starts and ends a move (routing change, admin call, config stages)? For the remaining strand case, process exit or not-ready pod? How does the engine report what keeps the source busy?
- **Decided** (gap review 2026-11-06): a move starts with a routing change while the old domain stays configured, and ends when maintenance finds no unfinished task of the queue in the source (metric `te_queue_move_remaining{queue,domain}`); removing a domain that still holds live tasks makes the instance not ready (`/readyz` `503`, log names the domain and counts) instead of failing startup. Remaining: the doc edit.

### G-OPS-26 — Expired hand-off and the blacklist
- **Kind**: contradiction
- **Docs**: DESIGN.md:539 and :806 (an expired hand-off is a liveness failure that counts toward the blacklist) vs. DESIGN.md:548, :2798 and PRD.md:715 (no strike)
- **Scenarios**: [worker/reassignment/1.03-positive-accept-window-expires-requeue-no-strike.md](worker/reassignment/1.03-positive-accept-window-expires-requeue-no-strike.md)
- **Assumed**: No strike for anybody; the task is requeued with liveness backoff and `liveness_requeues` +1.
- **Question**: Fix DESIGN.md:539 and :806. Also confirm that an unaccepted hand-off uses the liveness budget (it is not the task's fault either).
- **Decided** (gap review 2026-11-06): no blacklist strike for an expired hand-off (DESIGN.md:539, :806 fixed); it uses the liveness budget (`liveness_requeues` +1, backoff; exhausted → `timed_out` with `timeout_type: assignment`, G-RTY-02). Remaining: the doc edit.

### G-OPS-27 — Reassignment preconditions
- **Kind**: missing
- **Docs**: PRD.md:713; DESIGN.md:547, :1073, :1234
- **Scenarios**: [worker/reassignment/1.06-negative-reassign-refused.md](worker/reassignment/1.06-negative-reassign-refused.md)
- **Assumed**: A task that is not `claimed`/`running` → `400 failed_precondition` (`STATE`). A task whose hand-off is pending can be reassigned again (epoch +1, new window).
- **Question**: Confirm; and say what happens when the target identity is blacklisted on the queue, has no `claim` on it, or names a subject of another tenant.
- **Decided** (gap review 2026-11-06): confirmed: only `claimed`/`running` (else `400 STATE`); a pending hand-off can be reassigned again (epoch +1, new window); a target blacklisted on the queue → `400 failed_precondition` `{type: STATE, subject: TARGET_BLACKLISTED}`; the target's rights are not checked at reassign: its accept is authorized with its own scope (`404`/`403` then), and the hand-off expires. Accept works like a claim for the new holder: `claimed_at` and the full execution deadline are set from the accept, plus a new assignment deadline if the task is still `claimed` (a `running` task has none); `attempt` does not change. Remaining: the doc edit.

### G-OPS-30 — Claim by a blacklisted worker: `204` or `400`
- **Kind**: contradiction
- **Docs**: DESIGN.md:1235 ("Worker blacklisted for the queue → `failed_precondition` 400") vs. DESIGN.md:2433 and :2420 (blacklisted queues dropped, `204` when nothing is left); PRD.md:698 ("stop assigning")
- **Scenarios**: [worker/blacklist/1.02-positive-blacklisted-worker-claim-skips-queue.md](worker/blacklist/1.02-positive-blacklisted-worker-claim-skips-queue.md)
- **Assumed**: The queue is silently skipped; `204` when no other named queue has work.
- **Question**: Remove the error-table row, or define when `400` applies (e.g. every named queue blacklisted, with a long poll?).
- **Decided** (gap review 2026-11-06): the error-table row is removed: blacklisted queues are skipped silently; `204` when nothing else has work. Remaining: the doc edit.

### G-OPS-31 — Blacklist counter after the cool-down
- **Kind**: missing
- **Docs**: PRD.md:703-704; DESIGN.md:2814 (blacklist when `consecutive_failures >= threshold`), DESIGN.md:1906
- **Scenarios**: [worker/blacklist/1.04-positive-per-queue-threshold-duration-and-expiry.md](worker/blacklist/1.04-positive-per-queue-threshold-duration-and-expiry.md)
- **Assumed**: Not asserted; the scenario notes that, as written, one failure after expiry re-blacklists at once.
- **Question**: Reset the counter when the blacklisting starts or ends, or keep it as probation? State it in PRD §5.6.
- **Decided** (gap review 2026-11-06): the counter resets to 0 when the blacklisting starts, so after the cool-down the worker needs `threshold` new failures. Remaining: the doc edit.

### G-OPS-32 — No bound on how late retention removes data
- **Kind**: missing
- **Docs**: PRD.md:1089 ("once their retention has passed"); ADR-0009:80 (slot recycled ≥ `(N − 2) × W` after creation); DESIGN.md:849, :2167 (`cleanup_time`)
- **Scenarios**: [retention/1.01-positive-finished-task-removed-after-retention.md](retention/1.01-positive-finished-task-removed-after-retention.md)
- **Assumed**: A task finished soon after creation with the 100-day default is gone 105–119 days after creation; scenarios check presence just before the retention end and absence after the latest recycle.
- **Question**: State the guaranteed upper bound (retention + up to about two buckets) in PRD §5.14, and what `cleanup_time` means for a paced ring loop.
- **Decided** (gap review 2026-11-06): PRD §5.14 states two bounds: data under the common retention (drop horizon) is gone by `created_at + N × W` (119 days at defaults, at most retention + 3 × W); data under a shorter policy and rescued rows are gone within one maintenance tick after `terminal_at + retention`. `cleanup_time` is dropped; every step runs on the paced tick. Remaining: the doc edit.

### G-OPS-33 — Hard retention limit: measured from what, applied where
- **Kind**: unclear
- **Docs**: PRD.md:1101; ADR-0009:87; DESIGN.md:853
- **Scenarios**: [retention/1.03-positive-hard-retention-limit-force-cancels.md](retention/1.03-positive-hard-retention-limit-force-cancels.md)
- **Assumed**: Measured from `created_at`; applied to every non-terminal task in the domain at the next maintenance pass, not only to the slot being recycled; the task ends `cancelled`/`forcibly_cancelled` with no actor and is then kept for its normal retention.
- **Question**: Confirm the reference time and scope, and whether the hard limit can be set per tenant or type.
- **Decided** (gap review 2026-11-06): confirmed: measured from `created_at`; every maintenance pass force-cancels every non-terminal task in the domain older than `hard_retention_days` (`forcibly_cancelled`, no actor), which then keeps its normal retention from `terminal_at`; one value per domain, no per-tenant or per-type override. Remaining: the doc edit.

### G-OPS-34 — Per-tenant retention and legal hold have no shape
- **Kind**: missing
- **Docs**: PRD.md:1097; DESIGN.md:2173 (`per_tenant: {}`), :2198 (`max_retention_days`, never above `hard_retention_days`)
- **Scenarios**: [retention/1.04-positive-retention-policy-precedence.md](retention/1.04-positive-retention-policy-precedence.md), [retention/1.05-guardrail-legal-hold-does-not-delay-others.md](retention/1.05-guardrail-legal-hold-does-not-delay-others.md)
- **Assumed**: `per_tenant: { "<tenant_id>": { retention_days: N } }` in static config; a legal hold is a long per-tenant retention; no cap is applied.
- **Question**: Define the shape (config or API), whether it is runtime-changeable, whether subtrees inherit it, and whether it may exceed `hard_retention_days` or the trust limits.
- **Decided** (gap review 2026-11-06): runtime config (versioned, `POST /admin/config`): `retention.per_tenant: { "<tenant_id>": { retention_days } }`; a legal hold is a long per-tenant retention; capped at `max_retention_days`; no subtree inheritance; a change applies to every row not yet removed (the rescue step reads the effective retention at recycle time). The cap is `trust_limits.global.max_retention_days`; a larger value is refused `400 OUT_OF_RANGE` when the config is saved. Remaining: the doc edit.

### G-OPS-35 — Per-domain retention: independence and configuration
- **Kind**: unclear
- **Docs**: PRD.md:1101 ("Cleanup runs separately in each task domain"), PRD.md:1174 ("each domain has its own … retention"); DESIGN.md:862, :1980 (one global `te-maintenance` leader); DESIGN.md:2161-2173 (one global retention block)
- **Scenarios**: [retention/1.06-guardrail-cleanup-runs-per-domain.md](retention/1.06-guardrail-cleanup-runs-per-domain.md)
- **Assumed**: A failing pass in one domain does not stop removals in another; retention settings are global.
- **Question**: Does one leader run all domains' passes independently (no head-of-line blocking)? Can a domain have its own ring geometry and policies (as DESIGN.md:864 recommends for long-retention tenants)?
- **Decided** (gap review 2026-11-06): the maintenance leader runs each domain's passes as an independent loop (no head-of-line blocking); a domain may set its own ring geometry and retention policies, defaulting to the global block. Remaining: the doc edit.

### G-OPS-36 — Is `taskQueued` recorded when a task is created into `queued`?
- **Kind**: unclear
- **Docs**: PRD.md:879; DESIGN.md:524; `schemas/types/…task_created.v1~` (`initial_state`)
- **Scenarios**: [flows/1.01-flow-create-claim-execute-complete.md](flows/1.01-flow-create-claim-execute-complete.md)
- **Assumed**: No: `taskCreated` with `initial_state: queued` only.
- **Question**: List the events each create path records (queued, scheduled, blocked, waiting).
- **Decided** (gap review 2026-11-06): see G-SEC-23: every create path records only `taskCreated` with its `initial_state` (plus the wait events for `waiting`). Remaining: the doc edit.

### G-OPS-38 — How a worker acknowledges a graceful cancel
- **Kind**: missing
- **Docs**: PRD.md:348 ("ends the task as `cancelled`"); DESIGN.md:535 ("Worker completes after graceful cancellation"), :1070
- **Scenarios**: [flows/1.09-flow-graceful-cancel-running-task.md](flows/1.09-flow-graceful-cancel-running-task.md)
- **Assumed**: `POST /tasks/{id}/complete` with `result_code: cancelled` ends the task `cancelled` and removes the grace timer.
- **Question**: Name the call and result code, and say whether a worker may still complete with `success` after a cancel request.
- **Decided** (gap review 2026-11-06): `POST /tasks/{id}/complete` with `result_code: cancelled` (allowed only after a cancel request, PRD.md:1039) ends the task `cancelled` and deletes the grace timer; see G-RTY-17 for other codes. Remaining: the doc edit.

### G-OPS-39 — Step number of the runtime-config apply step
- **Kind**: contradiction
- **Docs**: DESIGN.md:1667 ("maintenance job (§3.2 RetentionService step 5)") vs. DESIGN.md:860 (it is step 6; step 5 is "Expire leftovers")
- **Scenarios**: [queues/3.03-positive-config-version-apply-status.md](queues/3.03-positive-config-version-apply-status.md)
- **Assumed**: The apply step is step 6.
- **Question**: Fix the reference at DESIGN.md:1667.

### G-OPS-40 — Scope of the `configure` check on `/admin/config`
- **Kind**: unclear
- **Docs**: PRD.md:923; DESIGN.md:993, :1129
- **Scenarios**: [queues/1.03-negative-queue-action-required.md](queues/1.03-negative-queue-action-required.md)
- **Assumed**: Checked once; a caller without `configure` gets `403` and nothing is saved.
- **Question**: Is `configure` checked per queue the body affects (so policies can grant tuning of some queues only), and how is a pattern that matches queues the caller may not configure handled?
- **Decided** (gap review 2026-11-06): one `configure` check against the queue base type (no ID), all-or-nothing: a caller without it gets `403` and nothing is saved. Per-queue delegation may come later behind a separate endpoint. Remaining: the doc edit.

### G-OPS-41 — Reassignment events: `reason` values and no accept event
- **Kind**: wire
- **Docs**: `schemas/types/…task_reassigned.v1~` (`reason` required, values not listed); DESIGN.md:547-548 (accept records no event); PRD.md:868
- **Scenarios**: [worker/reassignment/1.01-positive-admin-reassigns-running-task.md](worker/reassignment/1.01-positive-admin-reassigns-running-task.md), [worker/reassignment/1.02-positive-only-named-identity-accepts.md](worker/reassignment/1.02-positive-only-named-identity-accepts.md)
- **Assumed**: The engine supplies a `reason`; not asserted. Accept adds no history event.
- **Question**: List the `reason` values (or accept an optional free-text reason on `POST /reassign`), and decide whether accept is recorded (e.g. `taskClaimed` with the new assignee).
- **Decided** (gap review 2026-11-06): `POST /reassign` takes an optional free-text `reason` (≤ 1024 characters, G-OPS-47), recorded in `taskReassigned` (the field becomes optional); accept records `taskClaimed` with the new assignee and `handoff: true`. Remaining: the doc edit.

### G-OPS-42 — Limits and dedup reads during a queue move
- **Kind**: unclear
- **Docs**: Proposals › Domains › During a move (limits: "each domain enforces the limit minus the other domain's latest sampled use"; dedup: "read the source's `te_dedup` before inserting in the target"; supersession: "read the source head on first use and copy it")
- **Scenarios**: [domains/1.07-positive-placement-and-claims-during-a-move.md](domains/1.07-positive-placement-and-claims-during-a-move.md), [domains/1.08-guardrail-dedup-and-supersession-span-a-move.md](domains/1.08-guardrail-dedup-and-supersession-span-a-move.md)
- **Assumed**: a cross-domain dedup read happens before the target's create transaction, so a duplicate created in the source in the same instant can slip through; limits can be exceeded by one sampling interval.
- **Question**: Is the race between the source read and the target insert accepted? How is the other domain's concurrency use sampled (the leader of which domain)?
- **Decided** (gap review 2026-11-06): accepted and documented: during a move the cross-domain dedup read is best effort and limits may overshoot by one sampling interval; each domain's maintenance leader samples its own use and each domain reads the other's latest sample. Remaining: the doc edit.

### G-OPS-43 — Queue introspection shows limits but not the other effective settings
- **Kind**: missing
- **Docs**: PRD.md:491-495 (depth, running count, concurrency limits, effective depth limits, per-state counts); `schemas/types/gts.cf.core.tasks.queue.v1~.schema.json` (registration fields); DESIGN.md:2257-2262 (runtime config overrides), DESIGN.md:1129 (config versions)
- **Scenarios**: [queues/1.01-positive-queue-introspection.md](queues/1.01-positive-queue-introspection.md), [queues/3.02-positive-config-override-precedence.md](queues/3.02-positive-config-override-precedence.md)
- **Assumed**: `GET /queues[/{id}]` also returns `settings` (effective `default_priority`, `timeouts`, `retry_policy`, `tenant_fairness`, `dead_letter_queue`, `operated_by`, `accepted_types`, progress and blacklist settings), `limits.tenant_max_deferred_tasks`, and the `config_version` applied in the queue's domain.
- **Question**: Should introspection show effective settings (operators otherwise cannot see what an override changed), and is the applied config version per queue or per domain?
- **Decided** (wire review 2026-11-04): `GET /queues[/{id}]` returns effective `limits` (incl. `tenant_max_deferred_tasks`), `settings` and the applied `config_version`. Remaining: the doc edit.

### G-OPS-44 — Who returns a ring slot from `free` to `active`
- **Kind**: missing
- **Docs**: DESIGN.md:1434 (`te_ring_state.status` `active` / `recycling` / `free`), DESIGN.md:1439 (*Ring fence*), DESIGN.md:852 (Mark step); ADR-0009
- **Scenarios**: the draft reference DDL (`migration.sql`, kept outside this branch)
- **Assumed**: after truncation the slot is `free` for bucket `B + N`, and a writer treats `free` for its own bucket like `active`; nothing ever sets it back to `active`.
- **Question**: Is `free` a distinct state at all? If so, who moves the slot to `active` and when, and what does a fenced writer do when it finds `free`?
- **Decided** (gap review 2026-11-06): drop `free`: after the truncate the same step sets the slot `active` for bucket `B + N`; a fenced writer writes to the ring table when the slot's bucket equals the task's bucket, else to `_retained`. Remaining: the doc edit.

### G-OPS-45 — Retention of engine- and queue-scoped events
- **Kind**: missing
- **Docs**: DESIGN.md:1438 (engine-scoped events take the slot of their own id), DESIGN.md:1829 (`task_id` NULL); ADR-0009 (rescue is per task)
- **Scenarios**: the draft reference DDL (`migration.sql`, kept outside this branch)
- **Assumed**: `configUpdated`, `workerBlacklisted`, `queueHeld`, `queueReleased` are never rescued and disappear when their slot is recycled (about `(N − 2) × W` days).
- **Question**: What retention applies to events that have no task, and must any of them (audit of config changes) outlive the ring?
- **Decided** (gap review 2026-11-06): no special retention: events without a task follow their ring slot (about `(N − 2) × W` days), documented in DESIGN; long-term audit is a broker consumer's job. Remaining: the doc edit.

### G-OPS-46 — DESIGN indexes that no query uses
- **Kind**: unclear
- **Docs**: DESIGN.md:1473 (`ix_te_tasks_type`, "type-based queries and concurrency counting"), DESIGN.md:1530 (`ix_te_task_state_run_after`, "scheduled enqueue scan"), DESIGN.md:1531 (`ix_te_task_state_assignee`, "worker-based queries"), DESIGN.md:1927 (`ix_te_sessions_worker`, "operator views"), DESIGN.md:1948 (MySQL form of `run_after`), DESIGN.md:1795 (`ix_te_waits_timeout`, "timeout scan"); DESIGN.md §4.6 procTimers (scheduled and wait timeouts fire from `te_timers`)
- **Scenarios**: the draft reference DDL (`migration.sql`, kept outside this branch)
- **Assumed**: only indexes a query uses are created (benchmark later). Not created: `ix_te_tasks_type` (type queries read `te_task_state` / `te_task_results`), `ix_te_task_state_run_after` (scheduled tasks are enqueued by their `scheduled` timer), `ix_te_task_state_assignee` and `ix_te_sessions_worker` (no query filters by worker); `ix_te_waits_timeout` serves only a safety sweep for wait conditions whose `wait` timer was lost.
- **Question**: Name the query each index serves or drop it from DESIGN §3.7 (write-cost rule). Is a timer-loss safety sweep wanted?
- **Decided** (gap review 2026-11-06): drop `ix_te_tasks_type`, `ix_te_task_state_run_after`, `ix_te_task_state_assignee`, `ix_te_sessions_worker` from DESIGN §3.7. Keep a slow safety sweep (about every 5 min) that re-arms a missing `wait` timer and counts `te_wait_timer_repairs_total`: it uses `ix_te_waits_timeout` and, because timer conditions keep their time in `timer_resume_at`, a second partial index `(timer_resume_at) WHERE kind_code = timer AND state_code = pending`. Remaining: the doc edit.

### G-OPS-47 — No length limit for free-text `reason` on cancel and queue hold
- **Kind**: missing
- **Docs**: PRD.md:346-353 and `POST /tasks/<built-in function id>/cancel` `reason`; Proposals › Queue operations › Admin queue hold (`reason`)
- **Scenarios**: the draft reference DDL (`migration.sql`, kept outside this branch)
- **Assumed**: both are stored as `VARCHAR(1024)` (`te_task_state.cancel_reason`, `te_queue_holds.reason`); nothing is asserted about longer values.
- **Question**: Give the maximum length (characters or bytes) and the error for both fields.
- **Decided** (gap review 2026-11-06): `reason` on cancel and queue hold: at most 1024 characters, else `400 TOO_LONG` on `/reason`. Remaining: the doc edit.

