# Transactional Outbox

Reliable async message production with per-partition ordering guarantees.
Supports PostgreSQL, MySQL/MariaDB, and SQLite.

Four-stage pipeline: enqueue (inside your transaction) -> sequencer
(assigns per-partition sequence numbers) -> processor (calls your handler)
-> vacuum (GC). Two processing modes: transactional (exactly-once) and
leased (at-least-once with lease-based locking and framework-managed
cancellation).

## Usage

Default usage keeps the existing `toolkit_outbox_*` tables:

```rust
run_migrations_for_testing(&db, outbox_migrations()).await?;

let handle = Outbox::builder(db)
    .queue("orders", Partitions::of(4))
    .leased(OrderHandler { client })
    .start().await?;
```

### Custom table prefix

Use the same prefix for migrations and the runtime builder. The prefix creates
a complete independent table family by appending fixed suffixes such as
`_body`, `_partitions`, `_incoming`, `_outgoing`, and `_dead_letters`.

```rust
run_migrations_for_testing(
    &db,
    outbox_migrations_with_prefix("mini_chat_outbox")?,
).await?;

let handle = Outbox::builder(db)
    .table_prefix("mini_chat_outbox")?
    .queue("orders", Partitions::of(4))
    .leased(OrderHandler { client })
    .start().await?;
```

Changing the prefix points the outbox at a different table family. It does not
rename tables or move existing rows. To move data between prefixes, drain or
migrate the rows explicitly.

Prefixes are validated before SQL is generated: they must be non-empty ASCII
identifiers, start with a letter, contain only letters, digits, and underscores,
and stay short enough that derived table and index names fit common backend
identifier limits. Schema-qualified input such as `public.outbox`, quoting,
spaces, semicolons, and other punctuation are rejected. Table and index names
are SQL identifiers, so they cannot be bound as query parameters; only validated
identifiers are interpolated, while queue names, payloads, partition IDs, lease
IDs, limits, and filters remain SQL parameters.

At runtime, the validated table names and detected backend are compiled once
into an internal `OutboxStatements` catalog. Fixed-shape SQL is reused from that
catalog instead of being rebuilt through default table-name replacement on every
operation. Dynamic SQL is still built for runtime-cardinality cases such as
multi-row inserts and `IN (...)` lists.

For MySQL, migrations also create `<prefix>_body_id_sequence` and
`<prefix>_incoming_id_sequence`. Batch enqueue reserves body IDs first and
incoming IDs second by locking the singleton row with `SELECT ... FOR UPDATE`,
advancing `next_id`, then inserting rows with explicit IDs. On MySQL-compatible
clusters such as Percona XtraDB Cluster/Galera, retry the whole transaction on
deadlock, serialization, or certification-conflict errors.

### Single-message handler (leased)

```rust
struct OrderHandler {
    client: HttpClient,
}

#[async_trait]
impl LeasedMessageHandler for OrderHandler {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        let order: Order = match serde_json::from_slice(&msg.payload) {
            Ok(o) => o,
            Err(e) => return MessageResult::Reject(format!("bad payload: {e}")),
        };
        match self.client.post(&warehouse_url).json(&order).send().await {
            Ok(resp) if resp.status().is_success() => MessageResult::Ok,
            Ok(_) | Err(_) => MessageResult::Retry,
        }
    }
}

let handle = Outbox::builder(db)
.profile(OutboxProfile::low_latency())
.queue("orders", Partitions::of(4))
.leased(OrderHandler { client })
.start().await?;
```

### Transactional handler (exactly-once with DB writes)

```rust
struct AuditHandler;

#[async_trait]
impl TransactionalMessageHandler for AuditHandler {
    async fn handle(
        &self,
        txn: &dyn ConnectionTrait,
        msg: &OutboxMessage,
        _cancel: CancellationToken,
    ) -> HandlerResult {
        // DB writes here are atomic with the ack
        if let Err(e) = audit_log::ActiveModel { payload: Set(msg.payload.clone()), .. }
            .insert(txn).await
        {
            return HandlerResult::Retry { reason: format!("insert failed: {e}") };
        }
        HandlerResult::Success
    }
}

let handle = Outbox::builder(db)
.queue("audit", Partitions::of(2))
.transactional(AuditHandler)
.start().await?;
```

### Enqueue (inside a business transaction)

```rust
let outbox = handle.outbox();
// Atomic with your business logic:
outbox.enqueue( & txn, "orders", partition, payload, "application/json").await?;

// Batch enqueue:
outbox.enqueue_batch( & txn, "orders", & [
EnqueueMessage { partition: 0, payload: p1, payload_type: "application/json" },
EnqueueMessage { partition: 1, payload: p2, payload_type: "application/json" },
]).await?;
```

### Multi-queue with tuning

