#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use quota_enforcement_sdk::testing::RecordingSink;
use quota_enforcement_sdk::{
    DeliveryOutcome, DispatchError, NotificationDeliveryHandle, NotificationDeliveryV1,
    NotificationEvent, NotificationEventKind, QuotaEvent, QuotaNotificationSinkV1,
};
use toolkit_security::SecurityContext;

use super::{
    DISPATCHER_SUBJECT_ID, DISPATCHER_SUBJECT_TYPE, DeliveryLifecycle, DispatchLimits,
    NotificationDispatcher, dispatcher_context,
};
use crate::domain::quotas::events::{ChangeKind, quota_changed};
use crate::test_support::{RecordingMetrics, tenant};

const BUDGET: Duration = Duration::from_secs(28);

fn event() -> NotificationEvent {
    quota_changed(
        tenant(),
        None,
        None,
        ChangeKind::Created,
        time::OffsetDateTime::now_utc(),
    )
}

fn dispatcher(
    sinks: Vec<Arc<dyn QuotaNotificationSinkV1>>,
    metrics: &Arc<RecordingMetrics>,
) -> NotificationDispatcher {
    NotificationDispatcher::new(
        sinks,
        dispatcher_context().expect("context"),
        DispatchLimits::default(),
        Arc::clone(metrics) as _,
    )
}

fn transient(reason: &str) -> Result<(), DispatchError> {
    Err(DispatchError::Transient(reason.to_owned()))
}

/// A sink that never answers.
struct HangingSink {
    calls: AtomicUsize,
}

impl HangingSink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl QuotaNotificationSinkV1 for HangingSink {
    fn id(&self) -> &'static str {
        "hanging"
    }

    async fn dispatch(
        &self,
        _ctx: &SecurityContext,
        _event: QuotaEvent,
    ) -> Result<(), DispatchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[test]
fn the_dispatcher_context_is_a_named_system_subject_not_an_anonymous_one() {
    let ctx = dispatcher_context().expect("context");
    assert!(!ctx.is_anonymous());
    assert_eq!(ctx.subject_id(), DISPATCHER_SUBJECT_ID);
    assert_eq!(ctx.subject_type(), Some(DISPATCHER_SUBJECT_TYPE));
    assert_eq!(
        ctx.subject_tenant_id(),
        toolkit_security::constants::DEFAULT_TENANT_ID
    );
}

#[tokio::test]
async fn every_sink_receives_the_event_under_the_system_context() {
    let metrics = Arc::new(RecordingMetrics::default());
    let a = Arc::new(RecordingSink::new("a"));
    let b = Arc::new(RecordingSink::new("b"));
    let event = event();
    let outcome = dispatcher(vec![a.clone(), b.clone()], &metrics)
        .deliver(event.clone(), 0, BUDGET)
        .await;
    assert_eq!(outcome, DeliveryOutcome::Delivered);
    for sink in [&a, &b] {
        let received = sink.received();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].0.subject_id(), DISPATCHER_SUBJECT_ID);
        assert_eq!(received[0].1.event_id, event.event_id);
    }
    assert_eq!(metrics.dispatch_failures(), Vec::new());
    assert_eq!(metrics.outbox_rejections(), 0);
}

#[tokio::test]
async fn without_sinks_an_event_is_acknowledged_undelivered() {
    let metrics = Arc::new(RecordingMetrics::default());
    let outcome = dispatcher(Vec::new(), &metrics)
        .deliver(event(), 0, BUDGET)
        .await;
    assert_eq!(outcome, DeliveryOutcome::Delivered);
    assert_eq!(metrics.outbox_rejections(), 0);
}

