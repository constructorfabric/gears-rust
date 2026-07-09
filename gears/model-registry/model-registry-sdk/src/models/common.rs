// Created: 2026-05-06 by Constructor Tech
// Updated: 2026-05-07 by Constructor Tech
//! Provider-independent model types — enums, capability flags, the context
//! window, and performance characteristics.
//!
//! These types live on `ModelInfo<P>` directly (not on the per-provider
//! settings) because their shape is meaningful for every provider. The
//! per-model override policy is no longer a struct in this module — its
//! two fields (`allow_parameter_override`, `allow_extra_params`) are now
//! flat fields on [`crate::models::ModelInfoV1`].

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Lifecycle status of a model in the catalog.
///
/// Wire format is lowercase to match `DESIGN.md §3.1` and so `OData` filters
/// over `lifecycle_status` (per `DESIGN.md §3.3`) compare against the same
/// strings the JSONB column stores.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum LifecycleStatus {
    Production,
    Preview,
    Experimental,
    Deprecated,
    Sunset,
}

/// Approval status of a model for a tenant.
///
/// Wire format is lowercase to match `DESIGN.md §3.1` and `OData` filters
/// over `approval_status`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Revoked,
}

/// Operational status of a provider.
///
/// Wire format is lowercase to match `DESIGN.md §3.1`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ProviderStatus {
    Active,
    Disabled,
}

/// API kind that a model exposes.
///
/// A model may expose multiple API surfaces (e.g. `[Completion, Batch]` for
/// a chat model that's also reachable via the asynchronous batch API). Each
/// variant corresponds to a distinct LLM Gateway endpoint family.
///
/// Wire format is lowercase to match `DESIGN.md §3.1` and so `OData` filters
/// on `info.supported_api` (per `DESIGN.md §3.3`) compare against the same
/// strings stored in the JSONB `info` column.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SupportedApi {
    /// Synchronous chat / responses APIs.
    Completion,
    /// Embedding APIs.
    Embedding,
    /// Asynchronous batch API (see `gts.cf.llmgw.async.batch.v1~`). May
    /// coexist with `Completion` / `Embedding` on the same model.
    Batch,
}

/// Unified reasoning effort level used by `default_parameters.reasoning.effort`.
///
/// This enum is **neutral** — it exposes the levels every provider
/// understands. Provider-specific reasoning levels (e.g. `OpenAI`'s
/// `Minimal`, added with gpt-5) live on the per-provider settings as a
/// distinct enum (see `OpenAiReasoningEffort`), so adding an OpenAI-only
/// level doesn't perturb the shared enum.
///
/// Wire format is lowercase (`"none" | "low" | "medium" | "high" | "xhigh"`)
/// to match `gts.cf.llmgw.core.reasoning_config.v1~`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
    XHigh,
}

/// Service tier in the unified two-variant shape used by `default_parameters`
/// (matches the Open Responses request schema).
///
/// Provider-specific tiers (e.g. `OpenAI`'s full `auto | default | flex |
/// priority` set) live on the per-provider settings as a separate enum (see
/// `OpenAiServiceTier`).
///
/// Wire format is lowercase (`"auto" | "default"`) to match
/// `gts.cf.llmgw.core.create_response_body.v1~`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ServiceTier {
    Auto,
    Default,
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// Reasoning sub-capabilities indicating which reasoning controls the model
/// accepts.
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ReasoningCapability {
    /// Supports `reasoning_effort` parameter (low/medium/high).
    pub effort: bool,
    /// Supports toggling reasoning on/off.
    pub toggle: bool,
    /// Supports resuming/continuing a reasoning chain.
    pub resume: bool,
    /// Supports explicit reasoning token budget.
    pub budget: bool,
}

/// Web search capability flags.
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct WebSearchCapability {
    /// Whether web search is available.
    pub enabled: bool,
    /// Whether the model supports configuring an allow-list of domains to
    /// restrict search to.
    pub allowed_domains: bool,
    /// Whether the model supports configuring a deny-list of domains to
    /// exclude from search.
    pub excluded_domains: bool,
}

