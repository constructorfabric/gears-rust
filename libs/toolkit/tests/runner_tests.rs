#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Comprehensive tests for the `ToolKit` runner functionality
//!
//! Tests the core orchestration logic including lifecycle phases,
//! database strategies, shutdown options, and error handling.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use toolkit::{
    GearCtx,
    config::ConfigProvider,
    contracts::{Gear, OpenApiRegistry, RestApiCapability, RunnableCapability},
    registry::{GearRegistry, RegistryBuilder},
    runtime::{DbOptions, RunOptions, ShutdownOptions, run},
};

#[cfg(feature = "db")]
use toolkit::contracts::DatabaseCapability;

// Test tracking infrastructure
#[allow(dead_code)]
type CallTracker = Arc<Mutex<Vec<String>>>;

#[derive(Default)]
#[allow(dead_code)]
struct TestOpenApiRegistry;

impl OpenApiRegistry for TestOpenApiRegistry {
    fn register_operation(&self, _spec: &toolkit::api::OperationSpec) {}
    fn ensure_schema_raw(
        &self,
        root_name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        root_name.to_owned()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// Mock config provider for testing
#[derive(Clone)]
struct MockConfigProvider {
    configs: std::collections::HashMap<String, serde_json::Value>,
}

impl MockConfigProvider {
    fn new() -> Self {
        Self {
            configs: std::collections::HashMap::new(),
        }
    }

    fn with_config(mut self, gear_name: &str, config: serde_json::Value) -> Self {
        self.configs.insert(gear_name.to_owned(), config);
        self
    }
}

impl ConfigProvider for MockConfigProvider {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        self.configs.get(gear_name)
    }
}

// Test trait to add pipe method for more readable code
#[allow(dead_code)]
trait Pipe<T> {
    fn pipe<U, F: FnOnce(T) -> U>(self, f: F) -> U;
}

impl<T> Pipe<T> for T {
    fn pipe<U, F: FnOnce(T) -> U>(self, f: F) -> U {
        f(self)
    }
}

// Test gear implementations with lifecycle tracking
#[allow(dead_code)]
#[derive(Clone)]
struct TestGear {
    name: String,
    calls: CallTracker,
    should_fail_init: Arc<AtomicBool>,
    should_fail_db: Arc<AtomicBool>,
    should_fail_rest: Arc<AtomicBool>,
    should_fail_start: Arc<AtomicBool>,
    should_fail_stop: Arc<AtomicBool>,
}

#[allow(dead_code)]
impl TestGear {
    fn new(name: &str, calls: CallTracker) -> Self {
        Self {
            name: name.to_owned(),
            calls,
            should_fail_init: Arc::new(AtomicBool::new(false)),
            should_fail_db: Arc::new(AtomicBool::new(false)),
            should_fail_rest: Arc::new(AtomicBool::new(false)),
            should_fail_start: Arc::new(AtomicBool::new(false)),
            should_fail_stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn fail_init(self) -> Self {
        self.should_fail_init.store(true, Ordering::SeqCst);
        self
    }

    fn fail_db(self) -> Self {
        self.should_fail_db.store(true, Ordering::SeqCst);
        self
    }

    fn fail_rest(self) -> Self {
        self.should_fail_rest.store(true, Ordering::SeqCst);
        self
    }

    fn fail_start(self) -> Self {
        self.should_fail_start.store(true, Ordering::SeqCst);
        self
    }

    fn fail_stop(self) -> Self {
        self.should_fail_stop.store(true, Ordering::SeqCst);
        self
    }
}

#[async_trait::async_trait]
impl Gear for TestGear {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{}.init", self.name));
        if self.should_fail_init.load(Ordering::SeqCst) {
            anyhow::bail!("Init failed for gear {}", self.name);
        }
        Ok(())
    }
}

#[cfg(feature = "db")]
impl DatabaseCapability for TestGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{}.migrations", self.name));
        if self.should_fail_db.load(Ordering::SeqCst) {
            vec![Box::new(FailingMigration)]
        } else {
            vec![]
        }
    }
}

