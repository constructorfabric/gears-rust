//! Typed wire contract for the HTTP webhook transport.
//!
//! A webhook backend receives one JSON `POST` per plugin hook and answers with
//! either a session response (JSON) or an NDJSON stream of
//! [`StreamingEvent`](crate::models::StreamingEvent) lines. These types are
//! the contract: the Rust definitions, not generated schemas, are the source
//! of truth. See `docs/WEBHOOK-PROTOCOL.md` for the prose description.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::{Capability, CapabilityValue, Message};
use crate::plugin::SessionPluginResponse;

/// Value of the `event` key in every webhook request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookEvent {
    /// `on_session_type_configured` (session response expected).
    SessionTypeConfigured,
    /// `on_session_created` (session response expected).
    SessionCreated,
    /// `on_session_updated` (session response expected).
    SessionUpdated,
    /// `on_message` (NDJSON stream expected).
    Message,
    /// `on_message_recreate` (NDJSON stream expected).
    MessageRecreate,
    /// `on_session_summary` (NDJSON stream expected).
    SessionSummary,
}

/// Request body for the message hooks (`message`, `message_recreate`).
///
/// `enabled_capabilities` is omitted (never `null`) when the call carries none.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookMessageRequest<'a> {
    pub event: WebhookEvent,
    pub session_id: Uuid,
    pub message_id: Uuid,
    pub messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_capabilities: Option<&'a [CapabilityValue]>,
}

/// Request body for the session-scoped hooks (`session_type_configured`,
/// `session_created`, `session_updated`, `session_summary`).
///
/// `session_id` is `null` for `session_type_configured` (no session exists
/// yet). `enabled_capabilities` is omitted when the call carries none.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookSessionRequest<'a> {
    pub event: WebhookEvent,
    pub session_type_id: Uuid,
    pub session_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_capabilities: Option<&'a [CapabilityValue]>,
}

/// Response body of the session hooks. Two shapes are accepted: a bare
/// capability array, or an object with optional `capabilities` and `metadata`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum WebhookSessionResponse {
    /// `[ {capability}, .. ]`
    Capabilities(Vec<Capability>),
    /// `{ "capabilities": [..], "metadata": {..} }` (both keys optional;
    /// `metadata: null` is treated as absent).
    Object {
        #[serde(default)]
        capabilities: Vec<Capability>,
        #[serde(default)]
        metadata: Option<serde_json::Value>,
    },
}

impl From<WebhookSessionResponse> for SessionPluginResponse {
    fn from(resp: WebhookSessionResponse) -> Self {
        match resp {
            WebhookSessionResponse::Capabilities(capabilities) => Self::from(capabilities),
            WebhookSessionResponse::Object {
                capabilities,
                metadata,
            } => Self {
                capabilities,
                metadata,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_response_accepts_bare_array_and_object_shapes() {
        let cap = serde_json::json!({"name": "model", "value": {"type": "enum"}});
        let bare: WebhookSessionResponse =
            serde_json::from_value(serde_json::json!([cap])).unwrap();
        let r = SessionPluginResponse::from(bare);
        assert_eq!(r.capabilities.len(), 1);
        assert!(r.metadata.is_none());

        let obj: WebhookSessionResponse = serde_json::from_value(
            serde_json::json!({"capabilities": [cap], "metadata": {"k": 1}}),
        )
        .unwrap();
        let r = SessionPluginResponse::from(obj);
        assert_eq!(r.capabilities.len(), 1);
        assert_eq!(r.metadata, Some(serde_json::json!({"k": 1})));

        let empty: WebhookSessionResponse =
            serde_json::from_value(serde_json::json!({"metadata": null})).unwrap();
        let r = SessionPluginResponse::from(empty);
        assert!(r.capabilities.is_empty() && r.metadata.is_none());
    }
}
