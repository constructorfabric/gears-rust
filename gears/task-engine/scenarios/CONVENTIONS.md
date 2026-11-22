# Task Engine Scenarios: Conventions

Shared rules, fixtures and wire shapes for every scenario under `gears/task-engine/scenarios/`. Read this before writing or reviewing a scenario.

The Task Engine docs on `main` have **no OpenAPI contract** and only one JSON example (DESIGN.md:1184). The scenarios' contract is the draft OpenAPI in [#5791](https://github.com/constructorfabric/gears-rust/pull/5791) (branch `docs/task-engine-openapi`, `gears/task-engine/docs/openapi.yaml`); see §5. Where a scenario's Notes or a gap entry say where a shape comes from, they use these marks:

- **D**: named in PRD, DESIGN or an ADR on `main` (line given where useful).
- **A**: agreed in the wire review of the draft OpenAPI (2026-11-04) or the gap review (2026-11-06); not yet in the PRD or DESIGN. The gap it cites records the decision in a `Decided` line.
- **P**: comes from an accepted proposal of [PR #5730](https://github.com/constructorfabric/gears-rust/pull/5730) (§1).
- **S**: set by these scenarios because the docs say nothing. Every **S** field is a doc gap, recorded in [GAPS.md](GAPS.md) under `G-WIRE-*`.

When a doc and this file disagree, the doc wins. Fix this file and the scenarios that use it.

---

## 1. Baseline

Scenarios assert **`main` at `6efb0f856` plus every `Proposed` item of the design proposals in [PR #5730](https://github.com/constructorfabric/gears-rust/pull/5730)** (`gears/task-engine/docs/proposals/`, commit `d29da91fe`), taken as accepted. The scenarios call these items **accepted proposals**.

- A scenario whose behaviour comes from an accepted proposal says so in its header (`Depends on`), and cites the `main` lines the proposal replaces.
- `Open` proposal items are **not** assumed. Where the outcome depends on one, the scenario asserts only what holds either way, and adds a gap entry that names the open item.
- Anything the proposals do not mention stays as on `main`, unless a `Decided` line in [GAPS.md](GAPS.md) settles it. Items of the older local `TASK_ENGINE_PROPOSALS.md` that are not in the PR (bulk move and reprioritize, entries, the blacklist, session lease length, supersession scope, the counter items) are **not** assumed; optional `start` is assumed because the gap review decided it (G-EXE-11).
- **Gap decisions.** Every gap now has a `Decided` line: from the wire review (2026-11-04) or the gap review (2026-11-06). Scenarios, this file, [ERRORS.md](ERRORS.md), the draft OpenAPI and the reference DDL follow those decisions; what is left is the PRD, DESIGN, ADR and schema edit each entry names under `Remaining`.
- Behaviour removed by an accepted proposal gets a **guardrail** scenario showing the removed endpoint or option is refused (for example `POST /tasks/{id}/dependencies` → `404`, G-WAI-03).

Accepted items that change the API (quick reference; source file in brackets):

| Topic | Accepted change |
|---|---|
| Task mutability [01] | No `POST/DELETE /tasks/{id}/dependencies`, no consumer `POST /tasks/{id}/wait-conditions` (the worker path with a lease token stays). Create takes `predecessors[].on_predecessor_failure` (`cancel`\|`ignore`, default `cancel`) and an optional initial `wait` group. `PATCH /tasks/{id}/tags` **stays**, with a new limit on tags per task (`max_tags_per_task`, 16, G-CRT-06) and a `taskTagsUpdated` history event (G-SEC-28). The initial `wait` group may not be combined with predecessors or a future `run_after`, takes the phase's kinds except `children`, and is a deferred admission (G-CRT-01). A child needs `parent_lease_token` (no `update` alternative; missing → `400 REQUIRED`, G-WAI-02). The `update` action and `task_update.v1` stay, **for tags only**; a new `resolve` action (`task_resolve.v1`) guards `POST /wait-conditions/{id}/resolve`. |
| Claiming and ordering [02] | One claim names queues of one domain. `critical` first across the named queues (each queue offers its next `critical` task in its own order; the oldest offer by `(created_at, task_id)` wins, G-CLM-02), then rotation between queues in sorted-ID order, random start per new waiter group, one step per claim round (G-CLM-03, G-CLM-04). Two scan budgets per claim round (`fifo` rows, `round_robin` probes), each shared by its kind (G-CLM-06); the tenant cursor keeps its place. Tasks are taken with `SKIP LOCKED` after counters are reserved (plan, then reserve). Order inside a queue unchanged; exact only for one uncontended claimer (G-CLM-08). Strict FIFO not promised. Priority aging dropped (G-CLM-27). |
| Heartbeats and leases [03] | Heartbeat does not call the PDP. The SDK releases a dropped `Lease`. A task past its execution deadline ends `timed_out` whichever scan finds it; the deadline is set by the claim (G-EXE-10). A worker call whose scope no longer covers its task releases the task and returns `403` `CLAIM_REVOKED` (G-EXE-08). Reserved admission permits (`admission.reserved_execution_permits`, 64) for heartbeat, complete, release, wait set/park and the API cancel (G-EXE-07). PDP outage: `503`, fail closed (G-SEC-01). |
| Limits and counters [04] | Per-tenant depth limits (`tenant_max_queue_depth` **and** `tenant_max_deferred_tasks`) are checked against a count sampled every `queue_depth.sample_secs` (default 5 s); overshoot of at most one interval. `te_tenant_queue_backlog` is removed. Concurrency limits stay exact. |
| Domains [05] | A task's domain is fixed at creation. A queue has a domain set (primary, plus the source while a move lasts). Claim request: optional `domain`. Claim response: the task's `domain` on the session and on each lease. `GET /queues[/{id}]`: `domains[]`. Search by queue covers the queue's domain set; a read by ID probes the configured domains in a fixed order. Queue moves themselves are p3 (dual-worker model). |
| Queue operations [06] | `POST /admin/queues/{id}/hold` (`reason?`, `until?`) and `POST /admin/queues/{id}/release`, both queue `configure`. Held queue: creates accepted, no claims from it (a claim naming only held queues ends `204`), leased tasks finish, timers keep running and held time counts toward the queue timeout (G-OPS-07). A hold reaches every node within `runtime_config_poll_secs` (G-OPS-03); `until` ends it on each node's clock (G-OPS-08). Hold state in `GET /queues[/{id}]`. Events `queueHeld`, `queueReleased` (G-OPS-04), readable at `GET /queues/{id}/events` (G-OPS-05). |
| Waits [07] | Input and children waits stay p2; p1 carries the full wait model and `parent_task_id`, but setting an input, children or event condition, or `parent_task_id` on create, is refused in p1 with `400` `FEATURE_NOT_AVAILABLE` (G-WAI-43). p1 kinds: `timer` only. Event waits come after the others (firm p2). `resolve` has its own action, needs no `read` (G-SEC-18), and is safe to repeat: same value from the same caller → `200` + `Idempotency-Replayed: true`; a different value, or the same value from another subject → `400` `failed_precondition` `ALREADY_RESOLVED` (G-WAI-05); cancelled or timed-out condition → `400` `failed_precondition`. The condition stores `resolved_by`. |
| Dedup and supersession [08] | `authz_scope` dedup mode removed (a type that declares it fails registration, G-CRT-15). No PDP call inside the create transaction (authorize before `BEGIN`, re-check the plan, at most 3 attempts, then `409` `aborted` `STATE_CHANGED`, G-CRT-19). |
| API reads [09] | Search window as designed, except a search filtered only to `state eq 'dead_lettered'` (G-RTY-10). No unbounded embedded lists; `GET /tasks/{id}/events` is cursor-paged with a page-size limit and filters; search gets `parent_task_id`, `predecessor_id` and `dead_lettered_from` filters (G-RTY-11); limit on wait conditions per group (16, G-WAI-06). `GET /admin/dlq` removed (search `state eq 'dead_lettered'`). Single-task heartbeat removed (batch only). |

Cite a proposal as `Proposals › <Topic> › <item lead>` (for example `Proposals › Waits › resolve`).

The contract the scenarios assume is the draft OpenAPI (§5). It and the scenarios follow the same decisions; when they disagree, fix one of them. Neither is authoritative over the PRD and DESIGN.

## 2. Scenario file format

```markdown
# <Title: the behaviour, in plain words>

| | |
|---|---|
| Requirement | `cpt-cf-task-engine-fr-…` (PRD.md:NNN) · DESIGN.md:NNN · ADR-00NN |
| Phase | p1 \| p2 \| p3 |
| Test level | E2E \| Integration \| Both — <why, and for Both which part goes where> |
| Depends on | Proposals › <Topic> › <item> — omit the row when none |
| Gaps | G-XXX-NN, … — omit the row when none |

<One or two paragraphs: what the scenario shows and why it matters.>

## Setup
- Bulleted preconditions. Link other scenarios as setup instead of repeating them.

## Request            (or "## Exchange N — <title>" for flows)
```http
…literal request…
```

## Expected response
- Status line and the key assertions as bullets.
```json
…body…
```

## Side effects
- Predicates from §7. Omit the section when nothing changes, with an HTML comment saying so.

## Notes             (optional: doc ambiguities, why a value was chosen)
```

- **Test level** says where the scenario is tested. **E2E**: the pytest suite in `testing/e2e/suites/task_engine/`, over HTTP against a running example server: one node, wall clock, static-authn tokens, tenant-only static authz. **Integration**: Rust tests behind `--features integration`, engine in process on a real database: a controllable clock, two nodes, races, outages, PDP doubles with action-level policies, internal state, the SDK. **Both**: E2E for the visible path, integration for the rows that need the rest. Give the reason unless the level is plain E2E. [INDEX › Test levels](INDEX.md#test-levels) collects the rows; keep it in step.
- One rule per file. Cases of the same rule that differ only in input or starting state share one file as a **case table** (below); a file holds more than one exchange only for a flow.
- Show the literal HTTP exchange. Omit headers that add nothing (keep `Authorization`, `Content-Type`, `Idempotency-Key`, `Retry-After`, `Location`).
- Use the fixtures in §4. Do not invent new tenants, subjects, types or queues unless the scenario needs one; then define it in its Setup.
- Cite doc lines as `PRD.md:NNN`, `DESIGN.md:NNN`, `ADR-0017:NN`, `Proposals › Topic`. Line numbers are for `main` at `6efb0f856`.
- When a doc leaves the expected outcome open, write the scenario with the most likely outcome, mark it in **Notes**, and add a gap entry. Never leave the expected response blank.

### Naming

`{area}.{seq}-{positive|negative|guardrail|flow}-{slug}.md`, for example `1.03-negative-missing-or-malformed-idempotency-key.md`. Numbers restart in each folder. Slugs describe the behaviour, not the endpoint.

- **positive**: the documented happy path or an accepted variant.
- **negative**: a refusal of a bad request, with its error.
- **guardrail**: a protective rule (limits, fencing, removed endpoints, isolation, no oracle).
- **flow**: a multi-exchange journey with every exchange inline.

### Case tables

Use a case table when several cases share the Setup, the endpoint and the shape of the outcome, and differ only in one or two inputs or in the starting state: field limits, the states a cancel applies to, the refusals of one validation rule, the dimensions of one limit.

- Show **one representative case in full**: the literal request, the response body and the side effects.
- Then a `## Cases` table, one row per case. Each row says what differs from the representative case and the expected outcome, for example:

  | Case | Differs from the representative request | Expected | Notes |
  |---|---|---|---|
  | key too long | `tags` key of 129 chars | `400`, `TOO_LONG` on `/tags/0/key` | PRD.md:363 |
  | value too long | value of 257 chars | `400`, `TOO_LONG` on `/tags/0/value` | |
  | too many tags | 17 tags | `400`, `TOO_MANY_TAGS` on `/tags` | G-CRT-06 |

- Every row must be testable on its own: a test is one parametrised case per row. Give each row a short, stable name in the first column.
- Side effects that differ by row go into the row; shared side effects go in `## Side effects`.
- Merge only real variants of one rule. Different rules (for example "replay gives `409`" and "different request gives `400`") stay in separate files even when they hit the same endpoint.
- The file's `Gaps` row lists every gap ID used in any row.

### Where a behaviour lives

Each behaviour has one home. Other folders link to it instead of repeating it; a flow may repeat a step inline and link to its home.

| Behaviour | Home |
|---|---|
| `attempt` counting (claim, retry, release, resume) | `worker/claim/` |
| Session lease, heartbeat interval, session expiry and liveness requeue | `worker/sessions/` |
| Execution-deadline vs session-expiry precedence | `worker/sessions/` |
| Every queue-depth and deferred-limit refusal (create, release with delay, park, initial wait, re-enqueue into a full queue is the exception below) | `limits/depth/` |
| Re-enqueue and its refusals (state, dedup, `STALE_ORDER`, depth, domain) | `dead-letter/` |
| DLQ listing by search, and removed `GET /admin/dlq` | `dead-letter/` |
| Replay never refused as full (`409`, not `429`) | `tasks/idempotency/` |
| Initial wait group on create (placement, response) | `tasks/create/`; its resume in `waits/framework/` |
| Cancel cascade to children | `hierarchy/children/`; supersession's cascade in `tasks/supersede/` |
| Authorization outcomes that are not specific to one endpoint (no existence oracle, read-but-not-act `403`, tenant isolation, worker least privilege, `resolve`-only grants, type and queue policy, creator authority, barriers) | `auth/` |
| Endpoint-specific permission checks (`expedite` on create, `cancel` on supersession, `admin` on re-enqueue, `configure` on hold) | the endpoint's folder |
| Error envelope and the catalogue of reasons | [ERRORS.md](ERRORS.md) (a reference, not scenarios) |

Cross-references use relative paths: `[Claim one task](../claim/1.01-positive-claim-single-task.md)`.

## 3. Transport

- Base path: `/api/task_engine/v1` (D, DESIGN.md:966). Host `api.example.com`.
- Every request carries `Authorization: Bearer <token>` (fixtures below). The platform authenticates; the engine has no anonymous route (PRD.md:909).
- JSON bodies, `Content-Type: application/json`. Errors are `application/problem+json`.
- Enum values on the wire are snake_case names (D, PRD.md:1044).
- Times are RFC 3339 UTC with an `_at` suffix; durations are integer seconds with a `_secs` suffix (A, G-WIRE-13).
- Clock: scenarios start at `T0 = 2026-11-03T10:00:00Z` unless they say otherwise. Write later times as `T0+30s`.

## 4. Fixtures

### Tenants

| Placeholder | Meaning |
|---|---|
| `{root}` | Root tenant (platform services live here) |
| `{tenant_a}` | Tenant A, child of root |
| `{tenant_a1}` | Child of A (for subtree cases) |
| `{tenant_b}` | Tenant B, child of root, unrelated to A |
| `{tenant_c}` | Tenant C, child of root, behind a tenant barrier |

### Subjects and tokens

| Token | Subject | Tenant | Granted actions (PDP policy) |
|---|---|---|---|
| `<creator-a-token>` | `{creator_a}` user / service | A | task `create`, `read`, `update` (tags), `cancel`, `resolve` on the fixture types; queue `read` |
| `<creator-a2-token>` | `{creator_a2}` second subject | A | same as `creator_a` |
| `<worker-a-token>` | `{worker_a}` tenant worker service | A | task `claim`, `read`, `create` (for children) |
| `<platform-worker-token>` | `{platform_worker}` root service subject | root | task `claim`, `read`, `create`; may use `barrier_mode: ignore` on `operated_by: platform` queues |
| `<admin-token>` | `{admin}` operator | root | task `admin`, `read`, `cancel`, `expedite`; queue `read`, `configure` |
| `<creator-b-token>` | `{creator_b}` | B | same as `creator_a`, in B |
| `<worker-b-token>` | `{worker_b}` | B | same as `worker_a`, in B |
| `<readonly-a-token>` | `{reader_a}` | A | task `read` only |
| `<expired-token>` | — | — | expired bearer token |

A scenario that needs a narrower or wider policy states it in Setup ("PDP denies `cancel` to `{creator_a}` on this task").

Worker processes: `worker_id` values `reports-7f9c-1` and `reports-7f9c-2` (two processes of `{worker_a}`), `reports-b-1` for `{worker_b}`.

### Task types

| ID | Shape | Notes |
|---|---|---|
| `gts.cf.core.tasks.task.v1~acme.reports._.render.v1~` (**render**) | input `{ "report_id": string (required), "format": "pdf"\|"csv" }`; output `{ "url": string }` | tenant-owned, no traits; `authority.mode = creator_verified` with no checks |
| `gts.cf.core.tasks.task.v1~acme.graph._.metric_job.v1~` (**metric_job**) | input `{ "graph_id": string, "metric": string, "revision": integer }` | `dedup { key_fields: ["/graph_id", "/metric"], scope: tenant, window: active }`, `supersede { order_field: "/revision", mode: graceful }` (p2) |
| `gts.cf.core.tasks.task.v1~acme.billing._.export.v1~` (**export**) | input `{ "period": string }` | `dedup { key_fields: ["/period"], scope: subject, window: active_or_succeeded }` (p2) |
| `gts.cf.core.tasks.task.v1~cf.platform._.tenant_cleanup.v1~` (**cleanup**) | input `{ "tenant_id": string }` | platform-owned, `authority.mode = worker`, `default_queue = system` |
| `gts.cf.core.tasks.task.v1~acme.docs._.share.v1~` (**share**) | input `{ "document_id": string }` | `authority.mode = creator_verified`, check `{ source: "/document_id", resource_type: "gts.cf.docs._.document.v1~", action: "share" }` |

### Queues

| ID | Settings |
|---|---|
| `gts.cf.core.tasks.queue.v1~cf.core.tasks.default.v1` (**default**) | as shipped: unlimited concurrency, priority 100, assignment 300 s, execution 86,400 s, heartbeat 30 s, retries 3 / liveness 20, exponential 5–300 s, jitter 0.2, `round_robin`, DLQ **dead_letter** (`schemas/instances/…default.v1.json`) |
| `gts.cf.core.tasks.queue.v1~acme.reports._.render.v1` (**reports**) | `global_concurrency_limit: 50`, `per_tenant_concurrency_limit: 5`, `default_priority: 100`, timeouts `{ queue_timeout_secs: 3600, assignment_timeout_secs: 60, execution_timeout_secs: 900, heartbeat_timeout_secs: 30, lifetime_timeout_secs: 86400 }`, retry `{ max_retries: 3, max_liveness_requeues: 20, strategy: exponential, base_interval_secs: 5, max_interval_secs: 300, jitter_factor: 0.2, non_retryable_codes: ["INVALID_REPORT"] }`, `tenant_fairness: round_robin`, `dead_letter_queue: dead_letter` |
| `gts.cf.core.tasks.queue.v1~acme.reports._.bulk.v1` (**bulk**) | like **reports** but `tenant_fairness: fifo`, no DLQ |
| `gts.cf.core.tasks.queue.v1~cf.core.tasks.system.v1` (**system**) | shipped `operated_by: platform` |
| `gts.cf.core.tasks.queue.v1~cf.core.tasks.dead_letter.v1` (**dead_letter**) | `kind: dead_letter` |

In requests, write full IDs. In prose, the bold short names are fine.

### Fixtures defined in scenarios

Some scenarios needed one more subject, type or queue. They are defined in the Setup of the file named here; reuse that definition rather than redefining it.

| Fixture | Meaning | Defined in |
|---|---|---|
| `{worker_a2}` / `<worker-a2-token>` | second worker subject in tenant A (foreign-holder tests) | `worker/fencing/1.04` |
| `{creator_c}` / `<creator-c-token>` | creator in tenant C (behind a barrier) | `auth/1.16` |
| `{creator_a1}` / `<creator-a1-token>` | creator in tenant A1 | `auth/1.11` |
| `{approver_a}` | tenant A subject with only `read` + `resolve` | `auth/1.14` (`flows/1.05` uses `{approver}` for the same role) |
| `{ops_viewer}` | root subject with queue `read` only | `auth/1.15` |
| `{task_admin}` | task `admin` without queue `configure` | `queues/1.03` |
| `{ops_a}` | tenant operator with `admin`, `expedite` limited | `bulk/1.03` |
| `{te_publisher}` | the engine's event-publishing subject | `events/1.03` |
| `platform-ops-1`, `platform-ops-2` | `worker_id`s of `{platform_worker}` | `limits/concurrency/1.01` |
| **adhoc** queue | all timeouts null, no DLQ | `tasks/create/1.03` |
| **exports** queue | `accepted_types: [export]` | `tasks/create/1.12` |
| **narrow** queue | `global_concurrency_limit: 3` | `limits/concurrency/1.07` |
| **heavy** queue and `heavy` domain; `system` domain with type rule `cf.platform.*` | p3 domain fixtures | `bulk/1.04`, `domains/1.05` |
| **thumbnail** type | declares type defaults | `tasks/create/1.03`, `retry/1.06` |
| **metric_job_excl** type | supersession `mode: forced` | `tasks/supersede/1.01` |
| **purge** type | tenant-owned, `authority.mode = worker` (non-creatable) | `auth/1.08` |
| **scoped_job**, **loose_job** types | invalid dedup policies | `tasks/dedup/1.06` |
| Event fixtures | waitable topic `acme.orders._.order_events.v1` with `orders.paid` (one subject type) and `orders.note_added` (two); non-waitable `acme.audit._.audit_events.v1`; the worker's broker `consume` grants | `waits/event/1.01` |

Every GTS ID segment has five parts, `vendor.package.namespace.type.vN`; the fixtures use `_` as the namespace (`acme.reports._.render.v1`). IDs that a scenario means to be unknown (`acme.reports._.nosuch.v1`) are well-formed too, so the test reaches the lookup instead of a format error.

Ownership of the fixture types: **render**, **metric_job** and **export** are tenant-owned (registered in tenant A, visible to B unless a scenario says otherwise); **cleanup** is platform-owned.

### Placeholders

`{task_1}`, `{task_2}` … task IDs (UUID v7); series such as `{task_c001}` for large fan-outs. `{session_1}` session IDs. `{lt_1}` lease tokens. `{wg_1}` wait-group IDs. `{wc_1}` wait-condition IDs. `{entry_1}` entry IDs. `{evt_1}` history event IDs. `{bev_1}` broker event IDs. `<trace>` trace IDs. `{cursor_1}` opaque cursors.

### Multi-node and config notation

- Engine nodes are `N1`, `N2`; say which node serves each request when it matters (`worker/claim/1.10`).
- Engine config in Setup is written as the key path from DESIGN §4.1 (`claim_max_wait_secs = 50`). Platform health endpoints (`/readyz`) sit outside the base path. Flows may include Event Broker calls on `broker.example.com`.

## 5. Wire shapes

The contract is the draft OpenAPI in [#5791](https://github.com/constructorfabric/gears-rust/pull/5791) (`gears/task-engine/docs/openapi.yaml` on branch `docs/task-engine-openapi`, next to the SDK): every path, request and response body, status code, field limit and error response, with each operation's behaviour rules in its description. Scenarios show only the fields they assert and write the rest as `"…": "other fields as in CONVENTIONS §5.1"`; the subsections below name the schema or operation each shorthand means. When a scenario and the OpenAPI disagree, fix one of them.

**Model layers.** The Task Engine follows the Event Broker gear's split (`gears/system/event-broker`; `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md:60-80`):

| Layer | Where | Role |
|---|---|---|
| Domain model | DESIGN §3.1; `task-engine/src/domain/` (`#[domain_model]`) | Internal. Holds fields the API never shows (`idempotency_key`, `revision`, `Assignment.lease_token`, `session_id`, deadlines). Not a wire contract |
| SDK models | `task-engine-sdk/src/models/` | Public Rust contract, named after the OpenAPI schemas (`CreateTaskRequest`, `Task`, `Lease`); `CreateTaskRequest` carries `idempotency_key` until header binding is supported |
| REST DTOs | `api/rest/…/dto.rs` (`api_dto`) | The OpenAPI shapes, mapped from the domain with `From` |
| GTS schemas | `docs/schemas/` | Task payload (`input`, `context`, `output`) and type traits; queue `timeouts` and `retry_policy`; lifecycle event `data` |

A reference DDL for what the gear stores (`migration.sql`) was drafted with these decisions and is kept outside this branch; gap entries that mention it describe storage the gear's migrations must provide.

**Removed by the accepted proposals** (not in the contract; the router answers `404`, G-WAI-03): `POST /tasks/{id}/dependencies`, `DELETE /tasks/{id}/dependencies/{predecessor_id}`, the API-consumer path of `POST /tasks/{id}/wait-conditions`, `POST /tasks/{id}/heartbeat` (batch only), `GET /admin/dlq` (search `state eq 'dead_lettered'` instead), and the `authz_scope` dedup scope.

**Paging** follows `guidelines/DNA/REST/QUERYING.md` through the ToolKit OData extractor; where they differ the contract follows ToolKit (`limit` out of range → `400`, not `422`; `$orderby` with a cursor always refused).

| § | Shorthand in scenarios | In the OpenAPI |
|---|---|---|
| 5.1 | the task resource | schema `Task` (`assignment` never carries `lease_token`; `timeouts` and `retry_policy` are the effective values stored at create) |
| 5.2 | create | `createTask`, schema `CreateTaskRequest` (its description gives the check order) |
| 5.3 | claim | `claimTasks`, schemas `ClaimRequest`, `ClaimResponse`, `Lease` |
| 5.4 | worker calls | `startTask`, `heartbeatTasks`, `reportProgress`, `saveCheckpoint`, `completeTask`, `releaseTask`, `createEntry`, `updateTaskTags` |
| 5.5 | cancel | `cancelTask`, schema `CancelRequest` |
| 5.6 | admin | `reenqueueTask`, `reassignTask`, `acceptReassignedTask`, `holdQueue`, `releaseQueue`, `saveRuntimeConfig`, `getRuntimeConfigStatus` |
| 5.7 | bulk | `bulkCancel`, `bulkReprioritize`, `bulkRetry`, `bulkMove`, schema `BulkResult` |
| 5.8 | waits | `setWaitConditions`, `parkWaitGroup`, `resolveWaitCondition`, `listWaitConditions` |
| 5.9 | reads | `getTask`, `searchTasks`, `countTasks`, `listTaskEvents`, `listEntries`, `getEntry`, `listQueues`, `getQueue`, `listQueueEvents` |

### 5.10 Lifecycle events

Events are not in the OpenAPI (their `data` is defined by the event schemas in `docs/schemas/types/`), so their rules stay here.

Events in `GET /tasks/{id}/events` and on the broker topic `gts.cf.core.events.topic.v1~cf.core.tasks.task_events.v1`. Broker event type IDs are `gts.cf.core.events.event.v1~cf.core.tasks.<snake_name>.v1~` (D, `schemas/types/`); the history uses the camelCase name (`taskCreated`, PRD.md:879). Assert the event name and the payload keys a scenario cares about; check payload keys against the schema file.

Decided event rules (A; the schema files change under each gap's `Remaining`):

- An engine-driven entry into `queued` (a due `scheduled` task, an unblocked `blocked` task) records no event (G-SEC-23).
- Create records only `taskCreated` with `initial_state` (`queued`\|`scheduled`\|`blocked`\|`waiting`), plus the wait events for `waiting` (G-SEC-23, G-OPS-36, G-CRT-01). `taskQueued` means "returned to its queue" (`reason`: `released`, `reenqueued`, `revoked`) with an optional `run_after` (G-EXE-24, G-EXE-08).
- `taskRetrying` carries `cause` (`reported`\|`liveness`) and, for `liveness`, `timeout_type` (`heartbeat`\|`assignment`) (G-EXE-05). `taskTimedOut.timeout_type` for an exhausted liveness budget: `heartbeat` for a session expiry, `assignment` for an assignment deadline or an expired hand-off (G-RTY-02); `wait` when a wait timeout ends the task (G-WAI-16).
- `waitConditionSet` once per condition at set or arm; `taskWaiting` once per park, listing `mode` and `conditions[{wait_condition_id, kind}]` (G-WAI-12). `waitTimedOut` carries `reason: timeout | eval_budget_exceeded` (G-WAI-35).
- `taskCheckpointed` for every call that carries a checkpoint (G-WAI-15). `taskTagsUpdated { actor, set, removed }` (G-SEC-28). `taskUpdated { actor, changes }` for reprioritize and move (G-OPS-19).
- `queueHeld { queue, actor, reason, until? }`, `queueReleased { queue, actor?, automatic }`, subject = the queue, tenant = root (G-OPS-04).
- Published `id` equals the history event ID and `subject_type` is the task's concrete type (G-SEC-09, needs #5491). Until #5492 ships, tenants behind a barrier get no broker events; their history is complete (G-SEC-10). p1 publishes the `lifecycle` preset with no configuration (G-SEC-12).
- Field names: `actor` everywhere (not `created_by`/`requested_by`); `taskDeadLettered` carries `queue` (G-SEC-11).

## 6. Errors

Errors are RFC 9457 problem documents built from the platform canonical categories (D, DESIGN.md:1180-1236; `docs/arch/errors/DESIGN.md`). [ERRORS.md](ERRORS.md) shows the envelope, the category → status → `context` table, and every code with the scenario that owns it; the OpenAPI's error responses and `Problem` schema carry the same rules. The rules scenarios rely on most:

- A code sits where its category allows it: `context.reason` only for `permission_denied`, `aborted` and `unauthenticated`; `field_violations[].reason` for `invalid_argument`; `violations[{ type: "STATE", subject: <code> }]` for `failed_precondition`; quota `violations[].subject` (`queue_depth.<dimension>`, `concurrency`, `event_waits.<dimension>`) for `resource_exhausted` (G-WIRE-14).
- Field paths are RFC 6901 JSON Pointers from the body root; a header or query parameter is named as sent (G-SEC-21). `404` and `403` carry no field violations (G-WIRE-15).
- Every `429` and `503` carries `Retry-After` (G-SEC-03).
- A request about a task the caller may not read gets the same `404` as a missing task; `403` only when it may read but not act. Worker calls never answer `404` (PRD.md:918, G-EXE-09).
- Do not invent other `context` keys; put extra detail into a `description`, which never echoes a value the caller sent.

## 7. Side-effect vocabulary

| Kind | Form |
|---|---|
| Task state | `task {id} state is <state>` · `task {id} result_code is <code>` · `task {id}.<field> is <value>` · `task {id}.<field> advances from <a> to <b>` |
| Lease | `lease {lt} is invalid (later calls get LEASE_LOST)` · `task {id} lease_epoch is <n>` |
| Session | `session {id} lease_until is extended to <time>` · `session {id} is expired` |
| Concurrency counters | `<dimension>(<scope>) used changes by <±n>` with dimensions `queue_global`, `queue_tenant`, `type_global`, `type_tenant` (`te_concurrency_slots`, `te_tenant_concurrency_slots`, DESIGN.md:1560-1646). These names mean **concurrency** slots only; depth uses the `429` names `queue`, `tenant`, `domain_global`, `deferred` (A, G-CLM-30) |
| Depth | `{id} joins / leaves the queued set` · `… the deferred set`. The accepted sampled per-tenant depth (queued and deferred) removes `te_tenant_queue_backlog` (DESIGN.md:1647), so there is no depth counter row to assert; a refusal is visible only after the next sample |
| Timers | `timer <type> for {id} is armed at <time>` · `… is removed`; types: `scheduled`, `queue_timeout`, `assignment`, `execution`, `lifetime`, `sla`, `wait`, `cancel_grace`, `cascade`, `children_check` (DESIGN.md:1799-1823); one `wait` timer per task, armed at the earliest deadline of its pending parked conditions (timeouts and timer `resume_at`) and re-armed after each fire (A, G-WAI-09) |
| History | `history of {id} gains <eventName> with <payload assertion>`; queue-scoped: `history of queue <id> gains <eventName>` |
| Published | `event <eventName> for {id} is published to the task-events topic` (or "is not published under the default `lifecycle` preset": `taskQueued`, `taskClaimed`, `taskStarted`, `taskCheckpointed`, `taskProgressed` are recorded but not published by default, PRD.md:898) |
| Config | `config version <n> is saved` · `config version <n> is applied in domain <d>` |
| Rows | `te_<table>(<key>) is present / absent` — only when the table name is in DESIGN §3.7 |
| Replies | `subsequent <call> returns <status> [<reason>]` |
| Metrics | `metric <name>{labels} increases by <n>` (names from DESIGN §4.3 only) |

"Nothing changes" is a valid side-effect list for a refusal: write `<!-- No side effects: the request is refused before any write. -->`.

## 8. Gaps

[GAPS.md](GAPS.md) lists every place where a scenario had to choose because the docs are silent, unclear or contradictory. Each entry has an ID (`G-<AREA>-NN`), the docs it concerns, the scenarios that depend on it, what the scenario assumed, and the question to settle. Areas: `WIRE` (shapes), `CRT` (create, idempotency, dedup), `CLM` (claim, ordering, limits), `EXE` (sessions, fencing, execution, release), `RTY` (retry, timeouts, DLQ, cancel), `WAI` (waits, hierarchy, dependencies), `SEC` (auth, isolation, errors, reads, entries, events), `OPS` (queues, config, bulk, domains, reassignment, blacklist, retention).
