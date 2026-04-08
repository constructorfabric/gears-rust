# Transactional Outbox

Reliable async message production with per-partition ordering guarantees.
Supports PostgreSQL, MySQL/MariaDB, and SQLite.

Four-stage pipeline: enqueue (inside your transaction) -> sequencer
(assigns per-partition sequence numbers) -> processor (calls your handler)
-> vacuum (GC). Two processing modes: transactional (exactly-once) and
decoupled (at-least-once with lease-based locking).

## Usage

### Single-message handler (decoupled)

```rust
struct OrderHandler;

#[async_trait]
impl MessageHandler for OrderHandler {
    async fn handle(&self, msg: &OutboxMessage, cancel: CancellationToken) -> HandlerResult {
        let order: Order = serde_json::from_slice(&msg.payload).unwrap();
        match send_to_warehouse(&order).await {
            Ok(_) => HandlerResult::Success,
            Err(e) if msg.attempts > 3 => HandlerResult::Reject { reason: e.to_string() },
            Err(e) => HandlerResult::Retry { reason: e.to_string() },
        }
    }
}

let handle = Outbox::builder(db)
    .profile(OutboxProfile::low_latency())
    .queue("orders", Partitions::of(4))
        .decoupled(OrderHandler)
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
        audit_log::ActiveModel { payload: Set(msg.payload.clone()), .. }
            .insert(txn).await.unwrap();
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
outbox.enqueue(&txn, "orders", partition, payload, "application/json").await?;

// Batch enqueue:
outbox.enqueue_batch(&txn, "orders", &[
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
        .decoupled(OrderHandler)
    .queue("notifications", Partitions::of(4))
        .decoupled_with(NotifyHandler, DecoupledConfig {
            lease_duration: Duration::from_secs(60),
            ..Default::default()
        })
    .start().await?;

// Graceful shutdown
handle.stop().await;
```

## Benchmarks

### Worker Overhead

Infrastructure overhead (scheduling, notifiers, semaphores) with no-op actions:

```bash
cargo bench -p cf-modkit-db --features preview-outbox --bench worker_overhead
```

### Outbox Throughput

End-to-end throughput with per-partition ordering verification.
Requires a database feature flag:

```bash
# SQLite (local, no external DB needed)
cargo bench -p cf-modkit-db --features preview-outbox,sqlite --bench outbox_throughput

# PostgreSQL
cargo bench -p cf-modkit-db --features preview-outbox,pg --bench outbox_throughput -- postgres

# MySQL
cargo bench -p cf-modkit-db --features preview-outbox,mysql --bench outbox_throughput -- mysql
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
  cargo bench -p cf-modkit-db --features preview-outbox --bench worker_overhead \
  -- --warm-up-time 1 --measurement-time 3 --sample-size 10
```