#[cfg(feature = "db")]
struct FailingMigration;
#[cfg(feature = "db")]
impl sea_orm_migration::MigrationName for FailingMigration {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "m000_fail"
    }
}
#[cfg(feature = "db")]
#[async_trait::async_trait]
impl sea_orm_migration::MigrationTrait for FailingMigration {
    async fn up(
        &self,
        _manager: &sea_orm_migration::SchemaManager,
    ) -> Result<(), sea_orm_migration::sea_orm::DbErr> {
        Err(sea_orm_migration::sea_orm::DbErr::Custom(
            "intentional migration failure".to_owned(),
        ))
    }
    async fn down(
        &self,
        _manager: &sea_orm_migration::SchemaManager,
    ) -> Result<(), sea_orm_migration::sea_orm::DbErr> {
        Ok(())
    }
}

impl RestApiCapability for TestGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{}.register_rest", self.name));
        if self.should_fail_rest.load(Ordering::SeqCst) {
            anyhow::bail!("REST registration failed for gear {}", self.name);
        }
        Ok(router)
    }
}

#[async_trait::async_trait]
impl RunnableCapability for TestGear {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{}.start", self.name));
        if self.should_fail_start.load(Ordering::SeqCst) {
            anyhow::bail!("Start failed for gear {}", self.name);
        }
        Ok(())
    }

    async fn stop(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{}.stop", self.name));
        if self.should_fail_stop.load(Ordering::SeqCst) {
            anyhow::bail!("Stop failed for gear {}", self.name);
        }
        Ok(())
    }
}

// Helper to create a registry with test gears
#[allow(dead_code)]
fn create_test_registry(gears: Vec<TestGear>) -> anyhow::Result<GearRegistry> {
    let mut builder = RegistryBuilder::default();

    for gear in gears {
        let gear_name = gear.name.clone();
        let gear_name_str: &'static str = Box::leak(gear_name.into_boxed_str());
        let gear = Arc::new(gear);

        builder.register_core_with_meta(gear_name_str, &[], gear.clone() as Arc<dyn Gear>);
        #[cfg(feature = "db")]
        builder.register_db_with_meta(gear_name_str, gear.clone() as Arc<dyn DatabaseCapability>);
        builder.register_rest_with_meta(gear_name_str, gear.clone() as Arc<dyn RestApiCapability>);
        builder.register_stateful_with_meta(
            gear_name_str,
            gear.clone() as Arc<dyn RunnableCapability>,
        );
    }

    Ok(builder.build_topo_sorted()?)
}

// Helper function to create a mock DbManager for testing
#[cfg(feature = "db")]
fn create_mock_db_manager() -> Arc<toolkit_db::DbManager> {
    use figment::{Figment, providers::Serialized};

    // Create a simple figment with mock database configuration
    let figment = Figment::new().merge(Serialized::defaults(serde_json::json!({
        "test_gear": {
            "database": {
                "dsn": "sqlite::memory:",
                "params": {
                    "journal_mode": "WAL"
                }
            }
        }
    })));

    let home_dir = std::path::PathBuf::from("/tmp/test");

    Arc::new(toolkit_db::DbManager::from_figment(figment, home_dir).unwrap())
}

