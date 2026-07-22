// Created: 2026-07-09 by Constructor Tech
//! LLM Gateway SDK
//!
//! Rust models for the LLM Gateway's Open Responses–aligned domain, translated
//! from the JSON Schemas under `llm-gateway-sdk/schemas/`. Covers the core
//! request/response, item, content, tool, and streaming (server-sent event)
//! families.
//!
//! Schema polymorphism (an `allOf` chain with a `const` `type` discriminator)
//! maps to plain serde enums (`#[serde(tag = "type")]`).

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod errors;
pub mod gts;
pub mod models;
pub mod plugin_api;

pub use api::{LlmGatewayClientV1, ResponseEventStream};
pub use errors::LlmGatewayError;
pub use gts::LlmGatewayProviderPluginSpecV1;
pub use models::content::{
    Annotation, AudioFormat, ImageDetail, InputAudio, InputContentPart, InputFile, InputImage,
    InputText, InputVideo, LogProb, OutputContentPart, OutputText, Refusal, TopLogProb,
    UrlCitation,
};
pub use models::core::{
    CreateResponseBody, EmbeddingData, EmbeddingInput, EmbeddingRequest, EmbeddingResponse,
    EmbeddingVector, EncodingFormat, FallbackConfig, FallbackStrategy, IncludeField,
    IncompleteDetails, InputTokensDetails, NamedToolChoice, OutputTokensDetails, ReasoningConfig,
    ReasoningEffort, ReasoningSummary, ResponseError, ResponseInput, ResponseResource,
    ResponseStatus, ServiceTier, StreamOptions, TextFormat, TextFormatKind, TextVerbosity,
    ToolChoice, ToolChoiceMode, TruncationStrategy, Usage,
};
pub use models::extension::Extension;
pub use models::items::{
    DataOutput, FunctionCallItem, FunctionCallOutputItem, InputContent, InputItem, ItemReference,
    ItemStatus, MessageItem, MessageOutput, OutputItem, ReasoningContentPart, ReasoningItem,
    ReasoningOutput, ReasoningSummaryPart,
};
pub use models::role::Role;
pub use models::plugin::{MediaInputMode, ProviderCallCtx, ProviderPluginCapabilities};
pub use models::streaming::{
    ContentDeltaEvent, ContentPartEvent, DataEvent, ErrorEvent, FunctionCallArgumentsEvent,
    OutputItemEvent, OutputTextAnnotationAddedEvent, OutputTextEvent, ReasoningSummaryTextEvent,
    ResponseSnapshotEvent, StreamingEvent, SummaryPartEvent,
};
pub use models::tools::{
    AspectRatio, FunctionTool, ImageGenerationTool, ImageOutputFormat, ImageQuality,
    ImageResponseFormat, Resolution, Schema, Tool, ToolInlineGts, ToolReference,
};
pub use plugin_api::LlmGatewayProviderPluginClientV1;
