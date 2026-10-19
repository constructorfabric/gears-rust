#![allow(clippy::expect_used, clippy::unwrap_used)]

use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage, Partitions};

use super::{
    CHILD_WORK_QUEUE, LayoutError, OUTBOX_TABLE_PREFIX, QueueLayout, QueuePartitions, ROLLUP_QUEUE,
    UnwiredQueue, delivery_queue,
};

fn partitions(child_work: u16, rollup: u16, delivery: u16) -> QueuePartitions {
    QueuePartitions {
        child_work: Partitions::of(child_work),
        rollup: Partitions::of(rollup),
        delivery: Partitions::of(delivery),
    }
}

#[test]
fn every_queue_name_is_valid_for_the_outbox() {
    let layout = QueueLayout::new(partitions(16, 8, 8), vec![1, 2]).unwrap();
    for (queue, _) in layout.queues() {
        toolkit_db::outbox::Record::to(&queue, 0)
            .payload(Vec::new(), "probe")
            .build()
            .unwrap_or_else(|e| panic!("{queue}: {e}"));
    }
}

#[test]
fn a_layout_never_declares_a_queue_twice() {
    let layout = |epochs: Vec<u32>| QueueLayout::new(partitions(1, 1, 1), epochs);
    for epochs in [
        vec![],
        vec![0],
        vec![1, 1],
        vec![2, 1, 2],
        vec![1, 2, 3, 4, 5],
    ] {
        assert_eq!(
            layout(epochs.clone()),
            Err(LayoutError::FeedEpochs),
            "{epochs:?}"
        );
    }
    let queues: Vec<String> = layout(vec![2, 1])
        .unwrap()
        .queues()
        .into_iter()
        .map(|(q, _)| q)
        .collect();
    let mut distinct = queues.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), queues.len());
}

#[test]
fn each_kind_of_queue_gets_its_own_partition_count() {
    let layout = QueueLayout::new(partitions(16, 8, 4), vec![1, 2]).unwrap();
    assert_eq!(
        layout.queues(),
        vec![
            (CHILD_WORK_QUEUE.to_owned(), Partitions::of(16)),
            (ROLLUP_QUEUE.to_owned(), Partitions::of(8)),
            (delivery_queue(1), Partitions::of(4)),
            (delivery_queue(2), Partitions::of(4)),
        ]
    );
    // Up to the bound, every epoch is served.
    let epochs: Vec<u32> = (1..=4).collect();
    assert_eq!(
        QueueLayout::new(partitions(1, 1, 1), epochs.clone())
            .unwrap()
            .feed_epochs(),
        epochs.as_slice()
    );
}

#[test]
fn delivery_queues_are_per_feed_epoch() {
    assert_eq!(delivery_queue(1), "rating.delivery.e1");
    assert_eq!(delivery_queue(12), "rating.delivery.e12");
}

#[test]
fn the_outbox_tables_live_in_the_rating_namespace() {
    assert!(OUTBOX_TABLE_PREFIX.starts_with("bss_rating__"));
    assert!(toolkit_db::outbox::outbox_migrations_with_prefix(OUTBOX_TABLE_PREFIX).is_ok());
}

#[tokio::test]
async fn a_queue_with_no_worker_yet_keeps_its_message() {
    let message = OutboxMessage {
        partition_id: 0,
        seq: 7,
        payload: b"work".to_vec(),
        payload_type: "rating.child_work".to_owned(),
        created_at: chrono::Utc::now(),
        attempts: 3,
    };
    // `Retry` leaves the message where it is for the worker its feature brings; neither `Ok`
    // nor `Reject` may lose it.
    assert!(matches!(
        UnwiredQueue::new(CHILD_WORK_QUEUE).handle(&message).await,
        MessageResult::Retry
    ));
}
