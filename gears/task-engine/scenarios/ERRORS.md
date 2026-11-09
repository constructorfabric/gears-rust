# Task Engine errors: envelope and catalogue of reasons

| | |
|---|---|
| Requirement | `cpt-cf-task-engine-fr-rest-api` (PRD.md:1078) · `cpt-cf-task-engine-nfr-data-protection` (PRD.md:1360, :1364) · DESIGN.md:225, :1180-1236 · `docs/arch/errors/DESIGN.md:220, :521-536, :974` · `docs/arch/errors/categories/` |
| Phase | p1 |
| Gaps | G-SEC-01, G-SEC-02, G-SEC-03, G-SEC-19, G-SEC-21, G-SEC-22, G-WIRE-01, G-WIRE-14, G-WIRE-15, G-EXE-07, G-CLM-09, G-CLM-10 |

This is a **reference, not a scenario**. It shows the one error envelope every Task Engine `4xx`/`5xx` uses, maps each category to its status and `context` keys, and lists every Task Engine code with the scenario that owns it. Tests come from the owning scenarios; the examples here are the shapes those scenarios assert. The rules (where a code goes, field paths, `Retry-After` values) are in [CONVENTIONS §6](CONVENTIONS.md#6-errors) and are not repeated here. All of them follow the gap decisions in [GAPS.md](GAPS.md); the PRD and DESIGN edits are still owed.

Links are relative to the scenarios root.

---

## Envelope

Every error is `application/problem+json`, RFC 9457, built from a platform canonical category (`toolkit-canonical-errors`). The engine defines no error types of its own; its machine-readable detail lives in `context`.

```http
GET /api/task_engine/v1/tasks/{task_404} HTTP/1.1
Host: api.example.com
Authorization: Bearer <creator-a-token>
```

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "title": "Not Found",
  "status": 404,
  "detail": "Task with ID {task_404} not found",
  "instance": "/api/task_engine/v1/tasks/{task_404}",
  "trace_id": "<trace>",
  "context": { "resource_type": "gts.cf.core.tasks.task.v1~", "resource_name": "{task_404}" }
}
```

- `type`: `gts://gts.cf.core.errors.err.v1~cf.core.err.<category>.v1~`, always with the `gts://` prefix (`docs/arch/errors/DESIGN.md:525`; DESIGN.md:1186 to be fixed, G-WIRE-01).
- `title`: the category title. `status`: may differ from the category default only by a transport override in the same class (413, 428 below). `detail`: human text. `instance`: the request path. `trace_id`. `context`: category-specific, always present (`{}` when empty).
- Codes sit in the slot each category has (G-WIRE-14): `context.reason` only for `permission_denied`, `aborted` and `unauthenticated`; `field_violations[].reason` for `invalid_argument`; `violations[{type: "STATE", subject: <code>}]` for `failed_precondition`; quota `violations[].subject` for `resource_exhausted`. `404` and `403` carry no field violations (G-WIRE-15).
- Field paths are RFC 6901 JSON Pointers from the body root; headers and query parameters are named as sent (G-SEC-21).
- No error body ever contains payload contents ([data protection](#data-protection-no-payload-echo)) or PDP internals (PRD.md:918).

## Category → status → context

Any category may also carry `resource_type` and `resource_name`.

| Category | Title | Status | `context` keys | Section |
|---|---|---|---|---|
| `invalid_argument` | Invalid Argument | 400; 413 for payload/body size; 428 for a missing `Idempotency-Key` | one of `field_violations[]`, `format`, `constraint` | [invalid_argument](#invalid_argument--400-413-428) |
| `unauthenticated` | Unauthenticated | 401 | `reason?` | [unauthenticated](#unauthenticated--401) |
| `permission_denied` | Permission Denied | 403 | `reason` | [permission_denied](#permission_denied--403) |
| `not_found` | Not Found | 404 | nothing else | [not_found](#not_found--404) |
| `already_exists` | Already Exists | 409 | nothing else | [already_exists](#already_exists--409) |
| `aborted` | Aborted | 409 | `reason` | [aborted](#aborted--409) |
| `failed_precondition` | Failed Precondition | 400 (not 409: see [Event Broker contrast](#event-broker-contrast-failed_precondition-is-400-here-409-there)) | `violations[]` | [failed_precondition](#failed_precondition--400) |
| `resource_exhausted` | Resource Exhausted | 429 + `Retry-After` | `violations[]` | [resource_exhausted](#resource_exhausted--429) |
| `service_unavailable` | Service Unavailable | 503 + `Retry-After` | `retry_after_seconds` | [service_unavailable](#service_unavailable--503) |
| `internal` | Internal | 500 | nothing | [internal](#internal--500) |

`403` is only for a caller who may already read the target (or, on create, see the type). A request about a task the caller may not read gets the same `404` as a missing task (PRD.md:918, DESIGN.md:1016). Worker calls are the exception: they never answer `404` (G-EXE-09, [aborted](#aborted--409)).

---

## invalid_argument — 400, 413, 428

The caller sent something the engine will never accept as written. The body lists one field violation per problem; retrying the same request gives the same answer (DESIGN.md:1204, :1210-1213, :1217, :1220-1225, :1228; `docs/arch/errors/categories/03-invalid-argument.md`).

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "Input does not match the task type's input schema",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": {
    "resource_type": "gts.cf.core.tasks.task.v1~acme.reports._.render.v1~",
    "field_violations": [ { "field": "/input/report_id", "description": "required", "reason": "SCHEMA_VIOLATION" } ]
  }
}
```
(`POST /tasks` of **render** with `input: { "format": "pdf" }`.)

| Condition | `field_violations[].reason` on | Owner |
|---|---|---|
| Input, output, entry payload or resolve value fails its schema | `SCHEMA_VIOLATION` on the pointer (G-SEC-21) | [tasks/create/1.09](tasks/create/1.09-negative-input-fails-schema.md), [worker/execution/](worker/execution/), [entries/](entries/), [waits/input/](waits/input/) |
| Unknown, invisible or removed task type | `UNKNOWN_TASK_TYPE` on `/type` | [tasks/create/](tasks/create/) |
| Non-creatable type (invalid dedup or supersede policy, tenant-owned `authority.mode = worker`, trait outside trust limits) | `INVALID_TYPE_POLICY` on `/type` (G-CRT-15) | [auth/1.08](auth/1.08-negative-tenant-owned-worker-authority-type.md), [tasks/dedup/](tasks/dedup/) |
| A request value outside `trust_limits`; tenant type on a platform queue; type not in `accepted_types` | `TRUST_LIMIT` / `NOT_ALLOWED` on the setting (inherited values are clamped instead, G-CRT-05) | [tasks/create/](tasks/create/) |
| Dead-letter queue named on create, move or claim | `DEAD_LETTER_QUEUE` on `/queue` or `/queues/i` (G-CRT-02, G-CLM-18) | [tasks/create/](tasks/create/), [worker/claim/](worker/claim/) |
| Same `Idempotency-Key`, different request | `KEY_REUSED` on `Idempotency-Key` | [tasks/idempotency/](tasks/idempotency/) |
| `Idempotency-Key` empty or malformed | `INVALID_FORMAT` on `Idempotency-Key` (G-CRT-09) | [tasks/idempotency/](tasks/idempotency/) |
| Tags: too many, key or value too long, empty key | `TOO_MANY_TAGS` on `/tags`, `TOO_LONG`, `REQUIRED` (G-CRT-06) | [tasks/create/](tasks/create/) |
| `resource_id` over 256, dedup key-field value over 4 KiB, `reason` over 1024 | `TOO_LONG` (G-CRT-10, G-CRT-16, G-OPS-47) | [tasks/](tasks/), [queues/](queues/) |
| `run_after` after the lifetime deadline; `delay_secs` out of range | `OUT_OF_RANGE` (G-CRT-22, G-EXE-25) | [tasks/create/](tasks/create/), [worker/release/](worker/release/) |
| Initial `wait` with predecessors or a future `run_after`; `children` in it | `NOT_ALLOWED` on `/wait` (G-CRT-01) | [tasks/create/](tasks/create/) |
| p2 feature in p1 (`input`, `children`, `event` condition; `parent_task_id`) | `FEATURE_NOT_AVAILABLE` on the field (G-WAI-43) | [waits/](waits/), [hierarchy/](hierarchy/) |
| Wait group empty or over 16 conditions; timeout on a timer condition; `on_timeout` without a timeout; missing `lease_token` | `REQUIRED`, `TOO_MANY`, `NOT_ALLOWED` (G-WAI-06, G-WAI-09, G-WAI-32, G-WAI-01) | [waits/framework/](waits/framework/) |
| Missing `parent_lease_token`; parent too deep; same predecessor twice; too many predecessors | `REQUIRED`, `TOO_DEEP`, `DUPLICATE`, `DEPENDENCY_GRAPH_TOO_LARGE` (G-WAI-02, G-WAI-39, G-WAI-42) | [hierarchy/](hierarchy/) |
| Type outside its domain's type rule | `TASK_DOMAIN_MISMATCH` on `/queue` | [domains/](domains/) |
| Claim validation | `REQUIRED`, `INVALID_FORMAT`, `OUT_OF_RANGE`, `TOO_MANY_QUEUES`, `CROSS_DOMAIN` (G-CLM-18) | [worker/claim/](worker/claim/) |
| Batch heartbeat over 500 leases | `TOO_MANY` on `/leases` (G-EXE-04) | [worker/sessions/](worker/sessions/) |
| Completion result invalid (output schema, `TaskError` contract, engine-only result code) | on `/result_code`, `/output/…` or `/error/…` | [worker/execution/](worker/execution/) |
| Unknown entry type | `UNKNOWN_ENTRY_TYPE` on `/type` (G-EXE-19) | [entries/](entries/) |
| Event condition: unknown or hidden type, topic not waitable, subject type rules, timeout over the cap; bad CEL | `UNKNOWN_EVENT_TYPE`, `EVENT_TOPIC_NOT_WAITABLE`, `EVENT_SUBJECT_TYPE_REQUIRED`, `EVENT_SUBJECT_TYPE_NOT_ALLOWED`, `INVALID_MATCH_EXPRESSION` (G-WAI-29, G-WAI-30, G-WAI-34) | [waits/event/](waits/event/) |
| Search: bad `$orderby`, `view`, `$select`, window, `limit=0`, cursor errors, unsupported `$` option | `NOT_ALLOWED`, `OUT_OF_RANGE` on `$filter`, `INVALID_LIMIT`, `INVALID_CURSOR`, `FILTER_MISMATCH`, `ORDER_WITH_CURSOR`, `FIELD_SELECTION_MISMATCH` on `cursor` (G-SEC-26), `UNSUPPORTED_QUERY_PARAM` | [search/1.06](search/1.06-negative-unsupported-query.md) |
| Count with `$orderby`, `$select`, `limit`/`$top`, `cursor`/`$skiptoken` or `view` | on the parameter | [search/1.07](search/1.07-negative-count-rejects-paging-and-view.md) |
| History `limit=0` | `INVALID_LIMIT` on `limit` | [events/1.02](events/1.02-positive-history-cursor-paged.md) |
| Bulk request over `bulk_max_ids` (1000) | `BATCH_TOO_LARGE` on `/task_ids` (G-OPS-15) | [bulk/](bulk/) |
| A time past `2038-01-19T03:14:07Z` on MySQL | `OUT_OF_RANGE` (G-WIRE-20) | — |

- `description` names the rule broken, never the value sent ([data protection](#data-protection-no-payload-echo)).

### 413 — payload or request body over the limit

Category stays `invalid_argument`; the status is overridden to `413` (`cpt-cf-task-engine-nfr-payload-size`, PRD.md:1276-1278, :1500, :1502; DESIGN.md:1209, :2142-2144). Two limits: the combined task payload (input + context + output, uncompressed) of 64 KiB, and the HTTP body of 1 MiB, which the engine's own body-limit layer enforces before the body is read (G-CRT-24).

```http
HTTP/1.1 413 Content Too Large
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 413,
  "detail": "Request body exceeds 1048576 bytes",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": { "resource_type": "gts.cf.core.tasks.task.v1~", "constraint": "request body at most 1048576 bytes" }
}
```
(`POST /tasks` with `Content-Length: 2097152`.)

| Condition | Owner |
|---|---|
| Body over `max_request_body_bytes` (with or without `Content-Length`) | [tasks/create/1.11](tasks/create/1.11-negative-payload-and-body-size-limits.md) |
| Combined payload over 64 KiB on create, checkpoint (after the merge), completion | [tasks/create/1.11](tasks/create/1.11-negative-payload-and-body-size-limits.md), [worker/](worker/) |
| Entry payload, or a resolve value on its own, over 64 KiB | [entries/](entries/), [waits/input/](waits/input/) (G-WAI-22) |

- The `constraint` variant of the platform context is used because no field is at fault.

### 428 — Idempotency-Key missing

Category `invalid_argument` with a transport override to `428 Precondition Required` (`cpt-cf-task-engine-fr-idempotent-create`, PRD.md:276, :1510; DESIGN.md:718, :1079, :1205). Only types with a derived dedup identity may omit the header. It comes after authorization in the check order, so a denied caller learns nothing about the type's key rule (G-CRT-24).

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 428,
  "detail": "Idempotency-Key header is required",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": { "resource_type": "gts.cf.core.tasks.task.v1~acme.reports._.render.v1~",
               "field_violations": [ { "field": "Idempotency-Key", "description": "header is required", "reason": "REQUIRED" } ] }
}
```

| Condition | Owner |
|---|---|
| `POST /tasks` without `Idempotency-Key` for a type with no derived identity | [tasks/idempotency/](tasks/idempotency/) |

## unauthenticated — 401

Produced by the platform AuthN layer before the engine runs (DESIGN.md:974-975; `docs/arch/errors/categories/16-unauthenticated.md`). `context.reason` is the AuthN middleware's code.

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
  "title": "Unauthenticated",
  "status": 401,
  "detail": "Authentication required",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": { "reason": "MISSING_CREDENTIALS" }
}
```
(`GET /tasks` without `Authorization`.)

| Condition | `context.reason` | Owner |
|---|---|---|
| No `Authorization` header | `MISSING_CREDENTIALS` | [auth/1.04](auth/1.04-negative-unauthenticated-request.md) |
| Expired token | `TOKEN_EXPIRED` | [auth/1.04](auth/1.04-negative-unauthenticated-request.md) |
| Malformed or wrongly signed token | AuthN code | [auth/1.04](auth/1.04-negative-unauthenticated-request.md) |
| Health or metrics path under the base path, unauthenticated | AuthN code | [auth/1.04](auth/1.04-negative-unauthenticated-request.md) |

## permission_denied — 403

The caller can see the target but may not do this (DESIGN.md:1016, :1202, :1214, :1218, :1222, :1229, :1231; `docs/arch/errors/categories/07-permission-denied.md`). Every `403` carries a reason from the closed list below (G-SEC-19). A creator-check or condition index goes only into `detail` (G-WIRE-15).

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
  "title": "Permission Denied",
  "status": 403,
  "detail": "Urgency 'critical' needs the expedite action",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": { "resource_type": "gts.cf.core.tasks.task.v1~acme.reports._.render.v1~", "reason": "EXPEDITE_REQUIRED" }
}
```
(`POST /tasks` of **render** with `"urgency": "critical"` by `{creator_a}`, who has no `expedite`.)

| Condition | `context.reason` | Owner |
|---|---|---|
| Action denied on a readable task (cancel by a reader, token-scope ceiling, `update` grant on resolve); create denied for type or queue; search or bulk with no right at all; queue config or hold without `configure`; re-enqueue without `admin`; a worker that cancels, resolves, reassigns or re-enqueues | `ACTION_DENIED` | [auth/1.05](auth/1.05-negative-read-but-not-act.md), [auth/1.06](auth/1.06-negative-type-policy-denies-other-type.md), [auth/1.13](auth/1.13-guardrail-worker-least-privilege.md), [auth/1.15](auth/1.15-guardrail-queue-read-grants-no-task-data.md), [queues/](queues/), [dead-letter/1.04](dead-letter/1.04-negative-reenqueue-refusals.md), [bulk/](bulk/) |
| `critical` without `expedite` | `EXPEDITE_REQUIRED` | [tasks/create/1.06](tasks/create/1.06-guardrail-critical-urgency-needs-expedite.md) |
| Claim names a queue the policy provably excludes | `QUEUE_NOT_PERMITTED` (G-CLM-14) | [auth/1.12](auth/1.12-negative-claim-names-queue-outside-policy.md), [worker/claim/1.09](worker/claim/1.09-guardrail-queue-policy-limits-what-worker-receives.md) |
| Creator check denied / not decidable | `CREATOR_AUTHORITY`, `CREATOR_AUTHORITY_UNVERIFIABLE` | [auth/1.07](auth/1.07-negative-creator-authority-refused.md) |
| Worker call whose scope no longer covers the task it holds (task released) | `CLAIM_REVOKED` (G-EXE-08) | [worker/fencing/](worker/fencing/) |
| Event-wait `consume` denied (also when the tenant filter leaves no constraint) / not enforceable | `EVENT_CONSUME_DENIED`, `EVENT_CONSUME_UNVERIFIABLE` (G-WAI-28) | [waits/event/](waits/event/) |
| Dedup hit on a task the caller cannot read (no identifier) | `DUPLICATE_NOT_READABLE` | [tasks/dedup/](tasks/dedup/) |
| Supersession without `cancel` on the old task | `SUPERSEDE_CANCEL_DENIED` | [tasks/supersede/1.06](tasks/supersede/1.06-negative-caller-lacks-cancel-on-old-task.md) |

- Bulk endpoints answer `403` only when the PDP denies the action outright; otherwise `200` with a per-task `unauthorized` or `not_found` (G-OPS-14).

## not_found — 404

One answer for "does not exist" and "you may not see it" (PRD.md:857, :918; DESIGN.md:1016, :1199-1201; `docs/arch/errors/categories/05-not-found.md`). `context.resource_type` is the base type of the thing looked up; `resource_name` is the ID the caller sent. Nothing else: no field violation (G-WIRE-15).

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "title": "Not Found",
  "status": 404,
  "detail": "Entry {entry_404} not found",
  "instance": "/api/task_engine/v1/tasks/{task_1}/entries/{entry_404}",
  "trace_id": "<trace>",
  "context": { "resource_type": "gts.cf.core.tasks.entry.v1~", "resource_name": "{entry_404}" }
}
```
(`GET /tasks/{task_1}/entries/{entry_404}` by `{creator_a}`; no side effects.)

| Condition | Owner |
|---|---|
| Task missing, of another tenant, or not readable (every single-task route except worker calls) | [auth/1.10](auth/1.10-guardrail-no-existence-oracle.md), [auth/1.16](auth/1.16-flow-tenant-isolation-three-tenants.md), [tasks/read/](tasks/read/) |
| Task outside the caller's tenant scope or behind a barrier | [auth/1.11](auth/1.11-guardrail-tenant-scope-of-reads.md) |
| Task removed by retention | [tasks/read/1.03](tasks/read/1.03-negative-retention-removed-task-is-not-found.md) |
| Entry or wait condition missing or not readable (`resolve` needs no `read`, G-SEC-18) | [waits/](waits/), [entries/](entries/) |
| Parent or predecessor missing, foreign, in another domain, or with a wrong parent lease token | [hierarchy/children/](hierarchy/children/), [hierarchy/dependencies/](hierarchy/dependencies/) (G-WAI-02, G-WAI-41) |
| Claim, create or move naming an unregistered queue (`resource_type` = queue base type) | [worker/claim/1.11](worker/claim/1.11-negative-claim-naming-unknown-queue-not-found.md), [queues/1.02](queues/1.02-negative-unknown-queue-not-found.md), [tasks/create/](tasks/create/) (G-CRT-03) |
| Accept by anyone but the new assignee, or nothing to accept | [worker/reassignment/](worker/reassignment/) |
| Removed endpoints (`GET /admin/dlq`, dependency add/remove, single-task heartbeat): the router answers `404` | [dead-letter/1.05](dead-letter/1.05-guardrail-admin-dlq-endpoint-removed.md), [hierarchy/dependencies/](hierarchy/dependencies/) (G-WAI-03) |
| Unknown path under the base path (for example `/metrics` when authenticated) | [auth/1.04](auth/1.04-negative-unauthenticated-request.md) |

An unsupported method on an existing path (for example `DELETE` on an entry) is `405 Method Not Allowed` with an `Allow` header and no problem body: the router's default (G-EXE-22).

## already_exists — 409

An idempotent or deduplicated create found its task (`cpt-cf-task-engine-fr-idempotent-create`, PRD.md:276, :1493, :1510, :1537; DESIGN.md:718-719, :1203; `docs/arch/errors/categories/06-already-exists.md`). The response names the existing task; the caller reads it instead of creating another. A replay writes nothing, takes no capacity, and is never refused as full (depth is checked after dedup, G-CLM-24).

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.already_exists.v1~",
  "title": "Already Exists",
  "status": 409,
  "detail": "A task with this idempotency key already exists",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": { "resource_type": "gts.cf.core.tasks.task.v1~", "resource_name": "{task_1}" }
}
```
(The CONVENTIONS §5.2 request replayed unchanged with `Idempotency-Key: 7d1f0c2e-render-r-42`. `resource_type` is the task base type, G-CRT-25.)

| Condition | Owner |
|---|---|
| Replayed caller key, equal request (a retry without `queue`, or naming the resolved queue, is equal, G-CRT-11) | [tasks/idempotency/](tasks/idempotency/) |
| Duplicate derived dedup identity | [tasks/dedup/](tasks/dedup/) |
| Equal supersede order value (replay) | [tasks/dedup/](tasks/dedup/), [tasks/supersede/](tasks/supersede/) |
| Re-enqueue when another task now holds the dedup identity | [dead-letter/1.04](dead-letter/1.04-negative-reenqueue-refusals.md) |

- The replay status (`409` vs a cached `2xx`) is to be agreed as a platform rule in #5480 (DESIGN.md:718).

## aborted — 409

The request was valid but another writer got there first (`cpt-cf-task-engine-fr-lease-fencing`, PRD.md:722, :1520-1522; DESIGN.md:734, :1081, :1206-1208; `docs/arch/errors/categories/10-aborted.md`). Three reasons, each telling the client what to do (G-SEC-22).

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~",
  "title": "Aborted",
  "status": 409,
  "detail": "Lease is no longer valid",
  "instance": "/api/task_engine/v1/tasks/{task_1}/complete",
  "trace_id": "<trace>",
  "context": { "resource_type": "gts.cf.core.tasks.task.v1~", "resource_name": "{task_1}", "reason": "LEASE_LOST" }
}
```
(`POST /tasks/{task_1}/complete` with a superseded `{lt_1}` after the task was claimed again with `{lt_2}`; `metric te_lease_fencing_rejections_total{op="complete"} increases by 1`, the new holder's state is untouched.)

| Condition | `context.reason` | Owner |
|---|---|---|
| Worker call from anyone but the current holder with that token: stale or unknown token, unknown task, foreign tenant (worker calls never answer `404`); a zombie with an invalid result; a repeated non-terminal report after a lost response | `LEASE_LOST` (G-EXE-09, G-EXE-16, G-EXE-17) | [worker/fencing/](worker/fencing/) |
| Payload write lost the `revision` guard | `CONCURRENT_MODIFICATION` | [worker/](worker/) |
| Non-worker write whose state guard lost (cancel, resolve, re-enqueue, move, reprioritize, reassign); supersession out of retries | `STATE_CHANGED` (G-SEC-22, G-CRT-19) | [tasks/cancel/](tasks/cancel/), [tasks/supersede/](tasks/supersede/) |

- The response never echoes the token sent.

## failed_precondition — 400

The request is well formed, but the target is not in a state that allows it (`cpt-cf-task-engine-fr-state-result-enums`, PRD.md:1028, :1494; DESIGN.md:1230, :1233-1235; `docs/arch/errors/categories/09-failed-precondition.md`). The code is the violation's `subject`, with `type: "STATE"` (G-WIRE-14).

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
  "title": "Failed Precondition",
  "status": 400,
  "detail": "Task is completed; only failed or dead-lettered tasks can be re-enqueued",
  "instance": "/api/task_engine/v1/admin/tasks/{task_1}/reenqueue",
  "trace_id": "<trace>",
  "context": {
    "resource_type": "gts.cf.core.tasks.task.v1~", "resource_name": "{task_1}",
    "violations": [ { "type": "STATE", "subject": "STATE", "description": "task must be failed or dead_lettered" } ]
  }
}
```
(`POST /admin/tasks/{task_1}/reenqueue` by `{admin}` on a `completed` task; only `dead_lettered` or `failed` tasks can be re-enqueued, PRD.md:1028.)

| Condition | `violations[].subject` | Owner |
|---|---|---|
| Invalid state transition; repeated complete with a different result; move of a leased task; re-enqueue of a task that is not failed or dead-lettered | `STATE` | [worker/execution/](worker/execution/), [bulk/](bulk/), [dead-letter/1.04](dead-letter/1.04-negative-reenqueue-refusals.md) |
| Supersede submission with a lower order value (names no task) | `STALE_ORDER` (G-CRT-25) | [tasks/supersede/1.04](tasks/supersede/1.04-negative-order-not-above-latest-does-not-supersede.md) |
| Successful completion of a `wait_for_children` parent with unfinished children | `CHILDREN_PENDING` | [hierarchy/](hierarchy/) |
| Cancel of a task that ended other than cancelled | `TASK_FINISHED` (G-RTY-13) | [tasks/cancel/](tasks/cancel/) |
| `requested`/`graceful` cancel of a `cancellable: false` task | `NOT_CANCELLABLE` (G-RTY-14) | [tasks/cancel/](tasks/cancel/) |
| Park with nothing armed | `NO_ARMED_WAIT` (G-WAI-13) | [waits/framework/](waits/framework/) |
| Resolve with another value or by another subject; resolve of a non-input condition | `ALREADY_RESOLVED`, `NOT_RESOLVABLE` (G-WAI-05) | [waits/input/1.04](waits/input/1.04-negative-resolve-state-refusals.md) |
| Unsatisfiable dependency set at create | `DEPENDENCY_UNSATISFIABLE` (G-WAI-36) | [hierarchy/dependencies/](hierarchy/dependencies/) |
| Re-enqueue of a child whose parent ended | `PARENT_FINISHED` (G-WAI-24) | [hierarchy/children/](hierarchy/children/) |
| Reassign to a target blacklisted on the queue | `TARGET_BLACKLISTED` (G-OPS-27) | [worker/reassignment/](worker/reassignment/) |
| Entry over `max_entries_per_task` (1000) on the task | `TOO_MANY_ENTRIES` (G-EXE-21) | [entries/1.04](entries/1.04-negative-entry-create-refused.md) |
| Hold of a dead-letter queue (`resource_name` = the queue) | `DEAD_LETTER_QUEUE` (G-OPS-10) | [queues/](queues/) |

- A blacklisted worker's claim is not an error: the queue is skipped, `204` when nothing else has work (G-OPS-30).

## resource_exhausted — 429

Back-pressure (`cpt-cf-task-engine-fr-queue-depth`, PRD.md:436, :455, :1480, :1492, :1540; DESIGN.md:1216, :1226-1227, :1232, :2376; `docs/arch/errors/categories/08-resource-exhausted.md`). The response names only the full quota and never shows usage figures of any tenant. It always carries `Retry-After`, emitted by the library from the largest `retry_after_seconds` (G-SEC-03).

```http
HTTP/1.1 429 Too Many Requests
Retry-After: 22
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~",
  "title": "Resource Exhausted",
  "status": 429,
  "detail": "Queue depth limit reached",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": {
    "resource_type": "gts.cf.core.tasks.queue.v1~", "resource_name": "gts.cf.core.tasks.queue.v1~acme.reports._.render.v1",
    "violations": [ { "subject": "queue_depth.tenant", "description": "tenant queue depth limit reached", "retry_after_seconds": 22 } ]
  }
}
```
(`POST /tasks` by `{creator_a}` into **reports** while tenant A's latest sampled count of queued tasks there is at `tenant_max_queue_depth`. `Retry-After` is in [15, 30]. `metric te_queue_depth_rejections_total{queue="gts.cf.core.tasks.queue.v1~acme.reports._.render.v1",dimension="tenant"} increases by 1`; no task, no `te_dedup` row.)

| `violations[].subject` | When | `Retry-After` | Owner |
|---|---|---|---|
| `concurrency` | Claim assigned nothing and skipped a task for capacity | 1 s (the claim poll interval) | [limits/concurrency/](limits/concurrency/), [worker/claim/](worker/claim/) |
| `queue_depth.queue`, `queue_depth.tenant`, `queue_depth.domain_global`, `queue_depth.deferred` | Create, move, re-enqueue, delayed release, park, initial wait group | [15, 30] | [limits/depth/](limits/depth/), [dead-letter/1.04](dead-letter/1.04-negative-reenqueue-refusals.md) |
| `event_waits.tenant`, `event_waits.event_type`, `event_waits.unkeyed` | Event-wait set or arm (lease kept) | [15, 30] | [waits/event/](waits/event/) |
| rate limit (p2) | not named | — | [limits/](limits/) |

- `resource_type`/`resource_name`: the queue, except `queue_depth.deferred`, which names the task base type and no resource (G-CLM-10).
- Bulk move and bulk retry report a depth refusal per task as `limit_exceeded` instead (G-CLM-25).
- Replays (`409`) and engine-driven requeues are never refused as full (PRD.md:453-454; home [tasks/idempotency/](tasks/idempotency/)).

## service_unavailable — 503

A dependency the decision needs is down, or the node is overloaded; the engine refused without changing anything, and retrying later is safe (`cpt-cf-task-engine-fr-worker-principals`, PRD.md:958, :961; DESIGN.md:1022, :1041, :1215, :1219; `docs/arch/errors/categories/14-service-unavailable.md`). `Retry-After` = `unavailable_retry_after_secs` (5). The Event Broker is **not** such a dependency: a broker outage never fails a task operation ([events/1.07](events/1.07-guardrail-broker-down-tasks-continue.md)).

```http
HTTP/1.1 503 Service Unavailable
Retry-After: 5
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
  "title": "Service Unavailable",
  "status": 503,
  "detail": "Type registry unavailable",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": { "retry_after_seconds": 5 }
}
```
(`POST /tasks` with the CONVENTIONS §5.2 body while the GTS registry is unreachable and **render** is not in the engine's type cache; nothing is written. A cached type is still accepted during the outage, G-CRT-23.)

| Condition | Owner |
|---|---|
| GTS registry unreachable, type not cached (or its tenant's visibility entry expired) | [tasks/create/1.08](tasks/create/1.08-negative-registry-outage-uncached-type-503.md) |
| PDP unreachable on any endpoint: create, read, search, cancel, admin, claim, worker writes, creator checks, event-wait checks; nothing written, leases kept; heartbeat does not call the PDP | [auth/1.09](auth/1.09-negative-pdp-unreachable-fails-closed.md), [worker/fencing/](worker/fencing/), [worker/sessions/](worker/sessions/), [waits/event/](waits/event/) (G-SEC-01) |
| Node overload: admission permits exhausted (heartbeat, complete, release, wait set/park and cancel use a reserved pool) | [limits/](limits/) (G-EXE-07) |

## internal — 500

An infrastructure failure: a database error or a serialization bug (DESIGN.md:225; `docs/arch/errors/DESIGN.md:220`; `docs/arch/errors/categories/13-internal.md`). The body carries the trace ID for support and nothing else: no SQL, host names, stack traces or payload data. The write it interrupted is rolled back, so the task is unchanged.

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
  "title": "Internal",
  "status": 500,
  "detail": "Internal error",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": {}
}
```
(`POST /tasks` with the CONVENTIONS §5.2 body while the task-domain database drops the connection during the create transaction.)

| Condition | Owner |
|---|---|
| Database failure during a write: no task, no history, no outbox row; the idempotency key is not taken (it is written in the same transaction, DESIGN.md:718), so a retry with the same key creates the task once | no scenario yet; this example is the assertion |
| OData `Db(String)` error from search (`docs/toolkit_unified_system/07_odata_pagination_select_filter.md:580`) | [search/](search/) |

---

## Event Broker contrast: failed_precondition is 400 here, 409 there

The Task Engine keeps the platform default status `400` for `failed_precondition`. The Event Broker overrides the same category to `409` for its sequence conflicts. The Task Engine does not, so clients must not treat every `failed_precondition` as a conflict: in the Task Engine a `409` is always `already_exists` or `aborted`.

## Data protection: no payload echo

Payload and entry contents must not appear in error messages (`cpt-cf-task-engine-nfr-data-protection`, PRD.md:1360, :1364; DESIGN.md:2344, :2368; `docs/arch/errors/DESIGN.md:974`). A schema violation names the field and the rule, never the value the caller sent, because the value may be personal data and error bodies are logged and traced. The engine builds the description from the failed keyword and never forwards the validator's message (G-SEC-21).

```http
POST /api/task_engine/v1/tasks HTTP/1.1
Host: api.example.com
Authorization: Bearer <creator-a-token>
Content-Type: application/json
Idempotency-Key: 7d1f0c2e-render-pii

{ "type": "gts.cf.core.tasks.task.v1~acme.reports._.render.v1~",
  "queue": "gts.cf.core.tasks.queue.v1~acme.reports._.render.v1",
  "input": { "report_id": "r-42", "format": "jane.doe@example.com" } }
```

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "Input does not match the task type's input schema",
  "instance": "/api/task_engine/v1/tasks",
  "trace_id": "<trace>",
  "context": { "resource_type": "gts.cf.core.tasks.task.v1~acme.reports._.render.v1~",
               "field_violations": [ { "field": "/input/format", "description": "enum", "reason": "SCHEMA_VIOLATION" } ] }
}
```

- `400`, a field violation on `/input/format`. No member of the body (`detail`, `context`, any `description`) contains `jane.doe@example.com` or any other input value. Nothing is written.
- The log line for this request carries no payload either (log-scanning test, PRD.md:1364).
- Applies to every error that inspects payloads: output-schema violations on `complete`, entry schema violations, `resolve` values that fail `input_schema`, CEL parse errors that quote the expression's literals. Creator-check refusals name the check index only in `detail`, never the resource ID ([auth/1.07](auth/1.07-negative-creator-authority-refused.md)).
- Owning scenario: [tasks/create/1.09](tasks/create/1.09-negative-input-fails-schema.md) (input schema violation). Published events follow the same rule: [events/1.06](events/1.06-guardrail-events-carry-no-sensitive-data.md).
