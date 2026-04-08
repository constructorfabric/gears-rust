 # Feature: Transactional Outbox Pattern
- [ ] `p1` - **ID**: `cpt-cf-mini-chat-featstatus-usage-outbox`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-mini-chat-feature-usage-outbox`

## 1. Feature Context

### 1.1 Overview

This feature describes the transactional outbox pattern implemented in `modkit-db` (shared infra table `modkit_outbox_events`) so that event publishing is reliable and does not add synchronous network calls to the critical execution hot path.

The outbox is a **general-purpose infrastructure mechanism**.
Modules use it by publishing messages under a dedicated `(namespace, topic)` pair.

### 1.2 Purpose

This feature ensures the **Outbox Completeness Invariant**: for any domain operation that is defined to emit an outbox event, it MUST be impossible for that operation's side effects to commit without the corresponding `modkit_outbox_events` row being persisted in the same database transaction.

This invariant applies only to domain operations that are defined to emit outbox events (e.g., quota-bearing turn finalization in Mini Chat). It does not apply to read-only operations, pre-reserve validation failures, or state transitions that intentionally produce no event. The set of operations that require outbox emission is defined by each consuming module (see DESIGN.md section 5.7 for the Mini Chat normative list).

It also ensures events can be delivered asynchronously with at-least-once delivery semantics.

**Scope clarification**: The Outbox Completeness Invariant covers *transactional persistence* of the outbox row — not end-to-end delivery. A row that reaches `dead` status (section 4) satisfies the persistence invariant but represents a delivery failure. Dead rows MUST be surfaced via operational monitoring (see DoD `cpt-cf-mini-chat-dod-usage-outbox-dispatcher`); manual replay or escalation is outside the scope of this feature.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-mini-chat-actor-chat-user` | Initiates an operation whose commit MUST enqueue an outbox event (when side effects are applied). |
| `cpt-cf-mini-chat-actor-usage-outbox-dispatcher` | Background worker that claims pending outbox rows and publishes events to a downstream consumer with retries. |
| `cpt-cf-mini-chat-actor-outbox-consumer` | Downstream event consumer that processes deliveries idempotently. |

### 1.4 References

- ModKit lifecycle/stateful tasks documentation (stateful worker)

### 1.5 Implementation Shape (normative)

- Outbox events are enqueued through `modkit_db::outbox::enqueue(runner, msg)` where `runner: &impl DBRunner`.
- The enqueue call MUST run inside the same DB transaction as the side effects the event describes.
- A background dispatcher (ModKit `stateful` lifecycle task) claims events from `modkit_outbox_events` using a lease (`locked_by`, `locked_until`) and delivers them with retries.
- Producers SHOULD provide a stable `dedupe_key` to support idempotent enqueue and idempotent downstream processing.

### 1.6 Outbox Storage (normative)

Outbox events are stored in a shared infrastructure table owned by `modkit-db`:

`modkit_outbox_events`

**Columns (minimum)**:

- `id uuid pk`
- `namespace text not null`
- `topic text not null`
- `tenant_id uuid null`
- `dedupe_key text null`
- `payload jsonb not null`
- `status text not null` (`pending|processing|delivered|dead`)
- `attempts int not null default 0`
- `next_attempt_at timestamptz not null default now()`
- `locked_by uuid null`
- `locked_until timestamptz null`
- `last_error text null`
- `created_at timestamptz not null default now()`
- `updated_at timestamptz not null default now()` — MUST be set to `now()` by application code on every state transition (`claim`, `ack`, `nack`). Not managed by a DB trigger.

**Indexes (minimum)**:

- `index(status, next_attempt_at)`
- `index(locked_until)`

**Dedupe / idempotency (Postgres)**:

- Partial unique index on `(namespace, topic, dedupe_key)` where `dedupe_key IS NOT NULL`.

**`tenant_id` column vs `dedupe_key` — distinct purposes (normative)**:

The `tenant_id` column and the `dedupe_key` field serve different roles and MUST NOT be conflated:

