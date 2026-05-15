use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::Utc;
use modkit::client_hub::{ClientHub, ClientScope};
use types_registry_sdk::testing::make_test_instance;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient, TypesRegistryError,
};
use usage_collector_sdk::{
    Subject, UsageCollectorError, UsageCollectorPluginClientV1, UsageCollectorPluginSpecV1,
    UsageKind, UsageRecord,
};
use uuid::Uuid;

use super::Service;
use crate::config::{CircuitBreakerConfig, MetricConfig, UsageCollectorConfig};
use crate::domain::DomainError;

// ── MockRegistry ──────────────────────────────────────────────────

struct MockRegistry {
    instances: Vec<GtsInstance>,
    list_calls: std::sync::atomic::AtomicUsize,
}

impl MockRegistry {
    fn new(instances: Vec<GtsInstance>) -> Self {
        Self {
            instances,
            list_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl TypesRegistryClient for MockRegistry {
    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, TypesRegistryError> {
        Ok(vec![])
    }

    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, TypesRegistryError> {
        Ok(vec![])
    }

    async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, TypesRegistryError> {
        unimplemented!()
    }

    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, TypesRegistryError> {
        unimplemented!()
    }

    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, TypesRegistryError>> {
        unimplemented!()
    }

    async fn get_type_schemas_by_uuid(
        &self,
        _type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, TypesRegistryError>> {
        unimplemented!()
    }

    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, TypesRegistryError> {
        unimplemented!()
    }

    async fn register_instances(
        &self,
        _instances: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, TypesRegistryError> {
        Ok(vec![])
    }

    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, TypesRegistryError> {
        unimplemented!()
    }

    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, TypesRegistryError> {
        unimplemented!()
    }

    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, TypesRegistryError>> {
        unimplemented!()
    }

    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, TypesRegistryError>> {
        unimplemented!()
    }

    async fn list_instances(
        &self,
        _query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, TypesRegistryError> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.instances.clone())
    }
}

struct OkPlugin;

#[async_trait::async_trait]
impl UsageCollectorPluginClientV1 for OkPlugin {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }
}

fn plugin_content(gts_id: &str, vendor: &str) -> serde_json::Value {
    serde_json::json!({
        "id": gts_id,
        "vendor": vendor,
        "priority": 0,
        "properties": {}
    })
}

fn hub_with_plugin(
    instance_id: &str,
    vendor: &str,
    plugin: Arc<dyn UsageCollectorPluginClientV1>,
) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::default());
    let instance = make_test_instance(instance_id, plugin_content(instance_id, vendor));
    let reg: Arc<dyn TypesRegistryClient> = Arc::new(MockRegistry::new(vec![instance]));
    hub.register::<dyn TypesRegistryClient>(reg);
    hub.register_scoped::<dyn UsageCollectorPluginClientV1>(
        ClientScope::gts_id(instance_id),
        plugin,
    );
    hub
}

fn make_service() -> Service {
    let instance_id = format!(
        "{}test.usage.mock.svc_test.v1",
        UsageCollectorPluginSpecV1::gts_schema_id()
    );
    let hub = hub_with_plugin(&instance_id, "cyberfabric", Arc::new(OkPlugin));
    Service::new(UsageCollectorConfig::default(), hub)
}

fn make_service_with_vendor(hub: Arc<ClientHub>, vendor: &str) -> Service {
    Service::new(
        UsageCollectorConfig {
            vendor: vendor.to_owned(),
            ..UsageCollectorConfig::default()
        },
        hub,
    )
}

fn record(tenant_id: Uuid) -> UsageRecord {
    UsageRecord {
        tenant_id,
        module: "test-module".to_owned(),
        metric: "test.metric".to_owned(),
        kind: UsageKind::Gauge,
        value: 1.0,
        resource_id: Uuid::new_v4(),
        resource_type: "test.resource".to_owned(),
        subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
        idempotency_key: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        metadata: None,
    }
}

#[tokio::test]
async fn create_usage_record_delegates_to_plugin() {
    let svc = make_service();
    assert!(
        svc.create_usage_record(record(Uuid::new_v4()))
            .await
            .is_ok()
    );
}

// ── plugin timeout ────────────────────────────────────────────────

struct SlowPlugin;

#[async_trait::async_trait]
impl UsageCollectorPluginClientV1 for SlowPlugin {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageCollectorError> {
        tokio::time::sleep(Duration::from_mins(1)).await;
        Ok(())
    }
}

#[tokio::test]
async fn plugin_timeout_returns_timeout_error() {
    let instance_id = format!(
        "{}test.usage.mock.svc_test.v1",
        UsageCollectorPluginSpecV1::gts_schema_id()
    );
    let hub = hub_with_plugin(&instance_id, "cyberfabric", Arc::new(SlowPlugin));
    let svc = Service::new(
        UsageCollectorConfig {
            vendor: "cyberfabric".to_owned(),
            plugin_timeout: Duration::from_millis(1),
            ..UsageCollectorConfig::default()
        },
        hub,
    );
    let err = svc
        .create_usage_record(record(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Timeout),
        "expected Timeout, got {err:?}"
    );
}

// ── GTS plugin resolution caching ─────────────────────────────────

#[tokio::test]
async fn gts_plugin_selector_calls_registry_only_once_across_multiple_create_usage_records() {
    let instance_id = format!(
        "{}test.usage.mock.svc_test.v1",
        UsageCollectorPluginSpecV1::gts_schema_id()
    );
    let reg = Arc::new(MockRegistry::new(vec![make_test_instance(
        &instance_id,
        plugin_content(&instance_id, "cyberfabric"),
    )]));
    let hub = Arc::new(ClientHub::default());
    hub.register::<dyn TypesRegistryClient>(Arc::clone(&reg) as Arc<dyn TypesRegistryClient>);
    hub.register_scoped::<dyn UsageCollectorPluginClientV1>(
        ClientScope::gts_id(&instance_id),
        Arc::new(OkPlugin),
    );
    let svc = make_service_with_vendor(hub, "cyberfabric");
    let tenant = Uuid::new_v4();

    svc.create_usage_record(record(tenant)).await.unwrap();
    svc.create_usage_record(record(tenant)).await.unwrap();
    svc.create_usage_record(record(tenant)).await.unwrap();

    assert_eq!(
        reg.list_calls.load(Ordering::SeqCst),
        1,
        "GTS registry should be queried exactly once after initial resolution"
    );
}

// ── no plugin registered in hub ───────────────────────────────────

#[tokio::test]
async fn no_plugin_client_in_hub_returns_unavailable_error() {
    let instance_id = format!(
        "{}test.usage.mock.svc_test.v1",
        UsageCollectorPluginSpecV1::gts_schema_id()
    );
    let hub = Arc::new(ClientHub::default());
    let instance = make_test_instance(&instance_id, plugin_content(&instance_id, "cyberfabric"));
    let reg: Arc<dyn TypesRegistryClient> = Arc::new(MockRegistry::new(vec![instance]));
    hub.register::<dyn TypesRegistryClient>(reg);
    // plugin client NOT registered
    let svc = make_service_with_vendor(hub, "cyberfabric");
    let err = svc
        .create_usage_record(record(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::PluginUnavailable { .. }));
}

// ── get_module_config ─────────────────────────────────────────────

fn config_with_metrics(metrics: HashMap<String, MetricConfig>) -> UsageCollectorConfig {
    UsageCollectorConfig {
        metrics,
        ..UsageCollectorConfig::default()
    }
}

#[tokio::test]
async fn get_module_config_returns_not_configured_when_no_metrics_configured() {
    let svc = Service::new(
        UsageCollectorConfig::default(),
        Arc::new(ClientHub::default()),
    );
    let err = svc.get_module_config("any-module").unwrap_err();
    assert!(matches!(err, DomainError::ModuleNotConfigured { .. }));
}

#[tokio::test]
async fn get_module_config_returns_metric_when_modules_restriction_is_absent() {
    let mut metrics = HashMap::new();
    metrics.insert(
        "cpu.usage".to_owned(),
        MetricConfig {
            kind: UsageKind::Gauge,
            modules: None,
        },
    );
    let svc = Service::new(config_with_metrics(metrics), Arc::new(ClientHub::default()));
    let cfg = svc.get_module_config("any-module").unwrap();
    assert_eq!(cfg.allowed_metrics.len(), 1);
    assert_eq!(cfg.allowed_metrics[0].name, "cpu.usage");
    assert!(matches!(cfg.allowed_metrics[0].kind, UsageKind::Gauge));
    assert_eq!(cfg.max_metadata_bytes, 8192);
}

#[tokio::test]
async fn get_module_config_returns_metric_when_module_is_in_allow_list() {
    let mut metrics = HashMap::new();
    metrics.insert(
        "req.count".to_owned(),
        MetricConfig {
            kind: UsageKind::Counter,
            modules: Some(vec!["my-module".to_owned()]),
        },
    );
    let svc = Service::new(config_with_metrics(metrics), Arc::new(ClientHub::default()));
    let cfg = svc.get_module_config("my-module").unwrap();
    assert_eq!(cfg.allowed_metrics.len(), 1);
    assert_eq!(cfg.allowed_metrics[0].name, "req.count");
    assert!(matches!(cfg.allowed_metrics[0].kind, UsageKind::Counter));
    assert_eq!(cfg.max_metadata_bytes, 8192);
}

#[tokio::test]
async fn get_module_config_returns_not_configured_when_module_not_in_allow_list() {
    let mut metrics = HashMap::new();
    metrics.insert(
        "cpu.usage".to_owned(),
        MetricConfig {
            kind: UsageKind::Gauge,
            modules: Some(vec!["other-module".to_owned()]),
        },
    );
    let svc = Service::new(config_with_metrics(metrics), Arc::new(ClientHub::default()));
    let err = svc.get_module_config("my-module").unwrap_err();
    assert!(matches!(err, DomainError::ModuleNotConfigured { .. }));
}

#[tokio::test]
async fn get_module_config_returns_only_matching_metrics_from_mixed_config() {
    let mut metrics = HashMap::new();
    metrics.insert(
        "cpu.usage".to_owned(),
        MetricConfig {
            kind: UsageKind::Gauge,
            modules: None,
        },
    );
    metrics.insert(
        "disk.io".to_owned(),
        MetricConfig {
            kind: UsageKind::Counter,
            modules: Some(vec!["storage".to_owned()]),
        },
    );
    let config = UsageCollectorConfig {
        max_metadata_bytes: 16384,
        metrics,
        ..UsageCollectorConfig::default()
    };
    let svc = Service::new(config, Arc::new(ClientHub::default()));
    let cfg = svc.get_module_config("my-module").unwrap();
    assert_eq!(cfg.allowed_metrics.len(), 1);
    assert_eq!(cfg.allowed_metrics[0].name, "cpu.usage");
    assert_eq!(cfg.max_metadata_bytes, 16384);
}

// ── circuit breaker ──────────────────────────────────────────────────────────

/// A storage plugin that always returns a transient error.
struct FailPlugin;

#[async_trait::async_trait]
impl UsageCollectorPluginClientV1 for FailPlugin {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageCollectorError> {
        Err(UsageCollectorError::service_unavailable()
            .with_detail("simulated transient failure")
            .create())
    }
}

/// A storage plugin that counts every invocation via an atomic counter.
struct CountingPlugin {
    counter: Arc<AtomicUsize>,
    should_fail: bool,
}

impl CountingPlugin {
    fn failing(counter: Arc<AtomicUsize>) -> Self {
        Self {
            counter,
            should_fail: true,
        }
    }
}

#[async_trait::async_trait]
impl UsageCollectorPluginClientV1 for CountingPlugin {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageCollectorError> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        if self.should_fail {
            Err(UsageCollectorError::service_unavailable()
                .with_detail("simulated transient failure")
                .create())
        } else {
            Ok(())
        }
    }
}

