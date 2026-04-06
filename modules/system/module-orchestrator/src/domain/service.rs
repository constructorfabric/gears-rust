use std::collections::HashSet;
use std::sync::Arc;

use modkit::registry::ModuleRegistry;
use modkit::runtime::ModuleManager;
use modkit_macros::domain_model;

use super::model::{DeploymentMode, InstanceInfo, ModuleInfo};

/// Lightweight compiled-module metadata (owned data, no trait objects).
#[domain_model]
struct CompiledModule {
    name: String,
    capabilities: Vec<String>,
    deps: Vec<String>,
}

/// Service that assembles module information from catalog and runtime data.
#[domain_model]
pub struct ModulesService {
    /// Compiled modules snapshot (built once at init, immutable after).
    compiled: Vec<CompiledModule>,
    /// Runtime module manager for live instance queries.
    module_manager: Arc<ModuleManager>,
}

impl ModulesService {
    /// Build from a live `ModuleRegistry` and a `ModuleManager`.
    ///
    /// Extracts module metadata (names, deps, capability labels) from the registry
    /// and drops the registry afterwards — no trait objects are kept.
    #[must_use]
    pub fn new(registry: &ModuleRegistry, module_manager: Arc<ModuleManager>) -> Self {
        let compiled: Vec<CompiledModule> = registry
            .modules()
            .iter()
            .map(|entry| CompiledModule {
                name: entry.name().to_owned(),
                capabilities: entry
                    .caps()
                    .labels()
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
                deps: entry.deps().iter().map(|d| (*d).to_owned()).collect(),
            })
            .collect();

        Self {
            compiled,
            module_manager,
        }
    }

    /// List all registered modules, merging compile-time catalog data with runtime instances.
    #[must_use]
    pub fn list_modules(&self) -> Vec<ModuleInfo> {
        let mut modules = Vec::new();
        let mut seen_names = HashSet::new();

        // 1. Emit all compiled-in modules from the catalog.
        for cm in &self.compiled {
            seen_names.insert(cm.name.clone());

            let instances = self.get_module_instances(&cm.name);

            modules.push(ModuleInfo {
                name: cm.name.clone(),
                capabilities: cm.capabilities.clone(),
                dependencies: cm.deps.clone(),
                deployment_mode: DeploymentMode::CompiledIn,
                instances,
            });
        }

        // 2. Add any dynamically registered modules from ModuleManager
        //    that are not in the compiled catalog (external / out-of-process).
        for instance in self.module_manager.all_instances() {
            if seen_names.contains(&instance.module) {
                continue;
            }
            seen_names.insert(instance.module.clone());

            let instances = self.get_module_instances(&instance.module);

            modules.push(ModuleInfo {
                name: instance.module.clone(),
                capabilities: vec![],
                dependencies: vec![],
                deployment_mode: DeploymentMode::OutOfProcess,
                instances,
            });
        }

        // Sort by name for deterministic output
        modules.sort_by(|a, b| a.name.cmp(&b.name));

        modules
    }

    fn get_module_instances(&self, module_name: &str) -> Vec<InstanceInfo> {
        self.module_manager
            .instances_of(module_name)
            .into_iter()
            .map(|inst| {
                let grpc_services = inst
                    .grpc_services
                    .iter()
                    .map(|(name, ep)| (name.clone(), ep.uri.clone()))
                    .collect();

                InstanceInfo {
                    instance_id: inst.instance_id,
                    version: inst.version.clone(),
                    state: inst.state(),
                    grpc_services,
                }
            })
            .collect()
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod tests;
