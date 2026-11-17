// Created: 2026-11-16 by Constructor Tech
//! Models for the pass-through APIs (`cpt-cf-llm-gateway-fr-pass-through-apis-v1`).
//!
//! A pass-through route (`/llm-gateway/v1/passthrough/openai/{*path}`,
//! `/llm-gateway/v1/passthrough/anthropic/{*path}`) keeps the provider's
//! native request, response, and streaming format but runs the same Gateway
//! pipeline as `POST /responses`. The core hands the native request to the
//! provider plugin that implements the route's API
//! ([`crate::plugin_api::LlmGatewayProviderPluginClientV1::forward_passthrough`])
//! and gets back the native response together with the generic usage read
//! from it. The core prices that usage and adds the Gateway usage extensions
//! ([`PassthroughUsageExtension`]) to the provider's native `usage` object.
//!
//! These are in-process values between the core and a plugin; only
//! [`PassthroughApi`], the endpoint allowlist, and [`PassthroughUsageExtension`]
//! have a wire form.

use std::pin::Pin;

use futures_core::Stream;
use rust_decimal::Decimal;

use crate::errors::{LlmGatewayError, ProviderErrorCategory, ProviderFailure};
use crate::models::core::Usage;

/// Response header carrying the Gateway response `id` on every pass-through
/// response, success or error, JSON or SSE (for SSE, sent with the response
/// headers before the first event). The native body carries only the
/// provider's own id; this one is the idempotency key of the usage record and
/// the id in quota, logs, and traces.
pub const RESPONSE_ID_HEADER: &str = "x-llm-gateway-response-id";

/// Provider API exposed as a pass-through route.
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
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PassthroughApi {
    /// The `OpenAI` API (`/llm-gateway/v1/passthrough/openai/{*path}`).
    #[serde(rename = "openai")]
    OpenAi,
    /// The Anthropic API (`/llm-gateway/v1/passthrough/anthropic/{*path}`).
    Anthropic,
}

impl PassthroughApi {
    /// Lowercase wire name, also the route segment after `passthrough/`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }
}

/// One stateless provider endpoint a plugin forwards on its pass-through
/// route.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[non_exhaustive]
pub struct PassthroughEndpoint {
    /// Provider path, relative to the route (e.g. `/v1/responses`). Only
    /// `POST` is forwarded. The model listing (`GET /v1/models`,
    /// `GET /v1/models/{model_id}`) is not declared here: the core answers it
    /// from Model Registry without calling the plugin.
    pub path: String,
    /// Whether the endpoint generates output: a metered endpoint gets a quota
    /// lease, an AI credit cost, and usage enrichment; a non-metered one (for
    /// example Anthropic `/v1/messages/count_tokens`) gets none.
    pub metered: bool,
}

impl PassthroughEndpoint {
    /// A metered endpoint at `path`.
    #[must_use]
    pub fn metered(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            metered: true,
        }
    }

    /// A non-metered endpoint at `path`.
    #[must_use]
    pub fn not_metered(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            metered: false,
        }
    }
}

/// Pass-through support a plugin reports in its integration capabilities.
///
/// The endpoint list is the route's allowlist: the core checks the request
/// path against it before the capability check and quota admission, and
/// answers an undeclared path with a provider-format 404 without a provider
/// call. A plugin declares only stateless endpoints.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[non_exhaustive]
pub struct PassthroughCapability {
    /// The provider API the plugin implements.
    pub api: PassthroughApi,
    /// Stateless endpoints the plugin forwards.
    pub endpoints: Vec<PassthroughEndpoint>,
}

impl PassthroughCapability {
    /// Pass-through support for `api` with the given endpoint allowlist.
    #[must_use]
    pub fn new(api: PassthroughApi, endpoints: Vec<PassthroughEndpoint>) -> Self {
        Self { api, endpoints }
    }

    /// The declared endpoint at `path`, if any.
    #[must_use]
    pub fn endpoint(&self, path: &str) -> Option<&PassthroughEndpoint> {
        self.endpoints.iter().find(|e| e.path == path)
    }
}

