use super::*;
use async_trait::async_trait;
use chat_engine_sdk::error::PluginError;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---- A minimal in-process plugin double ----
struct StubPlugin {
    id: String,
    outcome: Mutex<Result<HealthStatus, PluginError>>,
    calls: AtomicUsize,
}

impl StubPlugin {
    fn new(id: &str, outcome: Result<HealthStatus, PluginError>) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_owned(),
            outcome: Mutex::new(outcome),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl ChatEngineBackendPlugin for StubPlugin {
    async fn health_check(&self) -> Result<HealthStatus, PluginError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // We need to clone the outcome since PluginError isn't Clone.
        let guard = self.outcome.lock();
        match &*guard {
            Ok(s) => Ok(s.clone()),
            Err(_) => Err(PluginError::transient("stub: unhealthy")),
        }
    }

    fn plugin_instance_id(&self) -> &str {
        &self.id
    }
}

// ---- A mock PluginConfigRepo (find returns whatever was last set) ----
struct StubRepo {
    next: Mutex<Option<JsonValue>>,
}

impl StubRepo {
    fn new(value: Option<JsonValue>) -> Arc<Self> {
        Arc::new(Self {
            next: Mutex::new(value),
        })
    }
}

#[async_trait]
impl PluginConfigRepo for StubRepo {
    async fn find(
        &self,
        _plugin_instance_id: &str,
        _session_type_id: Uuid,
    ) -> Result<Option<JsonValue>, ChatEngineError> {
        Ok(self.next.lock().clone())
    }

    async fn upsert(
        &self,
        _plugin_instance_id: &str,
        _session_type_id: Uuid,
        config: JsonValue,
    ) -> Result<(), ChatEngineError> {
        *self.next.lock() = Some(config);
        Ok(())
    }

    async fn delete(
        &self,
        _plugin_instance_id: &str,
        _session_type_id: Uuid,
    ) -> Result<(), ChatEngineError> {
        *self.next.lock() = None;
        Ok(())
    }
}

fn make_service(
    plugins: Vec<(String, Arc<dyn ChatEngineBackendPlugin>)>,
    repo: Arc<dyn PluginConfigRepo>,
) -> PluginService {
    let hub = Arc::new(ClientHub::new());
    for (id, p) in plugins {
        hub.register_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&id), p);
    }
    PluginService::new(hub, repo)
}

#[tokio::test]
async fn resolve_returns_not_found_when_unregistered() {
    let svc = make_service(vec![], StubRepo::new(None));
    let result = svc.resolve("missing");
    let err = match result {
        Ok(_) => panic!("expected Err, got Ok"),
        Err(e) => e,
    };
    assert!(matches!(
        err,
        ChatEngineError::NotFound {
            resource: "plugin",
            ..
        }
    ));
}

#[tokio::test]
async fn health_probe_healthy_is_silent_path() {
    let plugin = StubPlugin::new("ok", Ok(HealthStatus::Healthy));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin.clone();
    let svc = make_service(vec![("ok".into(), plugin_dyn)], StubRepo::new(None));
    let status = svc.health_probe("ok").await.expect("ok");
    assert_eq!(status, HealthStatus::Healthy);
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn health_probe_degraded_routes_with_warn() {
    let plugin = StubPlugin::new("d", Ok(HealthStatus::Degraded));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin.clone();
    let svc = make_service(vec![("d".into(), plugin_dyn)], StubRepo::new(None));
    let status = svc.health_probe("d").await.expect("ok");
    assert_eq!(status, HealthStatus::Degraded);
}

#[tokio::test]
async fn health_probe_unhealthy_routes_with_warn() {
    let plugin = StubPlugin::new("u", Ok(HealthStatus::Unhealthy));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin.clone();
    let svc = make_service(vec![("u".into(), plugin_dyn)], StubRepo::new(None));
    let status = svc.health_probe("u").await.expect("ok");
    assert_eq!(status, HealthStatus::Unhealthy);
}

#[tokio::test]
async fn health_probe_plugin_error_folds_to_unhealthy() {
    let plugin = StubPlugin::new("e", Err(PluginError::transient("boom")));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin.clone();
    let svc = make_service(vec![("e".into(), plugin_dyn)], StubRepo::new(None));
    let status = svc
        .health_probe("e")
        .await
        .expect("must return Ok despite plugin error");
    assert_eq!(status, HealthStatus::Unhealthy);
}

#[tokio::test]
async fn load_config_proxies_to_repo() {
    let svc = make_service(vec![], StubRepo::new(Some(serde_json::json!({"k": "v"}))));
    let cfg = svc
        .load_config("p", Uuid::nil())
        .await
        .expect("ok")
        .expect("some");
    assert_eq!(cfg, serde_json::json!({"k": "v"}));
}

#[tokio::test]
async fn save_config_is_readable_by_load_config() {
    // Mirrors the register_session_type -> create_session round-trip: a
    // config saved at registration must be observable by a later
    // load_config (it used to be dropped on the floor).
    let svc = make_service(vec![], StubRepo::new(None));
    assert!(
        svc.load_config("p", Uuid::nil())
            .await
            .expect("ok")
            .is_none(),
        "config must be absent before save",
    );
    svc.save_config("p", Uuid::nil(), serde_json::json!({"k": "v"}))
        .await
        .expect("save ok");
    let cfg = svc
        .load_config("p", Uuid::nil())
        .await
        .expect("ok")
        .expect("some after save");
    assert_eq!(cfg, serde_json::json!({"k": "v"}));
}
