use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::record_queue::{record_queue, serve};
use crate::domain::error::DomainError;
use crate::domain::record_intake::{ReceivedRecord, RecordHandOff};
use crate::test_support::received_chat_record;

const WAIT: Duration = Duration::from_secs(5);

fn size(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).expect("a size")
}

fn record() -> ReceivedRecord {
    received_chat_record(Uuid::new_v4(), Uuid::new_v4())
}

#[test]
fn a_full_queue_refuses_the_record() {
    let (hand_off, _queue) = record_queue(size(1));

    hand_off.hand_off(record()).expect("taken");
    let refused = hand_off.hand_off(record());

    assert!(
        matches!(refused, Err(DomainError::Unavailable(ref reason)) if reason == "the processing queue is full"),
        "{refused:?}"
    );
}

#[test]
fn a_stopped_queue_refuses_the_record() {
    let (hand_off, queue) = record_queue(size(4));
    drop(queue);

    let refused = hand_off.hand_off(record());

    assert!(
        matches!(refused, Err(DomainError::Unavailable(ref reason)) if reason == "processing has stopped"),
        "{refused:?}"
    );
}

#[tokio::test]
async fn each_taken_record_is_processed() {
    let (hand_off, queue) = record_queue(size(4));
    let (seen, mut processed) = mpsc::unbounded_channel();
    let records = [record(), record(), record()];
    for record in &records {
        hand_off.hand_off(record.clone()).expect("taken");
    }
    drop(hand_off);

    tokio::time::timeout(
        WAIT,
        serve(
            queue,
            move |record| {
                let seen = seen.clone();
                async move {
                    seen.send(record.tenant_id).expect("seen");
                }
            },
            size(2),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the worker ends when the queue is closed");

    let mut tenants = Vec::new();
    while let Ok(tenant) = processed.try_recv() {
        tenants.push(tenant);
    }
    tenants.sort();
    let mut expected: Vec<Uuid> = records.iter().map(|record| record.tenant_id).collect();
    expected.sort();
    assert_eq!(tenants, expected);
}

#[tokio::test]
async fn no_more_records_than_the_concurrency_run_at_once() {
    let (hand_off, queue) = record_queue(size(8));
    for _ in 0..5 {
        hand_off.hand_off(record()).expect("taken");
    }
    drop(hand_off);
    let gate = Arc::new(Semaphore::new(0));
    let running = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let worker = tokio::spawn(serve(
        queue,
        {
            let (gate, running, most, done) = (
                Arc::clone(&gate),
                Arc::clone(&running),
                Arc::clone(&most),
                Arc::clone(&done),
            );
            move |_| {
                let (gate, running, most, done) = (
                    Arc::clone(&gate),
                    Arc::clone(&running),
                    Arc::clone(&most),
                    Arc::clone(&done),
                );
                async move {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    gate.acquire().await.expect("open").forget();
                    running.fetch_sub(1, Ordering::SeqCst);
                    done.fetch_add(1, Ordering::SeqCst);
                }
            }
        },
        size(2),
        CancellationToken::new(),
    ));

    tokio::time::timeout(WAIT, async {
        while running.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("two records start");
    tokio::task::yield_now().await;
    assert_eq!(running.load(Ordering::SeqCst), 2);
    gate.add_permits(5);
    tokio::time::timeout(WAIT, worker)
        .await
        .expect("the worker ends")
        .expect("no panic");

    assert_eq!(
        (most.load(Ordering::SeqCst), done.load(Ordering::SeqCst)),
        (2, 5)
    );
}

#[tokio::test]
async fn a_cancel_stops_the_worker_and_the_records_in_flight() {
    let (hand_off, queue) = record_queue(size(4));
    hand_off.hand_off(record()).expect("taken");
    let started = Arc::new(Semaphore::new(0));
    let cancel = CancellationToken::new();
    let worker = tokio::spawn(serve(
        queue,
        {
            let started = Arc::clone(&started);
            move |_| {
                let started = Arc::clone(&started);
                async move {
                    started.add_permits(1);
                    std::future::pending::<()>().await;
                }
            }
        },
        size(2),
        cancel.clone(),
    ));
    tokio::time::timeout(WAIT, started.acquire())
        .await
        .expect("the record starts")
        .expect("open")
        .forget();

    cancel.cancel();
    tokio::time::timeout(WAIT, worker)
        .await
        .expect("the worker stops")
        .expect("no panic");

    assert!(matches!(
        hand_off.hand_off(record()),
        Err(DomainError::Unavailable(_))
    ));
}

#[tokio::test]
async fn a_cancel_stops_the_worker_waiting_for_a_record_after_the_queue_is_closed() {
    let (hand_off, queue) = record_queue(size(4));
    hand_off.hand_off(record()).expect("taken");
    drop(hand_off);
    let started = Arc::new(Semaphore::new(0));
    let cancel = CancellationToken::new();
    let worker = tokio::spawn(serve(
        queue,
        {
            let started = Arc::clone(&started);
            move |_| {
                let started = Arc::clone(&started);
                async move {
                    started.add_permits(1);
                    std::future::pending::<()>().await;
                }
            }
        },
        size(2),
        cancel.clone(),
    ));
    tokio::time::timeout(WAIT, started.acquire())
        .await
        .expect("the record starts")
        .expect("open")
        .forget();

    cancel.cancel();

    tokio::time::timeout(WAIT, worker)
        .await
        .expect("the worker stops")
        .expect("no panic");
}
