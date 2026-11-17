// Created: 2026-05-06 by Constructor Tech
// Updated: 2026-11-10 by Constructor Tech
//! Provider-independent model types — enums, capability flags, the context
//! window, cost, and performance characteristics.
//!
//! These types live on [`crate::models::ModelInfoV1`] directly because their
//! shape is meaningful for every provider. The per-model override policy is
//! expressed as flat fields (`allow_parameter_override`, `allow_extra_params`)
//! on [`crate::models::ModelInfoV1`].

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Sharing mode of a provider or model (DESIGN §3.1).
///
/// `Tenant` entries are visible to the owning tenant only; `Shared` entries
/// are visible to the owner and all descendant tenants. A model reaches a
/// descendant only when both the model and its provider are `Shared`.
///
/// Wire format is lowercase (`"tenant" | "shared"`) and matches the
/// `sharing_mode` columns and the `OData` field of the same name.
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
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SharingMode {
    /// Owner only. The default.
    #[default]
    Tenant,
    /// Owner and all descendant tenants.
    Shared,
}

impl SharingMode {
    /// Every variant, in declaration order.
    pub const ALL: &[Self] = &[Self::Tenant, Self::Shared];

    /// Lowercase wire/storage string — identical to the serde representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::Shared => "shared",
        }
    }

    /// Parse from the lowercase wire/storage string. `None` for any value
    /// outside the documented domain.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "tenant" => Some(Self::Tenant),
            "shared" => Some(Self::Shared),
            _ => None,
        }
    }
}

/// Approval state of a model, set by the owning tenant (DESIGN §3.1
/// Approval).
///
/// Only `Approved` models resolve or appear in the available list. Any state
/// may follow any other state.
///
/// Wire format is lowercase and matches the `approval_state` column and
/// `OData` field.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ApprovalState {
    Pending,
    Approved,
    Rejected,
    Revoked,
}

impl ApprovalState {
    /// Every variant, in declaration order.
    pub const ALL: &[Self] = &[Self::Pending, Self::Approved, Self::Rejected, Self::Revoked];

    /// Lowercase wire/storage string — identical to the serde representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Revoked => "revoked",
        }
    }

    /// Parse from the lowercase wire/storage string. `None` for any value
    /// outside the documented domain.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

/// API kind that a model exposes.
///
/// A model may expose multiple API surfaces (e.g. `[Completion, Batch]` for
/// a chat model that's also reachable via the asynchronous batch API). Each
/// variant corresponds to a distinct LLM Gateway endpoint family.
///
/// Wire format is lowercase to match `DESIGN.md §3.1`. A closed set owned by
/// the registry; adding a value is a non-breaking change within `v1`.
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
    /// Asynchronous batch API. May coexist with `Completion` / `Embedding`
    /// on the same model.
    Batch,
}

impl SupportedApi {
    /// Every variant, in declaration order.
    pub const ALL: &[Self] = &[Self::Completion, Self::Embedding, Self::Batch];

    /// Lowercase wire/storage string — identical to the serde representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::Embedding => "embedding",
            Self::Batch => "batch",
        }
    }

    /// Parse from the lowercase wire/storage string. `None` for any value
    /// outside the documented domain.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "completion" => Some(Self::Completion),
            "embedding" => Some(Self::Embedding),
            "batch" => Some(Self::Batch),
            _ => None,
        }
    }
}

/// Unified reasoning effort level used by `default_parameters.reasoning.effort`.
///
/// Provider-specific reasoning levels live in the provider settings schema
/// published by the LLM Gateway provider plugin, not here.
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
/// Provider-specific tiers live in the provider settings schema published
/// by the LLM Gateway provider plugin, not here.
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

/// Reasoning controls the model accepts.
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
#[serde(default)]
#[non_exhaustive]
pub struct ReasoningCapability {
    /// Accepts `reasoning.effort`.
    pub effort: bool,
    /// Reasoning can be switched off (`effort = none`).
    pub toggle: bool,
    /// Accepts reasoning items from a previous turn (encrypted content).
    pub resume: bool,
    /// Accepts `reasoning.budget_tokens`.
    pub budget: bool,
    /// Accepts `reasoning.summary`.
    pub summary: bool,
}

/// Web search capability flags.
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
#[serde(default)]
#[non_exhaustive]
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