- **`tenant_id` column** — used for routing, filtering, and downstream partitioning. The dispatcher MAY use it to scope claim queries to a specific tenant. Downstream consumers MAY use it for partition-aware processing. It is nullable to accommodate system-level events that are not tenant-scoped.
- **`dedupe_key`** — a producer-defined idempotency key whose structure is domain-specific. The partial unique index on `(namespace, topic, dedupe_key)` enforces at-most-once enqueue at the database level. The generic outbox library treats `dedupe_key` as an opaque string and performs NO validation on its structure.

**Dedupe key requiredness by event type (normative):**

The generic outbox library allows `dedupe_key` to be NULL (the partial unique index only applies when `dedupe_key IS NOT NULL`). However, modules MUST follow these rules when enqueuing events:

1. **Quota-bearing / billing events** — MUST have non-null `dedupe_key`
   - Events that result in quota debit, credit, or billing charges
   - Events that participate in financial reconciliation
   - Examples: Mini-Chat usage snapshots, subscription charges, refunds
   - Rationale: At-least-once delivery semantics require idempotent deduplication to prevent double-charging

2. **Critical state transitions** — SHOULD have non-null `dedupe_key`
   - Events that trigger irreversible downstream actions
   - Events used for audit trails or compliance logging
   - Examples: user deletion notifications, access revocations
   - Rationale: Prevents duplicate side effects in distributed systems

3. **Informational telemetry** — MAY have NULL `dedupe_key`
   - Non-critical metrics, analytics, or monitoring events
   - Events where duplicate delivery is acceptable
   - Examples: page view counters, system health heartbeats
   - Rationale: Reduces storage overhead when idempotency is not required

**Mini-Chat-specific convention (not enforced by generic library):** In the Mini Chat domain, the canonical format is `"{tenant_id}/{turn_id}/{request_id}"` (see DESIGN.md section 5.6). Mini Chat domain code MUST validate this format and enforce non-null `dedupe_key` for all quota-bearing events before calling `enqueue`. The validation is the caller's responsibility, not the outbox library's. Mini Chat downstream consumers MUST use the same canonical tuple `(tenant_id, turn_id, request_id)` — extracted from the `dedupe_key` or from the payload — for idempotent processing. Other modules define their own `dedupe_key` format and idempotency extraction rules in their respective feature specs.

The presence of `tenant_id` as a table column does not replace domain-level idempotency semantics embedded in `dedupe_key`. The two serve different purposes. When both are populated, modules SHOULD ensure consistency (tenant_id column matches the tenant component of dedupe_key), but this is a module-level convention, not a database constraint.

### 1.7 Proposed `modkit_db::outbox` v1 API (sketch)

This is the intended interface shape for the generalized outbox mechanism in `modkit-db`.
Modules consume this API with their own `namespace/topic`.

```rust
use modkit_db::secure::DBRunner;
use modkit_db::DBProvider;
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

pub struct OutboxMessage {
    pub namespace: &'static str,
    pub topic: &'static str,
    pub tenant_id: Option<Uuid>,
    pub dedupe_key: Option<String>,
    pub payload: Value,
}

pub async fn enqueue(
    runner: &impl DBRunner,
    msg: OutboxMessage,
) -> Result<Uuid, modkit_db::DbError>;

pub struct ClaimCfg {
    pub batch_size: u32,
    pub lease_duration: Duration,
    /// Maximum total delivery attempts (including the first).
    /// Claim query excludes rows where `attempts >= max_attempts`.
    /// Same value used by retry logic (section 3) for dead-lettering.
    pub max_attempts: u32,
}

pub struct ClaimedMessage {
    pub id: Uuid,
    pub namespace: String,
    pub topic: String,
    pub tenant_id: Option<Uuid>,
    pub dedupe_key: Option<String>,
    pub payload: Value,
    pub attempts: i32,
}

pub struct OutboxStore<E> {
    pub db: DBProvider<E>,
    pub worker_id: Uuid,
    pub namespace: String,
}

impl<E> OutboxStore<E>
where
    E: From<modkit_db::DbError> + Send + 'static,
{
    pub async fn claim_batch(&self, cfg: ClaimCfg) -> Result<Vec<ClaimedMessage>, E>;
    pub async fn ack(&self, id: Uuid) -> Result<(), E>;
    pub async fn nack(&self, id: Uuid, err: &str) -> Result<(), E>;
}
```

## 2. Actor Flows (CDSL)

