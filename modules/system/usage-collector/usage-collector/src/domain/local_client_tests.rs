use std::collections::HashMap;
use std::sync::Arc;

use modkit::client_hub::ClientHub;
use usage_collector_sdk::{UsageCollectorClientV1, UsageCollectorError, UsageKind};

use super::{Service, UsageCollectorLocalClient};
use crate::config::{MetricConfig, UsageCollectorConfig};

#[tokio::test]
async fn module_not_configured_maps_to_canonical_not_found() {
    let svc = Arc::new(Service::new(
        UsageCollectorConfig::default(),
        Arc::new(ClientHub::default()),
    ));
    let client = UsageCollectorLocalClient::new(svc);
    let err = client.get_module_config("unknown").await.unwrap_err();
    assert!(matches!(err, UsageCollectorError::NotFound { .. }));
}

#[tokio::test]
async fn module_config_returns_allowed_metrics() {
    let mut metrics = HashMap::new();
    metrics.insert(
        "cpu.usage".to_owned(),
        MetricConfig {
            kind: UsageKind::Gauge,
            modules: None,
        },
    );
    let svc = Arc::new(Service::new(
        UsageCollectorConfig {
            metrics,
            ..UsageCollectorConfig::default()
        },
        Arc::new(ClientHub::default()),
    ));
    let client = UsageCollectorLocalClient::new(svc);
    let cfg = client.get_module_config("any-module").await.unwrap();
    assert_eq!(cfg.allowed_metrics.len(), 1);
    assert_eq!(cfg.allowed_metrics[0].name, "cpu.usage");
    assert_eq!(cfg.allowed_metrics[0].kind, UsageKind::Gauge);
    assert_eq!(cfg.max_metadata_bytes, 8192);
}

#[tokio::test]
async fn module_config_preserves_counter_kind() {
    // Exercises the Counter arm of the `kind` mapping in `Service::get_module_config`
    // so a future regression that hardwires `kind: UsageKind::Gauge` is caught.
    let mut metrics = HashMap::new();
    metrics.insert(
        "requests.total".to_owned(),
        MetricConfig {
            kind: UsageKind::Counter,
            modules: None,
        },
    );
    let svc = Arc::new(Service::new(
        UsageCollectorConfig {
            metrics,
            ..UsageCollectorConfig::default()
        },
        Arc::new(ClientHub::default()),
    ));
    let client = UsageCollectorLocalClient::new(svc);
    let cfg = client.get_module_config("any-module").await.unwrap();
    assert_eq!(cfg.allowed_metrics.len(), 1);
    assert_eq!(cfg.allowed_metrics[0].name, "requests.total");
    assert_eq!(cfg.allowed_metrics[0].kind, UsageKind::Counter);
}
