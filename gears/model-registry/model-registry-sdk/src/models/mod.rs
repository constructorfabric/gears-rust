// Created: 2026-05-06 by Constructor Tech
// Updated: 2026-05-07 by Constructor Tech
//! Public models for the `model-registry` module.
//!
//! These data structures define the contract between the `model-registry`
//! module and its consumers. GTS by design needs serde + `JsonSchema` for
//! runtime schema reflection, so the GTS-decorated structs and their inner
//! types derive `serde::Serialize + serde::Deserialize + schemars::JsonSchema`
//! — see `docs/DESIGN.md` §3.1 "SDK serde policy".
//!
//! Layout:
//! - [`common`] — provider-independent enums (including the narrowed
//!   two-variant [`ServiceTier`]), capabilities, context window, performance.
//! - [`default_parameters`] — the unified user-facing
//!   [`DefaultInferenceParametersV1`] and its supporting types
//!   ([`TextFormat`], [`TextFormatKind`], [`TextVerbosity`],
//!   [`ReasoningConfig`], [`ReasoningSummary`], [`ToolChoice`],
//!   [`TruncationStrategy`]).
//! - [`providers`] — **extension point**: one file per provider, defining
//!   its flat provider-settings aggregate plus the nested cost struct.
//!   Per-provider types are versioned independently of the envelope (current
//!   generation uses the `V1` suffix; future revisions can ship alongside
//!   as `V2`, `V3`, …). Provider-specific helper enums (e.g.
//!   [`OpenAiServiceTier`]) live next to their provider's file. The set of
//!   shipped providers is open-ended — see [`providers`] for "how to add a
//!   new provider".
//! - [`info`] — [`ModelInfoV1<P>`], including the flat per-model override
//!   fields (`allow_parameter_override`, `allow_extra_params`).
//! - [`entity`] — [`ModelV1<P>`] and [`ProviderV1`].
//! - [`request`] — request DTOs ([`CreateProviderRequestV1`], …).

pub mod common;
pub mod default_parameters;
pub mod entity;
pub mod info;
pub mod providers;
pub mod request;

// Re-exports — the public surface of `model_registry_sdk::models::*`.

pub use common::{
    ApprovalStatus, ContextWindow, DisabledCapabilities, DisabledMediaCapability,
    DisabledReasoningCapability, DisabledWebSearchCapability, LifecycleStatus, MediaCapability,
    ModelCapabilities, ModelPerformance, ProviderStatus, ReasoningCapability, ReasoningEffort,
    ServiceTier, SupportedApi, WebSearchCapability,
};

pub use default_parameters::{
    DefaultInferenceParametersV1, NamedToolChoice, ReasoningConfig, ReasoningSummary, TextFormat,
    TextFormatKind, TextVerbosity, ToolChoice, ToolChoiceMode, TruncationStrategy,
};

pub use providers::{
    AnthropicCost, AnthropicJsonOutputFormat, AnthropicOutputConfig, AnthropicOutputEffort,
    AnthropicServiceTier, AnthropicSettingsV1, AnthropicThinking, AnthropicThinkingDisplay,
    AnthropicToolChoice, OpenAiCost, OpenAiEmbeddingEncoding, OpenAiEndpoint,
    OpenAiPromptCacheRetention, OpenAiReasoningEffort, OpenAiResponseFormat, OpenAiServiceTier,
    OpenAiSettingsV1,
};

pub use info::ModelInfoV1;

pub use entity::{ModelV1, ProviderV1};

pub use request::{
    CreateModelRequestV1, CreateProviderRequestV1, CreateProviderRequestV1Builder,
    UpdateModelRequestV1, UpdateProviderRequestV1,
};
