use super::*;
use modkit::registry::RegistryBuilder;
use modkit::runtime::{Endpoint, InstanceState, ModuleInstance, ModuleManager};
use uuid::Uuid;

// ---- Test helpers ----

// (name, deps, has_rest, has_system)
type ModuleSpec = (&'static str, &'static [&'static str], bool, bool);

#[domain_model]
#[derive(Default)]
struct DummyCore;
#[async_trait::async_trait]
impl modkit::Module for DummyCore {
    async fn init(&self, _ctx: &modkit::context::ModuleCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

#[domain_model]
#[derive(Default, Clone)]
struct DummyRest;
impl modkit::contracts::RestApiCapability for DummyRest {
    fn register_rest(
        &self,
        _ctx: &modkit::context::ModuleCtx,
        _router: axum::Router,
        _openapi: &dyn modkit::api::OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        Ok(axum::Router::new())
    }
}

#[domain_model]
#[derive(Default)]
struct DummySystem;
#[async_trait::async_trait]
impl modkit::contracts::SystemCapability for DummySystem {}

fn build_registry(modules: &[ModuleSpec]) -> ModuleRegistry {
    let mut b = RegistryBuilder::default();
    for &(name, deps, has_rest, has_system) in modules {
        b.register_core_with_meta(name, deps, Arc::new(DummyCore));
        if has_rest {
            b.register_rest_with_meta(name, Arc::new(DummyRest));
        }
        if has_system {
            b.register_system_with_meta(name, Arc::new(DummySystem));
        }
    }
    b.build_topo_sorted().unwrap()
}

// ---- Tests ----

#[test]
fn list_compiled_in_modules_from_registry() {
    let registry = build_registry(&[
        ("api_gateway", &[], true, true),
        ("nodes_registry", &["api_gateway"], true, false),
    ]);
    let manager = Arc::new(ModuleManager::new());
    let svc = ModulesService::new(&registry, manager);
    let modules = svc.list_modules();

    assert_eq!(modules.len(), 2);
    // Sorted by name
    assert_eq!(modules[0].name, "api_gateway");
    assert_eq!(modules[0].deployment_mode, DeploymentMode::CompiledIn);
    assert!(modules[0].capabilities.contains(&"rest".to_owned()));
    assert!(modules[0].capabilities.contains(&"system".to_owned()));
    assert!(modules[0].instances.is_empty());

    assert_eq!(modules[1].name, "nodes_registry");
    assert_eq!(modules[1].dependencies, vec!["api_gateway"]);
}

#[test]
fn dynamic_external_instances_appear_as_out_of_process() {
    let registry = build_registry(&[]);
    let manager = Arc::new(ModuleManager::new());

    let instance = Arc::new(
        ModuleInstance::new("external_svc", Uuid::new_v4())
            .with_version("2.0.0")
            .with_grpc_service("ext.Service", Endpoint::http("127.0.0.1", 9001)),
    );
    manager.register_instance(instance);

    let svc = ModulesService::new(&registry, manager);
    let modules = svc.list_modules();

    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "external_svc");
    assert_eq!(modules[0].deployment_mode, DeploymentMode::OutOfProcess);
    assert_eq!(modules[0].instances.len(), 1);
    assert_eq!(modules[0].instances[0].version, Some("2.0.0".to_owned()));
    assert!(
        modules[0].instances[0]
            .grpc_services
            .contains_key("ext.Service")
    );
}

#[test]
fn compiled_in_modules_show_instances_from_manager() {
    let registry = build_registry(&[("grpc_hub", &[], false, true)]);
    let manager = Arc::new(ModuleManager::new());

    let instance = Arc::new(ModuleInstance::new("grpc_hub", Uuid::new_v4()).with_version("0.1.0"));
    manager.register_instance(instance);

    let svc = ModulesService::new(&registry, manager);
    let modules = svc.list_modules();

    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "grpc_hub");
    assert_eq!(modules[0].deployment_mode, DeploymentMode::CompiledIn);
    assert_eq!(modules[0].instances.len(), 1);
}

#[test]
fn instance_state_maps_correctly() {
    let registry = build_registry(&[]);
    let manager = Arc::new(ModuleManager::new());

    let instance = Arc::new(ModuleInstance::new("svc", Uuid::new_v4()));
    // Default state is Registered
    manager.register_instance(instance);

    let svc = ModulesService::new(&registry, manager);
    let modules = svc.list_modules();

    assert_eq!(modules[0].instances[0].state, InstanceState::Registered);
}

#[test]
fn result_is_sorted_by_name() {
    let registry = build_registry(&[("zebra", &[], false, false), ("alpha", &[], false, false)]);
    let manager = Arc::new(ModuleManager::new());

    let svc = ModulesService::new(&registry, manager);
    let modules = svc.list_modules();

    assert_eq!(modules[0].name, "alpha");
    assert_eq!(modules[1].name, "zebra");
}
