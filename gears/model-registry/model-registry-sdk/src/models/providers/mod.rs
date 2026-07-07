// Created: 2026-05-07 by Constructor Tech
//! Per-provider settings — **the extension point for new providers**.
//!
//! Each file in this directory is one provider's typed settings: a flat
//! aggregate struct, the nested cost struct, and any provider-specific
//! helper enums. The aggregate shape is intentionally **flat** — connection
//! routing, default inference parameters, and provider-specific knobs sit
//! directly on the struct; only `cost` is nested. Provider-independent
//! defaults that the gateway exposes uniformly to callers live on
//! [`crate::models::ModelInfoV1::default_parameters`], not here.
//!
//! Per-provider types are versioned independently from the envelope. The
//! current generation uses the `V1` suffix; future revisions of any one
//! provider can ship alongside (e.g. `V2`, `V3`, …) with their own GTS
//! schema ids.
//!
//! The set of providers shipped with the SDK is **not closed** — additional
//! providers can be added in this directory without touching anything else
//! in the SDK. The current shipped set lives alongside this `mod.rs`.
//!
//! # Adding a new provider
//!
//! 1. Create `providers/<name>.rs` and define your provider settings
//!    aggregate (e.g. `<Name>SettingsV<n>`). Mirror the existing files: flat
//!    fields for connection routing and provider-wire defaults, a nested
//!    cost struct, and any provider-specific helper enums (response format
//!    variants, tool-choice shapes, etc.).
//!
//! 2. Decorate the aggregate with [`gts_macros::struct_to_gts_schema`]:
//!
//!    ```ignore
//!    #[struct_to_gts_schema(
//!        dir_path = "schemas",
//!        base = ModelInfoV1,
//!        type_id = "gts.cf.genai.model.info.v1~<vendor>.<package>.<name>.v<n>~",
//!        description = "<Name> provider settings",
//!        properties = "<comma-separated list of every flat field, ending with `cost`>"
//!    )]
//!    pub struct <Name>SettingsV<n> { /* … */ }
//!    ```
//!
//! 3. Wire it up in this `mod.rs`: add `pub mod <name>;` below and re-export
//!    the public types via `pub use <name>::{ … };`.
//!
//! 4. Add the new GTS schema id to the `GtsSchemaId` chain in
//!    [`docs/DESIGN.md`](../../../../docs/DESIGN.md) §3.1 (Key Domain Types
//!    fence) and add a sub-section under "Provider-specific settings".
//!
//! No central enum / no `match` to extend — the provider family is identified
//! solely by `info.gts_type`. Forward compat for unknown providers is
//! automatic: a model with a `gts_type` the SDK doesn't recognize still
//! carries its `provider_settings` as raw JSON (`serde_json::Value`), and
//! operators can wire up routing without an SDK release.

pub mod anthropic;
pub mod openai;

pub use anthropic::{
    AnthropicCost, AnthropicJsonOutputFormat, AnthropicOutputConfig, AnthropicOutputEffort,
    AnthropicServiceTier, AnthropicSettingsV1, AnthropicThinking, AnthropicThinkingDisplay,
    AnthropicToolChoice,
};

pub use openai::{
    OpenAiCost, OpenAiEmbeddingEncoding, OpenAiEndpoint, OpenAiPromptCacheRetention,
    OpenAiReasoningEffort, OpenAiResponseFormat, OpenAiServiceTier, OpenAiSettingsV1,
};