### Operation Commit Enqueues Outbox Row

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-flow-usage-outbox-enqueue`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Success Scenarios**:
 - An operation commits, and exactly one logical outbox event is enqueued atomically (resulting in at most one `modkit_outbox_events` row per `dedupe_key`).

**Error Scenarios**:
- The DB transaction fails: the described side effects and outbox insertion MUST both roll back.

**Behavior (normative)**:
- The outbox row insertion is part of the operation's commit: it MUST be in the **same DB transaction** as the committed side effects.
- Within that transaction:
  - The system MUST enqueue exactly one logical outbox event describing the committed side effects. If a `dedupe_key` conflict occurs, the event is considered already enqueued; no duplicate row is inserted.
- If an operation implementation uses a uniqueness guard for idempotent enqueue (e.g. a stable `dedupe_key` with a unique index):
  - A conflict on insert MUST be treated as "already enqueued".
- If the transaction fails/rolls back for any reason:
  - No `modkit_outbox_events` row is persisted.

**Payload requirements**:
- The outbox payload MUST include sufficient identifiers for idempotent downstream processing.
- The outbox row SHOULD include a stable `dedupe_key` suitable for idempotency.

### Outbox Dispatcher Publishes Events

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-flow-usage-outbox-dispatch`

**Actor**: `cpt-cf-mini-chat-actor-usage-outbox-dispatcher`

**Success Scenarios**:
 - Pending outbox rows are claimed using `FOR UPDATE SKIP LOCKED` and published to the downstream consumer.
- Claimed rows are marked `delivered` on success.

**Error Scenarios**:
- Publish fails (temporary): the row is returned to `pending` and scheduled for retry using backoff.
- Worker crashes after claiming: rows are reclaimed after lease expiry.

**Behavior (normative)**:
- The dispatcher is an internal background worker (implemented as a ModKit stateful lifecycle task).
- It MUST periodically poll for publishable outbox rows and process them until a shutdown `CancellationToken` is triggered.
  - **Polling interval**: configurable per-dispatcher via `poll_interval: Duration` (runtime configuration, supplied by the deploying module). No hardcoded default in this spec. The polling interval MUST be significantly less than `lease_duration` (recommended: `poll_interval <= lease_duration / 3`) to avoid spurious lease-expiry reclaims.
- Claiming MUST be safe under concurrency (multiple replicas/workers) and MUST use row-level locking via `FOR UPDATE SKIP LOCKED`.
- Claimed rows MUST be leased using `(locked_by, locked_until)` so that:
  - A crashing worker does not permanently strand a row.
  - Another worker can reclaim a row after lease expiry.
- For each claimed row, the dispatcher MUST publish the outbox payload to the downstream consumer.
  - **"Publish" definition**: Publish is an abstract operation supplied by the consuming module as a callback (e.g., `async fn(ClaimedMessage) -> Result<(), PublishError>`). The transport mechanism (in-process function call, HTTP, message queue) is module-defined and outside the scope of this spec. The dispatcher treats the callback return value as the publish outcome: `Ok(())` = success, `Err(...)` = failure. The dispatcher MUST NOT interpret payload contents.
- On publish success, the row MUST transition to `delivered` and be made ineligible for further dispatch.
- On publish failure, the row MUST be returned to `pending` and rescheduled by setting `next_attempt_at` using a retry policy, while recording `attempts` and `last_error`.

**Implementation note (normative)**:

- The dispatcher uses a `modkit_db::outbox::OutboxStore<E>` constructed from a `DBProvider<E>`.
- The store provides:
  - `claim_batch(...) -> Vec<ClaimedMessage>`
  - `ack(id)`
  - `nack(id, err)`
- `ack` MUST only succeed for rows currently leased by the same worker (guarded by `locked_by`).
- If `ack` fails the lease guard (row not leased by this worker, or lease expired and reclaimed by another worker), `ack` MUST return an error. The dispatcher MUST log the error and MUST NOT treat this as a publish failure requiring `nack`. The row is now owned by another worker or already delivered; no further action by this worker.

**Idempotency requirement**:
- The dispatcher MUST assume at-least-once delivery.
- Downstream processing MUST be idempotent on a stable key (e.g. the outbox `dedupe_key`).

