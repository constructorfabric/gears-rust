# Task Engine Scenarios: Conventions

Shared rules, fixtures and wire shapes for every scenario under `gears/task-engine/scenarios/`. Read this before writing or reviewing a scenario.

The Task Engine docs on `main` have **no OpenAPI contract** and only one JSON example (DESIGN.md:1184). The shapes below are the scenarios' working contract; [`../docs/openapi.yaml`](../docs/openapi.yaml) is the draft OpenAPI built from them, and branch `docs/task-engine-openapi` carries the same contract without review annotations (§5 › Draft OpenAPI). Each field is marked:

- **D**: named in PRD, DESIGN or an ADR on `main` (line given where useful).
- **A**: agreed in the wire review of [`../docs/openapi.yaml`](../docs/openapi.yaml) (2026-11-04) or the gap review (2026-11-06); not yet in the PRD or DESIGN. The gap it cites records the decision in a `Decided` line.
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

The draft contract for these shapes is [`../docs/openapi.yaml`](../docs/openapi.yaml). Its descriptions are written for API consumers; provenance and gap citations are in extensions next to them (§5 › Draft OpenAPI). It settles some shapes §5 leaves open (stored wait-condition fields, per-kind wait results, entry and history paging, runtime-config keys), each citing its gap in `x-gaps`. When it and §5 disagree, fix one of them; neither is authoritative over the PRD and DESIGN.

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

**Model layers.** The Task Engine follows the Event Broker gear's split (`gears/system/event-broker`; `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md:60-80`):

| Layer | Where | Role |
|---|---|---|
| Domain model | DESIGN §3.1; `task-engine/src/domain/` (`#[domain_model]`) | Internal. Holds fields the API never shows (`idempotency_key`, `revision`, `Assignment.lease_token`, `session_id`, deadlines). Not a wire contract |
| SDK models | `task-engine-sdk/src/models/` | Public Rust contract, transport-agnostic, named after the `openapi.yaml` schemas: `CreateTaskRequest` (create; carries `idempotency_key` until header binding is supported), `Task`, `Lease`, typed GTS IDs |
| REST DTOs | `api/rest/…/dto.rs` (`api_dto`) | The wire shapes below and in [`../docs/openapi.yaml`](../docs/openapi.yaml), mapped from the domain with `From` |
| GTS schemas | `docs/schemas/` | `gts.cf.core.tasks.task.v1~` defines only the payload (`input`, `context`, `output`) and type traits; the queue schema defines `timeouts` and `retry_policy`; event schemas define lifecycle `data` |