#[cfg(feature = "db")]
#[tokio::test]
async fn test_db_phase_failure_stops_lifecycle() {
    use toolkit::runtime::HostRuntime;

    let calls: CallTracker = Arc::new(Mutex::new(Vec::new()));
    let failing = TestGear::new("fail_db", calls.clone()).fail_db();

    let registry = create_test_registry(vec![failing]).expect("registry build");

    // Provide DB config for this gear so DB handle exists.
    let db_manager = {
        use figment::{Figment, providers::Serialized};
        let figment = Figment::new().merge(Serialized::defaults(serde_json::json!({
            "gears": {
                "fail_db": {
                    "database": {
                        "dsn": "sqlite::memory:",
                        "pool": { "max_conns": 1 }
                    }
                }
            }
        })));
        let home_dir = std::path::PathBuf::from("/tmp/test");
        Arc::new(toolkit_db::DbManager::from_figment(figment, home_dir).unwrap())
    };

    let cancel = CancellationToken::new();
    let hr = HostRuntime::new(
        registry,
        Arc::new(MockConfigProvider::new().with_config(
            "fail_db",
            serde_json::json!({
                "database": { "dsn": "sqlite::memory:", "pool": { "max_conns": 1 } },
                "config": {}
            }),
        )),
        DbOptions::Manager(db_manager),
        Arc::new(toolkit::client_hub::ClientHub::default()),
        cancel,
        Uuid::new_v4(),
        None,
    );

    let err = hr.run_gear_phases().await.unwrap_err();
    let chain = format!("{err:#}");
    assert!(
        chain.contains("intentional migration failure"),
        "expected migration failure in error chain, got: {chain}"
    );

    let events = calls.lock().unwrap().clone();
    assert!(events.contains(&"fail_db.migrations".to_owned()));
    assert!(
        !events.iter().any(|e| {
            std::path::Path::new(e)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("init"))
        }),
        "init must not run after db phase failure: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.ends_with(".register_rest")),
        "rest must not run after db phase failure: {events:?}"
    );
    assert!(
        !events.iter().any(|e| {
            std::path::Path::new(e)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("start"))
        }),
        "start must not run after db phase failure: {events:?}"
    );
}

#[tokio::test]
async fn test_db_options_none() {
    // Mock the registry to avoid inventory dependency in tests
    let cancel = CancellationToken::new();
    cancel.cancel(); // Immediate shutdown for test

    let opts = RunOptions::new(
        Arc::new(MockConfigProvider::new()),
        DbOptions::None,
        ShutdownOptions::Token(cancel),
        Uuid::new_v4(),
    );

    // This test requires registry discovery to work, which won't work in isolation
    // For now, let's test the individual components we can test
    let result = timeout(Duration::from_millis(100), run(opts)).await;

    // Should complete quickly due to immediate cancellation
    assert!(result.is_ok());
}

#[cfg(feature = "db")]
#[tokio::test]
async fn test_db_options_manager() {
    let cancel = CancellationToken::new();

    // Cancel after a brief delay
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    let opts = RunOptions::new(
        Arc::new(MockConfigProvider::new().with_config(
            "test_gear",
            serde_json::json!({
                "database": {
                    "dsn": "sqlite::memory:"
                },
                "config": {}
            }),
        )),
        DbOptions::Manager(create_mock_db_manager()),
        ShutdownOptions::Token(cancel),
        Uuid::new_v4(),
    );

    let result = timeout(Duration::from_secs(1), run(opts)).await;
    assert!(result.is_ok());
    let run_result = result.unwrap();
    // Should succeed with DbManager approach
    assert!(run_result.is_ok());
}

#[tokio::test]
async fn test_shutdown_options_token() {
    let cancel = CancellationToken::new();

    let opts = RunOptions::new(
        Arc::new(MockConfigProvider::new()),
        DbOptions::None,
        ShutdownOptions::Token(cancel.clone()),
        Uuid::new_v4(),
    );

    // Start the runner in a background task
    let runner_handle = tokio::spawn(run(opts));

    // Give it a moment to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Cancel it
    cancel.cancel();

    // Should complete quickly
    let result = timeout(Duration::from_millis(100), runner_handle).await;
    assert!(result.is_ok());
    let run_result = result.unwrap().unwrap();
    assert!(run_result.is_ok());
}

