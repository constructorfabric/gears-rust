//! Provider rules kept in the core (DESIGN §3.2 Capability Checker): which
//! generic generation parameters a model supports and whether a JSON Schema is
//! expressible in the provider's schema subset. Rules are selected per
//! provider type (`ModelInfoV1.gts_type`); unknown providers get the
//! permissive default.

use std::collections::HashMap;
use std::sync::Arc;

use model_registry_sdk::ModelInfoV1;
use toolkit_macros::domain_model;

/// Wire names of the generic generation parameters rules are asked about.
pub const GENERIC_PARAMETERS: &[&str] = &[
    "temperature",
    "top_p",
    "max_output_tokens",
    "max_tool_calls",
    "seed",
    "presence_penalty",
    "frequency_penalty",
    "top_logprobs",
    "truncation",
    "service_tier",
    "parallel_tool_calls",
    "tool_choice",
];

/// Cache-control hint carried by the SDK request.
pub const PROMPT_CACHE_KEY: &str = "prompt_cache_key";

/// Provider rules for one provider type.
pub trait ProviderRules: Send + Sync {
    /// Whether `model` supports the generic parameter `parameter` (one of
    /// [`GENERIC_PARAMETERS`]).
    fn supports_parameter(&self, model: &ModelInfoV1, parameter: &str) -> bool;

    /// Whether the provider has an equivalent of the cache-control hint
    /// `hint` (e.g. [`PROMPT_CACHE_KEY`]).
    fn supports_cache_control(&self, model: &ModelInfoV1, hint: &str) -> bool;

    /// The name of the first construct of `schema` that the provider's JSON
    /// Schema subset cannot express, or `None` when the schema is expressible.
    fn unsupported_schema_construct(
        &self,
        model: &ModelInfoV1,
        schema: &serde_json::Value,
    ) -> Option<String>;
}

/// Rules that support everything: the default for provider types without
/// their own rule set.
#[domain_model]
#[derive(Debug, Clone, Copy, Default)]
pub struct PermissiveRules;

impl ProviderRules for PermissiveRules {
    fn supports_parameter(&self, _model: &ModelInfoV1, _parameter: &str) -> bool {
        true
    }

    fn supports_cache_control(&self, _model: &ModelInfoV1, _hint: &str) -> bool {
        true
    }

    fn unsupported_schema_construct(
        &self,
        _model: &ModelInfoV1,
        _schema: &serde_json::Value,
    ) -> Option<String> {
        None
    }
}

/// Rule sets keyed by provider type with a permissive default.
#[domain_model]
#[derive(Clone)]
pub struct ProviderRulesRegistry {
    by_provider: HashMap<String, Arc<dyn ProviderRules>>,
    fallback: Arc<dyn ProviderRules>,
}

impl ProviderRulesRegistry {
    /// Registry with no provider-specific rule sets: every provider gets
    /// [`PermissiveRules`].
    #[must_use]
    pub fn permissive() -> Self {
        Self {
            by_provider: HashMap::new(),
            fallback: Arc::new(PermissiveRules),
        }
    }

    /// Register `rules` for the provider type `gts_type`.
    #[must_use]
    pub fn with_rules(
        mut self,
        gts_type: impl Into<String>,
        rules: Arc<dyn ProviderRules>,
    ) -> Self {
        self.by_provider.insert(gts_type.into(), rules);
        self
    }

    /// Rules for the provider type `gts_type`, or the permissive default.
    #[must_use]
    pub fn rules_for(&self, gts_type: &str) -> Arc<dyn ProviderRules> {
        Arc::clone(self.by_provider.get(gts_type).unwrap_or(&self.fallback))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "provider_rules_tests.rs"]
mod provider_rules_tests;