[`../docs/migration.sql`](../docs/migration.sql) is the reference DDL (tables, columns and indexes; the gear's migrations are authoritative); every wire field is a column there or derived from one. It lives only on this branch: `docs/task-engine-openapi` does not carry it.

So a field marked **D** below has a documented concept and domain name; the wire name is the domain name unless the row says otherwise. Where DESIGN's domain name or type cannot be the wire one (`type_id`/`queue_id` typed `Uuid`), the field is **S** (G-WIRE-16, G-WIRE-17). Lifecycle event `data` names the task's type `task_type` because the event envelope's own `type` is the event type; that mapping is intended.

### Draft OpenAPI

[`../docs/openapi.yaml`](../docs/openapi.yaml) is derived from PRD.md and DESIGN.md on `main` at `6efb0f856` plus every `[Proposed]` item of PR #5730, taken as accepted; `[Open]` proposal items are not assumed.

Each `description` is written for API consumers: behaviour, PDP action, phase, errors, limits. Review annotations sit in extensions beside it, which renderers and code generators ignore:

| Key | Holds |
|---|---|
| `x-source` | `P`, `A` or `S` as above; no key means **D** |
| `x-gaps` | the [GAPS.md](GAPS.md) IDs the item depends on |
| `x-refs` | the doc lines and proposal sections it comes from (`DESIGN.md:966`, `Proposals › Waits`) |
| `x-review` | notes for reviewers: why the shape was chosen, what is still open |

A response that is a `$ref` cannot carry extensions, so its notes go on the operation's `x-review`, prefixed with the status code. When a gap closes and DESIGN carries the shape, delete the item's extensions; the description should not need to change.

**Published copy.** Branch `docs/task-engine-openapi` (worktree `../gears-rust-te-openapi`) carries the spec for review together with the SDK. It is this file without the `x-*` extensions and the provenance paragraph: the same paths, parameters, schemas, enums and status codes (checked against d7b5ad62b on 2026-11-06). Keep the two in step: a wire change goes into both files, and annotations live only here.

**Removed by the accepted proposals** (not in the contract; the router answers `404`, G-WAI-03): `POST /tasks/{id}/dependencies`, `DELETE /tasks/{id}/dependencies/{predecessor_id}`, the API-consumer path of `POST /tasks/{id}/wait-conditions` (the worker path with a `lease_token` stays), `POST /tasks/{id}/heartbeat` (batch only), `GET /admin/dlq` (search `state eq 'dead_lettered'` instead), and the `authz_scope` dedup scope.

**Paging** follows `guidelines/DNA/REST/QUERYING.md` and the ToolKit OData surface (`toolkit::api::odata`). Where they differ the spec follows ToolKit: an out-of-range `limit` is `400` (guideline: `422`), and `$orderby` with a cursor is always refused (guideline: ignored when equal).

### 5.1 Task resource

Returned by `POST /tasks` (`201`), `GET /tasks/{id}`, inside claim responses, and as search items (narrowed by `view`, DESIGN.md:1165-1170).

```json
{
  "id": "{task_1}",
  "type": "gts.cf.core.tasks.task.v1~acme.reports._.render.v1~",
  "queue": "gts.cf.core.tasks.queue.v1~acme.reports._.render.v1",
  "owner_tenant_id": "{tenant_a}",
  "owner_id": null,
  "created_by": "{creator_a}",
  "state": "queued",
  "urgency": "normal",
  "priority": 100,
  "attempt": 0,
  "retry_count": 0,
  "liveness_requeues": 0,
  "cancellable": true,
  "cancel_requested": false,
  "input": { "report_id": "r-42", "format": "pdf" },
  "context": null,
  "result": null,
  "progress": null,
  "tags": [{ "key": "project", "value": "q4" }],
  "resource_refs": [],
  "parent_task_id": null,
  "wait_for_children": false,
  "propagate_cancel": true,
  "predecessors": [],
  "dependency_mode": null,
  "timeouts": { "queue_timeout_secs": 3600, "assignment_timeout_secs": 60, "execution_timeout_secs": 900, "heartbeat_timeout_secs": 30, "lifetime_timeout_secs": 86400 },
  "retry_policy": { "max_retries": 3, "max_liveness_requeues": 20, "strategy": "exponential", "base_interval_secs": 5, "max_interval_secs": 300, "jitter_factor": 0.2, "non_retryable_codes": ["INVALID_REPORT"] },
  "run_after": null,
  "sla_deadline": null,
  "assignment": null,
  "dead_lettered_from": null,
  "superseded_by": null,
  "created_at": "2026-11-03T10:00:00Z",
  "updated_at": "2026-11-03T10:00:00Z",
  "terminal_at": null
}
```

| Field | Src | Notes |
|---|---|---|
| `id`, `owner_tenant_id`, `owner_id`, `created_by`, `state`, `urgency`, `priority`, `attempt`, `cancellable`, `cancel_requested`, `input`, `context`, `result`, `progress`, `tags`, `resource_refs`, `parent_task_id`, `wait_for_children`, `propagate_cancel`, `timeouts`, `retry_policy`, `run_after`, `sla_deadline`, `created_at`, `updated_at` | D | Domain model, DESIGN.md:280-311 |
| `domain`, `cancel_request { mode, requested_by, requested_at, reason, grace_until, superseded_by }`, `assignment.execution_deadline` | A | stored data now projected (G-WIRE-19); `null` when not applicable. Scenarios that do not assert them may omit them |
| `type`, `queue` | A | GTS ID strings; DESIGN.md:282-283 `type_id`/`queue_id` typed `Uuid` to be retyped (G-WIRE-16, G-WIRE-17) |
| `retry_count`, `liveness_requeues` | A | Stored counters (DESIGN.md:538-539), returned (G-WIRE-04) |
| `predecessors[]` `{ task_id, on_predecessor_failure, outcome }`, `dependency_mode` (`all`\|`any`) | D/P/A | `on_predecessor_failure` is P; `outcome` (`pending`\|`succeeded`\|`failed_ignored`\|`failed`) replaces `satisfied` (A, G-WAI-37) |
| `assignment` `{ assignee_subject, worker_id, claimed_at, attempt, lease_epoch, execution_deadline }` | D/A | DESIGN.md:329-340; never carries `lease_token`; `execution_deadline` is set by the claim (G-EXE-10) |
| `dead_lettered_from`, `superseded_by`, `terminal_at` | D | DESIGN.md:545, :552; PRD.md:1546. Re-enqueue clears all three (G-RTY-12) |
| `tags` as `[{ key, value }]`, each key at most once | A | agreed in the wire review (G-WIRE-04); PRD says key-value tags |
| `timeouts`, `retry_policy` | A | Effective values, resolved and stored at create; `heartbeat_timeout_secs` is never `null` (G-CRT-04). Later config changes do not reach a live task (G-OPS-13); a move re-resolves the queue level (G-OPS-20) |

The task resource never returns the `Idempotency-Key` (G-CRT-09).

`result` when set (D, PRD.md:269):

```json
{ "result_code": "error", "output": null,
  "error": { "domain": "acme.reports", "code": "RENDER_TIMEOUT", "message": "renderer timed out", "context": null, "fault": "dependency", "retryable": true },
  "warnings": [] }
```

`resource_refs[]` (D, PRD.md:372-374): `{ "resource_type": "gts…~", "resource_id": "…", "verified": false }`.

### 5.2 Create: `POST /tasks`

```http
POST /api/task_engine/v1/tasks HTTP/1.1
Host: api.example.com
Authorization: Bearer <creator-a-token>
Content-Type: application/json
Idempotency-Key: 7d1f0c2e-render-r-42

{
  "type": "gts.cf.core.tasks.task.v1~acme.reports._.render.v1~",
  "queue": "gts.cf.core.tasks.queue.v1~acme.reports._.render.v1",
  "priority": 100,
  "input": { "report_id": "r-42", "format": "pdf" },
  "tags": [{ "key": "project", "value": "q4" }]
}
```

Required: `type` (A, G-WIRE-17) and `input` (D). `queue` (A, G-WIRE-17) is optional and defaults to the type's `default_queue`. Optional fields (D unless marked): `priority` (A: optional, default the queue's `default_priority`, G-CRT-26), `urgency`, `context`, `owner_id`, `timeouts` (any subset), `sla_deadline`, `retry_policy` (whole), `run_after`, `cancellable` (A: default `true`, G-CRT-27), `resource_refs`, `parent_task_id` + `parent_lease_token` (P: token required), `wait_for_children`, `propagate_cancel`, `predecessors[]` `{ task_id, on_predecessor_failure }` (A, G-WIRE-17; `on_predecessor_failure` P), `dependency_mode` (A: one per task, default `all`, G-WIRE-17, G-WAI-40), `wait` (P, initial wait group, shape as §5.8 without `lease_token`/`park`), `owner_tenant_id` (A, default the caller's tenant; the PDP decides, barrier `ignore` only on `operated_by: platform` queues, G-SEC-13).

Field rules (A unless marked):

- `Idempotency-Key`: 1–255 printable ASCII characters without spaces, case-sensitive; absent → `428`, empty or malformed → `400 INVALID_FORMAT` (G-CRT-09). On a retry the queue is compared with the queue resolved at create: absent or equal matches, another queue → `400` on `Idempotency-Key` (G-CRT-11).
- `tags`: at most `max_tags_per_task` (16); key 1–128, value 0–256 characters (G-CRT-06). `resource_refs[].resource_id`: at most 256 characters (G-CRT-10). A dedup key-field value: at most 4 KiB in JCS form (G-CRT-16).
- `run_after` in the past: stored as sent, task `queued`; later than `created_at` + lifetime timeout → `400` (G-CRT-22).
- `timeouts` and `retry_policy`: a request value above the trust limit → `400`; an inherited or defaulted value above it is clamped (G-CRT-05). A partial `retry_policy` needs the schema's required fields; omitted optional ones take schema defaults (G-CRT-04).
- `wait`: not with predecessors or a future `run_after` (`400`); p1 kinds `timer` only; `children` is `NOT_ALLOWED` in every phase; timeouts count from the create; a deferred admission; at most 16 conditions, none empty (G-CRT-01, G-WAI-06).
- `predecessors`: the same task twice → `400 DUPLICATE` (G-WAI-42); a parent or predecessor is looked up only in the new task's domain, so one in another domain is `404` (G-WAI-41). Hierarchy depth at most `max_hierarchy_depth` (10, G-WAI-39).

Check order (G-CRT-24): `413` body limit → JSON syntax → type (`400 UNKNOWN_TASK_TYPE` / `503`) → queue (`404` unknown, `400 DEAD_LETTER_QUEUE`, G-CRT-02, G-CRT-03) → PDP (`403` / `503`) → `expedite` (`403`) → `428` missing key → trust limits and domain (`400`) → schema and 64 KiB payload (`400` / `413`) → dedup and idempotency (`409`) → sampled depth (`429`, after dedup so a replay is never refused as full, G-CLM-24) → insert.

Success: `201 Created`, `Location: /api/task_engine/v1/tasks/{task_1}`, body = the full task resource (A, G-WIRE-03). History: `taskCreated` with `initial_state` only, never `taskQueued` (G-SEC-23).

Replay: `409` `already_exists` with `context.resource_type` = the task base type and `resource_name = {existing_id}` (D, DESIGN.md:1203; A, G-CRT-25). `STALE_ORDER` names no task.

### 5.3 Claim: `POST /tasks/claim`

```json
{ "worker_id": "reports-7f9c-1", "session_id": null,
  "queues": ["gts.cf.core.tasks.queue.v1~acme.reports._.render.v1"],
  "max_tasks": 1, "timeout_secs": 30 }
```

`worker_id`, `session_id`, `queues`, `max_tasks` are D (DESIGN.md:1064); `timeout_secs` (DESIGN's `timeout`) is A: seconds, default `0` = no wait (G-CLM-19). Optional `domain` is P (Proposals › Domains › p1 lays the foundation): every named queue must be in that domain; it is required only when a named queue is in two domains (a move in progress, p3). p1 scenarios omit it.

`200 OK`:

```json
{ "session_id": "{session_1}", "domain": "default", "heartbeat_interval_secs": 10,
  "claims": [ {
    "task": { "…": "task resource, state claimed" },
    "lease": { "lease_token": "{lt_1}", "lease_epoch": 1, "attempt": 1, "lease_secs": 30, "domain": "default" },
    "authority": { "mode": "creator_verified", "checked": [], "checked_at": "2026-11-03T10:00:00Z" },
    "resumed": null
  } ] }
```

| Field | Src | Notes |
|---|---|---|
| `session_id`, `heartbeat_interval_secs`, `claims[].task`, `claims[].lease` | D | DESIGN.md:1064, :1081 |
| `lease.lease_token`, `lease_epoch`, `attempt` | D | DESIGN.md:1081 |
| `lease.lease_secs` | A | session lease after this claim (ADR-0017:56) |
| `domain` (response and `lease.domain`) | P | the task domain of the session and of each task (Proposals › Domains); `default` on a single-domain deployment |
| `authority` | D | DESIGN.md:1043 |
| `resumed` `{ wait_group_id, outcome: satisfied\|timed_out, conditions: [{ id, kind, state, result }] }` | A | PRD.md:550 says the resumed worker receives the wait result; shape not given |

`204 No Content`: nothing eligible (also when every named queue is held or blacklisted for the worker, G-OPS-06, G-OPS-30). `429` + `Retry-After` (the claim poll interval, 1 s): an eligible task was skipped for capacity (D, PRD.md:429; A, G-CLM-09). A session is opened or renewed only by a claim that takes a task, so neither answer touches or returns a session (A, G-CLM-16). A `session_id` stored with another `worker_id` counts as unknown: a new session opens (G-CLM-17).

Validation (A, G-CLM-18), before any session or PDP work: `REQUIRED` (missing `worker_id`, missing or empty `queues`), `INVALID_FORMAT` (`worker_id`), `OUT_OF_RANGE` (`max_tasks`), `TOO_MANY_QUEUES` (distinct queues), `DEAD_LETTER_QUEUE`, `CROSS_DOMAIN`, each on its JSON Pointer (`/queues/1`); an unregistered queue → `404` (G-CLM-15). Then the PDP: a named queue the policy provably excludes → `403` `QUEUE_NOT_PERMITTED` (G-CLM-14).

The sequence diagram flattens the lease into each claim (`claims: [{task, lease_token, lease_epoch, attempt}]`, DESIGN.md:1297) while the endpoint table nests it (`claims: [{task, lease}]`, DESIGN.md:1064). Scenarios follow the table (G-WIRE-05).

### 5.4 Worker calls

All carry `lease_token` in the body (D, DESIGN.md:1081). `start`, `complete` and `release` return the full task resource, `checkpoint` returns `{ context }`, and each heartbeat result also carries `cancel_request` when a cancel is pending (A, G-WIRE-06).

| Call | Body | Success |
|---|---|---|
| `POST /tasks/{id}/start` | `{ lease_token }` | `200` task resource, `state: running` |
| `POST /tasks/heartbeat` (batch; the single-task heartbeat is removed, P) | `{ session_id, leases: [{ task_id, lease_token }] }` (D, DESIGN.md:1067) | `200 { session_id, heartbeat_interval_secs, results: [{ task_id, outcome: "ok", state, cancel_requested }, { task_id, outcome: "lease_lost" }] }` |
| `POST /tasks/{id}/progress` | `{ lease_token, percent, milestone?, completed_units?, total_units?, current_stage?, eta? }` (fields D, PRD.md:753) | `200 OK` (D, DESIGN.md:1314), empty body; the update is buffered until the next flush |
| `POST /tasks/{id}/checkpoint` | `{ lease_token, checkpoint: { … } }` | `200 { "context": { merged } }` |
| `POST /tasks/{id}/complete` | `{ lease_token, result_code, output?, error?, warnings? }`, flat (D, DESIGN.md:1316) | `200` task resource (terminal, or `scheduled` for a retryable error) |
| `POST /tasks/{id}/release` | `{ lease_token, delay_secs?, checkpoint? }` (D, DESIGN.md:1071) | `200` task resource (`queued` or `scheduled`) |
| `POST /tasks/{id}/entries` (worker) | `{ lease_token, type, payload }` | `201` + `Location` + entry `{ id, task_id, type, creator, payload, created_at }` (A, G-EXE-18) |

Consumer tag change (not a worker call, listed here for its shape): `PATCH /tasks/{id}/tags` `{ "set": [{ "key": "…", "value": "…" }], "remove": ["key"] }` (D, DESIGN.md:1075) → `200` task resource. Needs `update` (P: tags only). A key in both `set` and `remove` → `400`; the result over the per-task tag limit → `400` (P, limit value G-CRT-06).

A reported failure is `complete` with `result_code: error` (D: the API table has no `/fail`, DESIGN.md:1070; A, G-WIRE-07). The body is flat (G-EXE-15).

Worker-call rules (A):

- `start` is optional. `complete`, a reported failure, a cancel acknowledgement, release, progress and set-wait are allowed from `claimed` too (G-EXE-11, G-WAI-20). A repeated `start` by the holder is a no-op `200` (G-EXE-10). The execution deadline is set by the claim and never moved by `start`; each claim, including one after a resume, gets the full timeout (G-RTY-22).
- Fence failure: one unscoped re-read decides. Holder and token match but the scope no longer covers the task → the task is released, `403` `CLAIM_REVOKED`, history `taskQueued { reason: revoked }` without an actor (G-EXE-08). Anything else (unknown task, foreign tenant, stale token) → `409` `LEASE_LOST`; worker calls never answer `404` (G-EXE-09). A result that fails validation is checked against the fence before `400` is sent, so a lost lease wins (G-EXE-17).
- A repeated non-terminal report (retryable failure, release) after a lost response gets `LEASE_LOST`; the SDK treats it as applied (G-EXE-16, G-RTY-05). Terminal completions stay replayable.
- After expiry step 1 and before the requeue, a late write still succeeds (the token is the fence); the heartbeat already answers `lease_lost` (G-EXE-02).
- Heartbeat with an unknown, expired or foreign session: `200`, every lease `lease_lost`, the same body in all three cases (G-EXE-01). More than `heartbeat_batch_max` (500) leases → `400 TOO_MANY` on `/leases` (G-EXE-04). A cancel caused by a supersession carries `cancel_request.superseded_by` (G-CRT-17).
- Checkpoint merge is RFC 7386 JSON Merge Patch (`null` deletes a key); every call that carries a checkpoint records `taskCheckpointed` (G-EXE-13, G-WAI-15). Checkpoint is p1 (G-EXE-12).
- `release.delay_secs`: 0 to `release_max_delay_secs` (86,400); 0 = immediate; otherwise `400 OUT_OF_RANGE` (G-EXE-25). A delayed release refused by the deferred limit keeps the lease (`429`, G-EXE-23). History: `taskQueued { reason: released, run_after? }` (G-EXE-24).
- Output is validated against the type revision stored at create (G-EXE-14).
- Entries: an API consumer may add entries in any state until the task is removed; a worker needs its lease; at most `max_entries_per_task` (1000) per task, else `400` `failed_precondition` `TOO_MANY_ENTRIES` (G-EXE-21). Unknown entry type → `400 UNKNOWN_ENTRY_TYPE` (G-EXE-19). `PUT`/`PATCH`/`DELETE` on an entry → `405` with `Allow` (G-EXE-22).

### 5.5 Cancel: `POST /tasks/{id}/cancel`

`{ "mode"?: "requested" | "graceful" | "forced", "reason"?: string }`: mode values D (PRD.md:346); body optional, `mode` defaults to `requested` (A, G-RTY-18).

- Unleased task, any mode → `200`, task `cancelled` (result `cancelled`, or `forcibly_cancelled` for `forced`).
- Leased task, `requested`/`graceful` → `202 Accepted` with the task resource, unchanged except `cancel_requested: true` and `cancel_request` (A, G-WIRE-08).
- Leased task, `forced` → `200`, `cancelled` / `forcibly_cancelled`.
- Already ended in the cancelled family (`cancelled`, `forcibly_cancelled`, `superseded`, `dependency_failed`) → `200`, task unchanged, no event; any other terminal state → `400` `failed_precondition` `TASK_FINISHED` (A, G-RTY-13).
- `cancellable: false` with `requested`/`graceful` → `400` `failed_precondition` `NOT_CANCELLABLE`; `forced` still works (A, G-RTY-14).
- A flagged task: modes only move forward. `requested` → `graceful` arms the grace timer; the same mode again, or `requested` after `graceful`, is a no-op `202` (A, G-RTY-16). The grace period is the engine-wide `cancel_grace_period_secs` (G-RTY-19).
- Events: `taskCancelRequested` only when a leased task is flagged or escalated; an immediate or forced cancel records only `taskCancelled` with `actor` and `mode` (A, G-RTY-15).
- The worker acknowledges with `complete` `result_code: cancelled`. After a cancel request `success`/`warning` still end the task `completed`; any `error` ends it `cancelled` with no retry (A, G-RTY-17, G-OPS-38). After a graceful supersession, `error` and `cancelled` end it `superseded` (G-CRT-18).
- Cascade to children: each unfinished child is cancelled in the parent's mode; children end `cancelled`, or `forcibly_cancelled` when forced, never `superseded` (A, G-WAI-23, G-CRT-21). A parent with `propagate_cancel` that ends in any other non-success state cascades in `graceful` mode (G-WAI-38).
- `reason`: at most 1024 characters (G-OPS-47).

### 5.6 Admin

| Call | Body | Success |
|---|---|---|
| `POST /admin/tasks/{id}/reenqueue` | none, or `{ queue? }` (A, G-RTY-20: a dispatch queue routed to the task's domain; unknown → `404`, dead-letter → `400 DEAD_LETTER_QUEUE`; another queue re-resolves settings like a move) | `200` task resource, `queued` |
| `POST /tasks/{id}/reassign` | `{ subject_id, worker_id, reason? }` (D; `reason` A, G-OPS-41) | `200` task resource, no token |
| `POST /tasks/{id}/accept` | `{ worker_id, session_id? }` (D) | `200 { session_id, domain, heartbeat_interval_secs, task, lease, authority }` (A, G-WIRE-21) |
| `POST /admin/queues/{id}/hold` (P) | `{ reason?, until? }` | `200` queue resource |
| `POST /admin/queues/{id}/release` (P) | none | `200` queue resource |
| `POST /admin/config` | `{ "queues": { "<queue id or pattern>": { …settings } }, "types": { … } }` (S) | `202 { "config_version": 7 }` (D, DESIGN.md:1129) |
| `GET /admin/config/{version}` | — | `200 { config_version, domains: [{ domain, applied: true }] }` (S) |

Admin rules (A):

- Hold: `until` must be in the future (`400`); holding a held queue replaces `reason` and `until`, keeps `since`, sets `by`, records another `queueHeld`; releasing an unheld queue → `200` with `hold: null`, no event; holding a dead-letter queue → `400` `failed_precondition` `DEAD_LETTER_QUEUE` (G-OPS-08, G-OPS-09, G-OPS-10).
- Re-enqueue: clears `terminal_at`, the result, `dead_lettered_from` and a passed `sla_deadline` (G-RTY-12); a child whose parent ended → `400` `failed_precondition` `PARENT_FINISHED` (G-WAI-24).
- Reassign: only `claimed`/`running`; a pending hand-off can be reassigned again; a target blacklisted on the queue → `400` `failed_precondition` `TARGET_BLACKLISTED`; the target's rights are checked only when it accepts (G-OPS-27). Accept records `taskClaimed` with `handoff: true` (G-OPS-41). An expired hand-off gives no blacklist strike and uses the liveness budget (G-OPS-26).
- `POST /admin/config`: one `configure` check on the queue base type, all-or-nothing (G-OPS-40). Pattern specificity: exact ID, then longest literal prefix; equal-specificity overlaps are rejected at load; a runtime override beats a static one (G-CLM-21, G-OPS-12).

### 5.7 Bulk: `POST /tasks/bulk/{cancel|reprioritize|retry|move}`

Body `{ "task_ids": [...], …op fields }` (S: `mode` for cancel; `priority`/`urgency` for reprioritize; `queue` for move). Response `200 { "results": [{ "task_id", "outcome": "succeeded" | "unauthorized" | "not_found" | "invalid_state" | "cancel_requested" | "limit_exceeded" | "domain_mismatch" }] }` (D, DESIGN.md:1119; the last three A: G-OPS-16, G-CLM-25, G-OPS-21). Bulk reprioritize and move stay as on `main` (the proposals do not touch them).

- More than `bulk_max_ids` (1000) IDs → `400` `BATCH_TOO_LARGE` on `/task_ids` (A, G-OPS-15). An outright PDP denial of the action → `403` `ACTION_DENIED` for the whole request; otherwise denials are per task (A, G-OPS-14).
- `cancel_requested`: a leased task that was only flagged; `succeeded` means the task ended (G-OPS-16). `limit_exceeded`: refused by depth admission, others proceed (G-CLM-25). `domain_mismatch`: the destination is in another domain (G-OPS-21). An unknown or dead-letter destination refuses the whole request (`404` / `400 DEAD_LETTER_QUEUE`), because one destination serves every task (G-CRT-02, G-CRT-03). Bulk retry reports any per-task precondition refusal (`TASK_DOMAIN_MISMATCH`, `PARENT_FINISHED`, `STALE_ORDER`) as `invalid_state` (G-RTY-20).
- A move re-resolves queue-level settings, keeps request- and type-level ones, and re-arms the queue timeout from the move (G-OPS-20). Each change records `taskUpdated { actor, changes }` (G-OPS-19).

### 5.8 Waits

Set (worker): `POST /tasks/{id}/wait-conditions`

```json
{ "lease_token": "{lt_1}", "mode": "all", "park": true, "checkpoint": { "step": 3 },
  "conditions": [
    { "kind": "timer", "resume_at": "2026-11-03T11:00:00Z" },
    { "kind": "input", "input_schema": { "type": "object", "required": ["approved"], "properties": { "approved": { "type": "boolean" } } }, "prompt": "Approve export?", "timeout_secs": 86400, "on_timeout": "fail" },
    { "kind": "children" },
    { "kind": "event", "event_type": "gts.cf.core.events.event.v1~acme.orders._.paid.v1~", "subject_type": "gts.acme.orders._.order.v1~", "subject_id": "o-17", "timeout_secs": 3600, "on_timeout": "resume" }
  ] }
```

`mode` (`all`\|`any`), `park`, `checkpoint`, `conditions[]`, `kind`, `resume_at`, `input_schema`, `prompt`, `event_type`, `subject_type`, `subject_id`, `match_expression`, `on_timeout` (`fail`\|`resume`) are D (DESIGN.md:360-391, :1095). `timeout_secs` is A: relative seconds in, absolute `timeout_at` out (G-WIRE-09).

Success `200 { "wait_group": { "id", "mode", "parked": true, "conditions": [{ "id": "{wc_1}", "kind", "state": "pending", "timeout_at" }] }, "task_state": "waiting" }` (S).

A set whose group resolves at once (a past `resume_at`, no unfinished children) behaves like parking a satisfied armed group: `200 { "parked": false, "result": { …resumed shape } }`, lease kept, task stays `running`; the group is consumed: its conditions stay listed as `satisfied` but it is not current, so no claim returns it (A, G-WAI-10, G-WAI-13). Set is allowed from `claimed` (G-WAI-20). Missing `lease_token` → `400 REQUIRED` before any lookup (G-WAI-01). A condition with a timeout and no `on_timeout` gets `fail`; `on_timeout` without a timeout → `400` (G-WAI-32). A timeout on a `timer` condition → `400` (G-WAI-09). At most `wait.max_conditions_per_group` (16), none empty (G-WAI-06).

Park: `POST /tasks/{id}/wait-conditions/park` `{ lease_token, checkpoint? }` → `200 { "parked": false, "result": { …resumed shape } }` when already satisfied, else `200 { "parked": true, "task_state": "waiting" }` (D for behaviour, DESIGN.md:1096; S for shape). Nothing armed → `400` `failed_precondition` `NO_ARMED_WAIT`, lease kept (A, G-WAI-13). Arm works for every kind: an armed timer is satisfied once `resume_at` passes (checked at park), an armed input can be resolved, an armed children condition is evaluated at park (A, G-WAI-17, G-WAI-21).

A task has one current wait group at a time. A parked group stays current after it resolves, until a new set or arm makes another group current or the task ends, and every claim of the task returns it in `claims[].resumed` (A, G-WAI-14). A replaced group's conditions stay stored and listed (Proposals › Waits). A task resumed from a create-time group is claimed with `attempt` 1 (G-WAI-19). Timer conditions and wait timeouts fire from one `wait` timer per task at the earliest pending deadline (G-WAI-09).

Resolve (input): `POST /wait-conditions/{id}/resolve` `{ "value": { … } }` → `200 { "condition": { "id", "kind": "input", "state": "satisfied", "resolved_at", "resolved_by" } }` (S, `resolved_by` P). Needs `resolve` only, not `read` (P; A, G-SEC-18). Repeat (P, Proposals › Waits › `resolve`): same value (canonical JSON) from the same subject → `200`, same body, `Idempotency-Replayed: true`, no write; a different value, or the same value from another subject → `400` `failed_precondition` `ALREADY_RESOLVED` (A, G-WAI-05); a `cancelled` or `timed_out` condition → `400` `failed_precondition`; a timer, event or children condition → `400` `failed_precondition` `NOT_RESOLVABLE` (A, G-WAI-05). The value is checked alone against the 64 KiB payload limit (`413`, G-WAI-22).

p1 refusals (P, Proposals › Waits › Input and children waits stay p2; A, G-WAI-43): a condition of kind `input`, `children` or `event` (set by a worker, or in the create `wait` group), and `parent_task_id` on create, → `400` `invalid_argument` with a field violation (`reason: FEATURE_NOT_AVAILABLE`) on that field. `wait_for_children` and `propagate_cancel` are accepted and stored.

Condition states (A, G-WIRE-09): `pending` → `satisfied` \| `timed_out` \| `cancelled`.

### 5.9 Reads

- `GET /tasks/{id}` → task resource.
- **Paging** (every list that can grow; `guidelines/DNA/REST/QUERYING.md` through the ToolKit OData extractor): request `limit` (alias `$top`) and `cursor` (alias `$skiptoken`); scenarios write `limit` and `cursor`. Response `{ "items": [...], "page_info": { "next_cursor", "prev_cursor", "limit" } }`, no total. Default page size 25 (A, G-SEC-25), larger values clamped to the endpoint's cap, `limit=0` → `400` on `$top` (`INVALID_LIMIT`). A cursor is bound to its request: another `$filter` → `FILTER_MISMATCH`, `$orderby` with a cursor → `ORDER_WITH_CURSOR`, another `$select` or `view` → `FIELD_SELECTION_MISMATCH` (A, G-SEC-26), a bad token → `INVALID_CURSOR`; all `400`, violation on `cursor`.
- `GET /tasks/{id}/events?limit=50&cursor=…&$filter=event_type eq 'taskRetrying'` → `200 { "items": [{ "id": "{evt_1}", "event_type": "taskCreated", "actor": "{creator_a}", "payload": { … }, "timestamp": "…" }], "page_info": { … } }`. Oldest first by event ID; `$filter` on `event_type` (`eq`, `in`) and `timestamp`; default 25, cap 100 (P for paging and filters; A for the item shape, `actor` `null` for engine-caused events, G-WIRE-10; A for sort and page sizes, G-SEC-07). A `taskRetrying` item carries the full `error` (G-RTY-03).
- `GET /queues/{id}/events` → queue-scoped history (`queueHeld`, `queueReleased`, `configUpdated`, `workerBlacklisted`), same paging, needs queue `read` (A, G-OPS-05).
- `GET /tasks?$filter=…&$orderby=…&limit=…&view=short` → page of tasks (D, DESIGN.md:1136-1178); cap 100, 20 at `view=full`. An outright `read` denial → `403` `ACTION_DENIED`; a scope that matches nothing → empty page (A, G-SEC-20). Tag and resource-reference filters are engine functions, because the ToolKit parser has no `any()` lambda: `has_tag('project')`, `has_tag('project', 'q4')`, `has_resource_ref('<type>', '<id>')`; `parent_task_id eq …`, `predecessor_id eq …` and `dead_lettered_from eq …` are plain fields (A, G-SEC-05, G-RTY-11). The `created_at` window applies as designed (DESIGN.md:2824 exempts unfinished-only state filters); a search whose state filter restricts it to `dead_lettered` is exempt too, whatever its other filters (G-RTY-10). An unsupported `$orderby` key, a `$select` field outside the view or an unknown `view` → `NOT_ALLOWED` on that parameter; a range over the window → `OUT_OF_RANGE` on `$filter` (G-SEC-26). A `queue` filter on a dead-letter queue searches every domain (G-OPS-23). The 13 filter fields are a documented exception to the REST guideline's cap (G-SEC-27).
- `GET /tasks/count?$filter=…` → `{ "count": n, "capped": bool }` (D, DESIGN.md:1174).
- `GET /tasks/{id}/entries` (paged, `created_at asc`, `$filter` on `type`, `creator`, `created_at`), `GET /tasks/{id}/entries/{entry_id}`.
- `GET /tasks/{id}/wait-conditions` → paged list of every condition of the task, current and resolved (kept until retention, Proposals › Waits), `created_at asc`, `$filter` on `wait_group_id`, `state`, `kind` (S).
- `GET /queues`, `GET /queues/{id}` → queue resource (S):

```json
{ "id": "gts.cf.core.tasks.queue.v1~acme.reports._.render.v1", "kind": "dispatch",
  "limits": { "global_concurrency_limit": 50, "per_tenant_concurrency_limit": 5,
              "queue_max_queue_depth": 1000000, "tenant_max_queue_depth": 100000 },
  "domains": ["default"],
  "counts": { "queued": 12, "running": 3, "claimed": 0, "scheduled": 4, "blocked": 0, "waiting": 1 },
  "sampled_at": "2026-11-03T10:00:00Z",
  "hold": null }
```

`settings` (A, G-OPS-43): the effective non-limit settings (`default_priority`, `timeouts`, `retry_policy`, `tenant_fairness`, `dead_letter_queue`, `operated_by`, `accepted_types`, progress and blacklist settings) and `config_version`; `limits` also carries `tenant_max_deferred_tasks`. `domains` (P): the queue's domain set, primary first, plus the source domain while a move lasts. `hold` when set (P): `{ "since", "by", "reason", "until" }`. `sampled_at` is A: when the leader's sample was taken; all depth samples and the counts come from one pass, so tests wait until `sampled_at` is later than their setup (G-OPS-01, G-CLM-13). `counts` and `sampled_at` are returned only to callers that hold `configure` on the queue; others get the queue without them (A, G-OPS-02). A `dead_letter` queue is not routed, so its `domains` is empty (S).

### 5.10 Lifecycle events

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

All errors are RFC 9457 with the platform canonical categories (D, DESIGN.md:1180-1236; `docs/arch/errors/DESIGN.md`):

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.<category>.v1~",
  "title": "<Category Title>",
  "status": 409,
  "detail": "<human-readable detail>",
  "instance": "/api/task_engine/v1/<path>",
  "trace_id": "<trace>",
  "context": { … }
}
```

- `type` uses the `gts://` URI form of the platform errors DESIGN (`docs/arch/errors/DESIGN.md:525`). The Task Engine DESIGN example (DESIGN.md:1186) omits `gts://`: gap `G-WIRE-01`.
- **What the library can build** (`libs/toolkit-canonical-errors/src/context.rs`). Any category may add `resource_type` and `resource_name`. Beyond that:

  | Category | `context` keys |
  |---|---|
  | `invalid_argument` | exactly one of `field_violations[{field, description, reason}]`, `format`, `constraint` |
  | `failed_precondition` | `violations[{type, subject, description}]` (required) |
  | `resource_exhausted` | `violations[{subject, description, retry_after_seconds?}]` (required) |
  | `permission_denied`, `aborted` | `reason` (required) |
  | `unauthenticated` | `reason?` |
  | `service_unavailable` | `retry_after_seconds?` |
  | `not_found`, `already_exists` | nothing else |

- **Where a code goes** (A, G-WIRE-14, G-SEC-02, G-CLM-10). Only `permission_denied`, `aborted` and `unauthenticated` have `context.reason`. The other categories carry the code in the slot the library builds, and never in `context.reason`:

  | Category | Code goes in | Example |
  |---|---|---|
  | `invalid_argument` | `field_violations[].reason`; every code names a field | `{ "field": "/type", "reason": "UNKNOWN_TASK_TYPE" }` |
  | `failed_precondition` | `violations[{ type: "STATE", subject: <code> }]`, `resource_name` = the task or queue | `{ "type": "STATE", "subject": "STALE_ORDER" }` |
  | `resource_exhausted` | `violations[].subject` from a closed list: `queue_depth.queue`, `queue_depth.tenant`, `queue_depth.domain_global`, `queue_depth.deferred`, `concurrency`, `event_waits.<dimension>` | `{ "subject": "queue_depth.tenant", "retry_after_seconds": 30 }` |

  `resource_exhausted` names the queue in `resource_type`/`resource_name` for `queue`, `tenant`, `domain_global` and `concurrency`, and the task base type without a name for `deferred`. A plain state conflict uses `subject: "STATE"`.
- **Field paths** (A, G-SEC-21): an RFC 6901 JSON Pointer from the request body root (`/type`, `/queues/1`, `/wait/conditions/0/kind`, `/input/report_id`); a header or query parameter is named as sent (`Idempotency-Key`, `cursor`). A user-schema failure (input, output, entry, resolve value) has reason `SCHEMA_VIOLATION`, and its `description` names the failed keyword, never the value.
- **No field violations on `404` / `403`** (A, G-WIRE-15). A `404` carries only `resource_type` and `resource_name`. A `403` carries its reason; the creator-check or condition index goes only into `detail`.
- `field_violations[].reason` values (A, G-CRT-07 and the gap review): generic `REQUIRED`, `INVALID_FORMAT`, `OUT_OF_RANGE`, `TOO_LONG`, `TOO_MANY`, `DUPLICATE`, `PAYLOAD_TOO_LARGE`, `INVALID_ENUM_VALUE`, `NOT_ALLOWED`, `SCHEMA_VIOLATION`, `SCHEMA_TOO_LARGE`, `NOT_FOUND`, `UNSUPPORTED_QUERY_PARAM`; Task-Engine codes `UNKNOWN_TASK_TYPE`, `UNKNOWN_ENTRY_TYPE`, `UNKNOWN_EVENT_TYPE`, `TASK_DOMAIN_MISMATCH`, `DEPENDENCY_GRAPH_TOO_LARGE`, `EVENT_TOPIC_NOT_WAITABLE`, `EVENT_SUBJECT_TYPE_REQUIRED`, `EVENT_SUBJECT_TYPE_NOT_ALLOWED`, `INVALID_MATCH_EXPRESSION`, `BATCH_TOO_LARGE`, `TOO_MANY_TAGS`, `TOO_MANY_QUEUES`, `TOO_DEEP`, `DEAD_LETTER_QUEUE`, `CROSS_DOMAIN`, `TRUST_LIMIT`, `QUEUE_KIND`, `KEY_REUSED`, `INVALID_TYPE_POLICY`, `FEATURE_NOT_AVAILABLE`, and the cursor codes `INVALID_CURSOR`, `FILTER_MISMATCH`, `ORDER_WITH_CURSOR`, `FIELD_SELECTION_MISMATCH`, `INVALID_LIMIT`.
- `failed_precondition` subjects (A): `STATE`, `STALE_ORDER`, `CHILDREN_PENDING`, `TASK_FINISHED`, `NOT_CANCELLABLE`, `NO_ARMED_WAIT`, `ALREADY_RESOLVED`, `NOT_RESOLVABLE`, `DEPENDENCY_UNSATISFIABLE`, `PARENT_FINISHED`, `TARGET_BLACKLISTED`, `TOO_MANY_ENTRIES`, `DEAD_LETTER_QUEUE` (hold).
- `permission_denied` reasons (A, G-SEC-19): `ACTION_DENIED`, `EXPEDITE_REQUIRED`, `QUEUE_NOT_PERMITTED`, `CREATOR_AUTHORITY`, `CREATOR_AUTHORITY_UNVERIFIABLE`, `EVENT_CONSUME_DENIED`, `EVENT_CONSUME_UNVERIFIABLE`, `DUPLICATE_NOT_READABLE`, `SUPERSEDE_CANCEL_DENIED`, `CLAIM_REVOKED` (the task was released; the worker drops it).
- `aborted` reasons (A, G-SEC-22): `LEASE_LOST` (worker writes), `CONCURRENT_MODIFICATION` (payload revision), `STATE_CHANGED` (a non-worker write whose state guard lost, or a supersession out of retries).
- Do not invent other `context` keys; put extra detail into a `description`.
- `Retry-After` (A, G-SEC-03, G-CLM-09): sent on every `429` and `503`. The library change emits the header for `resource_exhausted` from the largest `violations[].retry_after_seconds`. Values: `CONCURRENCY_LIMIT` (subject `concurrency`) = the claim poll interval rounded up (1 s); depth and event-wait limits = `retry_after_secs` (30) jittered uniformly in [15, 30]; `503` = `unavailable_retry_after_secs` (5). Scenarios assert the header is a positive integer and, for depth, within [15, 30].
- `503` (A): PDP unreachable on any endpoint (G-SEC-01), registry unreachable for an uncached type (G-CRT-23), and node overload (admission shed, G-EXE-07). Overload is never `429`.
- Category → status (platform defaults, overrides as DESIGN says):

| Category | Title | Status in Task Engine |
|---|---|---|
| `invalid_argument` | Invalid Argument | 400; 413 for payload/body size; 428 for a missing `Idempotency-Key` |
| `unauthenticated` | Unauthenticated | 401 |
| `permission_denied` | Permission Denied | 403 |
| `not_found` | Not Found | 404 |
| `already_exists` | Already Exists | 409 |
| `aborted` | Aborted | 409 (`LEASE_LOST`, `CONCURRENT_MODIFICATION`, `STATE_CHANGED`) |
| `failed_precondition` | Failed Precondition | 400 (subjects listed above) |
| `resource_exhausted` | Resource Exhausted | 429 + `Retry-After` |
| `service_unavailable` | Service Unavailable | 503 |
| `internal` | Internal | 500 |

- `429` names only the full dimension, never usage numbers (PRD.md:455).
- A request about a task the caller may not read gets the same `404` as a missing task; `403` only when it may read but not act (PRD.md:918, DESIGN.md:1016).

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
