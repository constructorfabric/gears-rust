# Task Engine: Scenario Guide

Concrete behaviour scenarios for the Task Engine, written as literal HTTP exchanges with the expected response and side effects, in the style of the [Event Broker scenarios](../../system/event-broker/scenarios/INDEX.md). They have two jobs:

1. **Test basis.** Each file is one behaviour a test can assert. Requirement IDs and doc lines in each header make coverage traceable to the [PRD](../docs/PRD.md), [DESIGN](../docs/DESIGN.md) and [ADRs](../docs/ADR/).
2. **Gap finder.** Wherever the docs did not settle an outcome, the scenario cites an entry in [GAPS.md](GAPS.md). All 232 entries are decided (wire review 2026-11-04, gap review 2026-11-06) and the scenarios follow the decisions; what remains is the PRD, DESIGN and ADR edit each entry names. Error shapes and every reason code are catalogued in [ERRORS.md](ERRORS.md).

**Read [CONVENTIONS.md](CONVENTIONS.md) first.** It sets the baseline, the fixtures, the wire shapes (each marked documented, agreed, proposed or set by the scenarios, and drafted as [`../docs/openapi.yaml`](../docs/openapi.yaml); `main` has no OpenAPI yet), the error envelope, the side-effect vocabulary and the file format.

