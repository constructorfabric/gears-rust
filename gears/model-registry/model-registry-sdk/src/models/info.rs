// Created: 2026-05-06 by Constructor Tech
// Updated: 2026-11-10 by Constructor Tech
//! [`ModelInfoV1`] — the record LLM Gateway uses to evaluate a model
//! (DESIGN §3.1 Model Info).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::models::{
    ContextWindow, DefaultInferenceParametersV1, DisabledCapabilities, ModelCapabilities,
    ModelCostV1, ModelPerformance, SupportedApi,
};

/// Model info: general settings, user-facing information, cost, capabilities,
/// the context window, default parameters with their override policy, and the
/// opaque `provider_settings` document.
///
/// Not a GTS type. `gts_type` names the provider type, derived from
/// [`PROVIDER_BASE_TYPE`](crate::gts::PROVIDER_BASE_TYPE), whose schema fixes
/// the shape of `provider_settings`. The registry validates the document
/// against that schema on write and returns it unchanged; LLM Gateway selects
/// the plugin whose provider type equals `gts_type` and narrows
/// `provider_settings` to the plugin's typed view.
///
/// When absent from a serialized record, the optional fields, flags,
/// collections, `sort_order`, and every nested object except
/// `context_window` deserialize to their empty or default value.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct ModelInfoV1 {
    /// Provider GTS type, copied from the model's provider on create.
    /// Read-only: never part of a create or update request.
    pub gts_type: gts::GtsTypeId,
    /// The provider's own model identifier, sent to the provider. 1–255
    /// characters without whitespace or control characters. Mutable; not
    /// part of the canonical id, and several models of one provider may
    /// share it.
    pub provider_model_id: String,
    /// Absolute `http` or `https` URL of the model's endpoint, up to 2,048
    /// characters, without user information. The plugin calls it only
    /// through OAGW: the host and port name the OAGW upstream alias, and the
    /// scheme on the wire is the upstream's (HTTPS only). When absent, the
    /// plugin uses its provider's default URL; the registry stores no
    /// default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `CredStore` secret reference for the provider credential: 1–255
    /// characters from `[a-zA-Z0-9_-]`, the `SecretRef` format of
    /// `credstore-sdk`. Must name a secret the model's tenant owns; the
    /// registry checks this on create and on an update that changes it to a
    /// non-null value, and stores no secret material.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    /// Name shown to users, 1–255 characters.
    pub display_name: String,
    /// Free-text description, up to 4,096 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Model family label, for example `gpt-4` or `llama`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Organization that produced the model weights. Independent of the
    /// provider that serves the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// Whether the platform can load and unload this model on a managed
    /// runtime. Informational. Default false.
    #[serde(default)]
    pub managed: bool,
    /// Architecture class, for example `llama` or `mistral`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    /// On-disk size of locally hosted weights, at most 2^63 − 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Weight or serving format, for example `gguf` or `safetensors`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Deployment region, for example `eu-west-1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Infrastructure host, for example `Azure` or `self-hosted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosted_by: Option<String>,
    /// When the vendor last released this model version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_release_at: Option<DateTime<Utc>>,
    /// Display-only reasoning level label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_level: Option<String>,
    /// The model's release version as published by the vendor. Not the row
    /// `version` of [`ModelV1`](crate::models::ModelV1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    /// Position in model pickers; lower first. Default 0.
    #[serde(default)]
    pub sort_order: i32,
    /// Absolute `http` or `https` URL of the model icon, up to 2,048
    /// characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Display label for the relative cost, for example `3x`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier_display: Option<String>,
    /// Cost in AI credits.
    #[serde(default)]
    pub cost: ModelCostV1,
    /// Expected performance.
    #[serde(default)]
    pub performance: ModelPerformance,
    /// Deployment-specific metadata. Default empty.
    #[serde(default)]
    pub additional_info: HashMap<String, serde_json::Value>,
    /// API kinds the model serves.
    #[serde(default)]
    pub supported_api: HashSet<SupportedApi>,
    /// What the model can do.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// Capabilities an administrator turned off for this model. The
    /// effective capability is the declared capability minus the disabled
    /// one.
    #[serde(default)]
    pub disabled_capabilities: DisabledCapabilities,
    /// Token limits.
    pub context_window: ContextWindow,
    /// Provider-independent inference defaults.
    #[serde(default)]
    pub default_parameters: DefaultInferenceParametersV1,
    /// Whether callers may override `default_parameters` per request.
    /// Default false.
    #[serde(default)]
    pub allow_parameter_override: bool,
    /// Names of extra request parameters callers may pass to the provider,
    /// up to 64 names of up to 128 characters. Default empty.
    #[serde(default)]
    pub allow_extra_params: Vec<String>,
    /// Provider-specific settings. Shape defined by the provider type
    /// schema; opaque to the registry beyond validation.
    pub provider_settings: serde_json::Value,
}

impl ModelInfoV1 {
    /// Model info with the required fields set and every other field at its
    /// default: optional fields absent, flags false, `sort_order` 0,
    /// collections empty, nested objects at their defaults,
    /// `provider_settings` an empty object.
    #[must_use]
    pub fn new(
        gts_type: gts::GtsTypeId,
        provider_model_id: impl Into<String>,
        display_name: impl Into<String>,
        context_window: ContextWindow,
    ) -> Self {
        Self {
            gts_type,
            provider_model_id: provider_model_id.into(),
            base_url: None,
            secret_ref: None,
            display_name: display_name.into(),
            description: None,
            family: None,
            vendor: None,
            managed: false,
            architecture: None,
            size_bytes: None,
            format: None,
            region: None,
            hosted_by: None,
            last_release_at: None,
            reasoning_level: None,
            model_version: None,
            sort_order: 0,
            icon: None,
            multiplier_display: None,
            cost: ModelCostV1::default(),
            performance: ModelPerformance::default(),
            additional_info: HashMap::new(),
            supported_api: HashSet::new(),
            capabilities: ModelCapabilities::default(),
            disabled_capabilities: DisabledCapabilities::none(),
            context_window,
            default_parameters: DefaultInferenceParametersV1::default(),
            allow_parameter_override: false,
            allow_extra_params: Vec::new(),
            provider_settings: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use toolkit_gts::gts_id;

    use super::ModelInfoV1;
    use crate::models::{
        DefaultInferenceParametersV1, DisabledCapabilities, ModelCapabilities, ModelCostV1,
        ModelPerformance,
    };

    #[test]
    fn minimal_record_deserializes_with_defaults() {
        let info: ModelInfoV1 = serde_json::from_value(serde_json::json!({
            "gts_type": gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~"),
            "provider_model_id": "gpt-4o",
            "display_name": "GPT-4o",
            "context_window": { "max_input_tokens": 128_000 },
            "provider_settings": {},
        }))
        .expect("minimal record deserializes");

        assert_eq!(info.base_url, None);
        assert!(!info.managed);
        assert_eq!(info.sort_order, 0);
        assert_eq!(info.cost, ModelCostV1::default());
        assert_eq!(info.performance, ModelPerformance::default());
        assert!(info.additional_info.is_empty());
        assert!(info.supported_api.is_empty());
        assert_eq!(info.capabilities, ModelCapabilities::default());
        assert_eq!(info.disabled_capabilities, DisabledCapabilities::none());
        assert_eq!(
            info.default_parameters,
            DefaultInferenceParametersV1::default()
        );
        assert!(!info.allow_parameter_override);
        assert!(info.allow_extra_params.is_empty());
    }
}