#[tokio::test]
async fn test_shutdown_options_future() {
    let (tx, rx) = tokio::sync::oneshot::channel();

    let opts = RunOptions::new(
        Arc::new(MockConfigProvider::new()),
        DbOptions::None,
        ShutdownOptions::Future(Box::pin(async move {
            _ = rx.await;
        })),
        Uuid::new_v4(),
    );

    // Start the runner in a background task
    let runner_handle = tokio::spawn(run(opts));

    // Give it a moment to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Trigger shutdown via the future
    _ = tx.send(());

    // Should complete quickly
    let result = timeout(Duration::from_millis(100), runner_handle).await;
    assert!(result.is_ok());
    let run_result = result.unwrap().unwrap();
    assert!(run_result.is_ok());
}

#[tokio::test]
async fn test_runner_with_config_provider() {
    let cancel = CancellationToken::new();
    cancel.cancel(); // Immediate shutdown

    let config_provider = MockConfigProvider::new().with_config(
        "test_gear",
        serde_json::json!({
            "setting1": "value1",
            "setting2": 42
        }),
    );

    let opts = RunOptions::new(
        Arc::new(config_provider),
        DbOptions::None,
        ShutdownOptions::Token(cancel),
        Uuid::new_v4(),
    );

    let result = timeout(Duration::from_millis(100), run(opts)).await;
    assert!(result.is_ok());
}

// Integration test for complete lifecycle (will work once we have proper gear discovery mock)
#[tokio::test]
async fn test_complete_lifecycle_success() {
    // This test is a placeholder for when we can properly mock the gear discovery
    // For now, we test that the runner doesn't panic with minimal setup
    let cancel = CancellationToken::new();
    cancel.cancel(); // Immediate shutdown

    let opts = RunOptions::new(
        Arc::new(MockConfigProvider::new()),
        DbOptions::None,
        ShutdownOptions::Token(cancel),
        Uuid::new_v4(),
    );

    let result = run(opts).await;
    assert!(result.is_ok());
}

#[test]
fn test_run_options_construction() {
    let cancel = CancellationToken::new();

    let opts = RunOptions::new(
        Arc::new(MockConfigProvider::new()),
        DbOptions::None,
        ShutdownOptions::Token(cancel),
        Uuid::new_v4(),
    );

    // Test that we can construct RunOptions with all variants
    match opts.db {
        DbOptions::None => {}
        #[cfg(feature = "db")]
        DbOptions::Manager(_) => panic!("Expected DbOptions::None"),
    }

    match opts.shutdown {
        ShutdownOptions::Token(_) => {}
        _ => panic!("Expected ShutdownOptions::Token"),
    }
}

#[tokio::test]
async fn test_cancellation_during_startup() {
    let cancel = CancellationToken::new();

    let opts = RunOptions::new(
        Arc::new(MockConfigProvider::new()),
        DbOptions::None,
        ShutdownOptions::Token(cancel.clone()),
        Uuid::new_v4(),
    );

    // Start the runner in a background task
    let runner_handle = tokio::spawn(run(opts));

    // Cancel immediately to test cancellation handling
    cancel.cancel();

    // Should complete quickly due to cancellation
    let result = timeout(Duration::from_millis(100), runner_handle).await;
    assert!(
        result.is_ok(),
        "Runner should complete quickly when cancelled"
    );

    let run_result = result.unwrap().unwrap();
    assert!(
        run_result.is_ok(),
        "Runner should handle cancellation gracefully"
    );
}

#[tokio::test]
async fn test_multiple_config_provider_scenarios() {
    let cancel = CancellationToken::new();
    cancel.cancel(); // Immediate shutdown

    // Test with empty config
    let empty_config = MockConfigProvider::new();
    let opts = RunOptions::new(
        Arc::new(empty_config),
        DbOptions::None,
        ShutdownOptions::Token(cancel.clone()),
        Uuid::new_v4(),
    );

    let result = run(opts).await;
    assert!(result.is_ok(), "Should handle empty config");

    // Test with complex config
    let complex_config = MockConfigProvider::new()
        .with_config(
            "gear1",
            serde_json::json!({
                "setting1": "value1",
                "nested": {
                    "setting2": 42,
                    "setting3": true
                }
            }),
        )
        .with_config(
            "gear2",
            serde_json::json!({
                "array_setting": [1, 2, 3],
                "string_setting": "test"
            }),
        );

    let cancel2 = CancellationToken::new();
    cancel2.cancel();

    let opts2 = RunOptions::new(
        Arc::new(complex_config),
        DbOptions::None,
        ShutdownOptions::Token(cancel2),
        Uuid::new_v4(),
    );

    let result2 = run(opts2).await;
    assert!(result2.is_ok(), "Should handle complex config");
}

