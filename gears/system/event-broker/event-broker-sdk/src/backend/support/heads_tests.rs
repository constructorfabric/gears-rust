use std::time::Duration;

use futures_util::StreamExt;

use super::LocalHeads;
use crate::backend::{BackendError, Head, Partition, TopicPartition};
use crate::{GtsInstanceId, Sequence};

fn at(partition: u32) -> TopicPartition {
    TopicPartition::new(
        GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.topic.v1")
            .expect("valid id"),
        Partition::new(partition),
    )
}

fn head(partition: u32, sequence: i64) -> Head {
    Head {
        at: at(partition),
        sequence: Sequence::assigned(sequence),
    }
}

#[tokio::test]
async fn watch_yields_current_heads_first_then_changes() {
    let heads = LocalHeads::new();
    let mut watch = heads.watch(&[at(0), at(1)], async { Ok(vec![head(0, 4), head(1, 0)]) });
    assert_eq!(watch.next().await.map(Result::unwrap), Some(head(0, 4)));
    assert_eq!(watch.next().await.map(Result::unwrap), Some(head(1, 0)));
    heads.publish(head(1, 2));
    assert_eq!(watch.next().await.map(Result::unwrap), Some(head(1, 2)));
}

#[tokio::test]
async fn watch_skips_unnamed_partitions_and_stale_heads() {
    let heads = LocalHeads::new();
    let mut watch = heads.watch(&[at(0)], async { Ok(vec![head(0, 4)]) });
    assert_eq!(watch.next().await.map(Result::unwrap), Some(head(0, 4)));
    heads.publish(head(1, 9));
    heads.publish(head(0, 3));
    heads.publish(head(0, 5));
    assert_eq!(watch.next().await.map(Result::unwrap), Some(head(0, 5)));
}

#[tokio::test]
async fn a_head_published_while_current_is_read_is_kept() {
    let heads = std::sync::Arc::new(LocalHeads::new());
    let publisher = std::sync::Arc::clone(&heads);
    let mut watch = heads.watch(&[at(0)], async move {
        publisher.publish(head(0, 6));
        Ok(vec![head(0, 5)])
    });
    assert_eq!(watch.next().await.map(Result::unwrap), Some(head(0, 5)));
    assert_eq!(watch.next().await.map(Result::unwrap), Some(head(0, 6)));
}

#[tokio::test]
async fn a_failed_current_read_is_yielded_and_ends_the_watch() {
    let heads = LocalHeads::new();
    let mut watch = heads.watch(&[at(0)], async { Err(BackendError::unavailable("down")) });
    match watch.next().await {
        Some(Err(BackendError::Unavailable { reason, detail })) => {
            assert_eq!((reason.as_str(), detail.as_str()), ("down", ""));
        }
        other => panic!("expected an Unavailable item, got {other:?}"),
    }
    assert!(watch.next().await.is_none());
}

#[tokio::test]
async fn publish_never_moves_a_head_back() {
    let heads = LocalHeads::new();
    heads.publish(head(0, 5));
    heads.publish(head(0, 3));
    assert_eq!(heads.known(&at(0)), Some(Sequence::assigned(5)));
}

#[tokio::test]
async fn wait_above_returns_on_publish_and_times_out_without() {
    let heads = std::sync::Arc::new(LocalHeads::new());
    let publisher = std::sync::Arc::clone(&heads);
    let waiter = tokio::spawn(async move {
        heads
            .wait_above(&at(0), Sequence::NONE, Duration::from_secs(5))
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    publisher.publish(head(0, 1));
    assert!(waiter.await.expect("waiter task"));

    let idle = LocalHeads::new();
    assert!(
        !idle
            .wait_above(&at(0), Sequence::NONE, Duration::from_millis(20))
            .await
    );
}

#[tokio::test]
async fn a_head_above_readable_events_does_not_spin_the_waiting_read() {
    let heads = LocalHeads::new();
    heads.publish(head(0, 5));
    let probes = std::sync::atomic::AtomicUsize::new(0);
    let events = heads
        .read_waiting(
            &at(0),
            Sequence::assigned(3),
            Duration::from_millis(200),
            Duration::from_millis(50),
            || {
                probes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async { Ok(Vec::new()) }
            },
        )
        .await
        .expect("empty probes do not fail");
    assert!(events.is_empty());
    let probes = probes.into_inner();
    assert!(
        (2..=6).contains(&probes),
        "one probe per poll interval over the wait, got {probes}"
    );
}

#[tokio::test]
async fn a_waiting_read_wakes_on_a_head_above_the_one_it_saw() {
    let heads = std::sync::Arc::new(LocalHeads::new());
    heads.publish(head(0, 5));
    let publisher = std::sync::Arc::clone(&heads);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        publisher.publish(head(0, 6));
    });
    let probes = std::sync::atomic::AtomicUsize::new(0);
    let started = tokio::time::Instant::now();
    let read = heads
        .read_waiting(
            &at(0),
            Sequence::assigned(3),
            Duration::from_secs(10),
            Duration::from_secs(10),
            || {
                let probe = probes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async move {
                    if probe == 0 {
                        Ok(Vec::new())
                    } else {
                        Err(BackendError::unavailable("woke"))
                    }
                }
            },
        )
        .await;
    match read {
        Err(BackendError::Unavailable { reason, detail }) => {
            assert_eq!((reason.as_str(), detail.as_str()), ("woke", ""));
        }
        other => panic!("expected the second probe's error, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "woke on the published head, not the poll interval"
    );
}