## 3. Processes / Business Logic (CDSL)

### Enqueue Outbox Row (Transactional)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-algo-usage-outbox-enqueue`

**Input**:
- Operation outcome (completed/failed/aborted)
- Committed side effects summary (module-defined)
- Identifiers used for idempotency and downstream correlation

**Caller responsibility**: The `enqueue` function persists whatever message it receives; it does not inspect or filter by outcome. The *caller* (domain operation code) is responsible for deciding whether a given outcome requires an outbox event. The set of outbox-requiring outcomes is defined by each consuming module (see section 1.2). If the caller determines that no event is needed (e.g., a pre-reserve validation failure), it simply does not call `enqueue`.

**Output**:
- Persisted `modkit_outbox_events` row inserted atomically with the committed side effects

**Requirements**:
- The enqueue operation MUST run inside the same DB transaction as the described side effects.
- The outbox payload MUST be derived from already-validated internal state (no client-provided usage fields).
- Enqueue MUST be idempotent on `dedupe_key` when it is provided:
  - Multiple attempts to enqueue the same logical event MUST NOT produce multiple outbox rows.
  - This is implemented in storage via the dedupe unique index and an upsert/ignore-on-conflict insert.
- The outbox payload MUST include all information needed by the downstream consumer.
- The outbox row MUST be initialized with:
  - `namespace` and `topic` appropriate for the producer
  - `status = 'pending'`
  - `attempts = 0`
  - `next_attempt_at = now()`
  - `locked_by = NULL`, `locked_until = NULL`

### Claim Pending Outbox Rows (Lease + Skip Locked)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-algo-usage-outbox-claim`

**Input**:
- batch_size
- lease_duration
- worker_id

**Output**:
- A list of claimed rows ready to publish

**Requirements**:
- Claim MUST be performed in a DB transaction.
- Only rows eligible for dispatch MAY be claimed. The claim WHERE clause MUST match rows satisfying:
  - (`status = 'pending'` AND `next_attempt_at <= now()`)
  - OR (`status = 'processing'` AND `locked_until < now()`) — this covers lease-expired rows from crashed workers; the claim query atomically reclaims them. No separate recovery actor or sweep is required.
  - In both cases: `attempts < max_attempts` (from `ClaimCfg`; rows at or above the limit are ineligible and MUST be transitioned to `dead` by the dispatcher on next encounter or by a periodic sweep).
- The claim query MUST lock selected rows using `FOR UPDATE SKIP LOCKED`.
- Upon claim, the worker MUST atomically:
  - transition row to `processing` (or keep `processing` if reclaiming an expired lease)
  - increment `attempts` — this is the **total delivery attempt count**, starting from 0 on insert. After the first claim, `attempts = 1`. `max_attempts = 3` means at most 3 delivery attempts; the row is dead-lettered when `attempts >= max_attempts`.
  - set `locked_by = worker_id`
  - set `locked_until = now() + lease_duration`
  - set `updated_at = now()`
- Claim ordering MUST be deterministic (e.g., `ORDER BY created_at ASC, id ASC`) to reduce starvation risk.
- Claim MUST support multiple workers without double-claiming the same row.

### Retry Scheduling on Publish Failure

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-algo-usage-outbox-retry`

**Input**:
- publish error
- current attempts count
- retry policy configuration (max_attempts, base_delay, max_delay)

**Output**:
- Updated outbox row with next attempt time

**Requirements**:
- On publish failure, the dispatcher MUST record `last_error`.
- The dispatcher MUST clear any claim lease when rescheduling: `locked_by = NULL`, `locked_until = NULL`.
- The dispatcher MUST compute `next_attempt_at` using exponential backoff with jitter, bounded by a configured maximum:

  > `delay = min(base_delay * 2^(attempts - 1), max_delay)`
  > `jittered_delay = uniform_random(delay / 2, delay)`
  > `next_attempt_at = now() + jittered_delay`
  >
  > Where:
  > * `base_delay` — minimum retry interval. Runtime configuration supplied by the deploying module (e.g., 1 s). Immutable per dispatcher instance.
  > * `max_delay` — upper bound on computed delay before jitter. Runtime configuration supplied by the deploying module (e.g., 300 s). Immutable per dispatcher instance.
  > * `attempts` — current value of the row's `attempts` column (persisted, incremented at claim time per section 3 claim algo).
  > * Jitter: uniform random over `[delay/2, delay]` (equal-jitter strategy). Bounding is applied before jitter: `delay` is clamped to `max_delay` first, then jitter is applied to the clamped value.

- If `attempts >= max_attempts`, the dispatcher MUST transition the row to `dead` and MUST NOT retry it automatically. `max_attempts` is the same value as `ClaimCfg.max_attempts` (section 1.7); it represents total delivery attempts. Runtime configuration supplied by the deploying module, immutable per dispatcher instance.

## 4. States (CDSL)

### `modkit_outbox_events` Row State Machine

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-state-usage-outbox-row`

