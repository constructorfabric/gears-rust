//! Provider rules kept in the core (DESIGN §3.2 Capability Checker): which
//! generic generation parameters a model supports and whether a JSON Schema is
//! expressible in the provider's schema subset. Rules are selected per
//! provider type (`ModelInfoV1.gts_type`); unknown providers get the
//! permissive default.

use std::collections::HashMap;
use std::sync::Arc;

use model_registry_sdk::ModelInfoV1;
use toolkit_macros::domain_model;

use super::pricing::ModelPricing;

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

/// Wire name of the embeddings `input_type` parameter; rules are asked about
/// it through [`ProviderRules::supports_parameter`]. `dimensions` is not a
/// droppable parameter and has its own hook
/// ([`ProviderRules::supports_dimensions`]).
pub const INPUT_TYPE: &str = "input_type";

/// Provider rules for one provider type.
pub trait ProviderRules: Send + Sync {
    /// Whether `model` supports the generic parameter `parameter` (one of
    /// [`GENERIC_PARAMETERS`]).
    fn supports_parameter(&self, model: &ModelInfoV1, parameter: &str) -> bool;

    /// Whether the provider has an equivalent of the cache-control hint
    /// `hint` (e.g. [`PROMPT_CACHE_KEY`]).
    fn supports_cache_control(&self, model: &ModelInfoV1, hint: &str) -> bool;

    /// Whether `model` can produce embedding vectors of exactly `dimensions`
    /// components. `false` rejects the request with `unsupported_parameter`
    /// (`dimensions` is never dropped, DESIGN §3.2).
    fn supports_dimensions(&self, model: &ModelInfoV1, dimensions: u32) -> bool;

    /// The name of the first construct of `schema` that the provider's JSON
    /// Schema subset cannot express, or `None` when the schema is expressible.
    fn unsupported_schema_construct(
        &self,
        model: &ModelInfoV1,
        schema: &serde_json::Value,
    ) -> Option<String>;

    /// Token pricing of `model`, parsed from `provider_settings.cost`
    /// (`input_per_1k_micro`, `output_per_1k_micro`). A missing or
    /// unparseable rate is `None`; whether it is required is decided by the
    /// Quota Guard per operation.
    fn pricing(&self, model: &ModelInfoV1) -> ModelPricing;
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

    fn supports_dimensions(&self, _model: &ModelInfoV1, _dimensions: u32) -> bool {
        true
    }

    fn unsupported_schema_construct(
        &self,
        _model: &ModelInfoV1,
        _schema: &serde_json::Value,
    ) -> Option<String> {
        None
    }

    fn pricing(&self, model: &ModelInfoV1) -> ModelPricing {
        parse_pricing(model)
    }
}

/// Parse `provider_settings.cost` (`input_per_1k_micro`, `output_per_1k_micro`):
/// a rate that is absent or not a non-negative integer is `None`.
#[must_use]
pub fn parse_pricing(model: &ModelInfoV1) -> ModelPricing {
    let cost = model.provider_settings.get("cost");
    let rate = |key: &str| {
        cost.and_then(|c| c.get(key))
            .and_then(serde_json::Value::as_u64)
    };
    ModelPricing {
        input_per_1k_micro: rate("input_per_1k_micro"),
        output_per_1k_micro: rate("output_per_1k_micro"),
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
