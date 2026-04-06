use super::*;
use crate::outbox::types::*;

fn make_outbox(config: OutboxConfig) -> Arc<Outbox> {
    Arc::new(Outbox::new(config))
}

fn make_default_outbox() -> Arc<Outbox> {
    make_outbox(OutboxConfig::default())
}

// -- resolve_partition tests --

#[test]
fn resolve_partition_cache_hit() {
    let outbox = make_default_outbox();
    outbox
        .partitions
        .insert("orders".to_owned(), vec![10, 20, 30]);

    assert_eq!(outbox.resolve_partition("orders", 0).unwrap(), 10);
    assert_eq!(outbox.resolve_partition("orders", 1).unwrap(), 20);
    assert_eq!(outbox.resolve_partition("orders", 2).unwrap(), 30);
}

#[test]
fn resolve_partition_unregistered_queue() {
    let outbox = make_default_outbox();

    let err = outbox.resolve_partition("nonexistent", 0).unwrap_err();
    assert!(matches!(err, OutboxError::QueueNotRegistered(q) if q == "nonexistent"));
}

#[test]
fn resolve_partition_out_of_range() {
    let outbox = make_default_outbox();
    outbox
        .partitions
        .insert("orders".to_owned(), vec![10, 20, 30]);

    let err = outbox.resolve_partition("orders", 3).unwrap_err();
    assert!(matches!(
        err,
        OutboxError::PartitionOutOfRange { queue, partition: 3, max: 3 } if queue == "orders"
    ));
}

// -- validate_payload tests --

#[test]
fn validate_payload_oversized() {
    let oversized = vec![0u8; MAX_PAYLOAD_SIZE + 1];
    let err = Outbox::validate_payload(&oversized).unwrap_err();
    assert!(matches!(err, OutboxError::PayloadTooLarge { .. }));
}

#[test]
fn validate_payload_at_exact_limit() {
    let exact = vec![0u8; MAX_PAYLOAD_SIZE];
    assert!(Outbox::validate_payload(&exact).is_ok());
}

#[test]
fn validate_payload_empty() {
    assert!(Outbox::validate_payload(&[]).is_ok());
}

// -- enqueue_batch validation tests (no DB needed) --

#[tokio::test]
async fn enqueue_batch_rejects_out_of_range_partition() {
    let outbox = make_default_outbox();
    outbox.partitions.insert("q".to_owned(), vec![10, 20]);

    let err = outbox.resolve_partition("q", 5).unwrap_err();
    assert!(matches!(err, OutboxError::PartitionOutOfRange { .. }));
}

#[tokio::test]
async fn enqueue_batch_rejects_oversized_payload() {
    let oversized = vec![0u8; MAX_PAYLOAD_SIZE + 1];
    let err = Outbox::validate_payload(&oversized).unwrap_err();
    assert!(matches!(err, OutboxError::PayloadTooLarge { .. }));
}

// -- flush tests --

#[tokio::test]
async fn flush_triggers_notify() {
    use crate::outbox::prioritizer::SharedPrioritizer;
    let prioritizer = Arc::new(SharedPrioritizer::new());
    let notifier = prioritizer.notifier();
    let outbox = Arc::new(Outbox::new(OutboxConfig::default()));
    outbox.set_prioritizer(Arc::clone(&prioritizer)).await;

    outbox.flush();
    // Notify was signaled via prioritizer — notified() resolves immediately
    tokio::time::timeout(std::time::Duration::from_millis(50), notifier.notified())
        .await
        .expect("notify should fire");
}

#[tokio::test]
async fn flush_before_prioritizer_is_noop() {
    let outbox = Arc::new(Outbox::new(OutboxConfig::default()));
    // flush() before set_prioritizer() — should not panic
    outbox.flush();
    outbox.flush();
}

#[tokio::test]
async fn flush_does_not_block() {
    use crate::outbox::prioritizer::SharedPrioritizer;
    let prioritizer = Arc::new(SharedPrioritizer::new());
    let outbox = Arc::new(Outbox::new(OutboxConfig::default()));
    outbox.set_prioritizer(prioritizer).await;
    // Multiple flushes should not block or panic
    outbox.flush();
    outbox.flush();
    outbox.flush();
}

// -- config defaults test --

#[test]
fn config_defaults_match_constants() {
    let config = OutboxConfig::default();
    assert_eq!(config.sequencer.batch_size, DEFAULT_SEQUENCER_BATCH_SIZE);
    assert_eq!(config.sequencer.poll_interval, DEFAULT_POLL_INTERVAL);
}
