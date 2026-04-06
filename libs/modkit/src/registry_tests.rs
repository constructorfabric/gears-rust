use super::*;
use std::sync::Arc;

// Use the real contracts/context APIs from the crate to avoid type mismatches.
use crate::context::ModuleCtx;
use crate::contracts;

/* --------------------------- Test helpers ------------------------- */
#[derive(Default)]
struct DummyCore;
#[async_trait::async_trait]
impl contracts::Module for DummyCore {
    async fn init(&self, _ctx: &ModuleCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

/* ------------------------------- Tests ---------------------------- */

#[test]
fn topo_sort_happy_path() {
    let mut b = RegistryBuilder::default();
    // cores
    b.register_core_with_meta("core_a", &[], Arc::new(DummyCore));
    b.register_core_with_meta("core_b", &["core_a"], Arc::new(DummyCore));

    let reg = b.build_topo_sorted().unwrap();
    let order: Vec<_> = reg.modules().iter().map(|m| m.name).collect();
    assert_eq!(order, vec!["core_a", "core_b"]);
}

#[test]
fn unknown_dependency_error() {
    let mut b = RegistryBuilder::default();
    b.register_core_with_meta("core_a", &["missing_dep"], Arc::new(DummyCore));

    let err = b.build_topo_sorted().unwrap_err();
    match err {
        RegistryError::UnknownDependency { module, depends_on } => {
            assert_eq!(module, "core_a");
            assert_eq!(depends_on, "missing_dep");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn cyclic_dependency_detected() {
    let mut b = RegistryBuilder::default();
    b.register_core_with_meta("a", &["b"], Arc::new(DummyCore));
    b.register_core_with_meta("b", &["a"], Arc::new(DummyCore));

    let err = b.build_topo_sorted().unwrap_err();
    match err {
        RegistryError::CycleDetected { path } => {
            // Should contain both modules in the cycle
            assert!(path.contains(&"a"));
            assert!(path.contains(&"b"));
            assert!(path.len() >= 3); // At least a -> b -> a
        }
        other => panic!("expected CycleDetected, got: {other:?}"),
    }
}

#[test]
fn complex_cycle_detection_with_path() {
    let mut b = RegistryBuilder::default();
    // Create a more complex cycle: a -> b -> c -> a
    b.register_core_with_meta("a", &["b"], Arc::new(DummyCore));
    b.register_core_with_meta("b", &["c"], Arc::new(DummyCore));
    b.register_core_with_meta("c", &["a"], Arc::new(DummyCore));
    // Add an unrelated module to ensure we only detect the actual cycle
    b.register_core_with_meta("d", &[], Arc::new(DummyCore));

    let err = b.build_topo_sorted().unwrap_err();
    match err {
        RegistryError::CycleDetected { path } => {
            // Should contain all modules in the cycle
            assert!(path.contains(&"a"));
            assert!(path.contains(&"b"));
            assert!(path.contains(&"c"));
            assert!(!path.contains(&"d")); // Should not include unrelated module
            assert!(path.len() >= 4); // At least a -> b -> c -> a

            // Verify the error message is helpful
            let error_msg = format!("{}", RegistryError::CycleDetected { path: path.clone() });
            assert!(error_msg.contains("cyclic dependency detected"));
            assert!(error_msg.contains("->"));
        }
        other => panic!("expected CycleDetected, got: {other:?}"),
    }
}

#[test]
fn duplicate_core_reported_in_configuration_errors() {
    let mut b = RegistryBuilder::default();
    b.register_core_with_meta("a", &[], Arc::new(DummyCore));
    // duplicate
    b.register_core_with_meta("a", &[], Arc::new(DummyCore));

    let err = b.build_topo_sorted().unwrap_err();
    match err {
        RegistryError::InvalidRegistryConfiguration { errors } => {
            assert!(
                errors.iter().any(|e| e.contains("already registered")),
                "expected duplicate registration error, got {errors:?}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn rest_capability_without_core_fails() {
    let mut b = RegistryBuilder::default();
    b.register_core_with_meta("core_a", &[], Arc::new(DummyCore));
    // Register a rest capability for a module that doesn't exist
    b.register_rest_with_meta("unknown_module", Arc::new(DummyRest));

    let err = b.build_topo_sorted().unwrap_err();
    match err {
        RegistryError::UnknownModule(name) => {
            assert_eq!(name, "unknown_module");
        }
        other => panic!("expected UnknownModule, got: {other:?}"),
    }
}

#[test]
fn db_capability_without_core_fails() {
    let mut b = RegistryBuilder::default();
    b.register_core_with_meta("core_a", &[], Arc::new(DummyCore));
    // Register a db capability for a module that doesn't exist
    b.register_db_with_meta("unknown_module", Arc::new(DummyDb));

    let err = b.build_topo_sorted().unwrap_err();
    match err {
        RegistryError::UnknownModule(name) => {
            assert_eq!(name, "unknown_module");
        }
        other => panic!("expected UnknownModule, got: {other:?}"),
    }
}

#[test]
fn stateful_capability_without_core_fails() {
    let mut b = RegistryBuilder::default();
    b.register_core_with_meta("core_a", &[], Arc::new(DummyCore));
    // Register a stateful capability for a module that doesn't exist
    b.register_stateful_with_meta("unknown_module", Arc::new(DummyStateful));

    let err = b.build_topo_sorted().unwrap_err();
    match err {
        RegistryError::UnknownModule(name) => {
            assert_eq!(name, "unknown_module");
        }
        other => panic!("expected UnknownModule, got: {other:?}"),
    }
}

#[test]
fn capability_query_works() {
    let mut b = RegistryBuilder::default();
    let module = Arc::new(DummyCore);
    b.register_core_with_meta("test", &[], module);
    b.register_db_with_meta("test", Arc::new(DummyDb));
    b.register_rest_with_meta("test", Arc::new(DummyRest));

    let reg = b.build_topo_sorted().unwrap();
    let entry = &reg.modules()[0];

    assert!(entry.caps.has::<DatabaseCap>());
    assert!(entry.caps.has::<RestApiCap>());
    assert!(!entry.caps.has::<SystemCap>());

    assert!(entry.caps.query::<DatabaseCap>().is_some());
    assert!(entry.caps.query::<RestApiCap>().is_some());
    assert!(entry.caps.query::<SystemCap>().is_none());
}

#[test]
fn rest_host_capability_without_core_fails() {
    let mut b = RegistryBuilder::default();
    b.register_core_with_meta("core_a", &[], Arc::new(DummyCore));
    // Set rest_host to a module that doesn't exist
    b.register_rest_host_with_meta("unknown_host", Arc::new(DummyRestHost));

    let err = b.build_topo_sorted().unwrap_err();
    match err {
        RegistryError::UnknownModule(name) => {
            assert_eq!(name, "unknown_host");
        }
        other => panic!("expected UnknownModule, got: {other:?}"),
    }
}

#[test]
fn module_entry_getters_work() {
    let mut b = RegistryBuilder::default();
    b.register_core_with_meta("alpha", &[], Arc::new(DummyCore));
    b.register_core_with_meta("beta", &["alpha"], Arc::new(DummyCore));
    b.register_rest_with_meta("beta", Arc::new(DummyRest));

    let reg = b.build_topo_sorted().unwrap();
    let beta = reg.modules().iter().find(|e| e.name() == "beta").unwrap();

    assert_eq!(beta.name(), "beta");
    assert_eq!(beta.deps(), &["alpha"]);
    assert!(beta.caps().has::<RestApiCap>());
}

#[test]
fn test_module_registry_builds() {
    let registry = ModuleRegistry::discover_and_build();
    assert!(registry.is_ok(), "Registry should build successfully");
}

/* Test helper implementations */
#[derive(Default, Clone)]
struct DummyRest;
impl contracts::RestApiCapability for DummyRest {
    fn register_rest(
        &self,
        _ctx: &crate::context::ModuleCtx,
        _router: axum::Router,
        _openapi: &dyn crate::api::OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        Ok(axum::Router::new())
    }
}

#[derive(Default)]
struct DummyDb;
impl contracts::DatabaseCapability for DummyDb {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![]
    }
}

#[derive(Default)]
struct DummyStateful;
#[async_trait::async_trait]
impl contracts::RunnableCapability for DummyStateful {
    async fn start(&self, _cancel: tokio_util::sync::CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
    async fn stop(&self, _cancel: tokio_util::sync::CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct DummyRestHost;
impl contracts::ApiGatewayCapability for DummyRestHost {
    fn rest_prepare(
        &self,
        _ctx: &crate::context::ModuleCtx,
        router: axum::Router,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }
    fn rest_finalize(
        &self,
        _ctx: &crate::context::ModuleCtx,
        router: axum::Router,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }
    fn as_registry(&self) -> &dyn crate::contracts::OpenApiRegistry {
        panic!("DummyRestHost::as_registry should not be called in tests")
    }
}