fn make_cb_service(
    plugin: Arc<dyn UsageCollectorPluginClientV1>,
    threshold: u32,
    window: Duration,
    recovery: Duration,
) -> Service {
    let instance_id = format!(
        "{}test.usage.mock.cb_test.v1",
        UsageCollectorPluginSpecV1::gts_schema_id()
    );
    let hub = hub_with_plugin(&instance_id, "cyberfabric", plugin);
    Service::new(
        UsageCollectorConfig {
            vendor: "cyberfabric".to_owned(),
            circuit_breaker: CircuitBreakerConfig {
                failure_threshold: threshold,
                window,
                recovery_timeout: recovery,
            },
            ..UsageCollectorConfig::default()
        },
        hub,
    )
}

#[tokio::test]
async fn circuit_opens_after_n_consecutive_failures() {
    let threshold = 2u32;
    let svc = make_cb_service(
        Arc::new(FailPlugin),
        threshold,
        Duration::from_secs(10),
        Duration::from_millis(1),
    );

    for _ in 0..threshold {
        drop(svc.create_usage_record(record(Uuid::new_v4())).await);
    }

    let err = svc
        .create_usage_record(record(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::CircuitOpen),
        "expected CircuitOpen after {threshold} failures, got {err:?}"
    );
}

#[tokio::test]
async fn open_circuit_rejects_without_calling_plugin() {
    let counter = Arc::new(AtomicUsize::new(0));
    let threshold = 2u32;
    let svc = make_cb_service(
        Arc::new(CountingPlugin::failing(Arc::clone(&counter))),
        threshold,
        Duration::from_secs(10),
        Duration::from_millis(1),
    );

    for _ in 0..threshold {
        drop(svc.create_usage_record(record(Uuid::new_v4())).await);
    }
    let calls_to_open = counter.load(Ordering::SeqCst);

    for _ in 0..3 {
        let err = svc
            .create_usage_record(record(Uuid::new_v4()))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::CircuitOpen));
    }

    assert_eq!(
        counter.load(Ordering::SeqCst),
        calls_to_open,
        "plugin must not be invoked while circuit is open"
    );
}

#[tokio::test]
async fn failed_probe_reopens_circuit() {
    let threshold = 1u32;
    let svc = make_cb_service(
        Arc::new(FailPlugin),
        threshold,
        Duration::from_secs(10),
        Duration::from_millis(1),
    );

    drop(svc.create_usage_record(record(Uuid::new_v4())).await);

    let err = svc
        .create_usage_record(record(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::CircuitOpen));

    tokio::time::sleep(Duration::from_millis(5)).await;

    // Probe call: Open → HalfOpen, then fails → re-opens. Returns the plugin error.
    let probe_err = svc
        .create_usage_record(record(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(
        matches!(probe_err, DomainError::Plugin(_)),
        "probe should propagate plugin error, got {probe_err:?}"
    );

    let err = svc
        .create_usage_record(record(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::CircuitOpen));
}
