//! Models for the provider-plugin interface.
//!
//! A provider plugin (see [`crate::plugin_api::LlmGatewayProviderPluginClientV1`])
//! isolates one LLM provider behind a common trait. These types carry the
//! per-call context the core Gateway hands to a plugin and the integration-level
//! capabilities a plugin reports. Per-model feature capabilities (vision,
//! function calling, streaming, …) are owned by Model Registry
//! (`ModelCapabilities`) and validated by the core before dispatch — they are
//! deliberately not duplicated here.

use model_registry_sdk::ModelInfoV1;
use serde::{Deserialize, Serialize};

/// Per-call context the core Gateway passes to every provider-plugin method.
///
/// Carries what translation and transport need without the plugin reaching into
/// Model Registry itself. The core builds it from the resolved model and the
/// request being served.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderCallCtx {
    /// The resolved model info the core fetched from Model Registry, carried as
    /// the GTS-typed [`ModelInfoV1`] envelope (default `serde_json::Value`
    /// provider settings). Its `gts_type` is the authoritative provider routing
    /// key and equals the resolving plugin's declared `provider_type`. The
    /// plugin narrows it to its typed view (e.g. `ModelInfoV1<OpenAiSettingsV1>`)
    /// via [`ProviderCallCtx::typed_info`] and reads `provider_model_id`,
    /// `provider_settings`, and its connection routing (e.g. the `OAGW` alias,
    /// when the provider uses one) from there. `OAGW` injects credentials and
    /// applies circuit breaking; the plugin never reads or stores provider
    /// credentials.
    pub model_info: ModelInfoV1,

    /// Gateway request correlation id (the response `id`), propagated for
    /// tracing across usage, error, and audit events.
    pub request_id: String,
}

impl ProviderCallCtx {
    /// Narrow [`Self::model_info`] to a concrete provider view `Q`, validating
    /// its `gts_type` against `Q`'s GTS type id. Delegates to Model Registry's
    /// `ModelInfoV1::try_into_typed`.
    ///
    /// # Errors
    ///
    /// - [`gts::NarrowError::SchemaId`] when `model_info.gts_type` does not match
    ///   `Q`'s GTS type id (the plugin was handed a payload for another provider).
    /// - [`gts::NarrowError::Deserialize`] when the payload can't be
    ///   deserialized into `Q`.
    pub fn typed_info<Q>(&self) -> Result<ModelInfoV1<Q>, gts::NarrowError>
    where
        Q: gts::GtsSchema,
        for<'de> Q: gts::GtsDeserialize<'de>,
    {
        self.model_info.clone().try_into_typed::<Q>()
    }
}

/// How a provider plugin consumes media referenced by input content parts
/// (`input_image`, `input_file`, `input_audio`, `input_video`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaInputMode {
    /// External / base64 URLs are forwarded to the provider as-is; the core
    /// performs no `FileStorage` prefetch.
    UrlForward,
    /// The core must fetch bytes from `FileStorage` before dispatch and hand the
    /// plugin resolved content.
    PrefetchRequired,
}

/// Integration-level capabilities a provider plugin reports.
///
/// These describe how the provider *integration* behaves — concerns Model
/// Registry does not track. Per-model feature capabilities live in
/// `ModelInfoV1.capabilities` / `supported_api` and are validated by the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderPluginCapabilities {
    /// Whether the provider integration can stream responses at all
    /// (independent of any individual model's `streaming` flag).
    pub streaming_transport: bool,

    /// How the plugin expects media input to be delivered.
    pub media_input: MediaInputMode,
}