#[tokio::test]
async fn test_runner_timeout_scenarios() {
    // Test that runner doesn't hang indefinitely
    let cancel = CancellationToken::new();

    let opts = RunOptions::new(
        Arc::new(MockConfigProvider::new()),
        DbOptions::None,
        ShutdownOptions::Token(cancel.clone()),
        Uuid::new_v4(),
    );

    let runner_handle = tokio::spawn(run(opts));

    // Give it some time to start up
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Cancel after a short delay
    cancel.cancel();

    // Should complete within a reasonable time
    let result = timeout(Duration::from_millis(200), runner_handle).await;
    assert!(result.is_ok(), "Runner should complete within timeout");

    let run_result = result.unwrap().unwrap();
    assert!(run_result.is_ok(), "Runner should complete successfully");
}

// Test configuration scenarios
#[test]
fn test_config_provider_edge_cases() {
    let provider = MockConfigProvider::new()
        .with_config("test", serde_json::json!(null))
        .with_config("empty", serde_json::json!({}))
        .with_config(
            "complex",
            serde_json::json!({
                "a": {
                    "b": {
                        "c": "deep_value"
                    }
                }
            }),
        );

    // Test null config
    let null_config = provider.get_gear_config("test");
    assert!(null_config.is_some());
    assert!(null_config.unwrap().is_null());

    // Test empty config
    let empty_config = provider.get_gear_config("empty");
    assert!(empty_config.is_some());
    assert!(empty_config.unwrap().is_object());

    // Test complex config
    let complex_config = provider.get_gear_config("complex");
    assert!(complex_config.is_some());
    assert_eq!(complex_config.unwrap()["a"]["b"]["c"], "deep_value");

    // Test non-existent config
    let missing_config = provider.get_gear_config("nonexistent");
    assert!(missing_config.is_none());
}

// Placeholder tests for comprehensive lifecycle testing
// These would work with additional runner infrastructure that allows
// injecting test registries instead of using inventory discovery

/*
#[tokio::test]
async fn test_lifecycle_init_failure() {
    // This test demonstrates how we would test init phase failures
    // if the runner supported dependency injection of the registry

    let calls = Arc::new(Mutex::new(Vec::new()));
    let failing_gear =  TestGear::new("failing_gear", calls.clone()).fail_init();

    // Would need a version of run() that accepts a pre-built registry
    // let registry = create_test_registry(vec![failing_gear]).unwrap();
    // let result = run_with_registry(opts, registry).await;
    // assert!(result.is_err());
    // assert!(result.unwrap_err().to_string().contains("Init failed"));
}

#[tokio::test]
async fn test_lifecycle_complete_success() {
    // Demonstrates testing a complete successful lifecycle
    let calls = Arc::new(Mutex::new(Vec::new()));
    let gears = vec![
        TestGear::new("gear1", calls.clone()),
        TestGear::new("gear2", calls.clone()),
    ];

    // Would need runner API changes to support this
    // let registry = create_test_registry(gears).unwrap();
    // let result = run_with_registry(opts, registry).await;
    // assert!(result.is_ok());

    // Verify lifecycle call order
    // let call_log = calls.lock().unwrap();
    // assert!(call_log.contains(&"gear1.init".to_string()));
    // assert!(call_log.contains(&"gear2.init".to_string()));
}
*/