/// A pass-through request the core hands to the plugin.
///
/// The body is the consumer's native body as received: `model` still holds
/// the Gateway model identifier, and nothing has been removed. The plugin
/// first checks it against its denylists of stateful fields and paid provider
/// tools, then replaces `model` with `provider_model_id`, forces no-store, and
/// builds the provider request headers itself.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PassthroughRequest {
    /// The route's provider API.
    pub api: PassthroughApi,
    /// The declared endpoint path (e.g. `/v1/messages`), already checked
    /// against the plugin's allowlist.
    pub path: String,
    /// Whether the native request asks for streaming, as read by the core
    /// from the body. Selects [`PassthroughResponse::Stream`].
    pub stream: bool,
    /// Provider protocol headers to pass on (`anthropic-version`,
    /// `anthropic-beta`, `OpenAI-Beta`), names lowercase, beta values already
    /// filtered by the core's allowlist. The consumer's `Authorization` and
    /// `x-api-key` are never among them.
    pub headers: Vec<(String, String)>,
    /// The provider-native JSON body.
    pub body: serde_json::Value,
}

impl PassthroughRequest {
    /// A non-streaming request to `path` with no protocol headers.
    #[must_use]
    pub fn new(api: PassthroughApi, path: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            api,
            path: path.into(),
            stream: false,
            headers: Vec::new(),
            body,
        }
    }

    /// Mark the request as streaming.
    #[must_use]
    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Add a protocol header to pass on.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Generic usage the plugin read from a native response or event, and where
/// the native usage object is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PassthroughUsage {
    /// Generic usage normalized by the plugin, with `cost` left at 0 for the
    /// core.
    pub usage: Usage,
    /// RFC 6901 JSON pointer, into the native body or event data, of the
    /// provider's native `usage` object (e.g. `/usage`, `/response/usage`).
    /// The core adds [`PassthroughUsageExtension`] there. `None` when there is
    /// nothing to enrich (an event the consumer did not ask for and that is
    /// not relayed).
    pub pointer: Option<String>,
}

impl PassthroughUsage {
    /// Usage located at `pointer` in the native body or event.
    #[must_use]
    pub fn at(usage: Usage, pointer: impl Into<String>) -> Self {
        Self {
            usage,
            pointer: Some(pointer.into()),
        }
    }

    /// Usage with no native object to enrich.
    #[must_use]
    pub fn detached(usage: Usage) -> Self {
        Self {
            usage,
            pointer: None,
        }
    }
}

/// Gateway usage extensions the core adds to the provider's native `usage`
/// object on a pass-through response or event: the same fields, with the same
/// wire form, as the extensions of [`Usage`].
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[non_exhaustive]
pub struct PassthroughUsageExtension {
    /// Call cost in AI credits, computed by the core from the model's cost
    /// rates. A JSON string (`"0.0125"`).
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub cost: Decimal,
    /// `false` when usage could not be determined reliably.
    pub complete: bool,
    /// `true` when the amounts are an estimate.
    pub estimated: bool,
}

impl From<&Usage> for PassthroughUsageExtension {
    fn from(usage: &Usage) -> Self {
        Self {
            cost: usage.cost,
            complete: usage.complete,
            estimated: usage.estimated,
        }
    }
}

/// A non-streaming pass-through response, or an error response the provider
/// returned before a stream started.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PassthroughJsonResponse {
    /// The provider's HTTP status, relayed as is.
    pub status: u16,
    /// The provider-native body, with `model` already replaced back with the
    /// Gateway model identifier.
    pub body: serde_json::Value,
    /// Usage read from the body; `None` for a non-metered endpoint or an error
    /// without usage.
    pub usage: Option<PassthroughUsage>,
    /// For a provider error response: the normalized category the plugin
    /// mapped it to, which sets `x-should-retry` and the logged code. The
    /// native error body is still relayed unchanged.
    pub error_category: Option<ProviderErrorCategory>,
    /// Provider originals (`provider_request_id`, and for errors the provider
    /// code, message, and `retry_after`).
    pub details: ProviderFailure,
}

impl PassthroughJsonResponse {
    /// A native response with `status` and `body`, no usage, no error.
    #[must_use]
    pub fn new(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            body,
            usage: None,
            error_category: None,
            details: ProviderFailure::default(),
        }
    }

    /// Set the usage read from the body.
    #[must_use]
    pub fn with_usage(mut self, usage: PassthroughUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Mark the response as a provider error of `category`.
    #[must_use]
    pub fn with_error_category(mut self, category: ProviderErrorCategory) -> Self {
        self.error_category = Some(category);
        self
    }

    /// Set the provider originals.
    #[must_use]
    pub fn with_details(mut self, details: ProviderFailure) -> Self {
        self.details = details;
        self
    }
}