/// Tool-calling controls the model accepts.
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
#[serde(default)]
#[non_exhaustive]
pub struct ToolsCapability {
    /// Accepts `tools` (function calling).
    pub enabled: bool,
    /// Accepts `parallel_tool_calls`.
    pub parallel_tool_calls: bool,
    /// Accepts `max_tool_calls`.
    pub max_tool_calls: bool,
    /// Accepts `tool_choice = "required"`.
    pub tool_choice_required: bool,
    /// Accepts `tool_choice = "none"`.
    pub tool_choice_none: bool,
    /// Accepts a named function in `tool_choice`.
    pub tool_choice_named: bool,
}

/// Output-format controls the model accepts.
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
#[serde(default)]
#[non_exhaustive]
pub struct OutputCapability {
    /// Accepts `text.format = json_object`.
    pub json_mode: bool,
    /// Accepts `text.format = json_schema`.
    pub json_schema: bool,
    /// Accepts `strict = true` on a JSON schema format.
    pub strict_schema: bool,
    /// Accepts `text.verbosity`.
    pub verbosity: bool,
}

/// Sampling parameters the model accepts.
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
#[serde(default)]
#[non_exhaustive]
pub struct SamplingCapability {
    /// Accepts `temperature`.
    pub temperature: bool,
    /// Accepts `top_p`.
    pub top_p: bool,
    /// Accepts `seed`.
    pub seed: bool,
    /// Accepts `presence_penalty`.
    pub presence_penalty: bool,
    /// Accepts `frequency_penalty`.
    pub frequency_penalty: bool,
    /// Accepts `top_logprobs` and the `message.output_text.logprobs` include.
    pub logprobs: bool,
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
#[serde(default)]
#[non_exhaustive]
pub struct MediaCapability {
    /// Whether the capability is available.
    pub enabled: bool,
    /// Accepted (or produced) media types — RFC 6838 names. Empty when
    /// `enabled` is `false` or the provider doesn't surface a per-type list.
    pub supported_mime_types: Vec<String>,
}

/// Capability flags describing what the model can do.
///
/// Every field defaults to "not supported" when absent from JSON.
///
/// `Copy` is intentionally not derived: [`MediaCapability`] carries a
/// `Vec<String>`, so the struct is `Clone` only.
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(default)]
#[non_exhaustive]
pub struct ModelCapabilities {
    /// Supports image/vision input.
    pub vision: MediaCapability,
    /// Reasoning controls.
    pub reasoning: ReasoningCapability,
    /// Tool-calling controls.
    pub tools: ToolsCapability,
    /// Output-format controls.
    pub output: OutputCapability,
    /// Sampling parameters.
    pub sampling: SamplingCapability,
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
    /// Accepts `truncation`.
    pub truncation: bool,
    /// Accepts `instructions` (system prompt).
    pub instructions: bool,
}

