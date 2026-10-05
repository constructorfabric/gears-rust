# Feature: Transactional Outbox Pattern
- [ ] `p1` - **ID**: `cpt-cf-mini-chat-featstatus-usage-outbox`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-mini-chat-feature-usage-outbox`

## 1. Feature Context

### 1.1 Overview

Mini Chat publishes usage events, audit events and background work (attachment cleanup, chat cleanup, thread summaries) through the shared transactional outbox of the ToolKit database library. Producers write outbox rows in the same database transaction as their side effects; background tasks deliver them to handlers with at-least-once semantics. No synchronous call to billing or audit sits on the request hot path.

The outbox is a general-purpose library. A gear registers named **queues**, each split into a fixed number of **partitions**, and one handler per queue.

### 1.2 Purpose

This feature ensures the **Outbox Completeness Invariant**: for any domain operation that is defined to emit an outbox event, it MUST be impossible for that operation's side effects to commit without the corresponding outbox row being persisted in the same database transaction.

This invariant applies only to domain operations that are defined to emit outbox events (e.g., quota-bearing turn finalization in Mini Chat). It does not apply to read-only operations, pre-reserve validation failures, or state transitions that intentionally produce no event. The set of operations that require outbox emission is defined by each consuming gear (see DESIGN.md section 5.7 for the Mini Chat normative list).

Delivery is asynchronous and at-least-once.

**Scope clarification**: the invariant covers *transactional persistence* of the outbox row, not end-to-end delivery. A message a handler rejects is moved to the dead-letter table (section 4); it satisfies the persistence invariant but is a delivery failure. Dead letters MUST be surfaced via operational monitoring (see DoD `cpt-cf-mini-chat-dod-usage-outbox-dispatcher`). Mini Chat has no replay tooling of its own; the library's dead-letter operations (`replay`, `resolve`, `discard`) are the recovery path.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-mini-chat-actor-chat-user` | Initiates an operation whose commit MUST enqueue an outbox event (when side effects are applied). |
| `cpt-cf-mini-chat-actor-usage-outbox-dispatcher` | The outbox processor of the ToolKit database library: leases a partition, reads its messages in sequence order and calls the queue's handler. |
| `cpt-cf-mini-chat-actor-outbox-consumer` | Downstream consumer called by a handler (model-policy plugin `publish_usage`, audit plugin, provider file/vector-store APIs). MUST process redeliveries idempotently. |

### 1.4 References

- ToolKit outbox library documentation: usage, schema, handler contract, queue registration and lease configuration
- DESIGN.md §5.6 — usage event payload and `dedupe_key` format

### 1.5 Implementation Shape (normative)

- Producers enqueue a record addressed to a queue and partition and carrying a payload with its payload type. Domain services do not call the library directly; they go through a Mini Chat outbox enqueuer port that owns queue names and partition selection.
- The enqueue call MUST run inside the same DB transaction as the side effects the event describes.
- Enqueue returns a wake handle. The caller MUST fire it only after the transaction commits, and drop it on rollback. The wake handles of several enqueues in one unit of work combine into one. An unfired wake handle does not lose the message: the library's reconciler finds the partition later, so delivery is only delayed.
- Delivery runs in library background tasks (sequencer, per-partition processor, vacuum), started when the Mini Chat gear starts and stopped when it stops.
- Handlers implement the library's leased message handler contract and return `Ok`, `Retry` or `Reject`.
- The library has **no deduplication**. There is no `dedupe_key` column and no unique index on enqueue. Idempotency is the consumer's job (section 1.8).

### 1.6 Outbox Storage (normative)

The schema comes from the outbox library's migrations, which Mini Chat appends to its own migrations. Mini Chat uses the default table prefix `toolkit_outbox`, so the tables are shared with other gears in the same database and separated by queue name. Payloads are opaque bytes; Mini Chat always writes JSON with `payload_type = "application/json"`.