**Baseline.** `main` at `6efb0f856` plus every `Proposed` item of the design proposals in [PR #5730](https://github.com/constructorfabric/gears-rust/pull/5730), taken as accepted (CONVENTIONS §1). 95 scenarios depend on an accepted proposal and say so in a `Depends on` row; the docs on `main` have not been edited for those items yet.

## At a glance

| | Count |
|---|---:|
| Scenarios | 293 files, 130 of them case tables with 602 rows in total |
| by kind | 122 positive · 59 negative · 84 guardrail · 28 flow |
| by phase | 184 p1 · 83 p2 · 26 p3 (by the file's main phase; some rows are a later phase) |
| Gap entries | 232 (43 contradictions, 100 missing rules, 70 unclear, 5 open proposal items, 14 wire), all decided; the doc edits are pending |

## How to use these scenarios

- **Writing a test:** set up the fixtures from CONVENTIONS §4 and the file's Setup, send the request, assert the status and body, then assert each Side effects bullet. A file with a `## Cases` table is one parametrised test: one case per row, each row stating how it differs from the representative exchange. Tests need a controllable clock (scenarios use `T0`), a PDP double, and for some scenarios two engine nodes.
- **Choosing a test level:** [Test levels](#test-levels) says which scenarios belong in the E2E suite and which in integration tests.
- **Choosing a slice:** filter on the `Phase` row. A p1 test plan is the `p1` files of every folder plus flows 1.01, 1.03, 1.08 and 1.09.
- **When a gap's doc edit lands:** check the scenarios listed under it still match the docs, then delete the entry.
- **Adding a scenario:** follow CONVENTIONS §2 (format and naming) and §8 (gaps). Put it in the folder that answers the reader's question, not the folder of the endpoint it calls.

## Journeys

### API consumer: submit work and follow it

1. [Create a task](tasks/create/1.01-positive-minimal-create-queued.md): `POST /tasks` with an `Idempotency-Key` → `201`, `queued`.
2. [Retry safely](tasks/idempotency/1.01-positive-replay-returns-409-with-existing-id.md): the same key again → `409` with the existing ID; a [missing key](tasks/idempotency/1.03-negative-missing-or-malformed-idempotency-key.md) → `428`.
3. Follow it: [read by ID](tasks/read/), [search](search/), [history](events/).
4. Stop it if needed: [cancel](tasks/cancel/).
5. Answer a question it asks: [resolve an input wait](waits/input/).

### Worker: claim, run, finish

1. [Claim](worker/claim/1.01-positive-claim-single-task.md): `POST /tasks/claim` → session, lease token, `heartbeat_interval_secs`.
2. [Start](worker/execution/1.01-positive-start-claimed-task.md), then keep the session alive with a [batch heartbeat](worker/sessions/1.01-positive-batch-heartbeat-renews-session.md).
3. Report [progress](worker/execution/), save [checkpoints](worker/execution/), write [entries](entries/).
4. [Complete](worker/execution/1.04-positive-complete-accepted-results.md), [fail with a retryable error](retry/1.01-positive-retryable-error-schedules-retry.md), [release](worker/release/1.01-positive-release-returns-task-to-queue.md), or [park on a wait](waits/framework/1.01-positive-set-wait-parks-task.md).
5. Expect [fencing](worker/fencing/): a lost lease is final (`409 LEASE_LOST`).

> **End to end:** [create → claim → execute → complete](flows/1.01-flow-create-claim-execute-complete.md) composes these steps in one transcript.

### Operator

- [Inspect queues](queues/1.01-positive-queue-introspection.md), [hold a queue](queues/2.01-positive-hold-queue.md) during an incident ([flow](flows/1.07-flow-incident-hold-and-release.md)), change [runtime config](queues/).
- Recover failures: [dead-letter and re-enqueue](dead-letter/) ([flow](flows/1.02-flow-retries-dead-letter-reenqueue.md)), [bulk operations](bulk/), [reassignment](worker/reassignment/).

## Scenarios by area

### Tasks: create and identity (38)

#### tasks/create/ — `POST /tasks` (plus `PATCH /tasks/{id}/tags`)
- [positive-1.1 — A minimal create puts the task in `queued` with its timers armed](tasks/create/1.01-positive-minimal-create-queued.md) — 201 `queued` with the resolved timeouts and retry policy stored; `queue_timeout` and `lifetime` timers armed, no execution deadline until the claim (G-EXE-10); `taskCreated` recorded and published, no `taskQueued`; caller-key row stores the resolved queue (G-CRT-11).
- [positive-1.2 — `run_after` decides the starting state: future → `scheduled`, past → `queued`, beyond the lifetime → refused](tasks/create/1.02-positive-run-after-placement-and-lifetime-bound.md) — 3 cases: future → 201 `scheduled`, queue timeout armed only once `queued`; past → 201 `queued` at once, no scheduled timer; beyond `created_at` + lifetime → 400 `OUT_OF_RANGE` on `/run_after` (G-CRT-22); omitted priority takes the queue default (G-CRT-26).
- [positive-1.3 — Settings resolve request → type → queue → engine default, within the trust limits](tasks/create/1.03-positive-settings-resolution.md) — 3 cases: type defaults (queue from `default_queue`, timeouts per field, retry policy whole); engine default execution timeout 3600 and lease 30 when nobody sets one; inherited 86400 s clamped to the tenant-owned cap 3600 (G-CRT-04, G-CRT-05).
- [positive-1.4 — An initial wait group on create puts the task in `waiting`](tasks/create/1.04-positive-initial-wait-group-waiting.md) — 3 cases: timer → 201 `waiting`, deferred admission, no queue timeout until `queued`; input (p2) → 201 `waiting`, resolve → `queued`; `children` → 400 `NOT_ALLOWED` on `/wait` in every phase (G-CRT-01, G-WAI-43).
- [positive-1.5 — Tags and resource references are set at create; references are unverified without a check](tasks/create/1.05-positive-tags-and-resource-refs.md) — 201 with tags sorted by key and refs; `verified: false` in task, row and `taskCreated`; tag search scoped to the creator's tenant (G-CRT-06, G-WIRE-04).
- [guardrail-1.6 — `critical` urgency needs the `expedite` action: 403 without it, accepted with it](tasks/create/1.06-guardrail-critical-urgency-needs-expedite.md) — 3 cases: without `expedite` → 403 `EXPEDITE_REQUIRED` (not 404); `normal` → 201; with `expedite` → 201 `urgency: critical` (G-CRT-08).
- [negative-1.7 — An unknown type, or one the caller cannot see, is rejected before anything is stored](tasks/create/1.07-negative-unknown-task-type.md) — 2 cases: unregistered type and another tenant's private type get the identical 400 `UNKNOWN_TASK_TYPE` on `/type`; checked first, no schema oracle (G-CRT-24).
- [negative-1.8 — Registry unreachable: an uncached type fails closed with 503, a cached type is still served](tasks/create/1.08-negative-registry-outage-uncached-type-503.md) — 3 cases: uncached → 503 `Retry-After: 5`, fail closed; cached type → 201; per-tenant visibility past its TTL → 503 (G-CRT-23).
- [negative-1.9 — Input that fails the type's schema is rejected](tasks/create/1.09-negative-input-fails-schema.md) — 400 `SCHEMA_VIOLATION` field violations into `/input`; no value echoed in body or log; nothing stored, so the key stays free (G-SEC-21).
- [negative-1.10 — Create fields outside their limits are rejected: priority, tags, resource references](tasks/create/1.10-negative-field-limits.md) — 9 cases, all 400 field violations: priority 1001 and -1 (`OUT_OF_RANGE`); tag key > 128 or empty; tag value > 256; 17 tags (`TOO_MANY_TAGS`); repeated tag key (`DUPLICATE`); 17 > 16 resource references; `resource_id` > 256 (G-CRT-06, G-CRT-10, G-WIRE-04).
- [negative-1.11 — Oversized creates are rejected with 413: combined payload over 64 KB, body over 1 MB](tasks/create/1.11-negative-payload-and-body-size-limits.md) — 3 cases, all 413 `invalid_argument`: combined `input` + `context` over 65,536 bytes (`PAYLOAD_TOO_LARGE`); `Content-Length` over 1 MB refused from the headers within 10 ms; chunked body stopped after byte 1,048,577 (G-CRT-24).
- [negative-1.12 — Creates outside the type's trust limits or into a queue that may not hold the task are rejected](tasks/create/1.12-negative-placement-and-trust-limit-refusals.md) — 7 cases: 400 `TRUST_LIMIT` for execution timeout above the tenant-owned cap and lease below `min_lease_secs`; 400 `NOT_ALLOWED` for tenant type into `operated_by: platform` queue and type not in `accepted_types`; 400 `DEAD_LETTER_QUEUE` (G-CRT-02); unknown queue → 404 (G-CRT-03); partial retry policy → 400 `REQUIRED` (G-CRT-04).
- [positive-1.13 — Tags can be changed after create; nothing else can](tasks/create/1.13-positive-update-tags-after-create.md) — `PATCH` tags with `update` → 200, one `taskTagsUpdated` (G-SEC-28); 11 cases: running and terminal task → 200; same key in set and remove, key twice, over the tag limit, key too long, empty key → 400; reader, `resolve`-only, worker → 403 `ACTION_DENIED`; other tenant → 404.

#### tasks/idempotency/ — `POST /tasks` with `Idempotency-Key`
- [positive-1.1 — A replay returns 409 `already_exists` with the existing task's ID, whatever happened to the task since](tasks/idempotency/1.01-positive-replay-returns-409-with-existing-id.md) — 4 cases, all 409 `already_exists` naming the first task: still queued; after checkpoint and completion; after dead-lettering moved the queue (key row keeps the create-time queue, G-CRT-11); `priority` and tags changed (not applied) (G-CRT-25).
- [positive-1.2 — A key is held while its task is unfinished and for the 7-day window; then the same key creates a new task](tasks/idempotency/1.02-positive-key-window-release.md) — 3 cases: finished past the window → 201 new task, expired row deleted by the create; unfinished past the window → 409; finished late → key released in the terminal transaction, then 201 (G-CRT-12, G-CRT-13).
- [negative-1.3 — A create without a valid `Idempotency-Key` is rejected: 428 when missing, 400 when empty or malformed](tasks/idempotency/1.03-negative-missing-or-malformed-idempotency-key.md) — 8 cases: missing → 428 `REQUIRED`; empty, space, non-ASCII, 256 characters, malformed on a dedup type → 400 `INVALID_FORMAT` (G-CRT-09); missing with unknown type → 400 `UNKNOWN_TASK_TYPE`; missing with invalid input → 428 (G-CRT-24).
- [negative-1.4 — The same key with a different request is rejected on `Idempotency-Key`](tasks/idempotency/1.04-negative-same-key-different-request.md) — 5 cases: different input or a retry naming another queue → 400 `KEY_REUSED` on `Idempotency-Key`, no ID disclosed; retry naming the resolved queue, omitting a named queue, or as first sent → 409 (G-CRT-11).
- [guardrail-1.5 — Two subjects using the same key get two independent tasks](tasks/idempotency/1.05-guardrail-two-subjects-same-key-independent.md) — 201 new task for the second subject; neither sees the other's; a repeat names only the subject's own task.
- [guardrail-1.6 — A replay is answered 409 even when the queue is full, and uses no capacity](tasks/idempotency/1.06-guardrail-replay-not-refused-as-full.md) — 2 cases: per-tenant depth full and queue-wide depth full both answer 409, never 429, no capacity used (G-CRT-14, G-CLM-24). Home of the rule.
- [guardrail-1.7 — A refused create does not hold its key](tasks/idempotency/1.07-guardrail-refused-create-holds-no-key.md) — 400 `SCHEMA_VIOLATION` then 201 with the same key: refusals store no key.

#### tasks/dedup/ — `POST /tasks` for types with a `dedup` trait (p2)
- [positive-1.1 — A duplicate by derived identity returns 409 without any key](tasks/dedup/1.01-positive-duplicate-by-derived-identity.md) — 409 `already_exists` with the holder's ID, no key needed, no 428; fields outside `key_fields` ignored (G-CRT-25).
- [positive-1.2 — The dedup window decides when a finished task releases its identity](tasks/dedup/1.02-positive-window-release-and-reuse.md) — 4 cases: `active` holder dead-lettered → 201, head moves, old re-enqueue → 409; `active_or_succeeded` holder succeeded → 409 within retention; same past the row's `expires_at` → 201 (G-CRT-12); `active_or_succeeded` holder cancelled → 201 (G-CRT-13).
- [guardrail-1.3 — A key sent for a `dedup` type is ignored: not checked, not stored](tasks/dedup/1.03-guardrail-sent-key-is-ignored.md) — Different key → 409; same key, different identity → 201; no caller-key row stored.
- [guardrail-1.4 — The dedup scope decides who shares an identity: `tenant` across subjects, `subject` per subject, never across tenants](tasks/dedup/1.04-guardrail-identity-scope.md) — 3 cases: `tenant` scope, second subject → 409 naming the other's task; `subject` scope, second subject → 201; other tenant under `tenant` scope → 201, never crosses tenants.
- [guardrail-1.5 — A dedup hit on a task the caller may not read is 403 with no ID](tasks/dedup/1.05-guardrail-dedup-hit-on-unreadable-task-403.md) — 403 `DUPLICATE_NOT_READABLE`, no ID, when the holder is not readable; transaction rolled back (G-CRT-08).
- [negative-1.6 — A type with an invalid `dedup` policy cannot be created; an `authz_scope` type cannot be registered](tasks/dedup/1.06-negative-invalid-dedup-policy-or-authz-scope.md) — 2 cases: key field not a required input field → 400 `INVALID_TYPE_POLICY` on `/type`, never a fallback; `authz_scope` type fails registration → 400 `UNKNOWN_TASK_TYPE` (G-CRT-15).
- [negative-1.7 — A key field value over 4 KiB is rejected](tasks/dedup/1.07-negative-key-field-value-over-4kib.md) — 400 `TOO_LONG` on `/input/period` over `max_key_value_bytes` (4096, JCS form) (G-CRT-16).

#### tasks/supersede/ — `POST /tasks` for types with a `supersede` trait (p2)
- [positive-1.1 — A higher order value supersedes the group's unfinished task; how depends on its state and the mode](tasks/supersede/1.01-positive-higher-order-supersedes-by-old-task-state.md) — 4 cases, all 201: unleased → cancelled `superseded` at once, atomic; running graceful → `cancel_requested` + `cancel_grace`; running forced → cancelled at once, lease invalid, slots freed; old already finished → just creates, no `cancel` right needed (G-CRT-17).
- [positive-1.2 — How a gracefully superseded task ends: grace expiry and worker reports](tasks/supersede/1.02-positive-graceful-supersession-endings.md) — 6 cases: grace expires → `superseded`, not `forcibly_cancelled`; worker reports `cancelled` or any `error` → `superseded`, no retry; worker reports `success` or `warning` first → `completed`, `superseded_by` kept (G-CRT-18).
- [positive-1.3 — Supersession reaches the replaced task's children](tasks/supersede/1.03-positive-supersession-cancels-children.md) — Superseded parent's child is cancelled by the cascade in the same transaction with the supersession's mode (`forcibly_cancelled` here), never `superseded`, no actor (G-CRT-21).
- [negative-1.4 — An order value not above the group's latest never supersedes: lower is `STALE_ORDER`, equal is a replay](tasks/supersede/1.04-negative-order-not-above-latest-does-not-supersede.md) — 2 cases: lower → 400 `failed_precondition` `STALE_ORDER`, no task named, even after the newer task finished; equal → 409 replay naming the live task (G-CRT-25).
- [negative-1.5 — A supersede type rejects `cancellable: false`](tasks/supersede/1.05-negative-cancellable-false-on-supersede-type.md) — 400 `NOT_ALLOWED` on `/cancellable` for `cancellable: false`.
- [negative-1.6 — Without `cancel` on the task it would replace, the whole create is refused](tasks/supersede/1.06-negative-caller-lacks-cancel-on-old-task.md) — 403 `SUPERSEDE_CANCEL_DENIED`; no task created, old task untouched (G-CRT-08).
- [guardrail-1.7 — A replacement is not refused for the queue place its predecessor held](tasks/supersede/1.07-guardrail-replacement-not-refused-for-predecessor-capacity.md) — 3 cases: old queued in the same queue → 201 at a full per-tenant depth limit, net zero; old `scheduled` or `running` → normal check, 429 `queue_depth.tenant` (G-CRT-14).
- [guardrail-1.8 — Supersession is authorized before the transaction and re-planned if the old task changed](tasks/supersede/1.08-guardrail-plan-rechecked-when-old-task-gets-leased.md) — 3 cases: old task claimed mid-flight → 201 with a graceful cancel request, one `cancel` PDP call, none inside the transaction; head moved → 400 `STALE_ORDER`; plan changes on all 3 attempts → 409 `aborted` `STATE_CHANGED`, no `Retry-After` (G-CRT-19).
- [guardrail-1.9 — Concurrent submissions of one group converge on the highest order value](tasks/supersede/1.09-guardrail-concurrent-submissions-converge-on-highest.md) — Concurrent revisions 8 and 9: 9 → 201; 8 → 201 then superseded, or 400 `STALE_ORDER`; one live task, head at 9 (G-CRT-19).
- [positive-1.10 — Under `tenant` scope, one subject can supersede another subject's task](tasks/supersede/1.10-positive-cross-subject-supersession-under-tenant-scope.md) — 2 cases: `{creator_a2}` with `cancel` supersedes `{creator_a}`'s revision 7 → 201; without `cancel` → 403 `SUPERSEDE_CANCEL_DENIED` (G-CRT-08).
- [flow-1.11 — Graceful supersession of a running task: cancel request, then `superseded` when the worker stops](tasks/supersede/1.11-flow-graceful-supersede-running-task.md) — 201; old running task gets `cancel_requested` + `cancel_grace`; heartbeat shows `cancel_request.superseded_by`; worker's `cancelled` ends `superseded`, slots freed (G-CRT-17).

### Workers: claim and order (22)

#### worker/claim/ — `POST /tasks/claim`
- [positive-1.1 — Claim one task: session, lease, attempt and deadlines in one step](worker/claim/1.01-positive-claim-single-task.md) — `200`; new session, `domain` `default`, `lease_epoch` 1, `attempt` 1, `heartbeat_interval_secs` 10; queue timeout removed, assignment and execution deadlines armed by the claim (G-EXE-10); `taskClaimed` in history, not published.
- [positive-1.2 — Batch claim returns up to `max_tasks` tasks, each with its own lease](worker/claim/1.02-positive-batch-claim-token-per-task.md) — `max_tasks: 5` with 3 ready → `200` with 3 claims at once, one session, a distinct token per task; one task's token on another → `409 LEASE_LOST`.
- [positive-1.3 — A claim that finds nothing waits for its timeout, capped at `claim_max_wait_secs`, then answers `204`](worker/claim/1.03-positive-empty-claim-waits-then-204.md) — 3 cases: `timeout_secs: 5` → `204` at 5 s; `timeout_secs: 120` → accepted, `204` at the 50 s cap; `timeout_secs: 0` → `204` at once; no session opened (G-CLM-16).
- [positive-1.4 — The claim returns the task's owner tenant, creator, owner and creator authority](worker/claim/1.04-positive-claim-returns-owner-and-authority.md) — platform worker claim returns `owner_tenant_id` A, `created_by`, `owner_id` and verified `authority`.
- [positive-1.5 — `attempt` goes up on a claim after a retry or a liveness requeue, and stays the same after a release or a wait resume](worker/claim/1.05-positive-attempt-counting-across-claims.md) — 5 cases: first claim → 1; after retry / liveness requeue → 2; after release / wait resume → unchanged (resume carries `resumed`); `lease_epoch` +1 every time; each claim sets a fresh execution deadline (G-RTY-22). Home of `attempt` counting.
- [positive-1.6 — A claim reuses the caller's live `session_id`; an expired, unknown or foreign one opens a new session](worker/claim/1.06-positive-claim-reuses-live-own-session-else-opens-new.md) — 5 cases: live own → same session, renewed; expired / unknown → new session, old not revived; same subject with another `worker_id` → new session (G-CLM-17); another subject's → new session, theirs untouched, no oracle.
- [negative-1.7 — A claim with a bad `worker_id`, `max_tasks` or queue list is rejected with `400`](worker/claim/1.07-negative-claim-request-validation.md) — 13 cases: `400 invalid_argument`, one field violation, at once: `worker_id` missing / malformed / empty / 129 chars; `max_tasks` 33 / 0; `timeout_secs` −1; `queues` missing / empty; 17 queues `TOO_MANY_QUEUES`; a dead-letter queue (p2); `CROSS_DOMAIN` (p3); a duplicate entry is not refused (G-CLM-18).
- [negative-1.11 — A claim naming an unregistered queue gets `404`](worker/claim/1.11-negative-claim-naming-unknown-queue-not-found.md) — `404 not_found` naming the queue, at once; no session opened (G-CLM-15).
- [guardrail-1.8 — Two workers claiming at the same moment never get the same task](worker/claim/1.08-guardrail-two-workers-never-claim-same-task.md) — two concurrent claims on two tasks → each gets a different task; never the same task twice.
- [guardrail-1.9 — A worker allowed to claim only queue Q never receives another queue's task](worker/claim/1.09-guardrail-queue-policy-limits-what-worker-receives.md) — 3 cases: PDP allows **reports** or **export**: a **render** task in **bulk** is never returned (`204` after the timeout); an **export** task in **bulk** → `200`; a provably excluded queue → `403 QUEUE_NOT_PERMITTED` (G-CLM-14).
- [flow-1.10 — Long-poll claim wakes for a task created on another node within the poll interval](worker/claim/1.10-flow-long-poll-wakes-on-task-from-other-node.md) — claim parked on N1 gets a task created through N2 within one jittered `claim_poll_interval_ms`.

#### worker/ordering/ — `POST /tasks/claim` (dispatch order)
- [positive-1.1 — A `critical` task is claimed before `normal` tasks of its queue, whatever their tenant or age](worker/ordering/1.01-positive-critical-before-normal-in-queue.md) — B's newer priority-1000 `critical` task is claimed before A's older priority-0 `normal` task.
- [positive-1.2 — `round_robin` serves a small tenant's tasks between a big tenant's, whatever their priority](worker/ordering/1.02-positive-round-robin-interleaves-small-tenant.md) — A 1,000 tasks at priority 0, B 10 at 1000: a 20-task claim returns all 10 of B's.
- [positive-1.3 — Priority orders one tenant's own tasks, never one tenant against another](worker/ordering/1.03-positive-priority-orders-only-within-tenant.md) — within A, priority 10 beats an older priority 500; B's priority 1000 is not held behind A.
- [positive-1.4 — A `fifo` queue serves all tenants in one order: priority, then age, then task ID](worker/ordering/1.04-positive-fifo-queue-orders-by-priority-age-id.md) — `fifo`: claims come out priority, then age, across tenants.
- [positive-1.5 — The order in which a claim lists its queues does not change what it gets](worker/ordering/1.05-positive-queue-listing-order-does-not-matter.md) — listing **bulk** first still yields the older `critical` task in **reports**.
- [guardrail-1.6 — Neither priority nor age is compared across queues](worker/ordering/1.06-guardrail-priority-and-age-not-compared-across-queues.md) — priority-0 tasks in one queue and old priority-1000 tasks in another → one of each over two claims.
- [guardrail-1.7 — Under concurrent claims order is best effort: tests must not assert strict FIFO](worker/ordering/1.07-guardrail-strict-fifo-not-promised-under-concurrency.md) — four concurrent claims get four distinct tasks; tests must not assert FIFO order under concurrency.
- [guardrail-1.8 — Tenant rotation across nodes is approximate but never skips a tenant for more than a cycle per node](worker/ordering/1.08-guardrail-round-robin-across-nodes-is-approximate.md) — two nodes: each node's two claims serve one task of A and one of B; global interleaving not asserted.
- [flow-1.9 — Across named queues, `critical` tasks go first, oldest first, from any queue](worker/ordering/1.09-flow-critical-first-across-queues-oldest-first.md) — oldest `critical` task across named queues first (G-CLM-02), ahead of older `normal` work; then `normal` by rotation; a batch takes both `critical` tasks first (G-CLM-03).
- [flow-1.10 — For `normal` work a claim rotates between its queues, starting after the previous claim's start queue](worker/ordering/1.10-flow-rotation-between-queues-for-normal-work.md) — `normal` work alternates queues claim by claim on one node; a batch moves on from an empty start queue.
- [flow-1.11 — Each scan budget is shared by the queues of its kind in one claim round; a round that uses it up returns what it has](worker/ordering/1.11-flow-scan-budgets-per-kind-per-claim-round.md) — `claim_candidate_budget: 2` shared by both `round_robin` queues → claims alternate `429` and `200`; a `fifo` queue has its own row budget and still serves (G-CLM-06).

### Workers: leases and execution (37)

#### worker/sessions/ — `POST /tasks/heartbeat` (batch), session scan
- [positive-1.1 — A batch heartbeat renews the worker session and reports on every lease](worker/sessions/1.01-positive-batch-heartbeat-renews-session.md) — `200`, per-lease `ok` + `state` + `cancel_requested` (and the pending `cancel_request`), `heartbeat_interval_secs: 10`; one session write, no `te_task_state` write, per-task deadlines unchanged, no PDP call (G-WIRE-06).
- [positive-1.2 — Claiming a task with a shorter lease shortens the session lease and the heartbeat interval](worker/sessions/1.02-positive-short-lease-task-lowers-session-lease.md) — claiming a 12 s task returns `heartbeat_interval_secs: 4`, `lease_secs: 12` for the whole session; later heartbeats keep 4 s / 12 s after it completes; the idle SDK opens a new 30 s session (G-EXE-03).
- [negative-1.3 — A batch heartbeat with more leases than heartbeat_batch_max is rejected](worker/sessions/1.03-negative-batch-heartbeat-over-max.md) — 501 leases → `400` `invalid_argument`, `TOO_MANY` on `/leases`, session not renewed (G-EXE-04).
- [guardrail-1.4 — A heartbeat reports lease_lost for every lease the caller no longer holds, and renews nothing it should not](worker/sessions/1.04-guardrail-heartbeat-lease-lost-cases.md) — 5 cases, all `200` with the lost leases `lease_lost` and no state: stale lease in batch (others renewed), late heartbeat after expiry (session stays expired, next claim opens a new one), between expiry and requeue, foreign session and unknown session (same body, nothing renewed) (G-EXE-01).
- [guardrail-1.5 — The single-task heartbeat endpoint is gone](worker/sessions/1.05-guardrail-single-task-heartbeat-removed.md) — `POST /tasks/{id}/heartbeat` → `404`/`405`, session not renewed.
- [guardrail-1.6 — Heartbeats keep sessions alive while the PDP is unreachable](worker/sessions/1.06-guardrail-heartbeat-works-during-pdp-outage.md) — PDP unreachable → heartbeat still `200 ok`, no PDP call, no session expires; other worker writes `503` + `Retry-After: 5` (G-SEC-01).
- [guardrail-1.7 — Under overload claims are shed while heartbeats and completions still pass](worker/sessions/1.07-guardrail-reserved-admission-keeps-heartbeats-flowing.md) — shared permits full: claim `503` + `Retry-After: 5`, heartbeat and `complete` `200` on a reserved permit, progress `503` (G-EXE-07).
- [guardrail-1.8 — The SDK retries a shed heartbeat at once and ignores a Retry-After longer than the lease](worker/sessions/1.08-guardrail-sdk-heartbeat-retries-shed-heartbeat.md) — heartbeat `503 Retry-After: 60` → the SDK resends within about a second → `200`, lease extended (G-EXE-07).
- [guardrail-1.9 — A running task past its execution deadline ends timed_out even when the session scan finds it first](worker/sessions/1.09-guardrail-execution-deadline-wins-over-session-expiry.md) — session scan reaches an overdue running task first → `dead_lettered` / `timed_out`, no liveness requeue, no blacklist strike, `te_lease_expired_total{cause="execution"}` (G-EXE-10).
- [flow-1.10 — A worker that stops heartbeating loses its session, and its tasks are requeued with backoff](worker/sessions/1.10-flow-session-expiry-requeues-tasks.md) — no heartbeat for 30 s → session `expired` within one scan interval; both tasks `scheduled`, `liveness_requeues: 1`, backoff 4–5 s, slots freed, `taskRetrying` `cause: liveness`, one blacklist strike (G-EXE-05).

#### worker/fencing/ — lease token and holder checks on every worker call
- [positive-1.1 — Each new lease carries a larger lease epoch](worker/fencing/1.01-positive-lease-epoch-grows-per-lease.md) — claim after release → new token, `lease_epoch: 2`, `attempt` still 1; the epoch is visible in `assignment`, the token is not.
- [positive-1.2 — Repeating a completion with the same token and result returns the stored outcome](worker/fencing/1.02-positive-idempotent-completion-replay.md) — same token, same result → `200` stored outcome + `Idempotency-Replayed: true`, nothing written; a repeated retryable error or release gets `LEASE_LOST` (G-EXE-16).
- [negative-1.3 — A second completion with a different result is rejected](worker/fencing/1.03-negative-completion-replay-different-result.md) — same token, different result → `400` `failed_precondition` (`STATE`), stored result kept (G-WIRE-14).
- [negative-1.4 — Every lease-fenced worker call that fails the fence gets LEASE_LOST and writes nothing](worker/fencing/1.04-negative-failed-fence-returns-lease-lost.md) — 11 cases, all `409` `LEASE_LOST` with nothing written: checkpoint, progress (before buffering), complete, start, release, worker entry, checkpoint racing a payload write (never `CONCURRENT_MODIFICATION`), live token from another subject (not released), another tenant and unknown task ID (never `404`, G-EXE-09), write after session expiry and requeue (G-EXE-02).
- [guardrail-1.5 — Progress buffered under a lease that is lost before the flush is never saved](worker/fencing/1.05-guardrail-buffered-progress-dropped-after-lease-loss.md) — progress `200` then session expiry before the flush → snapshot dropped, `te_progress_stale_dropped_total` +1, no `taskProgressed`.
- [guardrail-1.6 — LEASE_LOST is final: the worker does not retry, and a retry would fail the same way](worker/fencing/1.06-guardrail-lease-lost-is-final.md) — after `409` `LEASE_LOST` the SDK sends nothing more for that lease; a manual resend gets the same `409` (G-EXE-16).
- [guardrail-1.7 — The lease token never appears in task reads, history, events or search](worker/fencing/1.07-guardrail-lease-token-never-exposed.md) — `GET` task, history, `view=full` search and broker events carry no `lease_token`; only `lease_epoch`.
- [guardrail-1.8 — A worker write re-checks claim: a revoked or narrowed grant releases the task with 403 CLAIM_REVOKED, an unreachable PDP fails closed with 503](worker/fencing/1.08-guardrail-worker-write-authorization-outcomes.md) — 4 cases: `claim` revoked, scope excludes the task on `complete` or on progress → `403` `CLAIM_REVOKED`, task `queued`, `taskQueued` `reason: revoked`, no budget/attempt/blacklist change (G-EXE-08); PDP unreachable → `503` + `Retry-After: 5`, task stays `running` and leased (G-SEC-01).
- [flow-1.9 — A worker that lost its lease cannot complete the task; the new holder's result is kept](worker/fencing/1.09-flow-zombie-complete-after-reclaim.md) — old token → `409` `LEASE_LOST`, nothing written; new holder's `complete` → `200`, its result kept.

#### worker/execution/ — `POST /tasks/{id}/start`, `/progress`, `/checkpoint`, `/complete`
- [positive-1.1 — Starting a claimed task makes it running; the execution deadline set by the claim stays](worker/execution/1.01-positive-start-claimed-task.md) — `200` `running`; assignment deadline cleared, execution deadline stays at claim + 900 s, token and epoch kept, `taskStarted` (G-EXE-10).
- [positive-1.2 — Progress is buffered, saved within one flush interval, and produces events only as its event mode allows](worker/execution/1.02-positive-progress-saved-and-event-modes.md) — 5 cases, all `200` empty body, saved within one 5 s flush: mode `none` (no event, even with policy `all`); `milestones` percent-only (no event) and stage change (one `taskProgressed`); `all` (one per flush).
- [positive-1.3 — A checkpoint is merged into the task context as a JSON Merge Patch](worker/execution/1.03-positive-checkpoint-merges-as-json-merge-patch.md) — 4 cases (replace/add, `null` deletes, nested merge, array replaced whole): `200` merged `context`, `revision` +1, `taskCheckpointed` (G-EXE-13, G-EXE-12).
- [positive-1.4 — A valid terminal completion ends the task, stores its result and frees its slots](worker/execution/1.04-positive-complete-accepted-results.md) — 5 cases: `success`, `warning` with a `TaskError`-shaped warning, `cancelled` after a cancel request, output checked against the creation revision (G-EXE-14), success without `start` (G-EXE-11); `200` terminal, slots freed, token kept for replay.
- [positive-1.5 — A reported failure records its fault, and the published event carries only its classification](worker/execution/1.05-positive-worker-fault-recorded.md) — retryable `error` with `fault: worker` → `200` `scheduled`, `retry_count: 1`, `result: null`, full error in `taskRetrying` (`cause: reported`), published event without `message`/`context` (G-RTY-03).
- [positive-1.6 — `start` is optional and safe to repeat: a second `start` is a no-op and a worker may finish from `claimed`](worker/execution/1.06-positive-start-optional-and-repeatable.md) — 6 cases, all `200`: second `start` on a running task is a no-op, deadline unmoved (G-EXE-10); success, reported failure, cancel acknowledgement, delayed release and set-wait straight from `claimed` (G-EXE-11).
- [negative-1.7 — Progress with a percent outside 0–100 is rejected](worker/execution/1.07-negative-progress-percent-out-of-range.md) — `percent: 140` → `400` `invalid_argument` on `/percent`, nothing buffered.
- [negative-1.8 — A checkpoint that pushes the task over 64 KB is rejected and changes nothing](worker/execution/1.08-negative-checkpoint-over-payload-limit.md) — merged input + context + output over 65,536 B → `413`, context, revision and lease unchanged (G-EXE-13).
- [negative-1.9 — Two concurrent payload writes on one task: one wins, the other gets CONCURRENT_MODIFICATION](worker/execution/1.09-negative-concurrent-payload-write-conflict.md) — two checkpoints on one revision → one `200`, the other `409` `CONCURRENT_MODIFICATION` (retryable), lease kept.
- [negative-1.10 — A completion whose result breaks the result rules is refused; the task and lease are unchanged](worker/execution/1.10-negative-complete-result-refused.md) — 9 cases: 8 `400` with the task `running` and the lease kept (output schema, engine-only code (4 codes), unknown code, `cancelled` without a cancel request, broken `TaskError`, `error` without error, `success`/`warning` with an error); a zombie with an invalid result → `409` `LEASE_LOST` (G-EXE-17).

#### worker/release/ — `POST /tasks/{id}/release`
- [positive-1.1 — Releasing a task gives it back without spending any budget, with an optional delay or checkpoint](worker/release/1.01-positive-release-returns-task-to-queue.md) — 9 cases: running (`queued`, slots freed, no budget/attempt/blacklist change), with delay (`scheduled`, `taskQueued` with `run_after`, G-EXE-24), zero and maximum delay, delay above the maximum or negative (`400` `OUT_OF_RANGE`, G-EXE-25), with checkpoint (merged in the same step), claimed before start, armed wait group cancelled.
- [guardrail-1.2 — The SDK gives back a lease whose handler ended without finishing the task](worker/release/1.02-guardrail-sdk-releases-dropped-lease.md) — handler panics → the heartbeat loop sends `release` (resent on `503`, stopped on `LEASE_LOST` or `CLAIM_REVOKED`); task `queued` without waiting for a timeout (G-EXE-16).

#### entries/ — `POST /tasks/{id}/entries`, `GET /tasks/{id}/entries[/{entry_id}]`
- [positive-1.1 — The lease holder or an API consumer records an immutable entry on a task](entries/1.01-positive-create-entry.md) — 5 cases: worker with lease, API consumer without lease (`create` action), gear-defined entry type validated against its schema, consumer entry on a `queued` task and on a finished task (any state, G-EXE-21); `201`, task row untouched, no lifecycle event.
- [positive-1.2 — Entries are listed by time and filtered by type, time range and creator](entries/1.02-positive-list-entries-filtered.md) — filter by type, creator and time range → `200` `JsonPage` with the two matching entries in `created_at` order (G-EXE-18).
- [positive-1.3 — One entry is read by its ID](entries/1.03-positive-get-entry-by-id.md) — `200` the entry; the same entry ID under another task or tenant → `404`.
- [negative-1.4 — An entry whose type is unknown, whose payload fails its schema or is over 64 KB, or that would exceed the per-task entry limit is refused and nothing is stored](entries/1.04-negative-entry-create-refused.md) — 5 cases: unknown entry type → `400` `UNKNOWN_ENTRY_TYPE`; registry unreachable → `503`; payload fails its schema → `400` with three violations; over 64 KB → `413`; 1000 entries → `400` `failed_precondition` `TOO_MANY_ENTRIES` (G-EXE-21); nothing stored, lease kept.
- [negative-1.5 — A subject that may only read the task cannot add an entry](entries/1.05-negative-reader-cannot-create-entry.md) — 3 cases: `read`-only subject → `403` `ACTION_DENIED`; another tenant → `404`; PDP unreachable → `503`.
- [guardrail-1.6 — Entries have no update or delete route](entries/1.06-guardrail-entries-cannot-be-changed-or-deleted.md) — `PATCH`/`PUT`/`DELETE` on an entry, even by an admin → `405` with `Allow: GET`, no problem body (G-EXE-22).

### Limits (13)

#### limits/concurrency/ — `POST /tasks/claim`, `POST /tasks/{id}/complete`, `/release`, `/wait-conditions`
- [positive-1.1 — A full concurrency dimension holds back only the tasks it covers; the claim serves other tenants, queues and types](limits/concurrency/1.01-positive-full-dimension-holds-back-only-its-tasks.md) — 3 cases: `queue_tenant` full → B's newer tasks claimed; `queue_global` full → the claim serves its other queue; `type_global` full (p2) → another type is claimed, then 429 `concurrency`; no write to skipped tasks.
- [positive-1.2 — A batch claim takes only as many tasks as the limits allow, and that is a success](limits/concurrency/1.02-positive-batch-takes-only-what-fits.md) — 2 slots left, `max_tasks: 5` → 200 with 2 claims, then 429 `concurrency`, `Retry-After: 1`.
- [positive-1.3 — A concurrency slot is freed on every exit from `claimed`/`running`: release, park and completion](limits/concurrency/1.03-positive-slot-freed-on-every-exit.md) — 3 cases: release without delay and park → the next claim is 200; a completion wakes a parked long-poll claim well before its timeout, never 429 (G-CLM-23).
- [positive-1.4 — Config overrides set concurrency limits by GTS pattern; the most specific pattern wins](limits/concurrency/1.04-positive-override-most-specific-pattern-wins.md) — exact-ID override (1) beats wildcard (2), which beats the registration (5): 429, then 200 with one claim, then 429 (p2).
- [negative-1.5 — Nothing assignable because of a full limit → `429` (quota `concurrency`) with `Retry-After: 1`, at the end of the long-poll](limits/concurrency/1.05-negative-nothing-assignable-returns-429.md) — 3 cases: `timeout_secs: 0` → 429 `concurrency` at once; `timeout_secs: 5` → 429 only at the timeout; several queues → no `resource_name` (G-CLM-10); `Retry-After: 1`, no usage figures (G-CLM-09).
- [guardrail-1.6 — A tenant worker never learns that another tenant is at its limit: `204`, not `429`](limits/concurrency/1.06-guardrail-other-tenants-full-limit-gives-204.md) — B's worker with nothing to do while A is at its limit → 204, not 429.
- [guardrail-1.7 — The global concurrency limit is never exceeded by concurrent claims](limits/concurrency/1.07-guardrail-global-limit-never-exceeded-under-concurrency.md) — six concurrent claims on a queue limited to 3 → at most 3 claimed, the rest 429 `concurrency`; no deadlock surfaces.

#### limits/depth/ — `POST /tasks`, `POST /tasks/{id}/release`, `/wait-conditions`, `/complete`
- [positive-1.1 — A depth limit of `0` means unlimited](limits/depth/1.01-positive-zero-means-unlimited.md) — limits set to `0` → create accepted above the defaults (201); `0` never stops a queue (G-CLM-13).
- [negative-1.2 — Create refused when a sampled queue depth is full: `429` naming the dimension `queue_depth.tenant`, `queue_depth.queue` or `queue_depth.domain_global`](limits/depth/1.02-negative-queue-depth-full-by-dimension.md) — 3 cases: `tenant`, `queue` (a tenant under its own limit), `domain_global` (even into an empty queue) → 429 against the sampled count; `Retry-After` in [15, 30], no figures (G-CLM-09, G-CLM-13).
- [negative-1.3 — Every admission into a deferred state is refused when the tenant's deferred limit is full: dimension `deferred`](limits/depth/1.03-negative-deferred-limit-refuses-every-deferral.md) — 5 cases, all 429 `queue_depth.deferred`: create with `run_after`, with a predecessor (p2), with an initial wait (G-CLM-26); release with `delay_secs` and park, where the worker keeps lease and slot (G-EXE-23). Home of deferred-limit refusals.
- [guardrail-1.4 — Engine moves and re-entries are never refused by depth or deferred limits](limits/depth/1.04-guardrail-engine-moves-never-refused.md) — 6 cases: retry and liveness requeue with deferred full; release without delay, `run_after` reached, wait resume and unblock (p2) with tenant depth full; never 429.
- [guardrail-1.5 — One tenant at its depth limit never affects another tenant](limits/depth/1.05-guardrail-one-tenant-full-does-not-affect-another.md) — A at its limit; B's create → 201.
- [flow-1.6 — Per-tenant depth may be passed by at most one sampling interval of the tenant's own creates](limits/depth/1.06-flow-tenant-depth-overshoot-bounded-by-one-sample.md) — a burst inside one sample interval may pass the tenant limit (each 201 or 429); after the next sample → 429 `queue_depth.tenant` (G-CLM-13).

### Failure, timeouts and cancellation (35)

#### retry/ — `POST /tasks/{id}/complete`, session and deadline scans, `POST /tasks` (`retry_policy`)
- [positive-1.1 — A retryable error with budget left schedules a retry after the base backoff](retry/1.01-positive-retryable-error-schedules-retry.md) — `200`, `state: scheduled` (never `failed`), `retry_count` 1, `attempt` unchanged, `run_after` in [base×0.8, base], `taskRetrying` `cause: reported` with the full error (G-RTY-03, G-RTY-04).
- [positive-1.2 — Retry backoff follows the strategy, stops at max_interval, is only shortened by jitter, never drops below 1 s, and never overflows](retry/1.02-positive-backoff-by-strategy-cap-and-jitter.md) — 7 cases: exponential 5/10/20/30 s (cap), linear 5/10/12 s (cap), fixed 7 s, full jitter in [1 s, d] never above `max_interval`, jitter 0.2 window, 1 s floor, n = 71 saturates to exactly `max_interval` (G-RTY-04, G-RTY-06).
- [positive-1.3 — A reported error that cannot be retried ends the task, dead-lettered with a DLQ and failed without one](retry/1.03-positive-error-without-retry-ends-failed-or-dead-lettered.md) — 4 cases: non-retryable code (budget left) → `dead_lettered`; `retryable: false` on **bulk** → `failed`; retries exhausted → `dead_lettered` with DLQ, `failed` without; result `error` in every case, no `taskRetrying`.
- [positive-1.4 — Running out of liveness requeues ends the task timed_out](retry/1.04-positive-liveness-budget-exhausted-timed-out.md) — third expiry with `max_liveness_requeues: 2` → `dead_lettered` / `timed_out` with retry budget left; `taskTimedOut` `heartbeat` (G-RTY-02).
- [positive-1.5 — max_liveness_requeues 0 requeues without limit until the lifetime timeout ends the task](retry/1.05-positive-unlimited-liveness-requeues-end-at-lifetime.md) — platform **cleanup** with `max_liveness_requeues: 0` survives 25 expiries; lifetime timer ends it `dead_lettered` / `timed_out` (G-RTY-06).
- [positive-1.6 — A retry policy on the request wins over the type and the queue, and is taken whole](retry/1.06-positive-task-retry-policy-taken-whole.md) — request policy beats type and queue; queue's `non_retryable_codes` not inherited, so `INVALID_REPORT` is retried; omitted fields take schema defaults (G-CRT-04).
- [negative-1.7 — Unlimited liveness requeues are refused without a lifetime timeout, and always for tenant-owned types](retry/1.07-negative-unlimited-liveness-requeues-without-lifetime.md) — 5 cases: platform type without lifetime → `400` `TRUST_LIMIT` on `/retry_policy/max_liveness_requeues`; with lifetime → `201`; tenant-owned explicit `0` → `400` with or without lifetime; inherited `0` clamped to 20 (G-RTY-06).
- [guardrail-1.8 — Repeating a retryable failure report gets LEASE_LOST and spends no second retry](retry/1.08-guardrail-repeated-retryable-report-is-lease-lost.md) — resending the same failure after a lost response → `409` `LEASE_LOST`; `retry_count` stays 1; the SDK treats it as applied (G-RTY-05, G-EXE-16).
- [flow-1.9 — A scheduled retry becomes queued at run_after, and the next claim starts attempt 2](retry/1.09-flow-retry-becomes-queued-then-new-attempt.md) — scheduled timer → `queued` + new queue timeout, no history event (G-SEC-23); next claim `attempt: 2`, `lease_epoch: 2`.
- [flow-1.10 — Lost workers never use the retry budget: four lease expiries, then still three retries](retry/1.10-flow-liveness-requeues-keep-retry-budget.md) — four lease expiries, then still three retries (first one waits at most `base`, own count), then dead-lettered.

#### timeouts/ — timer wheel, deadline and session scans (no request), `GET /tasks/{id}`, `POST /tasks/heartbeat`
- [positive-1.1 — A queue or lifetime timeout ends the task timed_out, dead-lettered with a DLQ and failed without one](timeouts/1.01-positive-queue-or-lifetime-timeout-ends-task-timed-out.md) — 5 cases: lifetime while `scheduled` with retries left → `dead_lettered`; queue timeout on **bulk** → `failed`, `attempt: 0`; queue timeout with DLQ → `dead_lettered`; lifetime and queue timeout on a held queue → `dead_lettered`, held time counts (G-OPS-07, G-RTY-08).
- [positive-1.2 — A claimed task not started within the assignment timeout is requeued as a liveness failure](timeouts/1.02-positive-assignment-timeout-requeues-with-backoff.md) — not started in 60 s despite heartbeats → `scheduled`, `liveness_requeues` 1, backoff, `taskRetrying` `timeout_type: assignment`; old `start` → `409` `LEASE_LOST` (G-RTY-02).
- [positive-1.3 — The execution timeout ends a running task even while its worker heartbeats, also at the engine default](timeouts/1.03-positive-execution-timeout-is-terminal.md) — 2 cases: 900 s from the claim while heartbeating → heartbeat `lease_lost`, `dead_lettered` / `timed_out`, never retried, no strike; engine default 3600 s on **adhoc** → `failed` / `timed_out` (G-EXE-10).
- [positive-1.4 — Time spent waiting does not count toward the execution timeout](timeouts/1.04-positive-waiting-time-not-counted-toward-execution.md) — after a 20-minute wait the new claim gets a fresh 900 s (per claim, G-RTY-22); heartbeat `ok` at `T0+2000s`.
- [positive-1.5 — Passing the SLA deadline publishes slaBreached and the task keeps running](timeouts/1.05-positive-sla-breach-task-keeps-running.md) — `slaBreached` at `T0+600s`; heartbeat `ok`, `running`, lease and execution deadline unchanged.
- [guardrail-1.6 — When a deadline and a session expiry both find the same task, the task is handled once](timeouts/1.06-guardrail-concurrent-deadline-detectors-act-once.md) — 2 cases: assignment deadline + session expiry → one requeue, `liveness_requeues` 1, one strike; lifetime + session expiry → `timed_out` / `lifetime` in either order, no requeue, no strike (G-RTY-07).
- [guardrail-1.7 — Deadlines fire no earlier than due and within one second after](timeouts/1.07-guardrail-timers-fire-within-one-second.md) — never before due, `terminal_at` within 1 s after; 99 % at 100,000 active tasks, no timer-lag metric (G-RTY-09).
- [flow-1.8 — Lifetime and SLA deadlines run from creation across a retry, a requeue and a wait](timeouts/1.08-flow-deadlines-survive-retries-requeues-and-waits.md) — same lifetime/SLA timers across retry, session expiry and park; SLA breached while waiting; lifetime ends it `dead_lettered` / `timed_out`.

#### dead-letter/ — `GET /tasks/{id}`, `GET /tasks?$filter=state eq 'dead_lettered'`, `POST /admin/tasks/{id}/reenqueue`, removed `GET /admin/dlq`
- [positive-1.1 — A dead-lettered task is parked in the dead-letter queue and records where it came from](dead-letter/1.01-positive-dead-lettered-task-parked-in-dlq.md) — `200`, `state: dead_lettered`, `queue` **dead_letter**, `dead_lettered_from` **reports**, result `error`; `taskDeadLettered` names `queue` (G-SEC-11).
- [positive-1.2 — Dead-lettered tasks are listed with a search on state dead_lettered, without a time window](dead-letter/1.02-positive-list-dead-lettered-by-search.md) — 5 cases: state filter → page across domains with no `created_at` window, a 40-day-old task included; state + DLQ queue filter searches every domain; `dead_lettered_from` filter keeps the exemption; `read` alone lists its scope, no `403` (G-RTY-10, G-RTY-11).
- [positive-1.3 — Re-enqueue puts a dead-lettered or failed task back in its queue with fresh budgets](dead-letter/1.03-positive-reenqueue-dead-lettered-or-failed-task.md) — 3 cases: dead-lettered → `200` back on **reports**, `queued`, budgets 0, `attempt` kept, result cleared, context and future SLA kept, lifetime restarted, `taskQueued` `reenqueued`; failed on **bulk** → stays on **bulk**, past `sla_deadline` dropped; body `queue` → **default**, settings re-resolved (G-RTY-12, G-RTY-20).
- [negative-1.4 — Re-enqueue is refused for the wrong state, without permission, and when admission fails, and the task stays as it was](dead-letter/1.04-negative-reenqueue-refusals.md) — 14 cases: `completed`, `cancelled`, unfinished → `400` `STATE`; no `admin` → `403`; unreadable → `404`; dedup identity held → `409` naming holder; newer supersede head → `400` `STALE_ORDER`; tenant depth full → `429` `queue_depth.tenant`; queue in another domain → `400` `TASK_DOMAIN_MISMATCH` (p3); unknown `queue` → `404`; DLQ as `queue` → `400` `DEAD_LETTER_QUEUE`; parent ended → `400` `PARENT_FINISHED`; lost race → `409` `STATE_CHANGED`.
- [guardrail-1.5 — The removed GET /admin/dlq endpoint is not served](dead-letter/1.05-guardrail-admin-dlq-endpoint-removed.md) — `404`, no task data.
- [guardrail-1.6 — Re-enqueue does not revive successors already cancelled with dependency_failed](dead-letter/1.06-guardrail-reenqueue-does-not-revive-successors.md) — successor stays `cancelled` / `dependency_failed` after re-enqueue and later success.

#### tasks/cancel/ — `POST /tasks/{id}/cancel`, `POST /tasks/heartbeat`, `POST /tasks/{id}/complete`
- [positive-1.1 — A cancel of a task that holds no lease ends it at once](tasks/cancel/1.01-positive-cancel-of-unleased-task-ends-at-once.md) — 5 cases: `queued` requested → `200` `cancelled`; `queued` forced → `forcibly_cancelled`; `scheduled` graceful, `blocked`, `waiting` → `cancelled` at once, timers / wait conditions removed, no grace timer; only `taskCancelled` (G-RTY-15).
- [positive-1.2 — A cancel of a leased task flags it for requested and graceful, and ends it at once for forced](tasks/cancel/1.02-positive-cancel-of-leased-task-by-mode.md) — 3 cases: requested → `202` with `cancel_request`, flag only, lease valid; graceful → `202` + `cancel_grace` at +60 s (G-RTY-19); forced → `200` `forcibly_cancelled`, old holder's writes `409` `LEASE_LOST`.
- [positive-1.3 — A graceful cancel after a requested one arms the grace timer once](tasks/cancel/1.03-positive-escalate-requested-cancel-to-graceful.md) — second cancel `202`, arms `cancel_grace` once; a repeat `graceful` or `requested` is a no-op `202`, no event, deadline kept (G-RTY-16).
- [positive-1.4 — A worker that finishes its work after a cancel request may complete it successfully](tasks/cancel/1.04-positive-worker-finishes-normally-after-cancel-request.md) — 4 cases: `success` / `warning` → `completed`, no `taskCancelled`; any `error` → `cancelled` with the error recorded, no retry, no DLQ (G-RTY-17).
- [negative-1.5 — A cancel of an ended task is refused, unless the task already ended cancelled](tasks/cancel/1.05-negative-cancel-of-ended-task-refused-unless-cancelled.md) — 7 cases: `completed`, `failed`, `dead_lettered` → `400` `TASK_FINISHED`; already `cancelled` (any cancel result code) → `200` unchanged, no event (G-RTY-13).
- [negative-1.6 — A cancel without the cancel action gets 403, and of an unreadable task the same 404 as a missing task](tasks/cancel/1.06-negative-cancel-without-permission.md) — 2 cases: read-only caller → `403` `ACTION_DENIED`; other tenant → `404`, same as a missing task.
- [negative-1.7 — A cancel with an unknown mode is rejected](tasks/cancel/1.07-negative-unknown-cancel-mode.md) — `mode: immediate` → `400` `INVALID_ENUM_VALUE` on `/mode`; an omitted mode means `requested` (G-RTY-18).
- [negative-1.8 — A worker may not report cancelled when no cancel was requested](tasks/cancel/1.08-negative-worker-reports-cancelled-without-request.md) — `400` `NOT_ALLOWED` on `/result_code`; task stays `running`, lease valid.
- [guardrail-1.9 — A task created cancellable false rejects requested and graceful cancels, and only forced stops it](tasks/cancel/1.09-guardrail-cancellable-false-allows-only-forced.md) — 3 cases: requested and graceful → `400` `NOT_CANCELLABLE`; forced → `200` `forcibly_cancelled` (G-RTY-14).
- [flow-1.10 — The worker sees the cancel request on its heartbeat and ends the task cancelled](tasks/cancel/1.10-flow-worker-acknowledges-cancel-request.md) — heartbeat shows `cancel_requested: true` with `cancel_request`; worker completes `cancelled` → `200` `cancelled`, replayable (G-OPS-38).
- [flow-1.11 — A graceful cancel the worker does not finish in time becomes forcibly_cancelled](tasks/cancel/1.11-flow-graceful-cancel-forced-after-grace-period.md) — `202`, `cancel_grace` at +60 s; then `forcibly_cancelled`, heartbeat `lease_lost` (G-RTY-19).

### Waits (41)

#### waits/framework/ — `POST /tasks/{id}/wait-conditions`, `POST /tasks/{id}/wait-conditions/park`, `GET /tasks/{id}/wait-conditions`, `POST /tasks/claim` (resume)
- [positive-1.1 — A worker sets a wait and the task is parked](waits/framework/1.01-positive-set-wait-parks-task.md) — 3 cases: plain, with checkpoint, from `claimed` (G-WAI-20). `200`, `task_state: waiting`; lease, session, assignee and deadlines cleared, slots freed, joins the deferred set, `waitConditionSet` + `taskWaiting` (G-WAI-12); a checkpoint is saved with the park, records `taskCheckpointed` and is seen by the resuming claim (G-WAI-15).
- [positive-1.2 — A resumed task comes back through a new claim, in the same attempt, with the wait result](waits/framework/1.02-positive-resume-claims-new-lease-same-attempt.md) — 2 cases: timer, input. New token, `lease_epoch` 2, `attempt` still 1, `resumed` carries the satisfied group (the resolved value for an input, G-WAI-08); the claim sets a fresh execution deadline; the group stays current, so a repeated claim returns the same `resumed` (G-WAI-14).
- [positive-1.3 — Park returns the result at once when the armed group is already satisfied](waits/framework/1.03-positive-park-returns-result-when-already-satisfied.md) — 2 cases: armed event, armed timer (checked at park, G-WAI-17). `200`, `parked: false` + result; same lease continues; group consumed but kept and listed `satisfied` (G-WAI-13, G-WAI-14).
- [positive-1.4 — A task created with an initial wait group starts waiting and resumes like any parked task](waits/framework/1.04-positive-initial-wait-group-resumes-created-task.md) — created `waiting` with a timer (a deferred admission, G-CRT-01); at `resume_at` → `queued`; first claim `attempt` 1, `lease_epoch` 1, `resumed` set (G-WAI-19).
- [positive-1.5 — Cancelling a waiting task ends it at once](waits/framework/1.05-positive-cancel-waiting-task-ends-at-once.md) — `requested` cancel → `200 cancelled`; leaves the deferred set; condition `cancelled` and kept until retention (G-WAI-14); timers removed; no `taskCancelRequested` (G-RTY-15).
- [guardrail-1.6 — The lease token of a parked task is dead](waits/framework/1.06-guardrail-old-token-after-park-gets-lease-lost.md) — late `complete` with the old token → `409 LEASE_LOST`; heartbeat reports `lease_lost`; task stays `waiting`.
- [negative-1.7 — Set-wait refusals leave the task and the lease untouched](waits/framework/1.07-negative-set-wait-refusals.md) — 7 cases: armed group pending (`400 failed_precondition`, `STATE`), empty group (`REQUIRED`), 17 conditions (`TOO_MANY`, limit 16) (G-WAI-06), timeout on a timer (G-WAI-09), `on_timeout` without timeout (G-WAI-32), no `lease_token` (`REQUIRED`, G-WAI-01), checkpoint over the payload limit (`413`); nothing stored, lease kept.
- [negative-1.8 — Park with no armed group is refused](waits/framework/1.08-negative-park-without-armed-group.md) — `400 failed_precondition`, `NO_ARMED_WAIT` (G-WAI-13); lease kept.
- [guardrail-1.9 — An armed group is dropped when the run ends without parking](waits/framework/1.09-guardrail-armed-group-dropped-when-run-ends.md) — 2 cases: release (`queued`, `resumed: null` on the next claim), complete (`completed`); either way the condition is `cancelled` and kept until retention (G-WAI-14), places freed, a later event matches nothing.
- [flow-1.10 — Flow: how an `ALL` or `ANY` group is decided by satisfactions and timeouts](waits/framework/1.10-flow-group-decision-outcomes.md) — 5 cases: `ALL` first timeout `fail` decides at once (timer cancelled); single input timeout `fail` (later resolve `400 failed_precondition`); `ANY` first satisfied cancels siblings; `ANY` timeouts drop out and one `fail` fails the task; `ANY` all `resume` timeouts resume with `outcome: timed_out` and a note per condition (G-WAI-16, G-WAI-35).
- [flow-1.11 — Flow: arm a wait, start the action, then park](waits/framework/1.11-flow-arm-then-park.md) — arm: task stays `running`, lease kept, places reserved, no deferred admission; park: `waiting`, timeouts start at park, checkpoint saved, no second `waitConditionSet` (G-WAI-12, G-WAI-17).
- [guardrail-1.12 — In p1, input, children and event waits and `parent_task_id` are refused with a clear 400](waits/framework/1.12-guardrail-p2-wait-kinds-and-parent-refused-in-p1.md) — 9 cases: worker input/children/event condition, arm with input, create with an input wait, create with a parent → `400 FEATURE_NOT_AVAILABLE`; create with a children wait → `400 NOT_ALLOWED` (G-CRT-01); `wait_for_children` alone `201` (G-WAI-43); timer-only control `200`.

#### waits/timer/ — `POST /tasks/{id}/wait-conditions` (timer conditions), timer wheel, `GET /tasks/{id}`
- [positive-1.1 — A timer condition resumes the task at `resume_at`; a past `resume_at` is satisfied at once without parking](waits/timer/1.01-positive-timer-resumes-at-resume-at-past-returns-at-once.md) — 4 cases: future (`200 waiting`, `queued` within 1 s of `resume_at`, never before), past (`200 parked: false` with the result, lease kept), `any` past + future (resolves at once), `all` past + future (parks until the future one) (G-WAI-13).
- [guardrail-1.2 — The lifetime deadline still ends a task that is waiting](waits/timer/1.02-guardrail-lifetime-timeout-ends-waiting-task.md) — timer set past the lifetime deadline → task ends `dead_lettered` / `timed_out` at the lifetime deadline; condition `cancelled` and kept (G-WAI-14); the timer never fires.
- [negative-1.3 — A timer condition without `resume_at` is refused](waits/timer/1.03-negative-timer-without-resume-at.md) — `400 invalid_argument`, `REQUIRED` on `/conditions/0/resume_at`; lease kept.
- [guardrail-1.4 — A pending timer wait survives an engine restart](waits/timer/1.04-guardrail-timer-wait-survives-restart.md) — condition still `pending` and its `te_timers` row armed after a full restart (G-WAI-09); resumes on time, exactly once.

#### waits/input/ — `POST /wait-conditions/{id}/resolve`, `POST /tasks/{id}/wait-conditions` (input)
- [positive-1.1 — A valid answer to an input wait resumes the parked task](waits/input/1.01-positive-resolve-valid-value-resumes-task.md) — resolve with `resolve` → `200` condition `satisfied`, kept until retention (G-WAI-14); task `waiting → queued`; `waitConditionSatisfied` + `taskResumed`. The claim side is framework 1.2 case `input`.
- [positive-1.2 — Repeating a resolve with the same value succeeds](waits/input/1.02-positive-repeat-with-same-value-succeeds.md) — same subject, same value → `200` with the stored outcome and `Idempotency-Replayed: true`; no second resume or event (G-WAI-05).
- [negative-1.3 — A provided value that is invalid or too large is refused and the condition stays open](waits/input/1.03-negative-resolve-value-refusals.md) — 2 cases: schema mismatch (`400 invalid_argument`, `SCHEMA_VIOLATION` on `/value/approved`), value over the payload limit (`413` on `/value`, G-WAI-22); condition stays `pending`.
- [negative-1.4 — A resolve on a condition that is no longer open to an answer is refused](waits/input/1.04-negative-resolve-state-refusals.md) — 6 cases: different value (either subject) or same value from another subject → `400 failed_precondition`, `ALREADY_RESOLVED`; non-input condition → `NOT_RESOLVABLE`; after cancelled or timed out → `STATE` (G-WAI-05); nothing changes, stored value not echoed.
- [negative-1.5 — An untrusted input schema is refused at set time](waits/input/1.05-negative-input-schema-refusals.md) — 4 cases: too large (`SCHEMA_TOO_LARGE`), too deep (`TOO_DEEP`), remote `$ref` (`NOT_ALLOWED`, nothing fetched), look-ahead pattern (`NOT_ALLOWED`); each `400` on `/conditions/0/input_schema`, lease kept.
- [guardrail-1.6 — Resolve checks the condition's task: `403` for a reader without `resolve`, `404` for a caller who cannot read it](waits/input/1.06-guardrail-resolve-permission-checks.md) — 2 cases: reader without `resolve` → `403 ACTION_DENIED` naming the task (G-SEC-19); other tenant → `404` like a random condition ID, value not validated first. General rules in `auth/`.

#### waits/children/ — `POST /tasks/{id}/wait-conditions` (children), `…/park`, `POST /tasks/claim` (resume)
- [positive-1.1 — A children wait with no unfinished child is satisfied at once](waits/children/1.01-positive-no-unfinished-children-satisfied-at-park.md) — one-step set → `200`, `parked: false` with the result; task stays `running` under the same lease; group consumed but listed `satisfied` (G-WAI-13).
- [guardrail-1.2 — A parent still resumes on time when its last child ends while it is parking](waits/children/1.02-guardrail-resume-within-bound-when-check-is-lost.md) — the child's check misses the uncommitted park; the sweep resumes the parent within 1 s, exactly once.
- [guardrail-1.3 — Two last children ending at the same moment resume the parent once](waits/children/1.03-guardrail-concurrent-last-children-resume-parent-once.md) — two `children_check` timers; guarded update → one `taskResumed`, both children counted.
- [guardrail-1.4 — A child re-enqueued after its parent resumed counts as unfinished again](waits/children/1.04-guardrail-child-reenqueued-after-parent-resumed.md) — `wait_for_children` parent's success → `400 failed_precondition`, `CHILDREN_PENDING` (G-WIRE-14); lease kept; the earlier resume's counts unchanged (G-WAI-24).
- [flow-1.5 — Flow: a parent parked on its children resumes once, after the last child ends, with counts per terminal state](waits/children/1.05-flow-parent-resumes-after-last-child-with-counts.md) — four children end completed / failed / cancelled / dead_lettered; one `taskResumed` within 1 s; claim gets counts 1/1/1/1, epoch 2, attempt 1 (G-WAI-19).
- [flow-1.6 — Flow: an armed children condition is checked only when parked](waits/children/1.06-flow-armed-children-condition-evaluated-only-at-park.md) — armed with no children stays `pending`; a child is created and ends; park → `parked: false` with counts, same lease (G-WAI-17).

#### waits/event/ — `POST /tasks/{id}/wait-conditions` (event conditions), event consumer, `POST /tasks/claim` (resume)
- [positive-1.1 — A matching event resumes a parked task, and only the event header is kept](waits/event/1.01-positive-matching-event-resumes-task-with-header-only.md) — orders.paid for o-17 in tenant A satisfies the condition; `waiting → queued`; claim returns the 8-field header, no `data`, epoch 2, attempt 1; places freed; defines the event fixtures.
- [positive-1.2 — A match expression is checked after the field match, against the event data](waits/event/1.02-positive-match-expression-checked-after-field-match.md) — subject ID, subject type and consume filter first, then CEL; `amount 40` no match, `amount 120.5` match; header only stored (p3).
- [positive-1.3 — An event condition without a timeout gets the maximum, with on_timeout fail](waits/event/1.03-positive-no-timeout-gets-max-wait-with-fail.md) — 2 cases: no `on_timeout` (`timeout_at` = park + 7 d, `fail` → `dead_lettered` / `timed_out`), `on_timeout: resume` alone (accepted, resumes at 7 d) (G-WAI-32).
- [positive-1.4 — A condition frees its event-wait places when it times out or its task is cancelled](waits/event/1.04-positive-cancel-and-timeout-free-wait-places.md) — a timed-out condition and a cancelled waiting task each give back their `te_tenant_wait_counters` places; the cancelled condition is kept (G-WAI-14); new waits are admitted.
- [negative-1.5 — An event condition that can never be satisfied or checked is refused when it is set](waits/event/1.05-negative-event-condition-validation.md) — 8 cases: non-waitable topic, unknown or hidden event type (identical, G-WAI-29), subject type required (also a single wildcard, G-WAI-30), subject type not allowed, timeout above `max_wait_secs`, invalid match expression (p3); each `400 invalid_argument` before any PDP call, nothing stored, lease kept.
- [guardrail-1.6 — An event wait the caller's broker rights do not fully allow is refused and stores nothing](waits/event/1.06-guardrail-event-wait-authorization-refusals.md) — 4 cases: consume denied (`403 EVENT_CONSUME_DENIED`), unenforceable constraint (`403 EVENT_CONSUME_UNVERIFIABLE`), all constraints dropped by tenant (`403 EVENT_CONSUME_DENIED`, G-WAI-28), PDP unreachable (`503`, G-WAI-31); nothing stored, lease kept, `te_event_wait_authz_total` outcome counted.
- [guardrail-1.7 — Decidable consume constraints are stored and keep other events from matching](waits/event/1.07-guardrail-consume-constraints-filter-every-event.md) — `subject In [o-17, o-18]` stored as `event_consume_filter` (`accepted_filtered`); an o-99 event never matches, o-18 does.
- [guardrail-1.8 — Over a per-tenant event-wait limit, setting a wait is refused and the worker keeps its lease](waits/event/1.08-guardrail-event-wait-limits-keep-lease.md) — 5 cases: unkeyed full (`429 resource_exhausted`, `event_waits.unkeyed`), keyed request still admitted, per-type full, tenant total full, refused on park too; jittered `Retry-After` (G-WAI-33), no usage figures, nothing stored.
- [guardrail-1.9 — An event of another tenant never satisfies a tenant's wait](waits/event/1.09-guardrail-other-tenant-event-never-matches.md) — tenant B's o-17 event leaves tenant A's o-17 wait pending; tenant A's event resumes it.
- [guardrail-1.10 — A redelivered event never satisfies a condition twice, even across task domains](waits/event/1.10-guardrail-redelivered-event-satisfies-once-per-domain.md) — crash between domains, then redelivery: the per-domain `te_event_inbox` absorbs it; each task resumes exactly once.
- [guardrail-1.11 — A run-time error in the expression is no match and does not touch the failure counter](waits/event/1.11-guardrail-runtime-error-is-no-match-and-not-counted.md) — missing field or type mismatch → no match, nothing recorded; `eval_failures` neither raised nor reset (G-WAI-35) (p3).
- [guardrail-1.12 — Ten consecutive budget overruns resolve the condition with eval_budget_exceeded](waits/event/1.12-guardrail-eval-budget-exhaustion-resolves-condition.md) — 10 consecutive overruns → `waitTimedOut` with `eval_budget_exceeded`, `on_timeout: resume` → `queued`, claim shows `outcome: timed_out`; `fail` → `dead_lettered` (G-WAI-35) (p3).
- [guardrail-1.13 — Setting, arming or parking event waits never changes the broker subscription](waits/event/1.13-guardrail-setting-waits-never-changes-subscription.md) — 1,000 tenants arm and park; no join or rebalance; topology version unchanged.

### Parent/child and dependencies (19)

#### hierarchy/children/ — `POST /tasks` (`parent_task_id` + `parent_lease_token`), `POST /tasks/{id}/complete`, `POST /tasks/{id}/cancel`
- [positive-1.1 — The parent's worker creates a child with the parent's lease token](hierarchy/children/1.01-positive-worker-creates-child-with-parent-lease-token.md) — `201`, child `queued` with `parent_task_id`, `hierarchy_depth` parent + 1; the parent's lease untouched; the token is never echoed.
- [positive-1.2 — A child's worker can create grandchildren, up to `max_hierarchy_depth`](hierarchy/children/1.02-positive-nested-children-up-to-max-depth.md) — 3 cases: grandchild → `201`; parent at depth 10 → `400 invalid_argument TOO_DEEP` on `/parent_task_id`; unknown parent → `404`, checked before depth (G-WAI-39).
- [positive-1.3 — Cancelling a parent cancels its unfinished children, unless it opted out](hierarchy/children/1.03-positive-parent-cancel-cascade-to-children.md) — 5 cases: unleased children cancelled inline; `requested` leaves a leased child `running` with `cancel_requested` (G-WAI-23); `graceful` adds a `cancel_grace` timer, then `forcibly_cancelled`; `propagate_cancel: false` with `forced` keeps children; forced cancel of 250 children inline + cascade timer, each once across a crash. Home of the cancel cascade.
- [positive-1.4 — A parent that fails cancels its unfinished children gracefully](hierarchy/children/1.04-positive-parent-failure-cancels-children-gracefully.md) — 3 cases: dead-lettered parent → unleased child `cancelled`, leased child `cancel_requested` (`graceful`) with a `cancel_grace` timer; `propagate_cancel: false` keeps them; a successful parent cascades nothing (G-WAI-38).
- [guardrail-1.5 — A child the caller may not link is refused without revealing anything about the parent](hierarchy/children/1.05-guardrail-child-link-refusals.md) — 5 cases: stale token, no token (`400 REQUIRED` on `/parent_lease_token`), waiting parent, terminal parent, foreign parent; all but no-token get the uniform `404 not_found` with no field violation (G-WIRE-15), never `403` or `LEASE_LOST`.
- [guardrail-1.6 — Holding the parent's lease is not enough: the child also needs `create`](hierarchy/children/1.06-guardrail-child-needs-create-action.md) — the lease alone is not enough: `403 permission_denied`, lease kept.
- [negative-1.7 — A `wait_for_children` parent cannot complete successfully while a child is unfinished](hierarchy/children/1.07-negative-complete-with-children-pending.md) — success of a `wait_for_children` parent → `400 failed_precondition`, violation `STATE` / `CHILDREN_PENDING`; lease kept.
- [negative-1.8 — A child whose queue is in another task domain does not find its parent (`404`)](hierarchy/children/1.08-negative-child-in-another-domain-parent-not-found.md) — p3: `404 not_found`, same body as an unknown parent, even with the parent's current lease (G-WAI-41).
- [guardrail-1.9 — A child created while its parent is being cancelled is either refused or cancelled, never orphaned](hierarchy/children/1.09-guardrail-child-created-while-parent-ends-never-orphaned.md) — either `404`, or `201` and the parent's cancel ends the child in the same transaction in the parent's mode (G-WAI-23); never a live child of a terminal parent.

#### hierarchy/dependencies/ — `POST /tasks` (`predecessors`, `dependency_mode`), worker `complete`, `POST /tasks/{id}/cancel`, `POST /admin/tasks/{id}/reenqueue`, removed `POST`/`DELETE /tasks/{id}/dependencies`
- [positive-1.1 — A task created with predecessors starts `blocked`, `queued` or is refused, by what its predecessors already did](hierarchy/dependencies/1.01-positive-create-with-predecessors-initial-state.md) — 4 cases: live predecessors (`201 blocked`, links `pending`, `cancel` default); completed (`201 queued`, `succeeded`); failed + `cancel` → `400 failed_precondition DEPENDENCY_UNSATISFIABLE` (G-WAI-36); failed + `ignore` → `201 blocked`, link `failed_ignored` (G-WAI-37).
- [positive-1.2 — A predecessor's end unblocks, waits on, or cancels its `blocked` successor, by mode and failure rule](hierarchy/dependencies/1.02-positive-predecessor-outcome-moves-successor.md) — 6 cases: `ALL` last success → `queued`; success with future `run_after` → `scheduled`; `ANY` first success unblocks; `ANY` all failed → `dependency_failed`; `ALL` failure on `cancel` cancels and cascades down a chain; `ignore` failure counts as satisfied.
- [guardrail-1.3 — A predecessor the caller cannot read, in another tenant or in another domain, gets 404, never 403](hierarchy/dependencies/1.03-guardrail-unreadable-predecessor-404.md) — 4 cases: other tenant, hidden by policy, removed by retention, other domain; uniform `404` with no field violation (G-WIRE-15), never `403`, a state-revealing `400` or `TASK_DOMAIN_MISMATCH` (G-WAI-41).
- [negative-1.4 — A predecessor list over 64 entries, or naming a task twice, is rejected](hierarchy/dependencies/1.04-negative-predecessor-list-refusals.md) — 3 cases: 65 → `400 invalid_argument DEPENDENCY_GRAPH_TOO_LARGE`; exactly 64 → `201 blocked`; a task listed twice → `400 DUPLICATE` on the second entry (G-WAI-42).
- [guardrail-1.5 — The dependency endpoints no longer exist](hierarchy/dependencies/1.05-guardrail-dependency-endpoints-removed.md) — 2 cases: `POST /tasks/{id}/dependencies`, `DELETE /tasks/{id}/dependencies/{p}`; `404` (no route, G-WAI-03); nothing changes.
- [guardrail-1.6 — Creates alone can never build a dependency cycle](hierarchy/dependencies/1.06-guardrail-no-cycle-possible-at-create.md) — graphs built only by creates are acyclic: `201 blocked`, no graph lock, no `CYCLE_DETECTED` (G-WAI-03); the only bound is the predecessor count.
- [guardrail-1.7 — A successor created while its predecessor ends is never left `blocked`](hierarchy/dependencies/1.07-guardrail-predecessor-ending-concurrently-never-left-blocking.md) — success race ends `queued`; failure race ends `400 DEPENDENCY_UNSATISFIABLE` or `blocked` then `dependency_failed`; never stuck `blocked`.
- [guardrail-1.8 — A failed predecessor with 250 successors cancels each exactly once, in bounded steps, across a crash](hierarchy/dependencies/1.08-guardrail-large-fanout-cascade-cancels-each-once.md) — `200 failed` after 100 inline; cascade-timer batches with a cursor finish the rest across a crash; an already-terminal successor is skipped; each `dependency_failed` once.
- [guardrail-1.9 — Re-enqueueing a failed predecessor does not revive its cancelled successors](hierarchy/dependencies/1.09-guardrail-reenqueue-does-not-revive-successors.md) — `dependency_failed` successors stay terminal even after the re-enqueued predecessor succeeds.
- [flow-1.10 — Flow: fan out three parts, then join them with a task that tolerates failed parts](hierarchy/dependencies/1.10-flow-fan-out-then-join.md) — three parts plus a join task with `ignore` links: one part fails, the join is never cancelled and moves to `queued` when the last part ends, then reads the parts.

### Reads, search and events (22)

#### tasks/read/ — reading one task
- [positive-1.1 — Get a task by ID returns the full task resource and never a lease token](tasks/read/1.01-positive-get-task-by-id.md) — `200`, the full resource with `assignment` (execution deadline set by the claim, G-EXE-10); never a lease token or idempotency key (G-CRT-09).
- [positive-1.2 — A finished task outside the search window is still readable by ID](tasks/read/1.02-positive-finished-task-outside-search-window-readable-by-id.md) — 40-day-old completed task: `GET` and history `200`; default search misses it, a 30-day `created_at` range finds it.
- [negative-1.3 — A task removed by retention answers 404, like one that never existed](tasks/read/1.03-negative-retention-removed-task-is-not-found.md) — after the retention pass, task, events and entries → `404 not_found`, same as a never-used ID; no `410`.
- [guardrail-1.4 — GET /tasks/{id} embeds no list that can grow without limit](tasks/read/1.04-guardrail-get-task-embeds-no-unbounded-list.md) — `200` with 1,200 events, 300 entries, 500 children: no `events`, `entries`, `children` or `wait_conditions`; only capped `predecessors`, `resource_refs`, `tags` (G-CRT-06).

#### search/ — search, views, bounds and count
- [positive-1.1 — Search without a time range covers the last 7 days, newest first](search/1.01-positive-default-window-newest-first.md) — no range → `created_at ge now − 7d` (none for a `dead_lettered`-only filter, G-RTY-10), `created_at desc`, `short` view with `assignment` and `dead_lettered_from`, page size 25 (G-SEC-04, G-SEC-25).
- [positive-1.2 — Search filters by state, queue, type, tag, resource reference, urgency, priority, assignee and task links](search/1.02-positive-filter-by-field.md) — 9 cases: state/queue/type, `state in`, `has_tag` with and without a value, `has_resource_ref` (count 2), urgency+priority, assignee, `parent_task_id`, `predecessor_id`; caller's scope only (G-SEC-05).
- [positive-1.3 — An explicit created_at range of up to 31 days, or an unfinished-only state filter, replaces the default window](search/1.03-positive-explicit-range-and-unfinished-only-window.md) — 5 cases: 30-day range replaces the default window; lower bound only → up to now; unfinished-only `state` filter has no window, even with a range over 31 days; `ne 'completed'` keeps the window.
- [positive-1.4 — The view decides which fields each search item carries: tiny, short, long, full](search/1.04-positive-views.md) — 4 cases: `tiny` (id, state, type); `short` adds scheduling fields, `assignment`, `dead_lettered_from`; `long` adds payloads, tags and refs; `full` adds ≤50 events, ≤20 entries, the current wait group and dependencies, pages ≤20 items (G-SEC-04, G-SEC-06).
- [positive-1.5 — GET /tasks/count counts what the same search would return, capped at search_max_count](search/1.05-positive-count.md) — 4 cases: `{count: 12, capped: false}` within the search window and scope; above, at and one over `search_max_count` (10,000) → `capped` only above; denied `read` → `403` (G-SEC-20).
- [negative-1.6 — A search query the engine cannot serve within its bounds is rejected with 400, never silently narrowed](search/1.06-negative-unsupported-query.md) — 13 cases, all `400 invalid_argument`: `$orderby` key or `terminal_at` without a terminal-only filter (`NOT_ALLOWED`), `$orderby` with cursor (`ORDER_WITH_CURSOR`), window over 31 days (`OUT_OF_RANGE`), `$count`/`$skip`/`$expand`/`$search` (`UNSUPPORTED_QUERY_PARAM`), `$select` outside the view or unknown `view` (`NOT_ALLOWED`), `limit=0` (`INVALID_LIMIT`), cursor with another filter (`FILTER_MISMATCH`) or view/`$select` (`FIELD_SELECTION_MISMATCH`), malformed cursor (`INVALID_CURSOR`) (G-SEC-26).
- [negative-1.7 — The count endpoint rejects $orderby, $select, $top, $skiptoken and view](search/1.07-negative-count-rejects-paging-and-view.md) — `$orderby`, `$select`, `$top`/`limit`, `$skiptoken`/`cursor`, `view` on count → `400 invalid_argument` naming the field; a range over 31 days → `400` too.
- [guardrail-1.8 — Page size is clamped to 100, and to 20 at view=full](search/1.08-guardrail-page-size-clamped.md) — `limit=500` → 100 items (20 at `full`), `page_info.limit` says so; `$top` alias identical, both together → `400`; default 25 (G-SEC-25).
- [guardrail-1.9 — Cursor paging stays stable while tasks are being added](search/1.09-guardrail-cursor-paging-stable-while-tasks-added.md) — 30 tasks created between pages: page 2 is exactly the rest, no repeats, no skips, new tasks absent; a changed filter or view on the cursor → `400` (G-SEC-26).

#### events/ — event history and lifecycle publishing
- [positive-1.1 — The task history records every change, in order, with id, time, actor, type and payload](events/1.01-positive-history-complete-and-ordered.md) — `200`, oldest first by event ID, page size 25: `taskCreated` (with `initial_state`, no `taskQueued`) → claimed → started → checkpointed → completed, each with id, time, `actor`, type, payload (G-SEC-07, G-SEC-23).
- [positive-1.2 — The history is cursor-paged with a capped page size and can be filtered by event type](events/1.02-positive-history-cursor-paged.md) — 9 cases: first and next page (all 1,200 exactly once), `event_type eq 'taskRetrying'` → 3 with the full `error`, `event_type in`, `timestamp` range, `limit=5000` clamped to 100, default 25, filter changed on a cursor → `400 FILTER_MISMATCH`, `limit=0` → `400 INVALID_LIMIT` (G-RTY-03, G-SEC-25).
- [positive-1.3 — A lifecycle event is published with the history's ID, the owner tenant, and the engine's publishing subject](events/1.03-positive-lifecycle-event-published-under-publishing-subject.md) — 2 cases: produced by `{te_publisher}` with the history ID, owner tenant and concrete subject type (G-SEC-09, needs #5491); barrier tenant C's event is not enqueued for the broker until #5492, history complete (G-SEC-10).
- [positive-1.4 — The publishing policy decides which events reach the broker; the history always records everything](events/1.04-positive-publishing-preset-and-per-type-policy.md) — 5 cases: `lifecycle` preset publishes `taskCreated`/`taskCompleted`, not execution events; release `taskQueued` not published (G-EXE-24); liveness `taskRetrying` published (G-EXE-05); p2 `per_type` match publishes only its list, unmatched falls back; history always complete (G-SEC-12).
- [positive-1.5 — An event the engine causes itself carries no actor](events/1.05-positive-engine-caused-event-has-no-actor.md) — queue timeout → `failed` `timed_out`; history `taskTimedOut` (`timeout_type: queue`) with `actor: null`, published (G-WIRE-10).
- [guardrail-1.6 — Events never carry lease tokens, task payloads, or an error's message and context](events/1.06-guardrail-events-carry-no-sensitive-data.md) — 3 cases: published `taskFailed`/`taskRetrying` errors carry only domain, code, fault, retryable (the history `taskRetrying` keeps the full error, G-RTY-03); no token, input, context or output in any event or history item.
- [guardrail-1.7 — With the Event Broker down, task operations continue and events are delivered later, in order](events/1.07-guardrail-broker-down-tasks-continue.md) — create `201`, cancel `200 cancelled`, history `200` during an outage; the outbox drains in order with the history IDs afterwards; readiness stays green (G-RTY-15).
- [guardrail-1.8 — A publish the broker refuses for authorization changes no task state and raises an alert](events/1.08-guardrail-publish-refused-for-authz-pages-operators.md) — missing `produce` grant: tasks complete, history complete, nothing published; outbox dead-letters rise and page operators; readiness stays green.
- [guardrail-1.9 — A redelivered event keeps its event ID, so consumers can deduplicate](events/1.09-guardrail-at-least-once-redelivery-keeps-event-id.md) — relay crash → `taskCompleted` published twice with the same `id`; one history entry (G-SEC-09).

### Security (16)

#### auth/ — authentication, authorization, tenant isolation, worker principals, creator authority
- [positive-1.1 — A passed creator check is recorded and handed to the worker on claim](auth/1.01-positive-creator-check-verified-and-shown-on-claim.md) — allowed check → `201`; claim carries `authority.checked` with `{ source, resource_type, action, verdict }` (G-SEC-24).
- [positive-1.2 — References no creator check covers are stored and shown as verified: false](auth/1.02-positive-unchecked-refs-stored-unverified.md) — ref with no check is `verified: false` on the task, claim, `long` search and published `taskCreated` (G-SEC-04).
- [positive-1.3 — A platform worker reaches a barrier tenant's task only on a claim of platform-operated queues the PDP lets ignore barriers](auth/1.03-positive-platform-worker-crosses-barrier-only-on-platform-queues.md) — 4 cases: **system** with PDP-allowed `ignore` returns C's **cleanup** task; PDP refuses `ignore` → `204`; tenant queue → A's task only; mixed claim → `respect`, `204` (G-SEC-13).
- [negative-1.4 — A request without valid credentials is refused with 401 on every engine route](auth/1.04-negative-unauthenticated-request.md) — 7 cases: missing, expired (`TOKEN_EXPIRED`), invalid token (before idempotency, no key taken), worker/admin route sweep, `/metrics` and health paths → `401`, `404` when authenticated.
- [negative-1.5 — A caller who may read the target but not perform the action gets 403](auth/1.05-negative-read-but-not-act.md) — 3 cases, all `403` `ACTION_DENIED`: reader cancels, read-only-scoped token (G-SEC-17), `update` grant on resolve (`task_resolve.v1` registered next to `task_update.v1`).
- [negative-1.6 — A subject allowed to create only type A gets 403 for type B](auth/1.06-negative-type-policy-denies-other-type.md) — `create` granted for **render** only → `403` `ACTION_DENIED` for **export**, nothing stored, no key taken.
- [negative-1.7 — A creator check that is denied or cannot be decided refuses the create, and nothing is stored](auth/1.07-negative-creator-authority-refused.md) — 2 cases: denied → `403` `CREATOR_AUTHORITY`; `InGroup` constraint → `403` `CREATOR_AUTHORITY_UNVERIFIABLE`; check/ref index in `detail` only, no field violations, nothing stored (G-SEC-19, G-WIRE-15).
- [negative-1.8 — A tenant-owned type declaring authority.mode = worker cannot be created](auth/1.08-negative-tenant-owned-worker-authority-type.md) — tenant type with `authority.mode = worker` → `400` `INVALID_TYPE_POLICY` on `/type` naming `authority.mode` (G-CRT-15).
- [negative-1.9 — With the PDP unreachable, reads, creates and creator checks fail closed](auth/1.09-negative-pdp-unreachable-fails-closed.md) — 4 cases: read, create, creator check → `503`, `Retry-After: 5`, nothing returned or written (G-SEC-01, G-SEC-03); heartbeat still succeeds.
- [guardrail-1.10 — A task the caller may not read answers exactly like a missing task (no existence oracle)](auth/1.10-guardrail-no-existence-oracle.md) — 7 cases: foreign, never-used and policy-hidden task; history, entries, wait conditions; cancel → identical `404` with the base task type only; resolve is the one exemption (G-SEC-18).
- [guardrail-1.11 — Reads see only the tenants the PDP scope allows, with barriers respected outside platform-operated queues](auth/1.11-guardrail-tenant-scope-of-reads.md) — 6 cases: subtree grant shows A1 to A; tenant-only grant and A1 subject get `404`; PDP constraints are the only tenant scope (G-SEC-14); root admin never sees barrier tenant C on tenant queues; a **system** task is read with `Ignore` (G-SEC-15).
- [negative-1.12 — A claim naming a queue the worker's queue policy excludes is refused with 403](auth/1.12-negative-claim-names-queue-outside-policy.md) — 4 cases: a provably excluded queue, alone or among allowed ones → `403` `QUEUE_NOT_PERMITTED` naming **bulk**, no session; allowed queues only or a constraint without `queue_id` → `204`, never a **bulk** task (G-CLM-14, G-SEC-16).
- [guardrail-1.13 — A worker with only claim, read and create cannot cancel, resolve or administer](auth/1.13-guardrail-worker-least-privilege.md) — 5 cases: worker cancel, resolve, reassign, re-enqueue, bulk retry → `403` `ACTION_DENIED` (bulk refused whole, G-OPS-14); lease and task untouched.
- [guardrail-1.14 — A subject granted only resolve can answer an input wait and nothing else](auth/1.14-guardrail-resolve-grants-nothing-else.md) — resolve-only subject resolves (`200`) without `read`, but `GET` and `cancel` give `404` (G-SEC-18).
- [guardrail-1.15 — Queue read lets a subject inspect queues but see no task data](auth/1.15-guardrail-queue-read-grants-no-task-data.md) — queue-only viewer: `GET /queues/{id}` `200` without `counts` (G-OPS-02), search `403`, task `404`.
- [flow-1.16 — No API path lets one tenant see or change another tenant's tasks (three tenants)](auth/1.16-flow-tenant-isolation-three-tenants.md) — A/B/C probe each other on get, events, entries, waits, cancel, bulk, search, count, tags and claim: `404`, per-task `not_found` or an empty `200` everywhere (G-SEC-20), claims never cross tenants.

### Operations (41)

#### queues/ — `GET /queues`, `GET /queues/{id}`, `POST /admin/queues/{id}/hold`, `POST /admin/queues/{id}/release`, `POST /admin/config`, `GET /admin/config/{version}`
- [positive-1.1 — Queue introspection shows effective limits and settings, and per-state counts to callers holding queue `configure`](queues/1.01-positive-queue-introspection.md) — 3 cases: list and detail `200` with effective limits, settings, `config_version`, per-state counts, `sampled_at` and `hold: null`; queue `read` without `configure` gets the same items without counts (G-OPS-02, G-WIRE-11).
- [negative-1.2 — An unregistered queue ID returns `404` on every queue endpoint](queues/1.02-negative-unknown-queue-not-found.md) — 3 cases: detail, hold, release → `404 not_found` with the queue base type; no hold record; unknown is not "not held" (G-OPS-09).
- [negative-1.3 — Queue endpoints need the queue action, not task rights](queues/1.03-negative-queue-action-required.md) — 5 cases: hold, release, config and config-version without `configure`, list without queue `read` → `403` `ACTION_DENIED`; config is one check on the queue base type (G-OPS-40, G-SEC-19).
- [positive-2.1 — An admin holds a queue](queues/2.01-positive-hold-queue.md) — `200` queue resource with `hold {since, by, reason, until}`; hold stored per domain, other nodes within 5 s; `queueHeld` recorded, published and readable at `/queues/{id}/events`; no task changes (G-OPS-03, G-OPS-05).
- [positive-2.2 — A held queue stops claims and nothing else](queues/2.02-positive-held-queue-stops-claims-only.md) — 8 cases: claim skips held queue, claim on held only long-polls then `204`, create `201`, long hold `429` on tenant depth, leased task heartbeats and completes, timers make tasks `queued` but held, queue and lifetime timeouts still fire (G-OPS-07).
- [positive-2.3 — Releasing a hold, by call or at `until`, lets claims resume](queues/2.03-positive-release-resumes-claims.md) — 2 cases: manual release `200` `hold: null`, `queueReleased` with actor, parked claims woken; automatic release at `until`, one `queueReleased` without actor from the leader's sweep (G-OPS-06, G-OPS-08).
- [guardrail-2.4 — Hold and release edge cases: repeat hold, `until` in the past, release when not held, dead-letter queue](queues/2.04-guardrail-hold-and-release-edge-cases.md) — 5 cases: re-hold `200` keeps `since`, replaces `reason`/`until`; `until` in the past or now `400 OUT_OF_RANGE`; release when not held `200`, no event; DLQ hold `400 failed_precondition` `DEAD_LETTER_QUEUE` (G-OPS-08, G-OPS-09, G-OPS-10).
- [positive-3.1 — A runtime config save is a new complete version, recorded once per affected queue](queues/3.01-positive-runtime-config-save-is-complete-version.md) — 3 cases: exact ID `202 {config_version}`, one `configUpdated`; pattern, one per queue whose effective values change; omitted key reverts to the registration (G-OPS-11).
- [positive-3.2 — A config override beats the queue registration, which beats the config default](queues/3.02-positive-config-override-precedence.md) — 6 cases: exact ID > longest literal prefix > registration > default, runtime beats static for the same pattern (G-CLM-21, G-OPS-12).
- [positive-3.3 — The config version endpoint shows which domains have applied a version](queues/3.03-positive-config-version-apply-status.md) — `200` with per-domain `applied` flags, pending then applied (G-WIRE-12).
- [positive-3.4 — A runtime timeout or retry change applies to new tasks; existing tasks keep their values](queues/3.04-positive-timeout-change-applies-to-new-tasks-only.md) — old task keeps 900 s, new task gets 1800 s; queue shows the new setting and `config_version` (G-OPS-13).
- [negative-3.5 — A queue registered outside the trust limits refuses new tasks, naming the setting](queues/3.05-negative-queue-outside-trust-limits-refuses-tasks.md) — heartbeat 5 s < 10 s → create `400 invalid_argument` `TRUST_LIMIT` on `/queue`, description naming `heartbeat_timeout_secs` (G-CRT-07).

#### bulk/ — `POST /tasks/bulk/cancel`, `POST /tasks/bulk/retry`, `POST /tasks/bulk/reprioritize`, `POST /tasks/bulk/move`
- [positive-1.1 — Bulk cancel authorizes and applies each task on its own and reports one outcome per task](bulk/1.01-positive-bulk-cancel-per-task-outcomes.md) — 7 cases: `succeeded`, `unauthorized`, missing and foreign-tenant `not_found` alike, terminal `invalid_state`, running `graceful` → `cancel_requested`; no `cancel` grant at all → `403` for the whole request (G-OPS-14).
- [positive-1.2 — Bulk retry re-enqueues `dead_lettered` and `failed` tasks one by one and refuses the rest per task](bulk/1.02-positive-bulk-retry-per-task-outcomes.md) — 7 cases: DLQ and failed `succeeded`, terminal `invalid_state`, no `admin` at all `403`, per-task `unauthorized`/`not_found`, full depth `limit_exceeded` (never `429`), domain/parent/stale-order refusals `invalid_state` (G-OPS-17, G-RTY-20).
- [positive-1.3 — Bulk reprioritize changes the priority or urgency of waiting tasks, per task](bulk/1.03-positive-bulk-reprioritize.md) — 4 cases: priority on queued/scheduled `succeeded` with `taskUpdated`, terminal `invalid_state`, `critical` without `expedite` `unauthorized`, with `expedite` `succeeded` (G-OPS-19).
- [positive-1.4 — Bulk move takes unleased tasks to another queue of the same domain and refuses the rest per task](bulk/1.04-positive-bulk-move-per-task-outcomes.md) — 7 cases: queued/scheduled/blocked `succeeded`, running/claimed and waiting `invalid_state`, cross-domain `domain_mismatch`, full depth `limit_exceeded`; unknown destination `404` and DLQ destination `400` refuse the whole request (G-OPS-21, G-CRT-02).
- [negative-1.5 — A bulk request over the size limit is rejected as a whole](bulk/1.05-negative-bulk-too-large.md) — 1,001 IDs → `400 invalid_argument` `BATCH_TOO_LARGE` on `/task_ids`; nothing applied (G-OPS-15).

#### worker/reassignment/ — `POST /tasks/{id}/reassign`, `POST /tasks/{id}/accept`
- [positive-1.1 — An admin reassigns a running task to another worker identity](worker/reassignment/1.01-positive-admin-reassigns-running-task.md) — `200` task resource, no token, `lease_epoch` 2, state/attempt/slots unchanged; old token invalid, task detached with a one-lease hand-off window; `taskReassigned` (G-OPS-27).
- [positive-1.2 — Only the named worker identity can accept a reassigned task, and gets a new lease](worker/reassignment/1.02-positive-only-named-identity-accepts.md) — 3 cases: named identity `200` with a new token (epoch 2 kept, execution timer re-armed, `taskClaimed` `handoff: true`); other `worker_id` or other principal `404`, hand-off stays open (G-OPS-41).
- [positive-1.3 — If nobody accepts within one lease, the task is requeued with no blacklist strike](worker/reassignment/1.03-positive-accept-window-expires-requeue-no-strike.md) — 2 cases: budget left → `scheduled` after liveness backoff, `liveness_requeues` +1, slots freed, no strike; budget exhausted → `timed_out`, dead-lettered (G-OPS-26).
- [guardrail-1.4 — After reassignment the old holder's calls get `LEASE_LOST`](worker/reassignment/1.04-guardrail-old-holder-gets-lease-lost.md) — heartbeat `lease_lost`, complete `409 LEASE_LOST`; task stays `running` with no result.
- [guardrail-1.5 — Concurrency slots stay held through the hand-off and are freed only when the task leaves](worker/reassignment/1.05-guardrail-slots-held-through-hand-off.md) — tenant stays at its limit (`429` concurrency for others) through reassign and accept; slot freed on completion.
- [negative-1.6 — Reassignment is refused without task `admin` or when the task has no holder](worker/reassignment/1.06-negative-reassign-refused.md) — 4 cases: readable `403` `ACTION_DENIED`, unreadable `404`, queued task `400 failed_precondition` (`STATE`), blacklisted target `400` `TARGET_BLACKLISTED` (G-OPS-27).

#### worker/blacklist/ — blacklist effects on `POST /tasks/claim`, `POST /tasks/{id}/complete` and the liveness scans
- [positive-1.1 — The tenth consecutive worker failure blacklists the worker on that queue](worker/blacklist/1.01-positive-tenth-worker-failure-blacklists-worker.md) — 2 cases: reported `fault=worker`, assignment timeout; blacklisted 60 min, counter reset to 0, `workerBlacklisted` with count and `until` (G-OPS-31).
- [positive-1.2 — A blacklisted worker's claim skips that queue only, for that (subject, `worker_id`) only](worker/blacklist/1.02-positive-blacklisted-worker-claim-skips-queue.md) — 4 cases: `204` on the queue alone, `200` from other queues, sibling process `200`, other principal's blacklist on the same `worker_id` has no effect (G-OPS-30).
- [guardrail-1.3 — What counts toward the blacklist counter, and what does not](worker/blacklist/1.03-guardrail-blacklist-counting-rules.md) — 6 cases: task/dependency faults and execution timeout do not count, one outage = one strike per queue, success resets that queue only, release and unaccepted hand-off give no strike (G-EXE-05, G-OPS-26).
- [positive-1.4 — A queue's own blacklist threshold and duration apply, and claims resume after it](worker/blacklist/1.04-positive-per-queue-threshold-duration-and-expiry.md) — threshold 3 / 10 min on **bulk**: `204` during, `200` after the cool-down; counter reset at blacklisting, so one later failure does not blacklist again (G-OPS-31).

#### retention/ — background retention (observed via `GET /tasks/{id}`, `/entries`, `/events`)
- [positive-1.1 — A finished task, with its entries and history, is removed after its retention](retention/1.01-positive-finished-task-removed-after-retention.md) — `200` before the 100-day default, `404` (task, entries, events) after the slot is recycled; queue-level events in the slot go with it (G-OPS-44, G-OPS-45).
- [guardrail-1.2 — A task that is live when its slot is recycled is rescued, and kept for its full retention from `terminal_at`](retention/1.02-guardrail-rescued-task-kept-until-retention-from-terminal-at.md) — 2 cases: finished late → rescued, removed within one maintenance tick after `terminal_at` + 100 d; still unfinished → never removed (G-OPS-32).
- [positive-1.3 — Past the hard retention limit an unfinished task is force-cancelled first](retention/1.03-positive-hard-retention-limit-force-cancels.md) — 3 cases: 366-day-old waiting and queued tasks end `cancelled`/`forcibly_cancelled` at the pass, 364-day-old one a day later; then normal retention (G-OPS-33).
- [positive-1.4 — Retention precedence: tenant policy, then config type pattern, then type trait, then global default](retention/1.04-positive-retention-policy-precedence.md) — 7 cases: tenant 365 d beats type pattern 7 d, pattern alone, no subtree inheritance, policy change applies to unremoved rows, config 14 d beats trait 30 d, per-tenant 400 d `400 OUT_OF_RANGE`, global default (G-OPS-34).
- [guardrail-1.5 — One tenant's legal hold does not delay cleanup of other tenants](retention/1.05-guardrail-legal-hold-does-not-delay-others.md) — slot truncated on schedule; tenants A and B `404`, only tenant C's rows (365-day policy) rescued (G-OPS-34).
- [guardrail-1.6 — Cleanup runs separately in each task domain](retention/1.06-guardrail-cleanup-runs-per-domain.md) — a failing `system` pass does not stop removals in `default`; the expired `system` task goes at its next successful pass; metrics per domain (G-OPS-35).

#### domains/ — domain routing and queue moves on `POST /tasks`, `POST /tasks/claim`, `GET /tasks`, `GET /queues`, readiness
- [positive-1.1 — A search not narrowed to one domain merges all domains with a stable cursor](domains/1.01-positive-search-across-domains-stable-cursor.md) — pages interleave both domains in order; no repeat or skip while tasks are added.
- [positive-1.2 — One dead-letter queue serves queues in two domains, and DLQ search lists both](domains/1.02-positive-one-dead-letter-queue-serves-two-domains.md) — `state eq 'dead_lettered'` search `200` with both domains' tasks, newest `terminal_at` first, each with its own `dead_lettered_from`.
- [positive-1.3 — An event that matches waits in two domains is applied exactly once in each, also after redelivery](domains/1.03-positive-event-applied-once-per-domain.md) — event `202`; both waits satisfied, one inbox row and one `waitConditionSatisfied` per domain; redelivery writes nothing.
- [guardrail-1.4 — A task stays in the domain it was created in after its queue is re-routed](domains/1.04-guardrail-task-domain-fixed-at-creation.md) — finished task still readable by ID and in a cross-domain search; a queue search covers only the new domain set; re-enqueue refusal linked to `dead-letter/` (G-OPS-22).
- [negative-1.5 — A create that would cross a task-domain boundary is rejected (type rule `400`, cross-domain link `404`)](domains/1.05-negative-create-across-domain-boundary.md) — 4 cases: type rule with explicit queue and via `default_queue` of a late-registered type → `400 TASK_DOMAIN_MISMATCH` on `/queue`; cross-domain child and predecessor → `404 not_found` (G-OPS-24).
- [positive-1.6 — A routing change of a queue with live tasks starts a move instead of stopping the queue](domains/1.06-positive-routing-change-with-live-tasks-starts-a-move.md) — 3 cases: queue `domains` becomes `["default", "heavy"]`, instance ready, move ends when the source drains; no live tasks → one domain; source domain removed entirely → not ready (`503`) (G-OPS-25).
- [positive-1.7 — During a move, new work goes to the target, linked work stays with its source, and a claim names one domain](domains/1.07-positive-placement-and-claims-during-a-move.md) — 8 cases: plain create → target; successor of a source task → source; claim without `domain` `400 REQUIRED`; claim per domain `200`; `domain` not in the set `400 CROSS_DOMAIN`; worker calls follow the lease's domain; re-enqueue after the move `400 TASK_DOMAIN_MISMATCH` (G-OPS-42, G-RTY-20).
- [guardrail-1.8 — Dedup and supersession still hold across the two domains of a move](domains/1.08-guardrail-dedup-and-supersession-span-a-move.md) — 4 cases: derived dedup hit and caller-key replay in the source → `409`; supersession of a source task `201` in the source; stale revision after the source task ended → `400` `STALE_ORDER` (G-OPS-42).

### End-to-end flows (9)

#### flows/ — end-to-end journeys
- [flow-1.1 — Flow: create → claim → start → heartbeat → progress → complete](flows/1.01-flow-create-claim-execute-complete.md) — `201 queued` → claim `200` (execution deadline from the claim, G-EXE-10) → start, heartbeat, progress → `completed`; history of four events, `lifecycle` publishes created/completed.
- [flow-1.2 — Flow: retryable failures → backoff → retries exhausted → dead-lettered → re-enqueue → success](flows/1.02-flow-retries-dead-letter-reenqueue.md) — 5/10/20 s backoff (jitter only shortens, G-RTY-04), dead-lettered on the 4th failure with no `taskFailed`; re-enqueue resets budgets and clears result and `dead_lettered_from` (G-RTY-12); attempt 5 succeeds.
- [flow-1.3 — Flow: a worker process dies, its task is requeued and claimed elsewhere, and the zombie is fenced](flows/1.03-flow-worker-crash-zombie-fenced.md) — session expiry → `scheduled` with liveness backoff (`taskRetrying`, `cause: liveness`) → claim with epoch 2, attempt 2 → zombie's complete `409 LEASE_LOST` (G-EXE-09); the new holder's result is kept.
- [flow-1.4 — Flow: fan-out to children, park on a children wait, resume with counts, and a join successor that ignores failures](flows/1.04-flow-fan-out-children-and-join.md) — children `201`, join `blocked`; parent parks `waiting`, resumes with `{completed: 2, dead_lettered: 1}`, same attempt, epoch +1 (G-WAI-14); join runs under `ignore`.
- [flow-1.5 — Flow: an approval step through an input wait answered with `resolve`](flows/1.05-flow-approval-through-input-wait.md) — `resolve`-only approver answers `200`; a repeat replays `200`, a different value → `400 ALREADY_RESOLVED` (G-WAI-05); task resumes with the value; unanswered → `timed_out` (G-WAI-16).
- [flow-1.6 — Flow: arm an event wait, start the external action, the event arrives, park returns at once](flows/1.06-flow-arm-event-wait-then-park.md) — park answers `parked: false` with the event header (no `data`); same lease, never `waiting`.
- [flow-1.7 — Flow: incident — hold a queue, producers keep creating, running work finishes, release, claims resume](flows/1.07-flow-incident-hold-and-release.md) — hold `200` (`queueHeld` in queue history), creates `201`, claims `204` while held, running work finishes; release → backlog claimed in order; held time counts toward the queue timeout (G-OPS-07).
- [flow-1.8 — Flow: a worker shuts down gracefully, releases its tasks, and another worker picks them up](flows/1.08-flow-graceful-worker-shutdown.md) — releases `200 queued` keep budgets, attempt, checkpoint and blacklist count; other worker claims both; the empty session expires silently.
- [flow-1.9 — Flow: cancel a running task in `graceful` mode, end to end](flows/1.09-flow-graceful-cancel-running-task.md) — `202` → heartbeat carries `cancel_request` → worker completes `cancelled` (G-OPS-38); ignored request → `forcibly_cancelled` at 60 s, late complete `409 LEASE_LOST`.

## Test levels

Where each scenario should be tested. Each file states it in its `Test level` row (CONVENTIONS §2); this section collects those rows. The level is a recommendation for the test plan, not part of the behaviour.

- **E2E**: a pytest suite in `testing/e2e/suites/task_engine/`, driving a running `cf-gears-example-server` over HTTP (`docs/TESTING.md` §5). One node, wall clock only, static-authn tokens, and `static-authz-plugin`, which scopes by tenant and knows no per-action, per-type or per-queue rules. Timeouts and backoff are made short (1–2 s per-task `timeouts`/`retry_policy`, or a fast queue in the suite config), and tests poll for the outcome instead of asserting exact timing.
- **Integration**: Rust tests behind `--features integration` in the engine crate (`docs/TESTING.md` §4), with the engine in process on a real database. Use it for what needs a controllable clock (retention, multi-day windows, timer precision), two engine nodes on one database, races, PDP/registry/broker outages or crashes, PDP doubles with action-level policies, internal state (depth sampler, scan budget, counters), or the Rust SDK.
- **Both**: an E2E test of the visible path, plus integration tests for the rows that need timing, races or action-level authorization. The **Why** column says which part goes where.

Notes:

- Claim-order scenarios (`worker/ordering/`) are integration tests: on a shared E2E server other tests leave tasks in the same queues. The one E2E check (`1.01`) uses a queue of its own.
- Event-wait E2E tests need the event-broker gear in the suite; `domains/1.05` needs a two-domain E2E profile; `waits/timer/1.04` needs a suite that owns and restarts its server, like `event_broker/test_restart_durability.py`.
- `waits/framework/1.12` (p2 waits refused in p1) is removed when p2 ships.
- If the trust limits do not allow 1–2 s timeouts, the **Both** scenarios that rely on short timers become integration-only.

| Level | p1 | p2 | p3 | Total |
|---|---:|---:|---:|---:|
| E2E | 96 | 51 | 5 | 152 |
| Integration | 58 | 22 | 20 | 100 |
| Both | 30 | 10 | 1 | 41 |

| Folder | E2E | Integration | Both |
|---|---:|---:|---:|
| `auth/` | 4 | 11 | 1 |
| `bulk/` | 3 | 0 | 2 |
| `dead-letter/` | 5 | 0 | 1 |
| `domains/` | 1 | 7 | 0 |
| `entries/` | 5 | 1 | 0 |
| `events/` | 4 | 4 | 1 |
| `flows/` | 8 | 0 | 1 |
| `hierarchy/children/` | 2 | 3 | 4 |
| `hierarchy/dependencies/` | 7 | 2 | 1 |
| `limits/concurrency/` | 4 | 2 | 1 |
| `limits/depth/` | 2 | 2 | 2 |
| `queues/` | 5 | 5 | 2 |
| `retention/` | 0 | 6 | 0 |
| `retry/` | 5 | 4 | 1 |
| `search/` | 8 | 0 | 1 |
| `tasks/cancel/` | 8 | 0 | 3 |
| `tasks/create/` | 10 | 2 | 1 |
| `tasks/dedup/` | 5 | 2 | 0 |
| `tasks/idempotency/` | 5 | 1 | 1 |
| `tasks/read/` | 2 | 2 | 0 |
| `tasks/supersede/` | 6 | 4 | 1 |
| `timeouts/` | 0 | 4 | 4 |
| `waits/children/` | 4 | 2 | 0 |
| `waits/event/` | 5 | 8 | 0 |
| `waits/framework/` | 11 | 0 | 1 |
| `waits/input/` | 5 | 1 | 0 |
| `waits/timer/` | 3 | 0 | 1 |
| `worker/blacklist/` | 0 | 4 | 0 |
| `worker/claim/` | 5 | 3 | 3 |
| `worker/execution/` | 8 | 1 | 1 |
| `worker/fencing/` | 5 | 2 | 2 |
| `worker/ordering/` | 0 | 10 | 1 |
| `worker/reassignment/` | 3 | 2 | 1 |
| `worker/release/` | 1 | 1 | 0 |
| `worker/sessions/` | 3 | 4 | 3 |

<details><summary><code>auth/</code> (16)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A passed creator check is recorded and handed to the worker on claim](auth/1.01-positive-creator-check-verified-and-shown-on-claim.md) | p1 | **Integration** | creator check needs a PDP that returns per-type decisions |
| [1.02 — References no creator check covers are stored and shown as verified: false](auth/1.02-positive-unchecked-refs-stored-unverified.md) | p2 | **E2E** |  |
| [1.03 — A platform worker reaches a barrier tenant's task only on a claim of platform-operated queues the PDP lets ignore barriers](auth/1.03-positive-platform-worker-crosses-barrier-only-on-platform-queues.md) | p1 | **Integration** | barrier + platform-queue PDP policy |
| [1.04 — A request without valid credentials is refused with 401 on every engine route](auth/1.04-negative-unauthenticated-request.md) | p1 | **E2E** | static-authn: no or bad token → 401 on every route (smoke) |
| [1.05 — A caller who may read the target but not perform the action gets 403](auth/1.05-negative-read-but-not-act.md) | p1 (row `update grant does not cover resolve`: p2) | **Integration** | action-level 403: static-authz is tenant-only |
| [1.06 — A subject allowed to create only type A gets 403 for type B](auth/1.06-negative-type-policy-denies-other-type.md) | p1 | **Integration** | type policy |
| [1.07 — A creator check that is denied or cannot be decided refuses the create, and nothing is stored](auth/1.07-negative-creator-authority-refused.md) | p1 | **Integration** | denied / undecidable creator check |
| [1.08 — A tenant-owned type declaring authority.mode = worker cannot be created](auth/1.08-negative-tenant-owned-worker-authority-type.md) | p1 | **E2E** | register type via types-registry, create refused |
| [1.09 — With the PDP unreachable, reads, creates and creator checks fail closed](auth/1.09-negative-pdp-unreachable-fails-closed.md) | p1 | **Integration** | PDP outage injection |
| [1.10 — A task the caller may not read answers exactly like a missing task (no existence oracle)](auth/1.10-guardrail-no-existence-oracle.md) | p1 | **Both** | E2E: other-tenant task = missing task; integration: same-tenant unreadable rows |
| [1.11 — Reads see only the tenants the PDP scope allows, with barriers respected outside platform-operated queues](auth/1.11-guardrail-tenant-scope-of-reads.md) | p1 | **Integration** | PDP tenant constraints, barriers |
| [1.12 — A claim naming a queue the worker's queue policy excludes is refused with 403](auth/1.12-negative-claim-names-queue-outside-policy.md) | p1 | **Integration** | queue policy |
| [1.13 — A worker with only claim, read and create cannot cancel, resolve or administer](auth/1.13-guardrail-worker-least-privilege.md) | p1 (row `resolve an input wait`: p2) | **Integration** | action-level grants |
| [1.14 — A subject granted only resolve can answer an input wait and nothing else](auth/1.14-guardrail-resolve-grants-nothing-else.md) | p2 | **Integration** | resolve-only grant |
| [1.15 — Queue read lets a subject inspect queues but see no task data](auth/1.15-guardrail-queue-read-grants-no-task-data.md) | p1 | **Integration** | queue-read-only grant |
| [1.16 — No API path lets one tenant see or change another tenant's tasks (three tenants)](auth/1.16-flow-tenant-isolation-three-tenants.md) | p1 | **E2E** | three static-authn tenants (smoke) |

</details>

<details><summary><code>bulk/</code> (5)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — Bulk cancel authorizes and applies each task on its own and reports one outcome per task](bulk/1.01-positive-bulk-cancel-per-task-outcomes.md) | p2 | **Both** | E2E per-state outcomes; integration per-task authz rows |
| [1.02 — Bulk retry re-enqueues `dead_lettered` and `failed` tasks one by one and refuses the rest per task](bulk/1.02-positive-bulk-retry-per-task-outcomes.md) | p2 | **Both** | E2E for the state and depth rows (use non-retryable codes to reach dead_lettered fast); integration for the authorization rows (action-level PDP policies) |
| [1.03 — Bulk reprioritize changes the priority or urgency of waiting tasks, per task](bulk/1.03-positive-bulk-reprioritize.md) | p2 | **E2E** |  |
| [1.04 — Bulk move takes unleased tasks to another queue of the same domain and refuses the rest per task](bulk/1.04-positive-bulk-move-per-task-outcomes.md) | p2 (p3 for the cross-domain case) | **E2E** |  |
| [1.05 — A bulk request over the size limit is rejected as a whole](bulk/1.05-negative-bulk-too-large.md) | p2 | **E2E** |  |

</details>

<details><summary><code>dead-letter/</code> (6)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A dead-lettered task is parked in the dead-letter queue and records where it came from](dead-letter/1.01-positive-dead-lettered-task-parked-in-dlq.md) | p2 | **E2E** | non-retryable error → DLQ at once |
| [1.02 — Dead-lettered tasks are listed with a search on state dead_lettered, without a time window](dead-letter/1.02-positive-list-dead-lettered-by-search.md) | p2 (domains p3) | **E2E** |  |
| [1.03 — Re-enqueue puts a dead-lettered or failed task back in its queue with fresh budgets](dead-letter/1.03-positive-reenqueue-dead-lettered-or-failed-task.md) | p2 | **E2E** |  |
| [1.04 — Re-enqueue is refused for the wrong state, without permission, and when admission fails, and the task stays as it was](dead-letter/1.04-negative-reenqueue-refusals.md) | p2 (domain case p3) | **Both** | E2E state rows; integration permission + admission rows |
| [1.05 — The removed GET /admin/dlq endpoint is not served](dead-letter/1.05-guardrail-admin-dlq-endpoint-removed.md) | p2 | **E2E** |  |
| [1.06 — Re-enqueue does not revive successors already cancelled with dependency_failed](dead-letter/1.06-guardrail-reenqueue-does-not-revive-successors.md) | p2 | **E2E** |  |

</details>

<details><summary><code>domains/</code> (8)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A search not narrowed to one domain merges all domains with a stable cursor](domains/1.01-positive-search-across-domains-stable-cursor.md) | p3 | **Integration** | p3 multi-domain deployment / moves |
| [1.02 — One dead-letter queue serves queues in two domains, and DLQ search lists both](domains/1.02-positive-one-dead-letter-queue-serves-two-domains.md) | p3 | **Integration** | p3 multi-domain deployment / moves |
| [1.03 — An event that matches waits in two domains is applied exactly once in each, also after redelivery](domains/1.03-positive-event-applied-once-per-domain.md) | p3 | **Integration** | p3 multi-domain deployment / moves |
| [1.04 — A task stays in the domain it was created in after its queue is re-routed](domains/1.04-guardrail-task-domain-fixed-at-creation.md) | p3 | **Integration** | p3 multi-domain deployment / moves |
| [1.05 — A create that would cross a task-domain boundary is rejected (type rule `400`, cross-domain link `404`)](domains/1.05-negative-create-across-domain-boundary.md) | p3 | **E2E** | needs a two-domain e2e profile |
| [1.06 — A routing change of a queue with live tasks starts a move instead of stopping the queue](domains/1.06-positive-routing-change-with-live-tasks-starts-a-move.md) | p3 | **Integration** | p3 multi-domain deployment / moves |
| [1.07 — During a move, new work goes to the target, linked work stays with its source, and a claim names one domain](domains/1.07-positive-placement-and-claims-during-a-move.md) | p3 | **Integration** | p3 multi-domain deployment / moves |
| [1.08 — Dedup and supersession still hold across the two domains of a move](domains/1.08-guardrail-dedup-and-supersession-span-a-move.md) | p3 | **Integration** | p3 multi-domain deployment / moves |

</details>

<details><summary><code>entries/</code> (6)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — The lease holder or an API consumer records an immutable entry on a task](entries/1.01-positive-create-entry.md) | p1 | **E2E** |  |
| [1.02 — Entries are listed by time and filtered by type, time range and creator](entries/1.02-positive-list-entries-filtered.md) | p1 | **E2E** |  |
| [1.03 — One entry is read by its ID](entries/1.03-positive-get-entry-by-id.md) | p1 | **E2E** |  |
| [1.04 — An entry whose type is unknown, whose payload fails its schema or is over 64 KB, or that would exceed the per-task entry limit is refused and nothing is stored](entries/1.04-negative-entry-create-refused.md) | p1 | **E2E** |  |
| [1.05 — A subject that may only read the task cannot add an entry](entries/1.05-negative-reader-cannot-create-entry.md) | p1 | **Integration** | read-only grant |
| [1.06 — Entries have no update or delete route](entries/1.06-guardrail-entries-cannot-be-changed-or-deleted.md) | p1 | **E2E** |  |

</details>

<details><summary><code>events/</code> (9)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — The task history records every change, in order, with id, time, actor, type and payload](events/1.01-positive-history-complete-and-ordered.md) | p1 | **E2E** |  |
| [1.02 — The history is cursor-paged with a capped page size and can be filtered by event type](events/1.02-positive-history-cursor-paged.md) | p1 | **E2E** |  |
| [1.03 — A lifecycle event is published with the history's ID, the owner tenant, and the engine's publishing subject](events/1.03-positive-lifecycle-event-published-under-publishing-subject.md) | p1 | **Both** | E2E consume from broker; integration publishing subject |
| [1.04 — The publishing policy decides which events reach the broker; the history always records everything](events/1.04-positive-publishing-preset-and-per-type-policy.md) | p1 — the default-preset rows; the `per_type` rows are p2 | **Integration** | per-type publishing policy matrix |
| [1.05 — An event the engine causes itself carries no actor](events/1.05-positive-engine-caused-event-has-no-actor.md) | p1 | **E2E** | short per-task queue timeout, check history actor |
| [1.06 — Events never carry lease tokens, task payloads, or an error's message and context](events/1.06-guardrail-events-carry-no-sensitive-data.md) | p1 | **E2E** |  |
| [1.07 — With the Event Broker down, task operations continue and events are delivered later, in order](events/1.07-guardrail-broker-down-tasks-continue.md) | p1 | **Integration** | broker outage + ordered catch-up |
| [1.08 — A publish the broker refuses for authorization changes no task state and raises an alert](events/1.08-guardrail-publish-refused-for-authz-pages-operators.md) | p1 | **Integration** | broker authz refusal + alert |
| [1.09 — A redelivered event keeps its event ID, so consumers can deduplicate](events/1.09-guardrail-at-least-once-redelivery-keeps-event-id.md) | p1 | **Integration** | redelivery injection |

</details>

<details><summary><code>flows/</code> (9)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — Flow: create → claim → start → heartbeat → progress → complete](flows/1.01-flow-create-claim-execute-complete.md) | p1 | **E2E** |  |
| [1.02 — Flow: retryable failures → backoff → retries exhausted → dead-lettered → re-enqueue → success](flows/1.02-flow-retries-dead-letter-reenqueue.md) | p2 (dead-letter) | **E2E** |  |
| [1.03 — Flow: a worker process dies, its task is requeued and claimed elsewhere, and the zombie is fenced](flows/1.03-flow-worker-crash-zombie-fenced.md) | p1 | **Both** | E2E with short heartbeat timeout; integration for precise requeue |
| [1.04 — Flow: fan-out to children, park on a children wait, resume with counts, and a join successor that ignores failures](flows/1.04-flow-fan-out-children-and-join.md) | p2 | **E2E** |  |
| [1.05 — Flow: an approval step through an input wait answered with `resolve`](flows/1.05-flow-approval-through-input-wait.md) | p2 | **E2E** |  |
| [1.06 — Flow: arm an event wait, start the external action, the event arrives, park returns at once](flows/1.06-flow-arm-event-wait-then-park.md) | p2 | **E2E** | suite must include event-broker |
| [1.07 — Flow: incident — hold a queue, producers keep creating, running work finishes, release, claims resume](flows/1.07-flow-incident-hold-and-release.md) | p1 | **E2E** |  |
| [1.08 — Flow: a worker shuts down gracefully, releases its tasks, and another worker picks them up](flows/1.08-flow-graceful-worker-shutdown.md) | p1 | **E2E** |  |
| [1.09 — Flow: cancel a running task in `graceful` mode, end to end](flows/1.09-flow-graceful-cancel-running-task.md) | p1 | **E2E** |  |

</details>

<details><summary><code>hierarchy/children/</code> (9)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — The parent's worker creates a child with the parent's lease token](hierarchy/children/1.01-positive-worker-creates-child-with-parent-lease-token.md) | p2 | **E2E** |  |
| [1.02 — A child's worker can create grandchildren, up to `max_hierarchy_depth`](hierarchy/children/1.02-positive-nested-children-up-to-max-depth.md) | p2 | **Both** | E2E `grandchild`; integration `too_deep` (needs a chain at the limit) |
| [1.03 — Cancelling a parent cancels its unfinished children, unless it opted out](hierarchy/children/1.03-positive-parent-cancel-cascade-to-children.md) | p2 | **Both** | E2E for `propagate_unleased`, `propagate_leased_child_requested`, `propagate_leased_child_graceful` up to the grace expiry, `opt_out_forced`; integration for the grace expiry (clock) and `forced_large_fanout_crash` (process crash) |
| [1.04 — A parent that fails cancels its unfinished children gracefully](hierarchy/children/1.04-positive-parent-failure-cancels-children-gracefully.md) | p2 | **Both** | E2E for the response and the immediate child outcomes; integration for the grace expiry (clock) |
| [1.05 — A child the caller may not link is refused without revealing anything about the parent](hierarchy/children/1.05-guardrail-child-link-refusals.md) | p2 | **Both** | E2E token/tenant rows; integration authz rows |
| [1.06 — Holding the parent's lease is not enough: the child also needs `create`](hierarchy/children/1.06-guardrail-child-needs-create-action.md) | p2 | **Integration** | create grant |
| [1.07 — A `wait_for_children` parent cannot complete successfully while a child is unfinished](hierarchy/children/1.07-negative-complete-with-children-pending.md) | p2 | **E2E** |  |
| [1.08 — A child whose queue is in another task domain does not find its parent (`404`)](hierarchy/children/1.08-negative-child-in-another-domain-parent-not-found.md) | p3 | **Integration** | p3 domains |
| [1.09 — A child created while its parent is being cancelled is either refused or cancelled, never orphaned](hierarchy/children/1.09-guardrail-child-created-while-parent-ends-never-orphaned.md) | p2 | **Integration** | race |

</details>

<details><summary><code>hierarchy/dependencies/</code> (10)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A task created with predecessors starts `blocked`, `queued` or is refused, by what its predecessors already did](hierarchy/dependencies/1.01-positive-create-with-predecessors-initial-state.md) | p2 | **E2E** |  |
| [1.02 — A predecessor's end unblocks, waits on, or cancels its `blocked` successor, by mode and failure rule](hierarchy/dependencies/1.02-positive-predecessor-outcome-moves-successor.md) | p2 | **E2E** |  |
| [1.03 — A predecessor the caller cannot read, in another tenant or in another domain, gets 404, never 403](hierarchy/dependencies/1.03-guardrail-unreadable-predecessor-404.md) | p2 | **Both** | E2E other-tenant row; integration unreadable and other-domain rows (PDP double, two-domain deployment) |
| [1.04 — A predecessor list over 64 entries, or naming a task twice, is rejected](hierarchy/dependencies/1.04-negative-predecessor-list-refusals.md) | p2 | **E2E** |  |
| [1.05 — The dependency endpoints no longer exist](hierarchy/dependencies/1.05-guardrail-dependency-endpoints-removed.md) | p2 | **E2E** |  |
| [1.06 — Creates alone can never build a dependency cycle](hierarchy/dependencies/1.06-guardrail-no-cycle-possible-at-create.md) | p2 | **E2E** |  |
| [1.07 — A successor created while its predecessor ends is never left `blocked`](hierarchy/dependencies/1.07-guardrail-predecessor-ending-concurrently-never-left-blocking.md) | p2 | **Integration** | race |
| [1.08 — A failed predecessor with 250 successors cancels each exactly once, in bounded steps, across a crash](hierarchy/dependencies/1.08-guardrail-large-fanout-cascade-cancels-each-once.md) | p2 | **Integration** | crash mid-cascade |
| [1.09 — Re-enqueueing a failed predecessor does not revive its cancelled successors](hierarchy/dependencies/1.09-guardrail-reenqueue-does-not-revive-successors.md) | p2 | **E2E** |  |
| [1.10 — Flow: fan out three parts, then join them with a task that tolerates failed parts](hierarchy/dependencies/1.10-flow-fan-out-then-join.md) | p2 | **E2E** |  |

</details>

<details><summary><code>limits/concurrency/</code> (7)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A full concurrency dimension holds back only the tasks it covers; the claim serves other tenants, queues and types](limits/concurrency/1.01-positive-full-dimension-holds-back-only-its-tasks.md) | p1 (row "type_global" p2) | **E2E** |  |
| [1.02 — A batch claim takes only as many tasks as the limits allow, and that is a success](limits/concurrency/1.02-positive-batch-takes-only-what-fits.md) | p1 | **E2E** |  |
| [1.03 — A concurrency slot is freed on every exit from `claimed`/`running`: release, park and completion](limits/concurrency/1.03-positive-slot-freed-on-every-exit.md) | p1 | **Both** | E2E for the release and park rows and the same-node "complete" row; Integration for the cross-node "complete" row (two nodes) |
| [1.04 — Config overrides set concurrency limits by GTS pattern; the most specific pattern wins](limits/concurrency/1.04-positive-override-most-specific-pattern-wins.md) | p2 | **Integration** | GTS-pattern override config matrix |
| [1.05 — Nothing assignable because of a full limit → `429` (quota `concurrency`) with `Retry-After: 1`, at the end of the long-poll](limits/concurrency/1.05-negative-nothing-assignable-returns-429.md) | p1 | **E2E** |  |
| [1.06 — A tenant worker never learns that another tenant is at its limit: `204`, not `429`](limits/concurrency/1.06-guardrail-other-tenants-full-limit-gives-204.md) | p1 | **E2E** |  |
| [1.07 — The global concurrency limit is never exceeded by concurrent claims](limits/concurrency/1.07-guardrail-global-limit-never-exceeded-under-concurrency.md) | p1 | **Integration** | concurrent claims |

</details>

<details><summary><code>limits/depth/</code> (6)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A depth limit of `0` means unlimited](limits/depth/1.01-positive-zero-means-unlimited.md) | p1 | **E2E** |  |
| [1.02 — Create refused when a sampled queue depth is full: `429` naming the dimension `queue_depth.tenant`, `queue_depth.queue` or `queue_depth.domain_global`](limits/depth/1.02-negative-queue-depth-full-by-dimension.md) | p1 | **Both** | E2E with `sample_secs = 1`, waiting until the queue's `sampled_at` passes the setup; integration per dimension |
| [1.03 — Every admission into a deferred state is refused when the tenant's deferred limit is full: dimension `deferred`](limits/depth/1.03-negative-deferred-limit-refuses-every-deferral.md) | p1 (row "create with predecessor" p2) | **Both** | E2E one deferral path; integration all paths |
| [1.04 — Engine moves and re-entries are never refused by depth or deferred limits](limits/depth/1.04-guardrail-engine-moves-never-refused.md) | p1 (row "unblock" p2) | **Integration** | engine re-entries into a full queue |
| [1.05 — One tenant at its depth limit never affects another tenant](limits/depth/1.05-guardrail-one-tenant-full-does-not-affect-another.md) | p1 | **E2E** |  |
| [1.06 — Per-tenant depth may be passed by at most one sampling interval of the tenant's own creates](limits/depth/1.06-flow-tenant-depth-overshoot-bounded-by-one-sample.md) | p1 | **Integration** | sampler interval bound |

</details>

<details><summary><code>queues/</code> (12)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — Queue introspection shows effective limits and settings, and per-state counts to callers holding queue `configure`](queues/1.01-positive-queue-introspection.md) | p1 | **Both** | E2E list and detail as `{admin}`; integration for the read-only caller without `counts` (needs an action-level PDP policy) |
| [1.02 — An unregistered queue ID returns `404` on every queue endpoint](queues/1.02-negative-unknown-queue-not-found.md) | p1 | **E2E** |  |
| [1.03 — Queue endpoints need the queue action, not task rights](queues/1.03-negative-queue-action-required.md) | p1 (list, hold, release) · p3 (config) | **Integration** | queue action vs task rights |
| [2.01 — An admin holds a queue](queues/2.01-positive-hold-queue.md) | p1 | **E2E** |  |
| [2.02 — A held queue stops claims and nothing else](queues/2.02-positive-held-queue-stops-claims-only.md) | p1 | **Both** | E2E claims stop / creates pass; integration timers keep running |
| [2.03 — Releasing a hold, by call or at `until`, lets claims resume](queues/2.03-positive-release-resumes-claims.md) | p1 | **E2E** |  |
| [2.04 — Hold and release edge cases: repeat hold, `until` in the past, release when not held, dead-letter queue](queues/2.04-guardrail-hold-and-release-edge-cases.md) | p1 | **E2E** |  |
| [3.01 — A runtime config save is a new complete version, recorded once per affected queue](queues/3.01-positive-runtime-config-save-is-complete-version.md) | p3 | **Integration** | p3 runtime config |
| [3.02 — A config override beats the queue registration, which beats the config default](queues/3.02-positive-config-override-precedence.md) | p2 (row **runtime beats static** p3) | **Integration** | config precedence |
| [3.03 — The config version endpoint shows which domains have applied a version](queues/3.03-positive-config-version-apply-status.md) | p3 | **Integration** | p3 |
| [3.04 — A runtime timeout or retry change applies to new tasks; existing tasks keep their values](queues/3.04-positive-timeout-change-applies-to-new-tasks-only.md) | p3 | **Integration** | p3 |
| [3.05 — A queue registered outside the trust limits refuses new tasks, naming the setting](queues/3.05-negative-queue-outside-trust-limits-refuses-tasks.md) | p1 | **E2E** |  |

</details>

<details><summary><code>retention/</code> (6)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A finished task, with its entries and history, is removed after its retention](retention/1.01-positive-finished-task-removed-after-retention.md) | p1 | **Integration** | days of clock + cleanup jobs |
| [1.02 — A task that is live when its slot is recycled is rescued, and kept for its full retention from `terminal_at`](retention/1.02-guardrail-rescued-task-kept-until-retention-from-terminal-at.md) | p1 | **Integration** | days of clock + cleanup jobs |
| [1.03 — Past the hard retention limit an unfinished task is force-cancelled first](retention/1.03-positive-hard-retention-limit-force-cancels.md) | p1 | **Integration** | days of clock + cleanup jobs |
| [1.04 — Retention precedence: tenant policy, then config type pattern, then type trait, then global default](retention/1.04-positive-retention-policy-precedence.md) | p1 | **Integration** | days of clock + cleanup jobs |
| [1.05 — One tenant's legal hold does not delay cleanup of other tenants](retention/1.05-guardrail-legal-hold-does-not-delay-others.md) | p1 | **Integration** | days of clock + cleanup jobs |
| [1.06 — Cleanup runs separately in each task domain](retention/1.06-guardrail-cleanup-runs-per-domain.md) | p3 | **Integration** | days of clock + cleanup jobs |

</details>

<details><summary><code>retry/</code> (10)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A retryable error with budget left schedules a retry after the base backoff](retry/1.01-positive-retryable-error-schedules-retry.md) | p1 | **Both** | E2E with `base_interval_secs: 1` (the 1 s floor makes the wait exactly 1 s); integration for the jitter window |
| [1.02 — Retry backoff follows the strategy, stops at max_interval, is only shortened by jitter, never drops below 1 s, and never overflows](retry/1.02-positive-backoff-by-strategy-cap-and-jitter.md) | p1 | **Integration** | backoff math (mostly unit) |
| [1.03 — A reported error that cannot be retried ends the task, dead-lettered with a DLQ and failed without one](retry/1.03-positive-error-without-retry-ends-failed-or-dead-lettered.md) | p1 (dead-letter routing p2) | **E2E** |  |
| [1.04 — Running out of liveness requeues ends the task timed_out](retry/1.04-positive-liveness-budget-exhausted-timed-out.md) | p1 (DLQ routing p2) | **Integration** | many lease expiries |
| [1.05 — max_liveness_requeues 0 requeues without limit until the lifetime timeout ends the task](retry/1.05-positive-unlimited-liveness-requeues-end-at-lifetime.md) | p1 | **Integration** | lifetime clock |
| [1.06 — A retry policy on the request wins over the type and the queue, and is taken whole](retry/1.06-positive-task-retry-policy-taken-whole.md) | p1 | **E2E** |  |
| [1.07 — Unlimited liveness requeues are refused without a lifetime timeout, and always for tenant-owned types](retry/1.07-negative-unlimited-liveness-requeues-without-lifetime.md) | p1 | **E2E** |  |
| [1.08 — Repeating a retryable failure report gets LEASE_LOST and spends no second retry](retry/1.08-guardrail-repeated-retryable-report-is-lease-lost.md) | p1 | **E2E** |  |
| [1.09 — A scheduled retry becomes queued at run_after, and the next claim starts attempt 2](retry/1.09-flow-retry-becomes-queued-then-new-attempt.md) | p1 | **E2E** |  |
| [1.10 — Lost workers never use the retry budget: four lease expiries, then still three retries](retry/1.10-flow-liveness-requeues-keep-retry-budget.md) | p1 | **Integration** | four lease expiries |

</details>

<details><summary><code>search/</code> (9)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — Search without a time range covers the last 7 days, newest first](search/1.01-positive-default-window-newest-first.md) | p1 | **Both** | E2E newest-first; integration 7-day window (needs aged rows) |
| [1.02 — Search filters by state, queue, type, tag, resource reference, urgency, priority, assignee and task links](search/1.02-positive-filter-by-field.md) | p1 (rows `tag`, `resource reference`, `children`, `successors`: p2) | **E2E** |  |
| [1.03 — An explicit created_at range of up to 31 days, or an unfinished-only state filter, replaces the default window](search/1.03-positive-explicit-range-and-unfinished-only-window.md) | p1 | **E2E** |  |
| [1.04 — The view decides which fields each search item carries: tiny, short, long, full](search/1.04-positive-views.md) | p1 | **E2E** |  |
| [1.05 — GET /tasks/count counts what the same search would return, capped at search_max_count](search/1.05-positive-count.md) | p1 | **E2E** |  |
| [1.06 — A search query the engine cannot serve within its bounds is rejected with 400, never silently narrowed](search/1.06-negative-unsupported-query.md) | p1 | **E2E** |  |
| [1.07 — The count endpoint rejects $orderby, $select, $top, $skiptoken and view](search/1.07-negative-count-rejects-paging-and-view.md) | p1 | **E2E** |  |
| [1.08 — Page size is clamped to 100, and to 20 at view=full](search/1.08-guardrail-page-size-clamped.md) | p1 | **E2E** |  |
| [1.09 — Cursor paging stays stable while tasks are being added](search/1.09-guardrail-cursor-paging-stable-while-tasks-added.md) | p1 | **E2E** |  |

</details>

<details><summary><code>tasks/cancel/</code> (11)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A cancel of a task that holds no lease ends it at once](tasks/cancel/1.01-positive-cancel-of-unleased-task-ends-at-once.md) | p1 (blocked case p2) | **E2E** |  |
| [1.02 — A cancel of a leased task flags it for requested and graceful, and ends it at once for forced](tasks/cancel/1.02-positive-cancel-of-leased-task-by-mode.md) | p1 | **E2E** |  |
| [1.03 — A graceful cancel after a requested one arms the grace timer once](tasks/cancel/1.03-positive-escalate-requested-cancel-to-graceful.md) | p1 | **Both** | grace timer |
| [1.04 — A worker that finishes its work after a cancel request may complete it successfully](tasks/cancel/1.04-positive-worker-finishes-normally-after-cancel-request.md) | p1 | **E2E** |  |
| [1.05 — A cancel of an ended task is refused, unless the task already ended cancelled](tasks/cancel/1.05-negative-cancel-of-ended-task-refused-unless-cancelled.md) | p1 (dependency-failed case p2) | **E2E** |  |
| [1.06 — A cancel without the cancel action gets 403, and of an unreadable task the same 404 as a missing task](tasks/cancel/1.06-negative-cancel-without-permission.md) | p1 | **Both** | E2E other-tenant 404; integration cancel-action 403 |
| [1.07 — A cancel with an unknown mode is rejected](tasks/cancel/1.07-negative-unknown-cancel-mode.md) | p1 | **E2E** |  |
| [1.08 — A worker may not report cancelled when no cancel was requested](tasks/cancel/1.08-negative-worker-reports-cancelled-without-request.md) | p1 | **E2E** |  |
| [1.09 — A task created cancellable false rejects requested and graceful cancels, and only forced stops it](tasks/cancel/1.09-guardrail-cancellable-false-allows-only-forced.md) | p1 | **E2E** |  |
| [1.10 — The worker sees the cancel request on its heartbeat and ends the task cancelled](tasks/cancel/1.10-flow-worker-acknowledges-cancel-request.md) | p1 | **E2E** |  |
| [1.11 — A graceful cancel the worker does not finish in time becomes forcibly_cancelled](tasks/cancel/1.11-flow-graceful-cancel-forced-after-grace-period.md) | p1 | **Both** | grace expiry |

</details>

<details><summary><code>tasks/create/</code> (13)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A minimal create puts the task in `queued` with its timers armed](tasks/create/1.01-positive-minimal-create-queued.md) | p1 | **E2E** |  |
| [1.02 — `run_after` decides the starting state: future → `scheduled`, past → `queued`, beyond the lifetime → refused](tasks/create/1.02-positive-run-after-placement-and-lifetime-bound.md) | p1 | **E2E** |  |
| [1.03 — Settings resolve request → type → queue → engine default, within the trust limits](tasks/create/1.03-positive-settings-resolution.md) | p1 | **E2E** |  |
| [1.04 — An initial wait group on create puts the task in `waiting`](tasks/create/1.04-positive-initial-wait-group-waiting.md) | p1 (timer); the **input** case is p2 | **E2E** |  |
| [1.05 — Tags and resource references are set at create; references are unverified without a check](tasks/create/1.05-positive-tags-and-resource-refs.md) | p2 | **E2E** |  |
| [1.06 — `critical` urgency needs the `expedite` action: 403 without it, accepted with it](tasks/create/1.06-guardrail-critical-urgency-needs-expedite.md) | p1 | **Integration** | expedite grant |
| [1.07 — An unknown type, or one the caller cannot see, is rejected before anything is stored](tasks/create/1.07-negative-unknown-task-type.md) | p1 | **Both** | E2E unknown type; integration "cannot see" row |
| [1.08 — Registry unreachable: an uncached type fails closed with 503, a cached type is still served](tasks/create/1.08-negative-registry-outage-uncached-type-503.md) | p1 | **Integration** | registry outage |
| [1.09 — Input that fails the type's schema is rejected](tasks/create/1.09-negative-input-fails-schema.md) | p1 | **E2E** |  |
| [1.10 — Create fields outside their limits are rejected: priority, tags, resource references](tasks/create/1.10-negative-field-limits.md) | p1 (priority) · p2 (tags, resource references) | **E2E** |  |
| [1.11 — Oversized creates are rejected with 413: combined payload over 64 KB, body over 1 MB](tasks/create/1.11-negative-payload-and-body-size-limits.md) | p1 | **E2E** |  |
| [1.12 — Creates outside the type's trust limits or into a queue that may not hold the task are rejected](tasks/create/1.12-negative-placement-and-trust-limit-refusals.md) | p1 | **E2E** |  |
| [1.13 — Tags can be changed after create; nothing else can](tasks/create/1.13-positive-update-tags-after-create.md) | p2 | **E2E** |  |

</details>

<details><summary><code>tasks/dedup/</code> (7)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A duplicate by derived identity returns 409 without any key](tasks/dedup/1.01-positive-duplicate-by-derived-identity.md) | p2 | **E2E** |  |
| [1.02 — The dedup window decides when a finished task releases its identity](tasks/dedup/1.02-positive-window-release-and-reuse.md) | p2 | **Integration** | 10-day window |
| [1.03 — A key sent for a `dedup` type is ignored: not checked, not stored](tasks/dedup/1.03-guardrail-sent-key-is-ignored.md) | p2 | **E2E** |  |
| [1.04 — The dedup scope decides who shares an identity: `tenant` across subjects, `subject` per subject, never across tenants](tasks/dedup/1.04-guardrail-identity-scope.md) | p2 | **E2E** |  |
| [1.05 — A dedup hit on a task the caller may not read is 403 with no ID](tasks/dedup/1.05-guardrail-dedup-hit-on-unreadable-task-403.md) | p2 | **Integration** | same-tenant unreadable |
| [1.06 — A type with an invalid `dedup` policy cannot be created; an `authz_scope` type cannot be registered](tasks/dedup/1.06-negative-invalid-dedup-policy-or-authz-scope.md) | p2 | **E2E** |  |
| [1.07 — A key field value over 4 KiB is rejected](tasks/dedup/1.07-negative-key-field-value-over-4kib.md) | p2 | **E2E** |  |

</details>

<details><summary><code>tasks/idempotency/</code> (7)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A replay returns 409 `already_exists` with the existing task's ID, whatever happened to the task since](tasks/idempotency/1.01-positive-replay-returns-409-with-existing-id.md) | p1 | **E2E** |  |
| [1.02 — A key is held while its task is unfinished and for the 7-day window; then the same key creates a new task](tasks/idempotency/1.02-positive-key-window-release.md) | p1 | **Integration** | 7-day window |
| [1.03 — A create without a valid `Idempotency-Key` is rejected: 428 when missing, 400 when empty or malformed](tasks/idempotency/1.03-negative-missing-or-malformed-idempotency-key.md) | p1 | **E2E** |  |
| [1.04 — The same key with a different request is rejected on `Idempotency-Key`](tasks/idempotency/1.04-negative-same-key-different-request.md) | p1 | **E2E** |  |
| [1.05 — Two subjects using the same key get two independent tasks](tasks/idempotency/1.05-guardrail-two-subjects-same-key-independent.md) | p1 | **E2E** |  |
| [1.06 — A replay is answered 409 even when the queue is full, and uses no capacity](tasks/idempotency/1.06-guardrail-replay-not-refused-as-full.md) | p1 | **Both** | sampled depth |
| [1.07 — A refused create does not hold its key](tasks/idempotency/1.07-guardrail-refused-create-holds-no-key.md) | p1 | **E2E** |  |

</details>

<details><summary><code>tasks/read/</code> (4)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — Get a task by ID returns the full task resource and never a lease token](tasks/read/1.01-positive-get-task-by-id.md) | p1 | **E2E** |  |
| [1.02 — A finished task outside the search window is still readable by ID](tasks/read/1.02-positive-finished-task-outside-search-window-readable-by-id.md) | p1 | **Integration** | needs a task older than the window |
| [1.03 — A task removed by retention answers 404, like one that never existed](tasks/read/1.03-negative-retention-removed-task-is-not-found.md) | p1 | **Integration** | retention |
| [1.04 — GET /tasks/{id} embeds no list that can grow without limit](tasks/read/1.04-guardrail-get-task-embeds-no-unbounded-list.md) | p1 | **E2E** |  |

</details>

<details><summary><code>tasks/supersede/</code> (11)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A higher order value supersedes the group's unfinished task; how depends on its state and the mode](tasks/supersede/1.01-positive-higher-order-supersedes-by-old-task-state.md) | p2 | **E2E** |  |
| [1.02 — How a gracefully superseded task ends: grace expiry and worker reports](tasks/supersede/1.02-positive-graceful-supersession-endings.md) | p2 | **Both** | grace expiry rows |
| [1.03 — Supersession reaches the replaced task's children](tasks/supersede/1.03-positive-supersession-cancels-children.md) | p2 | **E2E** |  |
| [1.04 — An order value not above the group's latest never supersedes: lower is `STALE_ORDER`, equal is a replay](tasks/supersede/1.04-negative-order-not-above-latest-does-not-supersede.md) | p2 | **E2E** |  |
| [1.05 — A supersede type rejects `cancellable: false`](tasks/supersede/1.05-negative-cancellable-false-on-supersede-type.md) | p2 | **E2E** |  |
| [1.06 — Without `cancel` on the task it would replace, the whole create is refused](tasks/supersede/1.06-negative-caller-lacks-cancel-on-old-task.md) | p2 | **Integration** | cancel grant |
| [1.07 — A replacement is not refused for the queue place its predecessor held](tasks/supersede/1.07-guardrail-replacement-not-refused-for-predecessor-capacity.md) | p2 | **Integration** | sampled depth |
| [1.08 — Supersession is authorized before the transaction and re-planned if the old task changed](tasks/supersede/1.08-guardrail-plan-rechecked-when-old-task-gets-leased.md) | p2 | **Integration** | race |
| [1.09 — Concurrent submissions of one group converge on the highest order value](tasks/supersede/1.09-guardrail-concurrent-submissions-converge-on-highest.md) | p2 | **Integration** | race |
| [1.10 — Under `tenant` scope, one subject can supersede another subject's task](tasks/supersede/1.10-positive-cross-subject-supersession-under-tenant-scope.md) | p2 | **E2E** |  |
| [1.11 — Graceful supersession of a running task: cancel request, then `superseded` when the worker stops](tasks/supersede/1.11-flow-graceful-supersede-running-task.md) | p2 | **E2E** |  |

</details>

<details><summary><code>timeouts/</code> (8)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A queue or lifetime timeout ends the task timed_out, dead-lettered with a DLQ and failed without one](timeouts/1.01-positive-queue-or-lifetime-timeout-ends-task-timed-out.md) | p1 (DLQ routing p2) | **Both** | E2E with 1–2 s per-task timeouts |
| [1.02 — A claimed task not started within the assignment timeout is requeued as a liveness failure](timeouts/1.02-positive-assignment-timeout-requeues-with-backoff.md) | p1 | **Both** | E2E with 1–2 s per-task timeouts; integration for exact timing |
| [1.03 — The execution timeout ends a running task even while its worker heartbeats, also at the engine default](timeouts/1.03-positive-execution-timeout-is-terminal.md) | p1 (DLQ routing p2) | **Both** | E2E with 1–2 s per-task timeouts; integration for exact timing |
| [1.04 — Time spent waiting does not count toward the execution timeout](timeouts/1.04-positive-waiting-time-not-counted-toward-execution.md) | p1 | **Integration** | clock |
| [1.05 — Passing the SLA deadline publishes slaBreached and the task keeps running](timeouts/1.05-positive-sla-breach-task-keeps-running.md) | p1 | **Both** | E2E short SLA; slaBreached via history |
| [1.06 — When a deadline and a session expiry both find the same task, the task is handled once](timeouts/1.06-guardrail-concurrent-deadline-detectors-act-once.md) | p1 | **Integration** | race of detectors |
| [1.07 — Deadlines fire no earlier than due and within one second after](timeouts/1.07-guardrail-timers-fire-within-one-second.md) | p1 | **Integration** | timer precision |
| [1.08 — Lifetime and SLA deadlines run from creation across a retry, a requeue and a wait](timeouts/1.08-flow-deadlines-survive-retries-requeues-and-waits.md) | p1 | **Integration** | clock across retry/requeue/wait |

</details>

<details><summary><code>waits/children/</code> (6)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A children wait with no unfinished child is satisfied at once](waits/children/1.01-positive-no-unfinished-children-satisfied-at-park.md) | p2 | **E2E** |  |
| [1.02 — A parent still resumes on time when its last child ends while it is parking](waits/children/1.02-guardrail-resume-within-bound-when-check-is-lost.md) | p2 | **Integration** | lost check race |
| [1.03 — Two last children ending at the same moment resume the parent once](waits/children/1.03-guardrail-concurrent-last-children-resume-parent-once.md) | p2 | **Integration** | race |
| [1.04 — A child re-enqueued after its parent resumed counts as unfinished again](waits/children/1.04-guardrail-child-reenqueued-after-parent-resumed.md) | p2 | **E2E** |  |
| [1.05 — Flow: a parent parked on its children resumes once, after the last child ends, with counts per terminal state](waits/children/1.05-flow-parent-resumes-after-last-child-with-counts.md) | p2 | **E2E** |  |
| [1.06 — Flow: an armed children condition is checked only when parked](waits/children/1.06-flow-armed-children-condition-evaluated-only-at-park.md) | p2 | **E2E** |  |

</details>

<details><summary><code>waits/event/</code> (13)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A matching event resumes a parked task, and only the event header is kept](waits/event/1.01-positive-matching-event-resumes-task-with-header-only.md) | p2 | **E2E** |  |
| [1.02 — A match expression is checked after the field match, against the event data](waits/event/1.02-positive-match-expression-checked-after-field-match.md) | p3 | **E2E** |  |
| [1.03 — An event condition without a timeout gets the maximum, with on_timeout fail](waits/event/1.03-positive-no-timeout-gets-max-wait-with-fail.md) | p2 | **E2E** |  |
| [1.04 — A condition frees its event-wait places when it times out or its task is cancelled](waits/event/1.04-positive-cancel-and-timeout-free-wait-places.md) | p2 | **Integration** | wait-place accounting |
| [1.05 — An event condition that can never be satisfied or checked is refused when it is set](waits/event/1.05-negative-event-condition-validation.md) | p2 (case `invalid_match_expression`: p3) | **E2E** |  |
| [1.06 — An event wait the caller's broker rights do not fully allow is refused and stores nothing](waits/event/1.06-guardrail-event-wait-authorization-refusals.md) | p2 | **Integration** | broker consume policy |
| [1.07 — Decidable consume constraints are stored and keep other events from matching](waits/event/1.07-guardrail-consume-constraints-filter-every-event.md) | p2 | **Integration** | consume constraints |
| [1.08 — Over a per-tenant event-wait limit, setting a wait is refused and the worker keeps its lease](waits/event/1.08-guardrail-event-wait-limits-keep-lease.md) | p2 | **Integration** | per-tenant limit config |
| [1.09 — An event of another tenant never satisfies a tenant's wait](waits/event/1.09-guardrail-other-tenant-event-never-matches.md) | p2 | **E2E** |  |
| [1.10 — A redelivered event never satisfies a condition twice, even across task domains](waits/event/1.10-guardrail-redelivered-event-satisfies-once-per-domain.md) | p2 (task domains p3) | **Integration** | redelivery |
| [1.11 — A run-time error in the expression is no match and does not touch the failure counter](waits/event/1.11-guardrail-runtime-error-is-no-match-and-not-counted.md) | p3 | **Integration** | internal counter |
| [1.12 — Ten consecutive budget overruns resolve the condition with eval_budget_exceeded](waits/event/1.12-guardrail-eval-budget-exhaustion-resolves-condition.md) | p3 | **Integration** | eval budget |
| [1.13 — Setting, arming or parking event waits never changes the broker subscription](waits/event/1.13-guardrail-setting-waits-never-changes-subscription.md) | p2 | **Integration** | broker internals |

</details>

<details><summary><code>waits/framework/</code> (12)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A worker sets a wait and the task is parked](waits/framework/1.01-positive-set-wait-parks-task.md) | p1 | **E2E** |  |
| [1.02 — A resumed task comes back through a new claim, in the same attempt, with the wait result](waits/framework/1.02-positive-resume-claims-new-lease-same-attempt.md) | p1 (case `input`: p2) | **E2E** |  |
| [1.03 — Park returns the result at once when the armed group is already satisfied](waits/framework/1.03-positive-park-returns-result-when-already-satisfied.md) | p2 (event kind); case `armed_timer` p1 | **E2E** |  |
| [1.04 — A task created with an initial wait group starts waiting and resumes like any parked task](waits/framework/1.04-positive-initial-wait-group-resumes-created-task.md) | p1 | **E2E** |  |
| [1.05 — Cancelling a waiting task ends it at once](waits/framework/1.05-positive-cancel-waiting-task-ends-at-once.md) | p1 | **E2E** |  |
| [1.06 — The lease token of a parked task is dead](waits/framework/1.06-guardrail-old-token-after-park-gets-lease-lost.md) | p1 | **E2E** |  |
| [1.07 — Set-wait refusals leave the task and the lease untouched](waits/framework/1.07-negative-set-wait-refusals.md) | p1 | **E2E** |  |
| [1.08 — Park with no armed group is refused](waits/framework/1.08-negative-park-without-armed-group.md) | p1 | **E2E** |  |
| [1.09 — An armed group is dropped when the run ends without parking](waits/framework/1.09-guardrail-armed-group-dropped-when-run-ends.md) | p2 (event kind) | **E2E** |  |
| [1.10 — Flow: how an `ALL` or `ANY` group is decided by satisfactions and timeouts](waits/framework/1.10-flow-group-decision-outcomes.md) | p2 (uses input conditions) | **Both** | timeout outcomes |
| [1.11 — Flow: arm a wait, start the action, then park](waits/framework/1.11-flow-arm-then-park.md) | p2 (the event kind; arm/park itself is p1, G-WAI-17) | **E2E** |  |
| [1.12 — In p1, input, children and event waits and `parent_task_id` are refused with a clear 400](waits/framework/1.12-guardrail-p2-wait-kinds-and-parent-refused-in-p1.md) | p1 (the refusal); the features themselves are p2 | **E2E** |  |

</details>

<details><summary><code>waits/input/</code> (6)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A valid answer to an input wait resumes the parked task](waits/input/1.01-positive-resolve-valid-value-resumes-task.md) | p2 | **E2E** |  |
| [1.02 — Repeating a resolve with the same value succeeds](waits/input/1.02-positive-repeat-with-same-value-succeeds.md) | p2 | **E2E** |  |
| [1.03 — A provided value that is invalid or too large is refused and the condition stays open](waits/input/1.03-negative-resolve-value-refusals.md) | p2 | **E2E** |  |
| [1.04 — A resolve on a condition that is no longer open to an answer is refused](waits/input/1.04-negative-resolve-state-refusals.md) | p2 | **E2E** |  |
| [1.05 — An untrusted input schema is refused at set time](waits/input/1.05-negative-input-schema-refusals.md) | p2 | **E2E** |  |
| [1.06 — Resolve checks the condition's task: `403` for a reader without `resolve`, `404` for a caller who cannot read it](waits/input/1.06-guardrail-resolve-permission-checks.md) | p2 | **Integration** | resolve grant |

</details>

<details><summary><code>waits/timer/</code> (4)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A timer condition resumes the task at `resume_at`; a past `resume_at` is satisfied at once without parking](waits/timer/1.01-positive-timer-resumes-at-resume-at-past-returns-at-once.md) | p1 | **E2E** |  |
| [1.02 — The lifetime deadline still ends a task that is waiting](waits/timer/1.02-guardrail-lifetime-timeout-ends-waiting-task.md) | p1 | **Both** | lifetime |
| [1.03 — A timer condition without `resume_at` is refused](waits/timer/1.03-negative-timer-without-resume-at.md) | p1 | **E2E** |  |
| [1.04 — A pending timer wait survives an engine restart](waits/timer/1.04-guardrail-timer-wait-survives-restart.md) | p1 | **E2E** | self-managed restart test (like event_broker test_restart_durability.py) |

</details>

<details><summary><code>worker/blacklist/</code> (4)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — The tenth consecutive worker failure blacklists the worker on that queue](worker/blacklist/1.01-positive-tenth-worker-failure-blacklists-worker.md) | p3 | **Integration** | p3; counters, durations |
| [1.02 — A blacklisted worker's claim skips that queue only, for that (subject, `worker_id`) only](worker/blacklist/1.02-positive-blacklisted-worker-claim-skips-queue.md) | p3 | **Integration** | p3; counters, durations |
| [1.03 — What counts toward the blacklist counter, and what does not](worker/blacklist/1.03-guardrail-blacklist-counting-rules.md) | p3 | **Integration** | p3; counters, durations |
| [1.04 — A queue's own blacklist threshold and duration apply, and claims resume after it](worker/blacklist/1.04-positive-per-queue-threshold-duration-and-expiry.md) | p3 | **Integration** | p3; counters, durations |

</details>

<details><summary><code>worker/claim/</code> (11)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — Claim one task: session, lease, attempt and deadlines in one step](worker/claim/1.01-positive-claim-single-task.md) | p1 | **E2E** |  |
| [1.02 — Batch claim returns up to `max_tasks` tasks, each with its own lease](worker/claim/1.02-positive-batch-claim-token-per-task.md) | p1 | **E2E** |  |
| [1.03 — A claim that finds nothing waits for its timeout, capped at `claim_max_wait_secs`, then answers `204`](worker/claim/1.03-positive-empty-claim-waits-then-204.md) | p1 | **Both** | E2E timeout_secs=1; integration claim_max_wait cap |
| [1.04 — The claim returns the task's owner tenant, creator, owner and creator authority](worker/claim/1.04-positive-claim-returns-owner-and-authority.md) | p1 | **E2E** |  |
| [1.05 — `attempt` goes up on a claim after a retry or a liveness requeue, and stays the same after a release or a wait resume](worker/claim/1.05-positive-attempt-counting-across-claims.md) | p1 | **Both** | E2E release/retry rows; integration liveness row |
| [1.06 — A claim reuses the caller's live `session_id`; an expired, unknown or foreign one opens a new session](worker/claim/1.06-positive-claim-reuses-live-own-session-else-opens-new.md) | p1 | **Both** | expired-session row |
| [1.07 — A claim with a bad `worker_id`, `max_tasks` or queue list is rejected with `400`](worker/claim/1.07-negative-claim-request-validation.md) | p1 (row "dead-letter queue" p2, row "two domains" p3) | **E2E** |  |
| [1.08 — Two workers claiming at the same moment never get the same task](worker/claim/1.08-guardrail-two-workers-never-claim-same-task.md) | p1 | **Integration** | race |
| [1.09 — A worker allowed to claim only queue Q never receives another queue's task](worker/claim/1.09-guardrail-queue-policy-limits-what-worker-receives.md) | p1 | **Integration** | queue policy |
| [1.10 — Long-poll claim wakes for a task created on another node within the poll interval](worker/claim/1.10-flow-long-poll-wakes-on-task-from-other-node.md) | p1 | **Integration** | two nodes |
| [1.11 — A claim naming an unregistered queue gets `404`](worker/claim/1.11-negative-claim-naming-unknown-queue-not-found.md) | p1 | **E2E** |  |

</details>

<details><summary><code>worker/execution/</code> (10)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — Starting a claimed task makes it running; the execution deadline set by the claim stays](worker/execution/1.01-positive-start-claimed-task.md) | p1 | **E2E** |  |
| [1.02 — Progress is buffered, saved within one flush interval, and produces events only as its event mode allows](worker/execution/1.02-positive-progress-saved-and-event-modes.md) | p1 (progress saved) · p2 (event modes) | **Both** | flush interval + event modes |
| [1.03 — A checkpoint is merged into the task context as a JSON Merge Patch](worker/execution/1.03-positive-checkpoint-merges-as-json-merge-patch.md) | p1 (G-EXE-12) | **E2E** |  |
| [1.04 — A valid terminal completion ends the task, stores its result and frees its slots](worker/execution/1.04-positive-complete-accepted-results.md) | p1 | **E2E** |  |
| [1.05 — A reported failure records its fault, and the published event carries only its classification](worker/execution/1.05-positive-worker-fault-recorded.md) | p1 (fault recorded) · p3 (blacklist effect) | **E2E** |  |
| [1.06 — `start` is optional and safe to repeat: a second `start` is a no-op and a worker may finish from `claimed`](worker/execution/1.06-positive-start-optional-and-repeatable.md) | p1 | **E2E** |  |
| [1.07 — Progress with a percent outside 0–100 is rejected](worker/execution/1.07-negative-progress-percent-out-of-range.md) | p1 | **E2E** |  |
| [1.08 — A checkpoint that pushes the task over 64 KB is rejected and changes nothing](worker/execution/1.08-negative-checkpoint-over-payload-limit.md) | p1 (checkpoint is p1, G-EXE-12) | **E2E** |  |
| [1.09 — Two concurrent payload writes on one task: one wins, the other gets CONCURRENT_MODIFICATION](worker/execution/1.09-negative-concurrent-payload-write-conflict.md) | p1 | **Integration** | race |
| [1.10 — A completion whose result breaks the result rules is refused; the task and lease are unchanged](worker/execution/1.10-negative-complete-result-refused.md) | p1 | **E2E** |  |

</details>

<details><summary><code>worker/fencing/</code> (9)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — Each new lease carries a larger lease epoch](worker/fencing/1.01-positive-lease-epoch-grows-per-lease.md) | p1 | **E2E** |  |
| [1.02 — Repeating a completion with the same token and result returns the stored outcome](worker/fencing/1.02-positive-idempotent-completion-replay.md) | p1 | **E2E** |  |
| [1.03 — A second completion with a different result is rejected](worker/fencing/1.03-negative-completion-replay-different-result.md) | p1 | **E2E** |  |
| [1.04 — Every lease-fenced worker call that fails the fence gets LEASE_LOST and writes nothing](worker/fencing/1.04-negative-failed-fence-returns-lease-lost.md) | p1 | **Both** | expiry rows integration |
| [1.05 — Progress buffered under a lease that is lost before the flush is never saved](worker/fencing/1.05-guardrail-buffered-progress-dropped-after-lease-loss.md) | p1 | **Integration** | flush vs lease loss |
| [1.06 — LEASE_LOST is final: the worker does not retry, and a retry would fail the same way](worker/fencing/1.06-guardrail-lease-lost-is-final.md) | p1 | **E2E** |  |
| [1.07 — The lease token never appears in task reads, history, events or search](worker/fencing/1.07-guardrail-lease-token-never-exposed.md) | p1 | **E2E** |  |
| [1.08 — A worker write re-checks claim: a revoked or narrowed grant releases the task with 403 CLAIM_REVOKED, an unreachable PDP fails closed with 503](worker/fencing/1.08-guardrail-worker-write-authorization-outcomes.md) | p1 | **Integration** | grant revocation, PDP outage |
| [1.09 — A worker that lost its lease cannot complete the task; the new holder's result is kept](worker/fencing/1.09-flow-zombie-complete-after-reclaim.md) | p1 | **Both** | short heartbeat timeout |

</details>

<details><summary><code>worker/ordering/</code> (11)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A `critical` task is claimed before `normal` tasks of its queue, whatever their tenant or age](worker/ordering/1.01-positive-critical-before-normal-in-queue.md) | p1 | **Both** | E2E smoke on a dedicated queue; integration full matrix |
| [1.02 — `round_robin` serves a small tenant's tasks between a big tenant's, whatever their priority](worker/ordering/1.02-positive-round-robin-interleaves-small-tenant.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |
| [1.03 — Priority orders one tenant's own tasks, never one tenant against another](worker/ordering/1.03-positive-priority-orders-only-within-tenant.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |
| [1.04 — A `fifo` queue serves all tenants in one order: priority, then age, then task ID](worker/ordering/1.04-positive-fifo-queue-orders-by-priority-age-id.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |
| [1.05 — The order in which a claim lists its queues does not change what it gets](worker/ordering/1.05-positive-queue-listing-order-does-not-matter.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |
| [1.06 — Neither priority nor age is compared across queues](worker/ordering/1.06-guardrail-priority-and-age-not-compared-across-queues.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |
| [1.07 — Under concurrent claims order is best effort: tests must not assert strict FIFO](worker/ordering/1.07-guardrail-strict-fifo-not-promised-under-concurrency.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |
| [1.08 — Tenant rotation across nodes is approximate but never skips a tenant for more than a cycle per node](worker/ordering/1.08-guardrail-round-robin-across-nodes-is-approximate.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |
| [1.09 — Across named queues, `critical` tasks go first, oldest first, from any queue](worker/ordering/1.09-flow-critical-first-across-queues-oldest-first.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |
| [1.10 — For `normal` work a claim rotates between its queues, starting after the previous claim's start queue](worker/ordering/1.10-flow-rotation-between-queues-for-normal-work.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |
| [1.11 — Each scan budget is shared by the queues of its kind in one claim round; a round that uses it up returns what it has](worker/ordering/1.11-flow-scan-budgets-per-kind-per-claim-round.md) | p1 | **Integration** | needs deterministic queue contents; shared e2e queues interfere |

</details>

<details><summary><code>worker/reassignment/</code> (6)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — An admin reassigns a running task to another worker identity](worker/reassignment/1.01-positive-admin-reassigns-running-task.md) | p3 | **E2E** |  |
| [1.02 — Only the named worker identity can accept a reassigned task, and gets a new lease](worker/reassignment/1.02-positive-only-named-identity-accepts.md) | p3 | **E2E** |  |
| [1.03 — If nobody accepts within one lease, the task is requeued with no blacklist strike](worker/reassignment/1.03-positive-accept-window-expires-requeue-no-strike.md) | p3 | **Integration** | accept-window expiry |
| [1.04 — After reassignment the old holder's calls get `LEASE_LOST`](worker/reassignment/1.04-guardrail-old-holder-gets-lease-lost.md) | p3 | **E2E** |  |
| [1.05 — Concurrency slots stay held through the hand-off and are freed only when the task leaves](worker/reassignment/1.05-guardrail-slots-held-through-hand-off.md) | p3 | **Integration** | slot accounting |
| [1.06 — Reassignment is refused without task `admin` or when the task has no holder](worker/reassignment/1.06-negative-reassign-refused.md) | p3 | **Both** | admin grant row integration |

</details>

<details><summary><code>worker/release/</code> (2)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — Releasing a task gives it back without spending any budget, with an optional delay or checkpoint](worker/release/1.01-positive-release-returns-task-to-queue.md) | p1 (release) · p2 (event waits) | **E2E** |  |
| [1.02 — The SDK gives back a lease whose handler ended without finishing the task](worker/release/1.02-guardrail-sdk-releases-dropped-lease.md) | p1 | **Integration** | Rust SDK behaviour |

</details>

<details><summary><code>worker/sessions/</code> (10)</summary>

| Scenario | Phase | Level | Why |
|---|---|---|---|
| [1.01 — A batch heartbeat renews the worker session and reports on every lease](worker/sessions/1.01-positive-batch-heartbeat-renews-session.md) | p1 | **E2E** |  |
| [1.02 — Claiming a task with a shorter lease shortens the session lease and the heartbeat interval](worker/sessions/1.02-positive-short-lease-task-lowers-session-lease.md) | p1 | **Both** | E2E exchanges 1–2; integration exchange 3 (Rust SDK behaviour) |
| [1.03 — A batch heartbeat with more leases than heartbeat_batch_max is rejected](worker/sessions/1.03-negative-batch-heartbeat-over-max.md) | p1 | **E2E** |  |
| [1.04 — A heartbeat reports lease_lost for every lease the caller no longer holds, and renews nothing it should not](worker/sessions/1.04-guardrail-heartbeat-lease-lost-cases.md) | p1 | **Both** | expiry rows integration |
| [1.05 — The single-task heartbeat endpoint is gone](worker/sessions/1.05-guardrail-single-task-heartbeat-removed.md) | p1 | **E2E** |  |
| [1.06 — Heartbeats keep sessions alive while the PDP is unreachable](worker/sessions/1.06-guardrail-heartbeat-works-during-pdp-outage.md) | p1 | **Integration** | PDP outage |
| [1.07 — Under overload claims are shed while heartbeats and completions still pass](worker/sessions/1.07-guardrail-reserved-admission-keeps-heartbeats-flowing.md) | p1 | **Integration** | overload |
| [1.08 — The SDK retries a shed heartbeat at once and ignores a Retry-After longer than the lease](worker/sessions/1.08-guardrail-sdk-heartbeat-retries-shed-heartbeat.md) | p1 | **Integration** | Rust SDK behaviour |
| [1.09 — A running task past its execution deadline ends timed_out even when the session scan finds it first](worker/sessions/1.09-guardrail-execution-deadline-wins-over-session-expiry.md) | p1 | **Integration** | clock race |
| [1.10 — A worker that stops heartbeating loses its session, and its tasks are requeued with backoff](worker/sessions/1.10-flow-session-expiry-requeues-tasks.md) | p1 | **Both** | E2E short heartbeat; integration backoff |

</details>

## Errors

[ERRORS.md](ERRORS.md) is a reference, not a set of scenarios: the problem envelope, the category → status → context table, every Task Engine reason with the scenario that owns it, the contrast with the Event Broker's `failed_precondition` status, and the no-payload-echo rule.

## Authoring rules

See [CONVENTIONS.md](CONVENTIONS.md): baseline (§1), file format, case tables, naming and where each behaviour lives (§2), transport (§3), fixtures (§4), wire shapes (§5), errors (§6), side-effect vocabulary (§7), gaps (§8).