```rust
let handle = Outbox::builder(db)
.profile(OutboxProfile::high_throughput())
.sequencer_tuning(WorkerTuning::sequencer_high_throughput().batch_size(500))
.vacuum_tuning(WorkerTuning::vacuum().idle_interval(Duration::from_secs(300)))
.queue("orders", Partitions::of(16))
.leased(OrderHandler { client: client.clone() })
.queue("notifications", Partitions::of(4))
.leased(NotifyHandler { client })
.lease(LeaseConfig {
duration: Duration::from_secs(60),
headroom: Duration::from_secs(5),
})
.start().await?;

// Graceful shutdown
handle.stop().await;
```

---

## Use-Case Scenarios

### Handler makes a remote HTTP call

The most common case. Implement `LeasedMessageHandler` - one message, one
result. Use `HttpClient` from `cf-gears-toolkit-http` for the outgoing call.

**Cancellation is framework-managed.** The processor drops the handler
future when the lease cancel point is reached (`lease_duration - ack_headroom`).
In-flight `HttpClient` calls are cancelled via drop - hyper closes the
connection.

```rust
use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use toolkit_http::HttpClient;

pub struct WebhookHandler {
    client: HttpClient,
    url: String,
}

#[async_trait::async_trait]
impl LeasedMessageHandler for WebhookHandler {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        let event: Event = match serde_json::from_slice(&msg.payload) {
            Ok(e) => e,
            Err(e) => return MessageResult::Reject(format!("bad payload: {e}")),
        };

        // HttpClient handles per-attempt timeout (30s) and retries (3x).
        // The Idempotency-Key header enables POST retry in the retry layer.
        let idempotency_key = format!("{}:{}", msg.partition_id, msg.seq);

        match self.client
            .post(&self.url)
            .header("Idempotency-Key", &idempotency_key)
            .json(&event)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => MessageResult::Ok,
            Ok(_) | Err(_) => MessageResult::Retry,
        }
    }
}
```

**Idempotency is required.** Leased processing provides at-least-once
delivery. If the lease expires before the ack transaction, the message is
re-delivered. The `Idempotency-Key` header (derived from `partition_id`
and `seq`) also enables `HttpClient` retry for POST/PATCH requests.

### Handler makes multiple sequential calls

When a single message requires several calls (enrich -> transform ->
publish), use `tokio::time::timeout_at` with a shared deadline:

```rust
#[async_trait::async_trait]
impl LeasedMessageHandler for PipelineHandler {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);

        let enriched = match tokio::time::timeout_at(
            deadline,
            self.enrichment_api.enrich(msg),
        ).await {
            Ok(Ok(data)) => data,
            _ => return MessageResult::Retry,
        };

        let transformed = match tokio::time::timeout_at(
            deadline,
            self.transform_api.transform(&enriched),
        ).await {
            Ok(Ok(data)) => data,
            _ => return MessageResult::Retry,
        };

        match tokio::time::timeout_at(
            deadline,
            self.publish_api.publish(&transformed),
        ).await {
            Ok(Ok(())) => MessageResult::Ok,
            _ => MessageResult::Retry,
        }
    }
}
```

`tokio::time::timeout_at` uses an absolute deadline - each call gets the
remaining budget, not a fresh timeout. If enrichment takes 15s, transform
and publish share the remaining 5s.

### Handler processes a batch with chunked API calls

Implement `LeasedHandler` directly for batch/chunked processing. Use
`Batch` for iteration, progress tracking, and remaining lease time:

```rust
use toolkit_db::outbox::{Batch, HandlerResult, LeasedHandler};

#[async_trait::async_trait]
impl LeasedHandler for BulkExportHandler {
    async fn handle(&self, batch: &mut Batch<'_>) -> HandlerResult {
        while !batch.is_empty() {
            let chunk = batch.next_chunk(10);
            if chunk.is_empty() {
                break;
            }

            let mut events = Vec::with_capacity(chunk.len());
            for msg in chunk {
                match serde_json::from_slice::<Event>(&msg.payload) {
                    Ok(e) => events.push(e),
                    Err(e) => {
                        return HandlerResult::Reject {
                            reason: format!("bad payload at seq {}: {e}", msg.seq),
                        };
                    }
                }
            }

            // batch.remaining() returns time until the lease cancel point.
            // The handler owns timeout decisions - the framework only
            // exposes facts (remaining time, message count).
            let timeout = batch.remaining() / 3;

            match tokio::time::timeout(timeout, self.api.bulk_send(&events)).await {
                Ok(Ok(())) => batch.ack_chunk(10),
                Ok(Err(e)) if e.is_transient() => {
                    return HandlerResult::Retry { reason: e.to_string() };
                }
                Ok(Err(e)) => {
                    return HandlerResult::Reject { reason: e.to_string() };
                }
                Err(_) => {
                    return HandlerResult::Retry { reason: "chunk timeout".into() };
                }
            }
        }
        HandlerResult::Success
    }
}
```

Chunk ack is all-or-nothing. If a bulk API partially succeeds, do NOT call
`ack_chunk()` - return `Retry` instead. Idempotency handles the duplicate.