/// Disablement flags for a [`MediaCapability`].
///
/// `disabled = true` removes the whole capability from the supported set.
/// Otherwise `disabled_mime_types` lists MIME types subtracted from the
/// supported list.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(default)]
#[non_exhaustive]
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
#[serde(default)]
#[non_exhaustive]
pub struct DisabledReasoningCapability {
    /// `reasoning.effort` is disabled.
    pub effort: bool,
    /// Reasoning toggle is disabled.
    pub toggle: bool,
    /// Resume / continue reasoning is disabled.
    pub resume: bool,
    /// Reasoning token budget is disabled.
    pub budget: bool,
    /// `reasoning.summary` is disabled.
    pub summary: bool,
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
#[serde(default)]
#[non_exhaustive]
pub struct DisabledWebSearchCapability {
    /// The whole web-search capability is disabled.
    pub disabled: bool,
    /// Configuring the allow-list of domains is disabled.
    pub allowed_domains: bool,
    /// Configuring the deny-list of domains is disabled.
    pub excluded_domains: bool,
}

/// Disablement flags for [`ToolsCapability`].
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
#[serde(default)]
#[non_exhaustive]
pub struct DisabledToolsCapability {
    /// The whole tool-calling capability is disabled.
    pub disabled: bool,
    /// `parallel_tool_calls` is disabled.
    pub parallel_tool_calls: bool,
    /// `max_tool_calls` is disabled.
    pub max_tool_calls: bool,
    /// `tool_choice = "required"` is disabled.
    pub tool_choice_required: bool,
    /// `tool_choice = "none"` is disabled.
    pub tool_choice_none: bool,
    /// Named `tool_choice` is disabled.
    pub tool_choice_named: bool,
}

/// Disablement flags for [`OutputCapability`].
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
#[serde(default)]
#[non_exhaustive]
pub struct DisabledOutputCapability {
    /// JSON mode is disabled.
    pub json_mode: bool,
    /// JSON schema output is disabled.
    pub json_schema: bool,
    /// Strict schema enforcement is disabled.
    pub strict_schema: bool,
    /// `text.verbosity` is disabled.
    pub verbosity: bool,
}

/// Disablement flags for [`SamplingCapability`].
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
#[serde(default)]
#[non_exhaustive]
pub struct DisabledSamplingCapability {
    /// `temperature` is disabled.
    pub temperature: bool,
    /// `top_p` is disabled.
    pub top_p: bool,
    /// `seed` is disabled.
    pub seed: bool,
    /// `presence_penalty` is disabled.
    pub presence_penalty: bool,
    /// `frequency_penalty` is disabled.
    pub frequency_penalty: bool,
    /// Log-probabilities are disabled.
    pub logprobs: bool,
}

/// Subtractive view over [`ModelCapabilities`]: which capability flags are
/// administratively blocked for this model.
///
/// Every boolean reads as "disabled". The struct shape mirrors
/// [`ModelCapabilities`] one-to-one but each sub-capability uses a
/// disabled-named twin (`DisabledMediaCapability`,
/// `DisabledReasoningCapability`, `DisabledToolsCapability`,
/// `DisabledOutputCapability`, `DisabledSamplingCapability`,
/// `DisabledWebSearchCapability`). Every field defaults to "not disabled"
/// when absent from JSON.
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(default)]
#[non_exhaustive]
pub struct DisabledCapabilities {
    /// Image / vision input is disabled.
    pub vision: DisabledMediaCapability,
    /// Reasoning controls disabled.
    pub reasoning: DisabledReasoningCapability,
    /// Tool-calling controls disabled.
    pub tools: DisabledToolsCapability,
    /// Output-format controls disabled.
    pub output: DisabledOutputCapability,
    /// Sampling parameters disabled.
    pub sampling: DisabledSamplingCapability,
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
    /// `truncation` is disabled.
    pub truncation: bool,
    /// `instructions` is disabled.
    pub instructions: bool,
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
#[non_exhaustive]
pub struct ContextWindow {
    pub max_input_tokens: u32,
    /// Maximum output tokens. `None` for embedding models that produce
    /// vectors instead of token sequences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Output vector dimensionality for embedding models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_vector_size: Option<u32>,
}

impl ContextWindow {
    /// A context window of `max_input_tokens` with no output limit or vector
    /// size.
    #[must_use]
    pub fn new(max_input_tokens: u32) -> Self {
        Self {
            max_input_tokens,
            max_output_tokens: None,
            output_vector_size: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

/// Estimated performance characteristics.
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
#[non_exhaustive]
pub struct ModelPerformance {
    /// Expected response latency in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_latency_ms: Option<u32>,
    /// Expected generation speed in tokens per second.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_second: Option<u32>,
}

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

/// Cost in AI credits, one shape for every provider type. Rates are in
/// micro-credits: 1 AI credit = 1,000,000 micro-credits. An absent rate means
/// the model does not price that kind of usage. The registry stores the rates
/// and applies no range limits; LLM Gateway computes the cost of a call from
/// them. Field names are the DESIGN §3.1 Cost names.
#[allow(clippy::struct_field_names)]
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
#[serde(default)]
#[non_exhaustive]
pub struct ModelCostV1 {
    /// Per 1,000,000 input tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_per_1m_micro: Option<u64>,
    /// Per 1,000,000 output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_per_1m_micro: Option<u64>,
    /// Per 1,000,000 input tokens read from the provider's prompt cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_per_1m_micro: Option<u64>,
    /// Per 1,000,000 input tokens written to the provider's prompt cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_per_1m_micro: Option<u64>,
    /// Per 1,000 web search calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search_per_1k_calls_micro: Option<u64>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_window_new_sets_only_max_input_tokens() {
        let cw = ContextWindow::new(128_000);
        assert_eq!(cw.max_input_tokens, 128_000);
        assert_eq!(cw.max_output_tokens, None);
        assert_eq!(cw.output_vector_size, None);
    }

    #[test]
    fn cost_defaults_to_all_absent_and_skips_absent_rates() {
        let cost: ModelCostV1 = serde_json::from_str("{}").unwrap();
        assert_eq!(cost, ModelCostV1::default());
        assert_eq!(serde_json::to_value(cost).unwrap(), serde_json::json!({}));

        let cost = ModelCostV1 {
            input_per_1m_micro: Some(2_500_000),
            ..ModelCostV1::default()
        };
        assert_eq!(
            serde_json::to_value(cost).unwrap(),
            serde_json::json!({ "input_per_1m_micro": 2_500_000 })
        );
    }

    /// `ALL` is the domain the API layer renders in "expected one of …"
    /// validation messages, and `as_str` / `from_wire` are the hand-written
    /// storage-layer twins of it — pinned together so they cannot drift apart.
    fn assert_wire_domain<T: Copy + PartialEq + std::fmt::Debug>(
        all: &[T],
        cases: &[(T, &str)],
        as_str: fn(T) -> &'static str,
        from_wire: fn(&str) -> Option<T>,
    ) {
        let from_cases: Vec<T> = cases.iter().map(|(v, _)| *v).collect();
        assert_eq!(all, from_cases.as_slice(), "ALL drifted from the variants");
        for (variant, expected) in cases {
            assert_eq!(as_str(*variant), *expected, "as_str drift on {variant:?}");
            assert_eq!(from_wire(expected), Some(*variant));
        }
        assert_eq!(from_wire(""), None);
        assert_eq!(from_wire("not-a-variant"), None);
    }

    #[test]
    fn sharing_mode_wire_domain() {
        assert_wire_domain(
            SharingMode::ALL,
            &[
                (SharingMode::Tenant, "tenant"),
                (SharingMode::Shared, "shared"),
            ],
            SharingMode::as_str,
            SharingMode::from_wire,
        );
        // Casing is significant: OData filters compare against the lowercase
        // strings the column stores.
        assert_eq!(SharingMode::from_wire("Tenant"), None);
        assert_eq!(SharingMode::default(), SharingMode::Tenant);
    }

    #[test]
    fn approval_state_wire_domain() {
        assert_wire_domain(
            ApprovalState::ALL,
            &[
                (ApprovalState::Pending, "pending"),
                (ApprovalState::Approved, "approved"),
                (ApprovalState::Rejected, "rejected"),
                (ApprovalState::Revoked, "revoked"),
            ],
            ApprovalState::as_str,
            ApprovalState::from_wire,
        );
    }

    #[test]
    fn supported_api_wire_domain() {
        assert_wire_domain(
            SupportedApi::ALL,
            &[
                (SupportedApi::Completion, "completion"),
                (SupportedApi::Embedding, "embedding"),
                (SupportedApi::Batch, "batch"),
            ],
            SupportedApi::as_str,
            SupportedApi::from_wire,
        );
    }

    /// Absent capability fields read as "not supported" / "not disabled".
    #[test]
    fn capabilities_default_when_absent() {
        let caps: ModelCapabilities = serde_json::from_str("{}").unwrap();
        assert_eq!(caps, ModelCapabilities::default());
        let caps: ModelCapabilities =
            serde_json::from_value(serde_json::json!({ "tools": { "enabled": true } })).unwrap();
        assert!(caps.tools.enabled && !caps.tools.parallel_tool_calls);
        assert_eq!(caps.sampling, SamplingCapability::default());

        let off: DisabledCapabilities = serde_json::from_str("{}").unwrap();
        assert_eq!(off, DisabledCapabilities::none());
    }

    #[test]
    fn capabilities_roundtrip() {
        let wire = serde_json::json!({
            "vision": { "enabled": true, "supported_mime_types": ["image/png"] },
            "reasoning": { "effort": true, "toggle": false, "resume": true, "budget": false, "summary": true },
            "tools": {
                "enabled": true, "parallel_tool_calls": true, "max_tool_calls": false,
                "tool_choice_required": true, "tool_choice_none": true, "tool_choice_named": false
            },
            "output": { "json_mode": true, "json_schema": true, "strict_schema": false, "verbosity": false },
            "sampling": {
                "temperature": true, "top_p": true, "seed": false,
                "presence_penalty": false, "frequency_penalty": false, "logprobs": true
            },
            "streaming": true,
            "file_input": { "enabled": false, "supported_mime_types": [] },
            "image_generation": { "enabled": false, "supported_mime_types": [] },
            "audio_input": { "enabled": false, "supported_mime_types": [] },
            "audio_output": { "enabled": false, "supported_mime_types": [] },
            "code_interpreter": false,
            "web_search": { "enabled": false, "allowed_domains": false, "excluded_domains": false },
            "truncation": true,
            "instructions": true
        });
        let caps: ModelCapabilities = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&caps).unwrap(), wire);
    }
}
