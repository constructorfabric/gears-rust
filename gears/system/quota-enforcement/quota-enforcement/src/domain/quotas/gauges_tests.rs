#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::testing::{InMemoryStorage, quota_draft, test_subject};
use quota_enforcement_sdk::{
    CapPatch, MetricId, QuotaEnforcementStoragePluginV1, QuotaId, QuotaPatch, StorageError,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_security::AccessScope;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

use super::{GaugeTiming, LifecycleGaugeRefresher, RefreshError};
use crate::domain::catalog::MetricCatalog;
use crate::domain::ports::lifecycle_gauges::{LifecycleCounts, LifecycleGaugeSink};
use crate::infra::types_registry::PlatformTypeRegistry;
use crate::test_support::{
    METRIC_OTHER, METRIC_REQUESTS, METRIC_TOKENS, RecordingGaugeSink, RecordingMetrics, actor,
    fail_registry_reads, metric_type_document, platform_registry, tenant,
};

const DIRECT_METRIC: &str = "gts.cf.core.quota.metric.v1~cf.qetest.metric.direct.v1~";

const REFRESH: Duration = Duration::from_secs(10);
const DEADLINE: Duration = Duration::from_secs(5);
const STALE_AFTER: Duration = Duration::from_secs(25);

fn timing() -> GaugeTiming {
    GaugeTiming {
        refresh: REFRESH,
        refresh_deadline: DEADLINE,
        stale_after: STALE_AFTER,
    }
}

struct Harness {
    storage: Arc<InMemoryStorage>,
    registry: Arc<MockTypesRegistry>,
    sink: Arc<RecordingGaugeSink>,
    refresher: Arc<LifecycleGaugeRefresher>,
}

impl Harness {
    fn new() -> Self {
        let storage = Arc::new(InMemoryStorage::new());
        let registry = platform_registry();
        registry.seed(
            DIRECT_METRIC,
            metric_type_document(
                DIRECT_METRIC,
                json!({ "kind": "counter", "enforcement": "direct" }),
                json!({}),
            ),
        );
        let catalog = Arc::new(MetricCatalog::new(
            Arc::new(PlatformTypeRegistry::new(
                registry.clone(),
                Duration::from_secs(3600),
            )),
            Arc::new(RecordingMetrics::default()),
            crate::config::CatalogSection::default().unknown_memory(),
        ));
        let sink = Arc::new(RecordingGaugeSink::default());
        let refresher = LifecycleGaugeRefresher::new(
            storage.clone(),
            catalog,
            sink.clone() as Arc<dyn LifecycleGaugeSink>,
            timing(),
        );
        Self {
            storage,
            registry,
            sink,
            refresher,
        }
    }

    async fn seed(&self, cap: Option<u64>) -> QuotaId {
        self.seed_on(quota_enforcement_sdk::testing::test_metric(), cap)
            .await
    }

    async fn seed_on(&self, metric: MetricId, cap: Option<u64>) -> QuotaId {
        let mut draft = quota_draft(test_subject("u1"), cap);
        draft.metric = metric;
        self.storage
            .create_quota(
                &actor(),
                &AccessScope::for_tenant(tenant().as_uuid()),
                draft,
                &[],
            )
            .await
            .expect("seed")
    }

    // A metric no refresh has read yet, whose first registry read never ends.
    async fn hang(&self) {
        self.registry
            .inject(Fault::on(Call::BatchGet).delay(Duration::from_hours(24)));
        self.seed_on(MetricId::parse(METRIC_REQUESTS).expect("metric"), Some(5))
            .await;
    }

    async fn refresh(&self) -> Result<LifecycleCounts, RefreshError> {
        self.refresher.refresh_once(&CancellationToken::new()).await
    }
}

fn counts(cap_zero: u64, cap_unbounded: u64, for_direct_metric: u64) -> LifecycleCounts {
    LifecycleCounts {
        cap_zero,
        cap_unbounded,
        for_direct_metric,
        conditions_unfit: 0,
    }
}

#[tokio::test]
async fn a_fresh_refresher_reads_the_counts_storage_holds() {
    let h = Harness::new();
    h.seed(Some(0)).await;
    h.seed(None).await;
    h.seed(Some(10)).await;
    assert_eq!(h.refresh().await.expect("refresh"), counts(1, 1, 0));
    assert!(
        h.sink.publications().is_empty(),
        "refresh_once publishes nothing"
    );
}

#[tokio::test]
async fn deactivation_and_cap_changes_move_the_counts_at_the_next_refresh() {
    let h = Harness::new();
    let zero = h.seed(Some(0)).await;
    let unbounded = h.seed(None).await;
    assert_eq!(h.refresh().await.expect("refresh"), counts(1, 1, 0));
    h.storage
        .deactivate_quota(
            &actor(),
            &AccessScope::for_tenant(tenant().as_uuid()),
            zero,
            &[],
        )
        .await
        .expect("deactivate");
    assert_eq!(h.refresh().await.expect("refresh"), counts(0, 1, 0));
    h.storage
        .update_quota(
            &actor(),
            &AccessScope::for_tenant(tenant().as_uuid()),
            unbounded,
            QuotaPatch {
                cap: Some(CapPatch::Bounded(0)),
                ..QuotaPatch::default()
            },
            &[],
        )
        .await
        .expect("bind to zero");
    assert_eq!(h.refresh().await.expect("refresh"), counts(1, 0, 0));
}

#[tokio::test]
async fn the_direct_count_takes_the_enforcement_trait_of_each_metric() {
    let h = Harness::new();
    h.seed(Some(10)).await;
    assert_eq!(h.refresh().await.expect("gated").for_direct_metric, 0);
    let direct = MetricId::parse(DIRECT_METRIC).expect("metric");
    h.seed_on(direct.clone(), Some(10)).await;
    h.seed_on(direct, Some(20)).await;
    assert_eq!(h.refresh().await.expect("direct").for_direct_metric, 2);
    h.seed_on(MetricId::parse(METRIC_OTHER).expect("metric"), Some(1))
        .await;
    assert_eq!(
        h.refresh()
            .await
            .expect("an unregistered metric is flagged, not counted")
            .for_direct_metric,
        2
    );
}

#[tokio::test]
async fn quotas_whose_conditions_do_not_fit_the_metric_type_as_it_is_now_are_counted() {
    let h = Harness::new();
    // The tokens type declares `region`; `zone` is a property it does not
    // have, as after an owner removes one.
    for (property, subject) in [("region", "u1"), ("zone", "u2"), ("zone", "u3")] {
        let mut draft = quota_draft(test_subject(subject), Some(10));
        draft.metric = MetricId::parse(METRIC_TOKENS).expect("metric");
        draft.metadata.insert(property.into(), "eu".into());
        h.storage
            .create_quota(&actor(), &AccessScope::allow_all(), draft, &[])
            .await
            .expect("quota");
    }
    assert_eq!(h.refresh().await.expect("refresh").conditions_unfit, 2);
}

#[tokio::test]
async fn a_registry_that_does_not_answer_costs_only_the_metrics_this_replica_does_not_hold() {
    let h = Harness::new();
    let direct = MetricId::parse(DIRECT_METRIC).expect("metric");
    h.seed_on(direct, Some(0)).await;
    assert_eq!(h.refresh().await.expect("held").for_direct_metric, 1);

    h.seed_on(MetricId::parse(METRIC_OTHER).expect("metric"), None)
        .await;
    fail_registry_reads(&h.registry);

    let sample = h.refresh().await.expect("the sample survives the outage");
    assert_eq!(sample.cap_zero, 1);
    assert_eq!(sample.cap_unbounded, 1);
    assert_eq!(sample.for_direct_metric, 1);
}

#[tokio::test(start_paused = true)]
async fn the_loop_publishes_keeps_the_sample_through_failures_withdraws_it_and_recovers() {
    let h = Harness::new();
    h.seed(Some(0)).await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(h.refresher.clone().run(cancel.clone()));

    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(h.sink.publications(), vec![Some(counts(1, 0, 0))]);

    h.storage
        .fail_with(StorageError::Unavailable("db down".into()));
    tokio::time::sleep(REFRESH + Duration::from_millis(10)).await;
    assert_eq!(
        h.sink.publications().len(),
        1,
        "a failed refresh keeps the last sample"
    );
    // Expiry falls inside the sleep between two refreshes and fires at once.
    tokio::time::sleep(STALE_AFTER).await;
    assert_eq!(h.sink.last(), Some(None), "withdrawn after stale_after");

    h.storage.clear_failure();
    tokio::time::sleep(REFRESH + Duration::from_millis(10)).await;
    assert_eq!(
        h.sink.publications(),
        vec![Some(counts(1, 0, 0)), None, Some(counts(1, 0, 0))],
        "exactly one withdrawal between the outage and the recovery"
    );

    // The disarmed expiry never fires again while refreshes keep succeeding.
    tokio::time::sleep(STALE_AFTER * 2).await;
    assert!(
        h.sink.publications().iter().skip(2).all(Option::is_some),
        "{:?}",
        h.sink.publications()
    );

    cancel.cancel();
    task.await.expect("run returns");
    assert_eq!(
        h.sink.last(),
        Some(None),
        "cancellation withdraws the sample"
    );
}

#[tokio::test(start_paused = true)]
async fn a_hanging_dependency_trips_the_deadline_and_then_the_staleness_bound() {
    let h = Harness::new();
    h.seed(Some(0)).await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(h.refresher.clone().run(cancel.clone()));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(h.sink.last(), Some(Some(counts(1, 0, 0))));

    h.hang().await;
    // Second refresh starts at REFRESH and times out at REFRESH + DEADLINE;
    // no cooperative cancel is involved.
    tokio::time::sleep(REFRESH + DEADLINE + Duration::from_millis(10)).await;
    assert_eq!(
        h.sink.publications().len(),
        1,
        "the sample survives the timeout"
    );
    tokio::time::sleep(STALE_AFTER).await;
    assert_eq!(
        h.sink.last(),
        Some(None),
        "withdrawn while the dependency still hangs"
    );

    cancel.cancel();
    task.await.expect("run returns");
}

#[tokio::test(start_paused = true)]
async fn an_abort_mid_refresh_withdraws_the_sample_and_discards_the_result() {
    let h = Harness::new();
    h.seed(Some(0)).await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(h.refresher.clone().run(cancel.clone()));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(h.sink.publications().len(), 1);

    h.hang().await;
    tokio::time::sleep(REFRESH + Duration::from_millis(10)).await;
    task.abort();
    let outcome = task.await;
    assert!(outcome.is_err_and(|e| e.is_cancelled()), "aborted");
    assert_eq!(
        h.sink.last(),
        Some(None),
        "the drop guard withdrew the sample"
    );
    tokio::time::sleep(STALE_AFTER * 2).await;
    assert_eq!(
        h.sink.publications().len(),
        2,
        "nothing is published after the abort"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_refresh_never_publishes_and_leadership_handover_moves_the_sample() {
    let a = Harness::new();
    a.seed(Some(0)).await;
    let cancel_a = CancellationToken::new();
    let task_a = tokio::spawn(a.refresher.clone().run(cancel_a.clone()));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(a.sink.last(), Some(Some(counts(1, 0, 0))));

    // Leadership lost while a refresh is in flight: the result is discarded.
    a.hang().await;
    tokio::time::sleep(REFRESH + Duration::from_millis(10)).await;
    cancel_a.cancel();
    task_a.await.expect("run returns");
    assert_eq!(a.sink.last(), Some(None));
    assert_eq!(a.sink.publications().len(), 2);

    // The new leader publishes from its own storage read.
    let b = Harness::new();
    b.seed(None).await;
    let cancel_b = CancellationToken::new();
    let task_b = tokio::spawn(b.refresher.clone().run(cancel_b.clone()));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(b.sink.last(), Some(Some(counts(0, 1, 0))));
    cancel_b.cancel();
    task_b.await.expect("run returns");
}

#[tokio::test]
async fn refresh_once_answers_cancellation_at_once() {
    let h = Harness::new();
    h.seed(Some(0)).await;
    h.hang().await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        h.refresh_with(&cancel).await.expect_err("cancelled"),
        RefreshError::Cancelled
    );
}

impl Harness {
    async fn refresh_with(
        &self,
        cancel: &CancellationToken,
    ) -> Result<LifecycleCounts, RefreshError> {
        self.refresher.refresh_once(cancel).await
    }
}
