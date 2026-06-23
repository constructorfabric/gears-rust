// Created: 2026-05-06 by Constructor Tech
// Updated: 2026-05-07 by Constructor Tech
//! [`ModelInfoV1<P>`] — provider-independent fields, the user-facing
//! [`DefaultInferenceParametersV1`], the flat per-model override fields
//! (`allow_parameter_override`, `allow_extra_params`), and the typed
//! `provider_settings: P` payload.
//!
//! Declared as the GTS base schema via [`struct_to_gts_schema`]; concrete
//! provider-settings types (e.g. `OpenAiSettingsV1`, shipped in
//! [`crate::models::providers`]) declare themselves as GTS leaves with
//! `base = ModelInfoV1`. The set of provider leaves is open-ended.
//!
//! `P` defaults to `serde_json::Value` (which implements `gts::GtsSchema`
//! upstream) so heterogeneous lists carry an opaque JSON blob; consumers
//! route on [`ModelInfoV1::gts_type`] and narrow to a typed view via
//! [`crate::models::Model::try_into_typed`].

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use gts_macros::struct_to_gts_schema;

use crate::models::{
    ContextWindow, DefaultInferenceParametersV1, DisabledCapabilities, ModelCapabilities,
    ModelPerformance, SupportedApi,
};

/// Complete model information: provider-independent metadata, capabilities,
/// the context window, performance, the user-facing default inference
/// parameters, the flat per-model override fields, and the
/// provider-specific `provider_settings` payload.
///
/// `P` defaults to `serde_json::Value` (which implements `gts::GtsSchema`
/// upstream) so heterogeneous lists (e.g. `list_tenant_models`) carry an
/// opaque JSON blob; consumers route on [`ModelInfoV1::gts_type`] and narrow
/// to a typed view via [`crate::models::Model::try_into_typed`].
///
/// # GTS schema
///
/// - **`schema_id`**: `gts.cf.genai.model.info.v1~`
/// - **base**: yes (root envelope; provider-specific leaves chain off it)
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = true,
    type_id = "gts.cf.genai.model.info.v1~",
    description = "Generic model info envelope: provider-independent metadata + provider_settings JSON payload",
    properties = "gts_type,display_name,family,vendor,managed,architecture,format,supported_api,provider_model_id,capabilities,context_window,default_parameters"
)]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ModelInfoV1<P: gts::GtsSchema = serde_json::Value> {
    // ── GTS schema identity ───────────────────────────────────────────
    /// Full GTS schema chain identifying this model's settings shape — e.g.
    /// `gts.cf.genai.model.info.v1~cf.genai._.openai.v1~`. Mirrors
    /// `Provider.gts_type` and is the canonical key for resolving the
    /// concrete shape of `provider_settings` (which is a raw JSON blob —
    /// `serde_json::Value` — by default).
    pub gts_type: gts::GtsTypeId,

    // ── Display / discovery ────────────────────────────────────────────
    /// Display name shown in UI.
    pub display_name: String,
    pub description: Option<String>,
    /// Model family (e.g. `"gpt-4"`, `"claude"`, `"llama"`).
    pub family: Option<String>,
    /// Model vendor — the organization that produced the model weights
    /// (e.g. `"OpenAI"`, `"Meta"`). Free-form string; independent of which
    /// provider serves the model.
    pub vendor: Option<String>,
    /// Infrastructure field (for local/managed LLMs): whether Gears can
    /// load/unload **this model** (e.g. install/pull/unload weights on a local
    /// runtime such as Ollama or LM Studio). This is a **per-model** flag and
    /// is distinct from the **per-provider** [`Provider::managed`] flag, which
    /// records whether Gears can manage the *provider* at all; a model can only
    /// be `managed` when its provider is also managed. Defaults to `false`
    /// (e.g. for API-only models).
    pub managed: bool,
    /// Infrastructure field (for local/managed LLMs): model architecture
    /// classifier (e.g. `"qwen"`, `"llama"`, `"mistral"`, `"gpt"`). Distinct
    /// from the free-form `family`/`vendor` labels, which are descriptive
    /// marketing/origin labels rather than an architecture taxonomy.
    pub architecture: Option<String>,
    /// Infrastructure field (for local/managed LLMs): on-disk model size in
    /// bytes, used for capacity planning of local/managed weights. `None` for
    /// models whose weights are not locally hosted (e.g. API-only).
    pub size_bytes: Option<u64>,
    /// Infrastructure field (for local/managed LLMs): model weight/serving
    /// format (e.g. `"gguf"`, `"mlx"`, `"safetensors"`, `"api-only"`).
    pub format: Option<String>,
    /// Deployment region (e.g. `"us-east-1"`, `"eu-west-1"`).
    pub region: Option<String>,
    /// Infrastructure host (e.g. `"Azure"`, `"AWS Bedrock"`, `"self-hosted"`).
    pub hosted_by: Option<String>,
    /// When the model version was last released by the vendor.
    pub last_release_at: Option<DateTime<Utc>>,
    /// Informational reasoning level label (e.g. `"high"`, `"medium"`).
    /// Display-only — not used for routing or parameter selection.
    pub reasoning_level: Option<String>,
    /// Model version string.
    pub version: Option<String>,
    /// Display order in model picker / lists.
    pub sort_order: Option<i32>,
    /// URL to model icon.
    pub icon: Option<String>,
    /// Human-readable cost multiplier label (e.g. `"1x"`, `"3x"`).
    pub multiplier_display: Option<String>,
    /// Estimated performance characteristics.
    pub performance: ModelPerformance,
    /// Last-resort escape hatch for deployment-specific metadata; typed
    /// fields on `provider_settings` are preferred.
    pub additional_info: HashMap<String, serde_json::Value>,

    // ── Promoted from the old `ApiResolution` ─────────────────────────
    /// Which API kinds this model exposes (completion, embedding).
    /// Promoted to common so consumers can filter on completion vs
    /// embedding without unwrapping the variant.
    pub supported_api: HashSet<SupportedApi>,
    /// Provider's model identifier — used both in `canonical_id`
    /// (`{provider_slug}::{provider_model_id}`) and sent to the provider in
    /// API requests. Promoted to common so the catalog UI / alias logic
    /// doesn't have to reach into `provider_settings`.
    pub provider_model_id: String,

    // ── Capabilities ───────────────────────────────────────────────────
    /// What the model can do.
    pub capabilities: ModelCapabilities,
    /// Capabilities that are administratively disabled.
    pub disabled_capabilities: DisabledCapabilities,
    /// Token limits.
    pub context_window: ContextWindow,

    // ── User-facing defaults & override policy ────────────────────────
    /// User-facing default inference parameters; mirrors the inference-knob
    /// subset of the Open Responses request schema
    /// (`gts.cf.llmgw.core.create_response_body.v1~`). Distinct from any
    /// provider-wire defaults living on `provider_settings`.
    pub default_parameters: DefaultInferenceParametersV1,
    /// Whether callers may override `default_parameters` per-request. Flat
    /// field on the envelope (no wrapper struct).
    pub allow_parameter_override: bool,
    /// Which extra (non-default) parameter names callers may pass alongside
    /// the request. Flat field on the envelope.
    pub allow_extra_params: Vec<String>,

    // ── Provider-specific payload ──────────────────────────────────────
    /// Provider-specific connection routing, provider-wire default
    /// parameters, and token pricing.
    pub provider_settings: P,
}