/// One provider-native SSE event of a pass-through stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PassthroughEvent {
    /// SSE `event:` field, when the provider sets one (Anthropic, `OpenAI`
    /// Responses).
    pub event: Option<String>,
    /// SSE `data:` payload as text, with `model` already replaced back with
    /// the Gateway model identifier (`[DONE]` for the `OpenAI` Chat Completions
    /// sentinel).
    pub data: String,
    /// Whether the core relays the event to the consumer. `false` for an
    /// event the plugin produced only for the Gateway, such as the `OpenAI`
    /// Chat Completions usage chunk the consumer did not ask for.
    pub relay: bool,
    /// Output tokens this event delivers, as counted by the plugin; the core
    /// sums them for the usage estimate when the stream ends without final
    /// usage.
    pub output_tokens: u64,
    /// Final usage, on the event that carries it.
    pub usage: Option<PassthroughUsage>,
}

impl PassthroughEvent {
    /// A relayed event with `data` and no usage.
    #[must_use]
    pub fn new(data: impl Into<String>) -> Self {
        Self {
            event: None,
            data: data.into(),
            relay: true,
            output_tokens: 0,
            usage: None,
        }
    }

    /// Set the SSE `event:` field.
    #[must_use]
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        self.event = Some(event.into());
        self
    }

    /// Mark the event as not relayed to the consumer.
    #[must_use]
    pub fn not_relayed(mut self) -> Self {
        self.relay = false;
        self
    }

    /// Set the output tokens this event delivers.
    #[must_use]
    pub fn with_output_tokens(mut self, tokens: u64) -> Self {
        self.output_tokens = tokens;
        self
    }

    /// Attach the final usage.
    #[must_use]
    pub fn with_usage(mut self, usage: PassthroughUsage) -> Self {
        self.usage = Some(usage);
        self
    }
}

/// Native event stream of a streaming pass-through call.
///
/// Provider error events are relayed as ordinary events. An `Err` item is a
/// failure after the stream started (transport break, timeout, cancellation)
/// and ends the stream; the core renders it as one provider-native error
/// event and reports usage as incomplete. Dropping the stream closes the
/// provider connection.
pub type PassthroughEventStream =
    Pin<Box<dyn Stream<Item = Result<PassthroughEvent, LlmGatewayError>> + Send>>;

/// Result of a pass-through call.
#[non_exhaustive]
pub enum PassthroughResponse {
    /// A native JSON response: a non-streaming call, or a provider error
    /// returned before a stream started.
    Json(Box<PassthroughJsonResponse>),
    /// A native SSE stream with HTTP status 200.
    Stream {
        /// The native events, in provider order.
        events: PassthroughEventStream,
        /// Request identifier the provider assigned, when known.
        provider_request_id: Option<String>,
    },
}

impl std::fmt::Debug for PassthroughResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(json) => f.debug_tuple("Json").field(json).finish(),
            Self::Stream {
                provider_request_id,
                ..
            } => f
                .debug_struct("Stream")
                .field("provider_request_id", provider_request_id)
                .finish_non_exhaustive(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_api_wire_names() {
        assert_eq!(
            serde_json::to_value(PassthroughApi::OpenAi).unwrap(),
            serde_json::json!("openai")
        );
        assert_eq!(
            serde_json::from_value::<PassthroughApi>(serde_json::json!("anthropic")).unwrap(),
            PassthroughApi::Anthropic
        );
        assert_eq!(PassthroughApi::OpenAi.as_str(), "openai");
    }

    #[test]
    fn capability_finds_declared_endpoints_only() {
        let caps = PassthroughCapability::new(
            PassthroughApi::Anthropic,
            vec![
                PassthroughEndpoint::metered("/v1/messages"),
                PassthroughEndpoint::not_metered("/v1/messages/count_tokens"),
            ],
        );
        assert!(caps.endpoint("/v1/messages").unwrap().metered);
        assert!(!caps.endpoint("/v1/messages/count_tokens").unwrap().metered);
        assert!(caps.endpoint("/v1/files").is_none());
    }

    #[test]
    fn usage_extension_carries_cost_as_string() {
        let usage = Usage::reported(10, 5).with_cost(Decimal::new(125, 4));
        assert_eq!(
            serde_json::to_value(PassthroughUsageExtension::from(&usage)).unwrap(),
            serde_json::json!({ "cost": "0.0125", "complete": true, "estimated": false })
        );
    }
}
