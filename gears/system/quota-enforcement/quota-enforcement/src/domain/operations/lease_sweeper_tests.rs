#![allow(clippy::expect_used)]
//! The lease sweeper over the in-memory double: it drains expired leases in
//! batches, publishes the backlog it leaves, and withdraws that sample when a
//! pass fails or leadership ends.

use std::sync::{Arc, Mutex};

use quota_enforcement_sdk::{AcquireLeaseOutcome, AcquireLeaseRequest, LeaseState, StorageError};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::super::harness_tests::{Harness, attribution};
use super::{LeaseSweepTiming, LeaseSweeper};
use crate::domain::ports::metrics::{LeaseBacklog, LeaseBacklogSink, MetricLabel};
use crate::test_support::ctx;

/// Every sample the sweeper published, in order.
#[derive(Default)]
struct Samples(Mutex<Vec<Option<LeaseBacklog>>>);

impl Samples {
    fn all(&self) -> Vec<Option<LeaseBacklog>> {
        self.0.lock().expect("lock").clone()
    }
}

impl LeaseBacklogSink for Samples {
    fn publish(&self, backlog: Option<LeaseBacklog>) {
        self.0.lock().expect("lock").push(backlog);
    }
}

fn tokens_label(h: &Harness) -> MetricLabel {
    h.classifications
        .label(
            &quota_enforcement_sdk::MetricId::parse(crate::test_support::METRIC_TOKENS)
                .expect("metric"),
        )
        .expect("the harness classifies the tokens metric")
}

fn sweeper(h: &Harness, batch: u32, samples: &Arc<Samples>) -> LeaseSweeper {
    LeaseSweeper::new(
        h.storage.clone(),
        Arc::new(h.classifications.clone()),
        Arc::clone(samples) as Arc<dyn LeaseBacklogSink>,
        LeaseSweepTiming {
            interval: std::time::Duration::from_millis(10),
            batch_size: std::num::NonZeroU32::new(batch).expect("batch"),
        },
    )
}

async fn leases(h: &Harness, count: usize) -> Vec<quota_enforcement_sdk::LeaseToken> {
    let mut tokens = Vec::new();
    for n in 0..count {
        let outcome = h
            .operations()
            .acquire_lease(
                &ctx(),
                AcquireLeaseRequest {
                    attribution: attribution(),
                    amount: 1,
                    ttl_secs: Some(60),
                    idempotency_key: format!("lease-{n}"),
                },
            )
            .await
            .expect("acquire");
        let AcquireLeaseOutcome::Acquired { token, .. } = outcome else {
            panic!("denied: {outcome:?}");
        };
        tokens.push(token);
    }
    tokens
}

#[tokio::test]
async fn a_sweep_drains_every_expired_lease_in_batches_and_publishes_what_is_left() {
    let h = Harness::new().await;
    let id = h.quota(Some(100)).await;
    let tokens = leases(&h, 3).await;
    h.storage.expire_leases();
    let samples = Arc::new(Samples::default());

    // Batches of two: a full batch, then a short one that ends the pass.
    let report = sweeper(&h, 2, &samples)
        .sweep_once(&CancellationToken::new(), OffsetDateTime::now_utc())
        .await;

    assert_eq!(report.reclaimed, 3);
    assert!(!report.failed);
    for token in tokens {
        assert_eq!(h.storage.lease_state(token), Some(LeaseState::AutoReleased));
    }
    assert_eq!(h.consumed(id), 0, "the holds went back");
    assert_eq!(
        samples.all(),
        vec![Some(vec![(tokens_label(&h), 0)])],
        "an empty backlog is a zero per quota-gated metric, not a missing point"
    );
}

#[tokio::test]
async fn an_aborted_sweep_loop_withdraws_the_sample() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let samples = Arc::new(Samples::default());
    let sweeper = Arc::new(sweeper(&h, 10, &samples));
    // Never cancelled: the coordinator's abort after an overrun stop budget
    // drops the loop mid-flight instead.
    let running = tokio::spawn({
        let sweeper = Arc::clone(&sweeper);
        async move { sweeper.run(CancellationToken::new()).await }
    });
    for _ in 0..100 {
        if !samples.all().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        samples.all().first().is_some_and(Option::is_some),
        "a leader publishes"
    );

    running.abort();
    assert!(running.await.expect_err("aborted").is_cancelled());

    assert_eq!(
        samples.all().last(),
        Some(&None),
        "an aborted leader must not leave its sample behind"
    );
}

#[tokio::test]
async fn a_failed_pass_withdraws_the_backlog_sample() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    leases(&h, 1).await;
    h.storage.expire_leases();
    h.storage
        .fail_with(StorageError::Unavailable("backend down".to_owned()));
    let samples = Arc::new(Samples::default());

    let report = sweeper(&h, 10, &samples)
        .sweep_once(&CancellationToken::new(), OffsetDateTime::now_utc())
        .await;

    assert!(report.failed);
    assert_eq!(report.reclaimed, 0);
    assert_eq!(
        samples.all(),
        vec![None],
        "a stale count would hide the outage"
    );
}

#[tokio::test]
async fn a_cancelled_sweep_starts_no_batch() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let tokens = leases(&h, 1).await;
    h.storage.expire_leases();
    let samples = Arc::new(Samples::default());
    let cancel = CancellationToken::new();
    cancel.cancel();

    let report = sweeper(&h, 10, &samples)
        .sweep_once(&cancel, OffsetDateTime::now_utc())
        .await;

    assert_eq!(report.reclaimed, 0);
    assert_eq!(h.storage.lease_state(tokens[0]), Some(LeaseState::Active));
}

#[tokio::test]
async fn leadership_ending_withdraws_the_sample() {
    let h = Harness::new().await;
    h.quota(Some(100)).await;
    let samples = Arc::new(Samples::default());
    let sweeper = Arc::new(sweeper(&h, 10, &samples));
    let cancel = CancellationToken::new();
    let running = tokio::spawn({
        let sweeper = Arc::clone(&sweeper);
        let cancel = cancel.clone();
        async move { sweeper.run(cancel).await }
    });

    // The first tick fires at once; wait for its sample, then step down.
    for _ in 0..100 {
        if !samples.all().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    cancel.cancel();
    running.await.expect("join");

    let all = samples.all();
    assert!(
        all.first().is_some_and(Option::is_some),
        "a leader publishes"
    );
    assert_eq!(
        all.last(),
        Some(&None),
        "a replica that stepped down publishes nothing"
    );
}