| Table | Purpose | Key columns |
|---|---|---|
| `toolkit_outbox_body` | Message payload, written once | `id`, `payload` (bytes), `payload_type`, `created_at`, `trace` |
| `toolkit_outbox_partitions` | One row per `(queue, partition)` | `id`, `queue`, `partition`, `sequence`; unique `(queue, partition)` |
| `toolkit_outbox_incoming` | Enqueued, not yet sequenced | `id`, `partition_id` → partitions, `body_id` → body |
| `toolkit_outbox_outgoing` | Sequenced, ready for the processor | `id`, `partition_id`, `body_id`, `seq`, `sequenced_at` |
| `toolkit_outbox_processor` | Per-partition cursor and lease | `partition_id` (PK), `processed_seq`, `attempts`, `last_error`, `locked_by`, `locked_until` |
| `toolkit_outbox_dead_letters` | Rejected messages with an inline payload copy | `partition_id`, `seq`, `payload`, `payload_type`, `created_at`, `failed_at`, `last_error`, `attempts`, `status` (`pending` / `reprocessing` / `resolved` / `discarded`), `completed_at`, `deadline`, `trace` |
| `toolkit_outbox_vacuum_counter` | Vacuum bookkeeping per partition | `partition_id`, `counter` |
| `toolkit_outbox_trace` | Optional batch-completion tracking (not used by Mini Chat) | `trace`, `owner_instance`, `queue`, `entities`, `pending`, … |

On MySQL the migration also creates `toolkit_outbox_body_id_sequence` and `toolkit_outbox_incoming_id_sequence`.

There is no per-message status column. A message's state follows from where its row is and from the partition cursor (section 4). Leases are held per partition in `toolkit_outbox_processor`, not per message.

### 1.7 Outbox library API used by Mini Chat

At start-up Mini Chat registers each queue (section 1.8) with its partition count and its handler. A queue can override the lease configuration; a lease override applies only to that queue, and queues without one use the default. The thread-summary queue sets the lease duration to `thread_summary_worker.claim_timeout_secs`.

A producer enqueues a record (queue, partition, JSON payload, `payload_type = "application/json"`) inside its transaction and fires the returned wake handle after commit.

Handler, message and lease contract of the library (illustrative):

```rust
#[async_trait]
pub trait LeasedMessageHandler: Send + Sync {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult;
}

pub struct OutboxMessage {
    pub partition_id: i64,
    pub seq: i64,
    pub payload: Vec<u8>,
    pub payload_type: String,
    pub created_at: DateTime<Utc>,
    /// Retries of this message so far (0 on first delivery).
    pub attempts: i16,
}

pub enum MessageResult {
    Ok,             // advance the cursor
    Retry,          // transient: redeliver this message and the rest of the partition after backoff
    Reject(String), // permanent: move to dead letters, continue with the next message
}

pub struct LeaseConfig {
    pub duration: Duration, // default 30 s
    pub headroom: Duration, // default 2 s; the handler is cancelled at duration - headroom
}
```

### 1.8 Mini Chat queues

All queues use the same partition count, `outbox.num_partitions` (power of two, 1–64, default 4). The partition is the partition key UUID, read as a 128-bit integer, modulo `num_partitions`.

