//! Admission-control configuration, read from `gears.admission-control.config`.
//!
//! Every struct rejects unknown keys and every absent key takes its default.
//! Built-in policies are compiled at init ([`AdmissionControlConfig::compile_builtins`]);
//! any compile error fails startup.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context as _;
use gts::GtsIdPattern;
use serde::{Deserialize, Serialize};
use toolkit_canonical_errors::CanonicalError;
use toolkit_policy_evaluation::{EvaluationBackend, RegoBackend, screen_denylist};
use types_registry_sdk::TypesRegistryClient;

use crate::domain::builtin::{BUILTIN_ENTRYPOINT, BuiltinPolicy, BuiltinPolicySet};
use crate::domain::service::ServiceSettings;

/// Configuration of the admission-control gear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdmissionControlConfig {
    /// Engine selection. Default: none; the gate refuses everything the
    /// built-in policies do not.
    pub engine: Option<EngineConfig>,
    /// Engine call bound in milliseconds. Default `100`.
    pub engine_timeout_ms: u64,
    /// Per-policy built-in evaluation bound in milliseconds. Default `3`.
    pub builtin_timeout_ms: u64,
    /// Largest number of properties per request. Default `256`.
    pub max_properties: usize,
    /// Largest serialized size of a request's properties, in bytes. Default `65_536`.
    pub max_context_bytes: usize,
    /// Capacity of the refusal-event queue. Default `1_024`.
    pub event_queue_capacity: usize,
    /// Built-in policies, in evaluation order. Default: none.
    pub builtin_policies: Vec<BuiltinPolicyConfig>,
}

impl Default for AdmissionControlConfig {
    fn default() -> Self {
        Self {
            engine: None,
            engine_timeout_ms: 100,
            builtin_timeout_ms: 3,
            max_properties: 256,
            max_context_bytes: 65_536,
            event_queue_capacity: 1_024,
            builtin_policies: Vec::new(),
        }
    }
}

/// Which policy engine plugin to resolve at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    /// Vendor of the engine plugin.
    pub vendor: String,
    /// GTS instance id pinning one engine plugin instance. Default: none; the
    /// instance is chosen by vendor and priority.
    #[serde(default)]
    pub instance_id: Option<String>,
}

/// One built-in policy as configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinPolicyConfig {
    /// Stable identity.
    pub id: String,
    /// Operator-facing description.
    #[serde(default)]
    pub description: Option<String>,
    /// GTS type patterns the policy applies to (a concrete type id is a
    /// pattern without wildcard).
    pub resource_types: Vec<String>,
    /// Actions the policy applies to. Default: empty, every action.
    #[serde(default)]
    pub actions: Vec<String>,
    /// Rego content defining the boolean rule `deny`.
    pub content: String,
}

impl AdmissionControlConfig {
    /// Service settings from the numeric keys.
    #[must_use]
    pub fn service_settings(&self) -> ServiceSettings {
        ServiceSettings {
            engine_timeout: Duration::from_millis(self.engine_timeout_ms),
            builtin_timeout: Duration::from_millis(self.builtin_timeout_ms),
            max_properties: self.max_properties,
            max_context_bytes: self.max_context_bytes,
        }
    }

    /// Compiles the built-in policies: syntax check, compilation with
    /// entrypoint `deny` and the determinism/resource denylist screen.
    ///
    /// # Errors
    ///
    /// The first invalid policy, naming it.
    pub fn compile_builtins(&self) -> anyhow::Result<BuiltinPolicySet> {
        let backend = RegoBackend::new();
        let mut seen = HashSet::new();
        let mut policies = Vec::with_capacity(self.builtin_policies.len());
        for config in &self.builtin_policies {
            anyhow::ensure!(
                !config.id.trim().is_empty() && seen.insert(config.id.as_str()),
                "built-in policy id `{}` is blank or used more than once",
                config.id
            );
            let id = config.id.as_str();
            anyhow::ensure!(
                !config.resource_types.is_empty(),
                "built-in policy `{id}`: `resource_types` must not be empty"
            );
            let patterns = config
                .resource_types
                .iter()
                .map(|raw| {
                    GtsIdPattern::try_new(raw).map_err(|e| {
                        anyhow::anyhow!(
                            "built-in policy `{id}`: invalid resource type `{raw}`: {e}"
                        )
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            backend
                .validate_syntax(&config.content)
                .with_context(|| format!("built-in policy `{id}`"))?;
            let document = backend
                .compile(id, &config.content, BUILTIN_ENTRYPOINT)
                .with_context(|| format!("built-in policy `{id}`"))?;
            let denylisted = screen_denylist(document.as_ref());
            anyhow::ensure!(
                denylisted.is_empty(),
                "built-in policy `{id}`: references denylisted builtins {denylisted:?}"
            );
            policies.push(BuiltinPolicy {
                id: config.id.clone(),
                resource_types: config.resource_types.clone(),
                patterns,
                actions: config.actions.clone(),
                document,
            });
        }
        Ok(BuiltinPolicySet::new(policies))
    }
}

/// Resolves every concrete resource type named by a built-in policy through
/// the types registry (one batch call).
///
/// # Errors
///
/// A type the registry does not know, or a registry failure: startup fails
/// rather than loading a policy that might never fire.
pub async fn resolve_resource_types(
    policies: &BuiltinPolicySet,
    registry: &dyn TypesRegistryClient,
) -> anyhow::Result<()> {
    let ids: Vec<String> = policies
        .concrete_resource_types()
        .collect::<HashSet<_>>()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let answers = registry.get_type_schemas(ids.clone()).await;
    for id in ids {
        match answers.get(&id) {
            Some(Ok(_)) => {}
            Some(Err(CanonicalError::NotFound { .. })) => {
                anyhow::bail!("built-in policy resource type `{id}` is not registered")
            }
            Some(Err(err)) => {
                anyhow::bail!("types registry failed resolving `{id}`: {}", err.detail())
            }
            None => anyhow::bail!("types registry returned no answer for `{id}`"),
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