### Handler needs fire-and-forget side work

Use `tokio::spawn` when work must survive handler cancellation (e.g.,
best-effort metrics). The spawned task runs independently - it is NOT
cancelled when the handler future is dropped.

```rust
async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
    let event = match serde_json::from_slice(&msg.payload) {
        Ok(e) => e,
        Err(e) => return MessageResult::Reject(format!("bad payload: {e}")),
    };

    // Main work - cancelled if the handler future is dropped.
    if let Err(e) = self.api.send(&event).await {
        return MessageResult::Retry;
    }

    // Fire-and-forget - survives handler cancellation.
    let metrics = self.metrics.clone();
    tokio::spawn(async move {
        if let Err(e) = metrics.record_delivery(&event).await {
            tracing::warn!(error = %e, "fire-and-forget metric failed");
        }
    });

    MessageResult::Ok
}
```

Use `tokio::spawn` only for best-effort side effects (metrics, logging,
notifications). Avoid it for main business logic - spawned tasks have no
lease guarantee and no backpressure.

### Custom lease for slow handlers

Chain `.lease(LeaseConfig { .. })` after `.leased()`. Defaults: 30s
duration, 2s headroom. The handler cancel point fires at
`duration - headroom`:

```rust
use toolkit_db::outbox::LeaseConfig;

Outbox::builder(db)
    .queue("slow-export", partitions)
    .leased(SlowHandler { api })
    .lease(LeaseConfig {
        duration: Duration::from_secs(300),
        headroom: Duration::from_secs(5),
    })
    .start()
    .await?;
```

The headroom reserves time for the ack DB round-trip after the handler
finishes. It is a fixed cost (not a percentage of the lease).

### Error mapping: HTTP status -> MessageResult

**Do not blanket-reject all 4xx.** A 4xx may come from an intermediate
proxy, API gateway, or service mesh sidecar - not the target service.
Only reject when you are confident the error is permanent and caused by
the message payload itself.

| HTTP status           | `MessageResult` | Rationale                                                                |
|-----------------------|-----------------|--------------------------------------------------------------------------|
| 2xx                   | `Ok`            | Success                                                                  |
| 429                   | `Retry`         | Rate limited - `HttpClient` retries this automatically                   |
| 500 / 502 / 503 / 504 | `Retry`         | Server-side transient error                                              |
| 401 / 403             | `Retry`         | Likely transient - token rotation, IAM propagation delay                 |
| 404                   | `Retry`         | Resource may not be provisioned yet (eventual consistency)               |
| 409                   | `Retry`         | Concurrent write conflict                                                |
| 400                   | `Reject`        | Malformed payload - a bug in the producer, will not self-resolve         |
| 422                   | `Reject`        | Validation failure - the payload content is wrong                        |
| 410                   | `Reject`        | Resource explicitly deleted - no point retrying                          |
| Timeout               | `Retry`         | The call may have succeeded server-side - idempotency handles redelivery |
| Transport error       | `Retry`         | Network issue, connection reset                                          |

**When in doubt, `Retry`.** Dead-lettering is permanent - it removes the
message from processing. A retry that succeeds on the next cycle costs
almost nothing. A false reject loses the message until someone manually
replays it from the dead-letter table.

**`Reject` is for bugs, not for infrastructure.** If the remote service
returns an error because of the message content (bad schema, missing
required field, invalid enum value), that is a `Reject` - the message
will never succeed no matter how many times it is retried. Everything
else is infrastructure noise that will resolve itself.

---

## Benchmarks

### Worker Overhead

Infrastructure overhead (scheduling, notifiers, semaphores) with no-op actions:

```bash
cargo bench -p cf-gears-toolkit-db --features preview-outbox --bench worker_overhead
```

### Outbox Throughput

End-to-end throughput with per-partition ordering verification.
Requires a database feature flag:

```bash
# SQLite (local, no external DB needed)
cargo bench -p cf-gears-toolkit-db --features preview-outbox,sqlite --bench outbox_throughput

# PostgreSQL
cargo bench -p cf-gears-toolkit-db --features preview-outbox,pg --bench outbox_throughput -- postgres

# MySQL
cargo bench -p cf-gears-toolkit-db --features preview-outbox,mysql --bench outbox_throughput -- mysql
```

### Makefile Targets

```bash
make bench-pg              # PostgreSQL standard
make bench-pg-longhaul     # PostgreSQL 1M + 10M messages
make bench-mysql           # MySQL standard
make bench-sqlite          # SQLite standard
make bench-db              # All engines
make bench-db-longhaul     # All engines, long-haul
```

### Resource-Limited Runs

```bash
systemd-run --user --scope -p MemoryMax=4G -p CPUQuota=200% \
  cargo bench -p cf-gears-toolkit-db --features preview-outbox --bench worker_overhead \
  -- --warm-up-time 1 --measurement-time 3 --sample-size 10
```