#[tokio::test]
async fn a_transient_failure_retries_for_every_sink_and_is_counted_against_its_sink() {
    let metrics = Arc::new(RecordingMetrics::default());
    let failing = Arc::new(RecordingSink::answering("failing", vec![transient("busy")]));
    let healthy = Arc::new(RecordingSink::new("healthy"));
    let outcome = dispatcher(vec![failing.clone(), healthy.clone()], &metrics)
        .deliver(event(), 0, BUDGET)
        .await;
    assert_eq!(outcome, DeliveryOutcome::Retry);
    assert_eq!(
        healthy.received().len(),
        1,
        "the healthy sink was still called"
    );
    assert_eq!(
        metrics.dispatch_failures(),
        vec![("failing".to_owned(), NotificationEventKind::QuotaChanged)]
    );
    assert_eq!(metrics.outbox_rejections(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_sink_that_does_not_answer_times_out_without_holding_up_the_others() {
    let metrics = Arc::new(RecordingMetrics::default());
    let hanging = HangingSink::new();
    let healthy = Arc::new(RecordingSink::new("healthy"));
    let started = tokio::time::Instant::now();
    let outcome = dispatcher(vec![hanging.clone(), healthy.clone()], &metrics)
        .deliver(event(), 0, BUDGET)
        .await;
    assert_eq!(outcome, DeliveryOutcome::Retry);
    assert_eq!(started.elapsed(), DispatchLimits::default().sink_timeout);
    assert_eq!(hanging.calls.load(Ordering::SeqCst), 1);
    assert_eq!(healthy.received().len(), 1);
    assert_eq!(
        metrics.dispatch_failures(),
        vec![("hanging".to_owned(), NotificationEventKind::QuotaChanged)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_call_never_outlasts_the_lease_budget() {
    let metrics = Arc::new(RecordingMetrics::default());
    let budget = Duration::from_millis(250);
    let started = tokio::time::Instant::now();
    let outcome = dispatcher(vec![HangingSink::new()], &metrics)
        .deliver(event(), 0, budget)
        .await;
    assert_eq!(outcome, DeliveryOutcome::Retry);
    assert_eq!(
        started.elapsed(),
        budget,
        "the budget, not the 2 s sink timeout"
    );
}

#[tokio::test]
async fn a_permanent_failure_dead_letters_at_once_and_counts_a_rejection() {
    let metrics = Arc::new(RecordingMetrics::default());
    let refusing = Arc::new(RecordingSink::answering(
        "refusing",
        vec![Err(DispatchError::Permanent("schema".to_owned()))],
    ));
    let busy = Arc::new(RecordingSink::answering("busy", vec![transient("busy")]));
    let outcome = dispatcher(vec![refusing, busy], &metrics)
        .deliver(event(), 0, BUDGET)
        .await;
    let DeliveryOutcome::Reject(reason) = outcome else {
        panic!("expected a rejection, got {outcome:?}");
    };
    assert!(reason.contains("refusing"), "{reason}");
    assert_eq!(metrics.outbox_rejections(), 1);
    assert_eq!(
        metrics.dispatch_failures().len(),
        2,
        "every failing sink is counted"
    );
}

#[tokio::test]
async fn a_still_failing_event_is_retried_below_the_limit_and_dead_lettered_at_it() {
    let metrics = Arc::new(RecordingMetrics::default());
    let limits = DispatchLimits::default();
    let sink = Arc::new(RecordingSink::answering(
        "busy",
        vec![transient("busy"), transient("busy")],
    ));
    let dispatcher = NotificationDispatcher::new(
        vec![sink],
        dispatcher_context().expect("context"),
        limits,
        Arc::clone(&metrics) as _,
    );
    assert_eq!(
        dispatcher
            .deliver(event(), limits.max_attempts - 1, BUDGET)
            .await,
        DeliveryOutcome::Retry
    );
    assert_eq!(metrics.outbox_rejections(), 0);
    let outcome = dispatcher
        .deliver(event(), limits.max_attempts, BUDGET)
        .await;
    let DeliveryOutcome::Reject(reason) = outcome else {
        panic!("expected a rejection at the limit, got {outcome:?}");
    };
    assert!(reason.contains("failed 11 times"), "{reason}");
    assert_eq!(metrics.outbox_rejections(), 1);
}

#[test]
fn an_undecodable_row_counts_one_rejection() {
    let metrics = Arc::new(RecordingMetrics::default());
    dispatcher(Vec::new(), &metrics).undeliverable("quota-changed", "not json");
    assert_eq!(metrics.outbox_rejections(), 1);
}

/// A handle that counts its stops.
struct CountingHandle(Arc<AtomicUsize>);

#[async_trait]
impl NotificationDeliveryHandle for CountingHandle {
    async fn stop(self: Box<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn the_lifecycle_stops_the_pipeline_it_holds_once() {
    let stops = Arc::new(AtomicUsize::new(0));
    let lifecycle = DeliveryLifecycle::default();
    assert!(!lifecycle.is_running());
    lifecycle.stop().await;
    assert!(
        lifecycle
            .hold(Box::new(CountingHandle(Arc::clone(&stops))))
            .is_none()
    );
    assert!(lifecycle.is_running());
    lifecycle.stop().await;
    lifecycle.stop().await;
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert!(!lifecycle.is_running());
}