**States**: pending, processing, delivered, dead

**Initial State**: pending

**State semantics (normative)**:
- `pending`:
  - Row is eligible for claiming when `next_attempt_at <= now()`.
  - Row MUST NOT be published unless it is first claimed.
- `processing`:
  - Row is claimed by a dispatcher and has an active lease (`locked_until`).
  - Row may be re-published in crash scenarios (at-least-once delivery).
  - If `now() > locked_until`, the row becomes reclaimable. The claim query (section 3, `cpt-cf-mini-chat-algo-usage-outbox-claim`) handles this inline: it includes `processing` rows with expired leases in its WHERE clause and atomically reclaims them. No separate recovery actor or sweep is required for the `processing → processing` (re-lease) transition.
- `delivered`:
  - Terminal state.
  - Row MUST NOT transition out of `delivered`.
- `dead`:
  - Terminal state for permanent failures (attempts exceeded `max_attempts`).
  - Row MUST NOT be retried automatically.

## 5. Definitions of Done

### Provide Transactional Outbox Persistence

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-dod-usage-outbox-transactional`

For any domain operation defined to emit an outbox event, the system **MUST** persist a `modkit_outbox_events` row in the same DB transaction as that operation's committed side effects.

**Implements**:
- `cpt-cf-mini-chat-flow-usage-outbox-enqueue`
- `cpt-cf-mini-chat-algo-usage-outbox-enqueue`

**Touches**:
- DB: `modkit_outbox_events`

### Provide Stateful Usage Outbox Dispatcher

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-dod-usage-outbox-dispatcher`

The system **MUST** run a background dispatcher as a stateful lifecycle task that:
- Claims rows using `FOR UPDATE SKIP LOCKED`.
- Uses a lease (`locked_until`) to ensure rows are recoverable after crashes.
- Retries failed publishes using backoff and records `last_error`.

**Implements**:
- `cpt-cf-mini-chat-flow-usage-outbox-dispatch`
- `cpt-cf-mini-chat-algo-usage-outbox-claim`
- `cpt-cf-mini-chat-algo-usage-outbox-retry`
- `cpt-cf-mini-chat-state-usage-outbox-row`

**Touches**:
- DB: `modkit_outbox_events`

### Enforce Idempotent Publish Contract

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-dod-usage-outbox-idempotency`

The system **MUST** ensure that event delivery is safe under retries and replays by using a stable dedupe key and requiring downstream processing to be idempotent on that key.

**Implements**:
- `cpt-cf-mini-chat-flow-usage-outbox-dispatch`

**Touches**:
- DB: `modkit_outbox_events` (dedupe key)

## 6. Acceptance Criteria

- [ ] For any domain operation defined to emit an outbox event, at most one `modkit_outbox_events` row exists per logical domain event (identified by `dedupe_key`), enforced by the partial unique index; on successful first commit, exactly one row is persisted in the same DB transaction as that operation's side effects.
- [ ] If a producer loses an idempotency race, it observes "already enqueued" and no duplicate `modkit_outbox_events` row is inserted.
- [ ] Dispatcher can run concurrently (multiple replicas) without double-processing rows (verified via `SKIP LOCKED` + lease).
- [ ] If the dispatcher crashes after claiming rows, those rows become eligible for reclaim after lease expiry.
- [ ] Publish failures reschedule rows with increasing `next_attempt_at` and record `last_error`.
- [ ] Publishing is safe under retries (downstream processing is idempotent on dedupe key).