/// Shared shape for media-typed capabilities — `vision`, `file_input`,
/// `image_generation`, `audio_input`, `audio_output`.
///
/// Captures both whether the capability is available and which media types
/// the model accepts (or produces). MIME types follow [RFC 6838][1] —
/// lowercased canonical spelling (e.g. `audio/mpeg`, not `audio/MP3`). An
/// empty `supported_mime_types` means "no per-type list surfaced by the
/// provider"; consumers should treat that as "best-effort, defer to the
/// provider's documented support".
///
/// The disablement counterpart is [`DisabledMediaCapability`].
///
/// [1]: https://datatracker.ietf.org/doc/html/rfc6838
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct MediaCapability {
    /// Whether the capability is available.
    pub enabled: bool,
    /// Accepted (or produced) media types — RFC 6838 names. Empty when
    /// `enabled` is `false` or the provider doesn't surface a per-type list.
    pub supported_mime_types: Vec<String>,
}

/// Capability flags describing what the model can do.
///
/// `Copy` is intentionally not derived: [`MediaCapability`] carries a
/// `Vec<String>`, so the struct is `Clone` only.
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[non_exhaustive]
pub struct ModelCapabilities {
    /// Supports image/vision input.
    pub vision: MediaCapability,
    /// Reasoning controls (effort, toggle, resume, budget).
    pub reasoning: ReasoningCapability,
    /// Supports function/tool calling.
    pub function_calling: bool,
    /// Supports structured output via response schema (JSON schema).
    pub response_schema: bool,
    /// Supports streaming responses.
    pub streaming: bool,
    /// Supports file input (e.g. PDFs, documents).
    pub file_input: MediaCapability,
    /// Can generate images.
    pub image_generation: MediaCapability,
    /// Accepts audio input (speech-to-text, audio understanding).
    pub audio_input: MediaCapability,
    /// Produces audio output (text-to-speech).
    pub audio_output: MediaCapability,
    /// Supports code interpreter / sandboxed execution.
    pub code_interpreter: bool,
    /// Web search capability.
    pub web_search: WebSearchCapability,
}

/// Disablement flags for a [`MediaCapability`].
///
/// `disabled = true` removes the whole capability from the supported set.
/// Otherwise `disabled_mime_types` lists MIME types subtracted from the
/// supported list.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DisabledMediaCapability {
    /// The whole capability is disabled.
    pub disabled: bool,
    /// MIME types disabled out of the parent capability's supported list.
    /// Lower-cased RFC 6838 names.
    pub disabled_mime_types: Vec<String>,
}

/// Disablement flags for [`ReasoningCapability`].
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct DisabledReasoningCapability {
    /// `reasoning_effort` parameter is disabled.
    pub effort: bool,
    /// Reasoning toggle is disabled.
    pub toggle: bool,
    /// Resume / continue reasoning is disabled.
    pub resume: bool,
    /// Reasoning token budget is disabled.
    pub budget: bool,
}

/// Disablement flags for [`WebSearchCapability`].
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct DisabledWebSearchCapability {
    /// The whole web-search capability is disabled.
    pub disabled: bool,
    /// Configuring the allow-list of domains is disabled.
    pub allowed_domains: bool,
    /// Configuring the deny-list of domains is disabled.
    pub excluded_domains: bool,
}