| Queue (default name, config key) | Payload | Partition key | Handler | Lease | Retry bound |
|---|---|---|---|---|---|
| `mini-chat.usage_snapshot` (`outbox.queue_name`) | `UsageEvent` (SDK) | `tenant_id` | usage handler → model-policy plugin `publish_usage()` | default (30 s) | none; `Retry` until the plugin succeeds or returns `Permanent` |
| `mini-chat.attachment_cleanup` (`outbox.cleanup_queue_name`) | attachment cleanup event (enqueued by attachment deletion and by the upload reaper, `event_type = attachment_upload_abandoned`, without `secondary_ref`) | `tenant_id` | attachment cleanup handler → provider file delete, then Anthropic secondary delete | default | `cleanup_worker.max_attempts`, counted in `attachments.cleanup_attempts`; then the attachment is `failed` and the message `Reject` (dead letter). A delete answered with 2xx or 404 is success, any other status a failed attempt |
| `mini-chat.chat_cleanup` (`outbox.chat_cleanup_queue_name`) | chat cleanup event | `chat_id` | chat cleanup handler → per-attachment file deletes, vector store delete | default | `cleanup_worker.max_attempts` per attachment (attachment then `failed`, handler continues). A failing vector-store delete returns `Retry` until the delivery that reaches `cleanup_worker.max_attempts` (the message's `attempts`; deliveries that waited for pending attachments count), then `Reject`; the `chat_vector_stores` row is kept for a dead-letter replay |
| `mini-chat.thread_summary` (`outbox.thread_summary_queue_name`) | thread summary task | `chat_id` | thread-summary handler → non-streaming LLM call, summary persist, system usage event | `thread_summary_worker.claim_timeout_secs` | `thread_summary_worker.max_attempts` (the message's `attempts`); then `Reject`. A summary model missing from the catalog or disabled → `Reject` at once (`result = model_unavailable`) |
| `mini-chat.audit` (`outbox.audit_queue_name`) | audit envelope (turn / mutation / delete event) | `tenant_id` | audit handler → audit plugin `emit_*` | 60 s | none; `Retry` on transient errors. A payload that does not deserialize → `Reject`, checked before the plugin is resolved. No audit plugin registered → `Ok` (event dropped, counted as `audit_emit_total{result="dropped"}`; looked up again on the next delivery). Instance found but its client missing from ClientHub → `Retry` |

Partitioning by `chat_id` serialises work for one chat (cleanup and summaries for the same chat run in order). Partitioning by `tenant_id` keeps one tenant's usage and audit events in order.

The thread-summary lease comes from `claim_timeout_secs` because the handler makes an LLM call (with prompt-too-long retries) that the default 30 s lease would cancel and redeliver mid-call.

**Usage-event idempotency.** `UsageEvent.dedupe_key` is a payload field, not a library feature. Turn finalization sets `"{tenant_id}/{turn_id}/{request_id}"`; the thread-summary handler sets `"{tenant_id}/thread_summary_update/{system_request_id}"` (all UUIDs in 32-char lowercase hex, see DESIGN.md §5.6). The outbox can deliver the same event more than once; the model-policy plugin MUST drop duplicates by `dedupe_key`.

## 2. Actor Flows (CDSL)

### Operation Commit Enqueues Outbox Row

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-flow-usage-outbox-enqueue`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Success Scenarios**:
- An operation commits, and its outbox messages (e.g. one usage event and one audit event for a finalized turn) are written to `toolkit_outbox_body` / `toolkit_outbox_incoming` in the same transaction. After the commit the combined wake handle is fired.

**Error Scenarios**:
- The DB transaction fails: the side effects and the outbox rows both roll back, and the wake handle is dropped.
- The payload exceeds the library size limit (64 KiB): building the record fails before any statement runs, and the operation returns a payload-too-large error.

**Behavior (normative)**:
- The outbox insert is part of the operation's commit: it MUST be in the **same DB transaction** as the committed side effects.
- The caller MUST NOT enqueue the same logical event twice in one operation. The library does not detect duplicates; retried domain operations are guarded by the domain CAS (e.g. turn finalization), not by the outbox.
- If the transaction fails/rolls back for any reason, no outbox row is persisted.

**Payload requirements**:
- The payload MUST include enough identifiers for idempotent downstream processing (for usage events: `dedupe_key`, `tenant_id`, `turn_id`, `request_id`).

### Outbox Dispatcher Publishes Events

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-flow-usage-outbox-dispatch`

**Actor**: `cpt-cf-mini-chat-actor-usage-outbox-dispatcher`

**Success Scenarios**:
- The sequencer moves incoming rows to `toolkit_outbox_outgoing` with a per-partition `seq`.
- The processor for a partition takes the partition lease, reads the next messages after `processed_seq`, calls the handler for each in order, and advances `processed_seq` for every `Ok`.

**Error Scenarios**:
- Handler returns `Retry`: the cursor stays on that message; the message and everything after it in the partition are redelivered after exponential backoff. `attempts` is incremented.
- Handler returns `Reject(reason)`: the message is copied to `toolkit_outbox_dead_letters` and the cursor moves past it.
- The instance crashes, or the handler overruns the lease: the lease expires and any instance can take the partition; the un-acked messages are redelivered.

**Behavior (normative)**:
- Processing is sequential within a partition and parallel across partitions. Any instance can process any partition; ownership is the lease in `toolkit_outbox_processor` (`locked_by`, `locked_until`), taken with a conditional update (`locked_by IS NULL OR locked_until < now()`).
- The handler is called outside a DB transaction. The library cancels the handler future at `lease.duration - lease.headroom` so the ack can still commit inside the lease.
- A `Retry` blocks the rest of its partition until it succeeds. Handlers whose failures can persist SHOULD bound retries and return `Reject` after a limit (Mini Chat does this for cleanup and thread-summary queues; section 1.8).
- The handler MUST NOT interpret delivery as exactly-once.

**Idempotency requirement**:
- Delivery is at-least-once. A message is redelivered if the lease expires before the ack.
- Downstream processing MUST be idempotent: usage events by `dedupe_key`; cleanup handlers treat a provider 404 as success and skip attachments whose cleanup is already terminal; audit consumers must tolerate duplicates and dedupe on `(tenant_id, event_type, request_id)` for `turn_completed`, `turn_failed` and `turn_delete`, and on `(tenant_id, event_type, new_request_id)` for `turn_retry` and `turn_edit`; a redelivery is byte-identical (the same stored outbox payload).

## 3. Processes / Business Logic (CDSL)

### Enqueue Outbox Row (Transactional)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-algo-usage-outbox-enqueue`

**Input**:
- Queue name (from the `outbox` config section)
- Partition key (`tenant_id` or `chat_id`, section 1.8)
- Serialized JSON payload

**Caller responsibility**: `enqueue` persists whatever it receives. The calling domain service decides whether an outcome requires an event (see section 1.2) and does not call `enqueue` otherwise.

**Output**:
- One `toolkit_outbox_body` row and one `toolkit_outbox_incoming` row, inserted in the caller's transaction
- A wake handle to fire after commit

**Requirements**:
- The enqueue MUST run inside the same DB transaction as the described side effects.
- The payload MUST be derived from already-validated internal state (no client-provided usage fields).
- The payload MUST include all information the handler needs; handlers MUST NOT depend on rows that may be deleted before delivery (e.g. cleanup payloads carry provider file ids and the secondary-upload reference resolved at enqueue time).
- The wake handle MUST be fired only after a successful commit.

### Claim Pending Outbox Rows (Lease + Skip Locked)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-algo-usage-outbox-claim`

**Input**:
- Queue and partition
- Lease configuration of the queue
- Processor batch size (library processor tuning)

**Output**:
- An ordered batch of outbox messages for one partition

**Requirements** (provided by the library):
- The processor takes the partition lease by setting `locked_by` / `locked_until = now() + lease.duration` on the `toolkit_outbox_processor` row, only if the row is unleased or the lease has expired. On PostgreSQL and MySQL, partition and processor rows are locked with `FOR UPDATE SKIP LOCKED` so instances do not wait on each other.
- It reads outgoing rows with `seq > processed_seq` in `seq` order.
- One partition is processed by at most one instance at a time while the lease is valid.
- After an expired lease, another instance takes the partition and continues from `processed_seq`; messages the previous holder handled but did not ack are delivered again.

### Retry Scheduling on Publish Failure

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-algo-usage-outbox-retry`

**Input**:
- Handler result
- The message's `attempts`
- Processor tuning (`retry_base`, `retry_max`)

**Output**:
- Advanced cursor, a dead letter, or a delayed redelivery

**Requirements**:
- `Retry` increments `attempts` on the partition's processor row and schedules the next attempt with exponential backoff between `retry_base` and `retry_max` (default processor tuning: 1 s to 60 s).
- `Reject` moves the message to `toolkit_outbox_dead_letters` with its payload, `attempts` and the reject reason as `last_error`.
- The library has no `max_attempts`. Bounding retries is the handler's job, using the message's `attempts` or its own counter:
  - the thread-summary handler returns `Reject` when the delivery is its `thread_summary_worker.max_attempts`-th, and at once when the summary model is missing from the catalog or disabled (`mini_chat_thread_summary_execution_total{result="model_unavailable"}`);
  - the attachment cleanup handler counts failures in `attachments.cleanup_attempts` and returns `Reject` at `cleanup_worker.max_attempts`; the chat cleanup handler marks such an attachment `failed` and continues, and returns `Reject` for a failing vector-store delete on the delivery that reaches `cleanup_worker.max_attempts` (the message's `attempts`);
  - the usage and audit handlers retry until the plugin succeeds or reports a permanent error; the audit handler rejects a corrupt payload at once.

## 4. States (CDSL)

### Outbox Message State Machine

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-state-usage-outbox-row`

**States**: incoming, sequenced, processed, dead-lettered

**Initial State**: incoming

**State semantics (normative)**:
- `incoming`: a row in `toolkit_outbox_incoming`. Committed but not yet ordered. Not visible to handlers.
- `sequenced`: a row in `toolkit_outbox_outgoing` with `seq > processed_seq` of its partition. Eligible for delivery in `seq` order. May be delivered more than once (retry, lease expiry).
- `processed`: `seq <= processed_seq`. Terminal. The vacuum later deletes the outgoing and body rows.
- `dead-lettered`: the handler returned `Reject`. The message is in `toolkit_outbox_dead_letters` with status `pending`. It is not retried automatically. Operator actions: `replay` (→ `reprocessing`), `resolve` (→ `resolved`), `discard` (→ `discarded`).

## 5. Definitions of Done

### Provide Transactional Outbox Persistence

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-dod-usage-outbox-transactional`

For any domain operation defined to emit an outbox event, the system **MUST** persist the outbox rows (`toolkit_outbox_body`, `toolkit_outbox_incoming`) in the same DB transaction as that operation's committed side effects, and fire the returned wake handle only after commit.

**Implements**:
- `cpt-cf-mini-chat-flow-usage-outbox-enqueue`
- `cpt-cf-mini-chat-algo-usage-outbox-enqueue`

**Touches**:
- DB: `toolkit_outbox_body`, `toolkit_outbox_incoming`
- Components: outbox enqueuer port; producers: turn finalization, turn mutations (retry, edit, delete), attachment service, chat service, upload reaper

### Provide Stateful Usage Outbox Dispatcher

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-dod-usage-outbox-dispatcher`

The system **MUST** register every Mini Chat queue with a leased message handler at start-up so that:
- Messages are processed in order within a partition under a partition lease.
- Leases expire so that partitions held by a crashed instance are taken over.
- Transient failures are retried with backoff; permanent failures are dead-lettered.
- Dead letters are visible to operators: the usage and audit handlers log at `error` level on `Reject` (audit emits also record `result = reject` in metrics); the cleanup handlers and the thread-summary handler (bounded reject after `max_attempts`) log their rejects at `warn` level.

**Implements**:
- `cpt-cf-mini-chat-flow-usage-outbox-dispatch`
- `cpt-cf-mini-chat-algo-usage-outbox-claim`
- `cpt-cf-mini-chat-algo-usage-outbox-retry`
- `cpt-cf-mini-chat-state-usage-outbox-row`

**Touches**:
- DB: `toolkit_outbox_partitions`, `toolkit_outbox_outgoing`, `toolkit_outbox_processor`, `toolkit_outbox_dead_letters`
- Components: gear start-up (queue registration), usage and audit handlers, cleanup handlers, thread-summary handler

### Enforce Idempotent Publish Contract

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-dod-usage-outbox-idempotency`

The system **MUST** keep delivery safe under redelivery: every usage event carries a stable `dedupe_key`, and downstream consumers MUST be idempotent on it. The outbox library does not deduplicate.

**Implements**:
- `cpt-cf-mini-chat-flow-usage-outbox-dispatch`

**Touches**:
- Payload: `UsageEvent.dedupe_key` (Mini Chat SDK)

## 6. Acceptance Criteria

- [ ] For any domain operation defined to emit an outbox event, the outbox rows are committed in the same transaction as the operation's side effects; on rollback no outbox row exists.
- [ ] Messages of one partition are delivered in enqueue order; two instances never process the same partition while its lease is valid.
- [ ] If an instance crashes while holding a partition lease, another instance continues from `processed_seq` after the lease expires.
- [ ] A handler `Retry` redelivers the message with increasing delay; a `Reject` moves it to `toolkit_outbox_dead_letters` and processing of the partition continues.
- [ ] Thread-summary and cleanup messages that keep failing are dead-lettered after their configured `max_attempts`.
- [ ] Every usage event has a non-null `dedupe_key` in the canonical format, so the consumer can drop redeliveries.
