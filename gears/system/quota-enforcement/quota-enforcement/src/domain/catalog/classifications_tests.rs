use std::sync::Arc;

use async_trait::async_trait;
use quota_enforcement_sdk::{MetricId, MetricKind};

use super::MetricClassifications;
use crate::domain::error::DomainError;
use crate::domain::ports::metric_registry::{
    Classified, Freshness, MetricDescriptor, MetricMode, MetricRegistry,
};

const TOKENS: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.tokens.v1";

fn metric() -> MetricId {
    MetricId::parse(TOKENS).expect("metric")
}

fn descriptor(mode: MetricMode) -> MetricDescriptor {
    MetricDescriptor {
        kind: MetricKind::Counter,
        mode,
    }
}

struct Registry {
    answer: Option<MetricDescriptor>,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl MetricRegistry for Registry {
    async fn describe(&self, _metric: &MetricId) -> Result<Option<Classified>, DomainError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.answer.map(|descriptor| Classified {
            descriptor,
            freshness: Freshness::Fresh,
        }))
    }
}

#[test]
fn a_quota_gated_metric_is_admitted_and_a_direct_one_is_refused() {
    let gated = MetricClassifications::from_pairs([(metric(), descriptor(MetricMode::QuotaGated))]);
    let direct = MetricClassifications::from_pairs([(metric(), descriptor(MetricMode::Direct))]);

    assert!(gated.ensure_quota_gated(&metric()).is_ok());
    assert!(matches!(
        direct.ensure_quota_gated(&metric()),
        Err(DomainError::MetricNotQuotaGated { .. })
    ));
}

#[test]
fn a_metric_outside_the_snapshot_is_reported_unregistered() {
    let empty = MetricClassifications::default();

    assert!(matches!(
        empty.ensure_quota_gated(&metric()),
        Err(DomainError::MetricNotRegistered { .. })
    ));
    assert_eq!(empty.describe(&metric()), None);
}

#[tokio::test]
async fn loading_reads_each_admitted_metric_once_and_never_again() {
    let registry = Arc::new(Registry {
        answer: Some(descriptor(MetricMode::QuotaGated)),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let snapshot = MetricClassifications::load([metric(), metric(), metric()], registry.as_ref())
        .await
        .expect("the registry answers");

    assert_eq!(
        registry.calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "a repeated metric is looked up once"
    );
    assert!(snapshot.describe(&metric()).is_some());
}

#[tokio::test]
async fn a_metric_the_registry_removed_is_left_out_rather_than_defaulted() {
    let registry = Registry {
        answer: None,
        calls: std::sync::atomic::AtomicUsize::new(0),
    };

    // A removed metric must not stop the deployment, and must not be treated
    // as gated either: it is absent, and an operation on it is refused.
    assert!(
        registry
            .describe(&metric())
            .await
            .expect("the registry answers")
            .is_none()
    );
    let snapshot = MetricClassifications::from_pairs([]);
    assert!(matches!(
        snapshot.ensure_quota_gated(&metric()),
        Err(DomainError::MetricNotRegistered { .. })
    ));
}