/// Subtractive view over [`ModelCapabilities`]: which capability flags are
/// administratively blocked for this model.
///
/// Every boolean reads as "disabled". The struct shape mirrors
/// [`ModelCapabilities`] one-to-one but each sub-capability uses a
/// disabled-named twin (`DisabledMediaCapability`,
/// `DisabledReasoningCapability`, `DisabledWebSearchCapability`).
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[non_exhaustive]
pub struct DisabledCapabilities {
    /// Image / vision input is disabled.
    pub vision: DisabledMediaCapability,
    /// Reasoning controls disabled.
    pub reasoning: DisabledReasoningCapability,
    /// Function / tool calling is disabled.
    pub function_calling: bool,
    /// Response-schema-bound output is disabled.
    pub response_schema: bool,
    /// Streaming is disabled.
    pub streaming: bool,
    /// File input is disabled.
    pub file_input: DisabledMediaCapability,
    /// Image generation is disabled.
    pub image_generation: DisabledMediaCapability,
    /// Audio input is disabled.
    pub audio_input: DisabledMediaCapability,
    /// Audio output is disabled.
    pub audio_output: DisabledMediaCapability,
    /// Code interpreter is disabled.
    pub code_interpreter: bool,
    /// Web search is disabled.
    pub web_search: DisabledWebSearchCapability,
}

impl DisabledCapabilities {
    /// "Nothing is disabled" — all flags `false`, all lists empty.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }
}

// ---------------------------------------------------------------------------
// Context Window
// ---------------------------------------------------------------------------

/// Token limits for the model's context window.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ContextWindow {
    pub max_input_tokens: u32,
    /// Maximum output tokens. `None` for embedding models that produce
    /// vectors instead of token sequences.
    pub max_output_tokens: Option<u32>,
    /// Output vector dimensionality for embedding models.
    pub output_vector_size: Option<u32>,
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

/// Estimated performance characteristics.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ModelPerformance {
    /// Expected response latency in milliseconds.
    pub response_latency_ms: Option<u32>,
    /// Expected generation speed in tokens per second.
    pub tokens_per_second: Option<u32>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_status_wire_format_is_lowercase() {
        // Pinned to match DESIGN.md §3.1 — lifecycle_status is queryable via
        // OData on the catalog list endpoint and must match the JSONB-stored
        // string casing.
        for (variant, expected) in [
            (LifecycleStatus::Production, "\"production\""),
            (LifecycleStatus::Preview, "\"preview\""),
            (LifecycleStatus::Experimental, "\"experimental\""),
            (LifecycleStatus::Deprecated, "\"deprecated\""),
            (LifecycleStatus::Sunset, "\"sunset\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: LifecycleStatus = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn approval_status_wire_format_is_lowercase() {
        for (variant, expected) in [
            (ApprovalStatus::Pending, "\"pending\""),
            (ApprovalStatus::Approved, "\"approved\""),
            (ApprovalStatus::Rejected, "\"rejected\""),
            (ApprovalStatus::Revoked, "\"revoked\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: ApprovalStatus = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn provider_status_wire_format_is_lowercase() {
        for (variant, expected) in [
            (ProviderStatus::Active, "\"active\""),
            (ProviderStatus::Disabled, "\"disabled\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: ProviderStatus = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn supported_api_wire_format_is_lowercase() {
        // Pinned to match DESIGN.md §3.1; OData filters on
        // `info.supported_api` (DESIGN.md §3.3) compare against these strings.
        for (variant, expected) in [
            (SupportedApi::Completion, "\"completion\""),
            (SupportedApi::Embedding, "\"embedding\""),
            (SupportedApi::Batch, "\"batch\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: SupportedApi = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn reasoning_effort_wire_format_is_lowercase() {
        // Pinned to match gts.cf.llmgw.core.reasoning_config.v1~ — gateway
        // schema expects lowercase strings.
        for (variant, expected) in [
            (ReasoningEffort::None, "\"none\""),
            (ReasoningEffort::Low, "\"low\""),
            (ReasoningEffort::Medium, "\"medium\""),
            (ReasoningEffort::High, "\"high\""),
            (ReasoningEffort::XHigh, "\"xhigh\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: ReasoningEffort = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn service_tier_wire_format_is_lowercase() {
        // Pinned to match gts.cf.llmgw.core.create_response_body.v1~.
        for (variant, expected) in [
            (ServiceTier::Auto, "\"auto\""),
            (ServiceTier::Default, "\"default\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: ServiceTier = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }
}
