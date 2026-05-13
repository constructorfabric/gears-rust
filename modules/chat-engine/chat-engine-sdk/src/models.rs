use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Tenant identifier. Opaque string from the auth token, used to scope all
/// queries. Newtype distinguishes it from `UserId` at compile time so call
/// sites cannot accidentally swap tenant and user arguments.
///
/// `#[serde(transparent)]` keeps the on-the-wire and DB JSON representation
/// as a plain string, so this is a pure compile-time refinement.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TenantId(String);

impl TenantId {
    /// Constructs a `TenantId`, rejecting empty strings.
    ///
    /// # Panics
    ///
    /// Panics if `s` is empty. An empty tenant id would silently scope queries
    /// to no rows (or all rows, depending on the ORM) and so represents a
    /// latent authorization bug; it must never reach the data layer.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        let s = s.into();
        assert!(!s.is_empty(), "TenantId must not be empty");
        Self(s)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl From<String> for TenantId {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for TenantId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl AsRef<str> for TenantId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// End-user identifier (opaque string from the auth token). Newtype
/// distinguishes it from `TenantId` at compile time.
///
/// `#[serde(transparent)]` keeps the wire/DB representation as a plain string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserId(String);

impl UserId {
    /// Constructs a `UserId`, rejecting empty strings.
    ///
    /// # Panics
    ///
    /// Panics if `s` is empty. An empty user id would defeat ownership checks
    /// downstream and must never reach the data layer.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        let s = s.into();
        assert!(!s.is_empty(), "UserId must not be empty");
        Self(s)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl From<String> for UserId {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for UserId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl AsRef<str> for UserId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A chat session: the top-level container that groups a conversation's
/// messages, tenant/user ownership, backend plugin binding, and lifecycle.
///
/// `Debug` is implemented manually to redact `share_token` — it is a
/// cryptographic bearer secret that grants read-only access to the session
/// and must never appear in logs, tracing spans, or test output.
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier (primary key).
    pub session_id: Uuid,
    /// Tenant that owns the session; all queries are scoped by this value.
    pub tenant_id: TenantId,
    /// End-user who created the session (opaque string from the auth token).
    pub user_id: UserId,
    /// Optional client identifier (e.g., app/device) that initiated the session.
    pub client_id: Option<String>,
    /// Session type this session is bound to; determines which backend plugin
    /// handles messages and which capabilities are exposed. May be `None` for
    /// session types that haven't been configured yet.
    pub session_type_id: Option<Uuid>,
    /// Capability values (from the `Capability` schema declared by the plugin)
    /// actually enabled for this session — typed as JSON because the shape is
    /// plugin-defined. Use `CapabilityValue` for structured access.
    pub enabled_capabilities: Option<serde_json::Value>,
    /// Opaque per-session metadata (client-defined). Chat Engine never
    /// interprets this field beyond storing/retrieving it. Also used internally
    /// to persist `memory_strategy`, `retention_policy`, and `share_expires_at`
    /// under reserved keys.
    pub metadata: Option<serde_json::Value>,
    /// Current lifecycle state (active / archived / soft_deleted / hard_deleted).
    pub lifecycle_state: LifecycleState,
    /// Cryptographically-random token granting read-only access to a shared
    /// view of this session. Present only while sharing is active.
    pub share_token: Option<String>,
    /// Creation timestamp (UTC, RFC3339 on the wire).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last-modified timestamp (UTC, RFC3339 on the wire).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `share_token` is a bearer secret that grants read-only access to
        // this session — anyone with the value can hijack the share link.
        // We surface presence/absence so observability is preserved without
        // leaking the token.
        let share_token_redacted: Option<&'static str> =
            self.share_token.as_ref().map(|_| "<redacted>");
        f.debug_struct("Session")
            .field("session_id", &self.session_id)
            .field("tenant_id", &self.tenant_id)
            .field("user_id", &self.user_id)
            .field("client_id", &self.client_id)
            .field("session_type_id", &self.session_type_id)
            .field("enabled_capabilities", &self.enabled_capabilities)
            .field("metadata", &self.metadata)
            .field("lifecycle_state", &self.lifecycle_state)
            .field("share_token", &share_token_redacted)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// Lifecycle state of a session.
///
/// Allowed transitions:
/// - `Active` ↔ `Archived`, `Active` → `SoftDeleted`, `Active` → `HardDeleted`
/// - `Archived` → `SoftDeleted`, `Archived` → `HardDeleted`
/// - `SoftDeleted` → `Active`, `SoftDeleted` → `HardDeleted`
/// - `HardDeleted` is terminal
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// Session is live and accepts reads/writes.
    Active,
    /// Session is hidden from default listings but remains readable; can be
    /// restored to `Active`.
    Archived,
    /// Session marked for deletion; hidden from listings, reversible via restore.
    SoftDeleted,
    /// Session physically deleted (terminal state); messages and subtree gone.
    HardDeleted,
}

impl LifecycleState {
    /// Canonical lowercase string representation (DB storage format).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
            Self::SoftDeleted => "soft_deleted",
            Self::HardDeleted => "hard_deleted",
        }
    }

    /// Parse from lowercase string (returns `None` for unknown values).
    #[must_use]
    pub fn from_str_value(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "archived" => Some(Self::Archived),
            "soft_deleted" => Some(Self::SoftDeleted),
            "hard_deleted" => Some(Self::HardDeleted),
            _ => None,
        }
    }

    /// Check whether a transition from `self` to `target` is valid per the
    /// session lifecycle state machine.
    #[must_use]
    pub fn can_transition_to(&self, target: &Self) -> bool {
        matches!(
            (self, target),
            (Self::Active, Self::Archived)
                | (Self::Active, Self::SoftDeleted)
                | (Self::Active, Self::HardDeleted)
                | (Self::Archived, Self::Active)
                | (Self::Archived, Self::SoftDeleted)
                | (Self::Archived, Self::HardDeleted)
                | (Self::SoftDeleted, Self::Active)
                | (Self::SoftDeleted, Self::HardDeleted)
        )
    }
}

impl std::fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::LifecycleState;

    #[test]
    fn every_documented_valid_transition_is_allowed() {
        // Mirrors the doc comment on `LifecycleState` exactly — change one,
        // change the other.
        let valid_edges = [
            (LifecycleState::Active, LifecycleState::Archived),
            (LifecycleState::Active, LifecycleState::SoftDeleted),
            (LifecycleState::Active, LifecycleState::HardDeleted),
            (LifecycleState::Archived, LifecycleState::Active),
            (LifecycleState::Archived, LifecycleState::SoftDeleted),
            (LifecycleState::Archived, LifecycleState::HardDeleted),
            (LifecycleState::SoftDeleted, LifecycleState::Active),
            (LifecycleState::SoftDeleted, LifecycleState::HardDeleted),
        ];
        for (from, to) in valid_edges {
            assert!(
                from.can_transition_to(&to),
                "{from:?} -> {to:?} should be a valid transition"
            );
        }
    }

    #[test]
    fn representative_invalid_transitions_are_rejected() {
        // HardDeleted is terminal — nothing leaves it.
        assert!(!LifecycleState::HardDeleted.can_transition_to(&LifecycleState::Active));
        assert!(!LifecycleState::HardDeleted.can_transition_to(&LifecycleState::Archived));
        // Self-loops are not real transitions.
        assert!(!LifecycleState::Active.can_transition_to(&LifecycleState::Active));
    }
}

/// A registered session type — pairs a human-readable name with the backend
/// plugin instance that will process its sessions' messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionType {
    /// Unique session type identifier (primary key).
    pub session_type_id: Uuid,
    /// Human-readable name used by developers when registering a type.
    pub name: String,
    /// GTS plugin instance ID bound to this session type. `None` means the
    /// type is registered but not yet wired to a backend.
    pub plugin_instance_id: Option<String>,
    /// Creation timestamp (UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last-modified timestamp (UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A message node in the immutable conversation tree.
///
/// Messages form a DAG rooted at the session: each message (except the first)
/// has a `parent_message_id`, and siblings sharing the same parent are
/// *variants* differentiated by `variant_index`. Exactly one sibling per parent
/// is `is_active=true`, which defines the current conversation path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Unique message identifier (primary key).
    pub message_id: Uuid,
    /// Session this message belongs to.
    pub session_id: Uuid,
    /// Parent message in the tree; `None` for the first (root) message.
    pub parent_message_id: Option<Uuid>,
    /// Ordinal among siblings sharing the same `parent_message_id` within the
    /// same session. Starts at 0 and increments per recreate.
    #[serde(default)]
    pub variant_index: u32,
    /// True if this variant is currently on the active conversation path.
    /// Exactly one sibling per parent should be active.
    #[serde(default)]
    pub is_active: bool,
    /// Who produced the message: user / assistant / system.
    pub role: MessageRole,
    /// Message payload (plugin-defined shape: text, content parts array, etc.).
    pub content: serde_json::Value,
    /// External file UUIDs referenced by this message. Chat Engine forwards
    /// them opaquely — file content is never fetched by Chat Engine itself.
    #[serde(default)]
    pub file_ids: Vec<Uuid>,
    /// Per-message metadata (model used, finish_reason, usage, etc.). Typed as
    /// JSON because it is plugin-defined.
    pub metadata: Option<serde_json::Value>,
    /// `true` once the assistant finished streaming (or the message was
    /// persisted whole). User messages are always complete on creation.
    #[serde(default = "default_true")]
    pub is_complete: bool,
    /// Hide this message from client UIs (e.g., system messages, internal
    /// summaries that should not appear in the transcript).
    #[serde(default)]
    pub is_hidden_from_user: bool,
    /// Exclude this message from the history sent to backend plugins
    /// (e.g., messages already covered by a newer summary).
    #[serde(default)]
    pub is_hidden_from_backend: bool,
    /// Creation timestamp (UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last-modified timestamp (UTC). Typically changes only when an assistant
    /// placeholder is filled in.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

fn default_true() -> bool {
    true
}

/// Message author role.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    /// End-user input.
    User,
    /// Model/plugin-generated response.
    Assistant,
    /// Internal/system message (summaries, tool output, injected context).
    System,
}

/// Schema declaration of a capability supported by a backend plugin.
///
/// Returned from `on_session_type_configured` / `on_session_created` /
/// `on_session_updated` to tell Chat Engine *what is tunable*. Chat Engine
/// stores these in `session.enabled_capabilities` and exposes the menu to
/// clients. See also `CapabilityValue` for the chosen-value counterpart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    /// Capability identifier (e.g., `"model"`, `"temperature"`, `"stream"`).
    pub name: String,
    /// Schema descriptor for allowed values — plugin-defined JSON. Typical
    /// shape: `{ type: "enum", enum_values: [...], default_value: ... }` or
    /// `{ type: "float", min: 0.0, max: 2.0, default_value: 0.7 }`.
    pub value: serde_json::Value,
}

/// A concrete capability value chosen by the client for a specific call.
///
/// Passed in `PluginCallContext.enabled_capabilities` — Chat Engine forwards
/// these to the plugin so it knows which options were selected. Compare with
/// `Capability` which is the schema side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityValue {
    /// Must match a capability `name` previously declared by the plugin.
    pub name: String,
    /// The chosen value (e.g., `"gpt-4"`, `0.9`, `false`). Must validate
    /// against the schema in the corresponding `Capability.value`.
    pub value: serde_json::Value,
}

/// Summary of one variant at a given tree position — returned when listing
/// variants for a message so clients can render navigation UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantInfo {
    /// Variant's message ID (the sibling itself).
    pub message_id: Uuid,
    /// Ordinal of this variant among siblings.
    pub variant_index: u32,
    /// How many variants exist at this position (including this one).
    pub total_variants: u32,
    /// True iff this variant is currently on the active path.
    pub is_active: bool,
}

/// Per-session memory strategy controlling how much conversation history is
/// sent to the backend plugin on each call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryStrategy {
    /// Send the entire active path.
    Full,
    /// Send only the most recent `window_size` messages.
    SlidingWindow {
        /// Number of recent messages to keep; must be ≥ 1.
        window_size: u32,
    },
    /// Send AI-generated summary + the last `recent_messages_to_keep` messages.
    Summarized {
        /// Number of most-recent messages to preserve unsummarized; must be ≥ 2.
        recent_messages_to_keep: u32,
    },
}

/// Message retention policy — when messages in a session should be cleaned up.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RetentionPolicy {
    /// Keep messages forever (default).
    None,
    /// Delete messages older than `max_age_days`.
    AgeBased {
        /// Maximum age in days before cleanup; must be ≥ 1.
        max_age_days: u32,
    },
    /// Keep at most `max_message_count` messages; oldest are cleaned up first.
    CountBased {
        /// Maximum number of retained messages; must be ≥ 1.
        max_message_count: u32,
    },
}

/// Plugin health status returned by `ChatEngineBackendPlugin::health_check`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// Plugin is fully operational.
    Healthy,
    /// Plugin is reachable but reporting partial degradation.
    Degraded,
    /// Plugin is unreachable or reporting failure.
    Unhealthy,
}

/// NDJSON streaming event emitted by a plugin during response generation.
///
/// Serialized with a `"type"` discriminator: `"start"`, `"chunk"`, `"complete"`,
/// or `"error"`. A well-formed response stream is: one `Start` → zero or more
/// `Chunk` → one `Complete` (or `Error` at any point).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamingEvent {
    /// Marks the beginning of an assistant message's stream.
    Start(StreamingStartEvent),
    /// A partial content chunk; multiple `Chunk`s concatenate to the full text.
    Chunk(StreamingChunkEvent),
    /// Stream completed successfully; may carry final metadata.
    Complete(StreamingCompleteEvent),
    /// Stream terminated with an error; no more events follow.
    Error(StreamingErrorEvent),
}

/// Opens a stream for a given assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingStartEvent {
    /// ID of the assistant message being streamed.
    pub message_id: Uuid,
}

/// A single text fragment appended to the assistant message in flight.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingChunkEvent {
    /// ID of the assistant message this chunk belongs to.
    pub message_id: Uuid,
    /// Text payload to append to the message content.
    pub chunk: String,
}

/// Signals the assistant message is fully persisted and the stream is closing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingCompleteEvent {
    /// ID of the completed assistant message.
    pub message_id: Uuid,
    /// Final plugin-defined metadata (model used, finish_reason, token usage,
    /// etc.). Omitted from the wire when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Signals a mid-stream failure; the assistant message may be incomplete.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingErrorEvent {
    /// ID of the assistant message that failed to stream.
    pub message_id: Uuid,
    /// Human-readable error description (may include plugin error code).
    pub error: String,
}

#[cfg(test)]
mod streaming_event_wire_format_tests {
    //! Pins the on-wire JSON shape of `StreamingEvent` and its payload structs
    //! to the snake_case contract documented in `api/README.md`, `api/http-protocol.json`,
    //! and ADR-0006 §Streaming Event Types. If you find yourself updating these
    //! tests to change `message_id` → `messageId` (or similar), update the
    //! OpenAPI spec and announce a breaking wire-protocol change first.

    use super::{
        StreamingChunkEvent, StreamingCompleteEvent, StreamingErrorEvent, StreamingEvent,
        StreamingStartEvent,
    };
    use uuid::Uuid;

    fn fixed_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    }

    #[test]
    fn start_event_serializes_with_snake_case() {
        let json = serde_json::to_value(StreamingEvent::Start(StreamingStartEvent {
            message_id: fixed_id(),
        }))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "start",
                "message_id": "00000000-0000-0000-0000-000000000001",
            })
        );
    }

    #[test]
    fn chunk_event_serializes_with_snake_case() {
        let json = serde_json::to_value(StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: fixed_id(),
            chunk: "hello".into(),
        }))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "chunk",
                "message_id": "00000000-0000-0000-0000-000000000001",
                "chunk": "hello",
            })
        );
    }

    #[test]
    fn complete_event_serializes_with_snake_case() {
        let json = serde_json::to_value(StreamingEvent::Complete(StreamingCompleteEvent {
            message_id: fixed_id(),
            metadata: Some(serde_json::json!({ "usage": { "input_units": 1 } })),
        }))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "complete",
                "message_id": "00000000-0000-0000-0000-000000000001",
                "metadata": { "usage": { "input_units": 1 } },
            })
        );
    }

    #[test]
    fn complete_event_omits_metadata_when_none() {
        let json = serde_json::to_value(StreamingEvent::Complete(StreamingCompleteEvent {
            message_id: fixed_id(),
            metadata: None,
        }))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "complete",
                "message_id": "00000000-0000-0000-0000-000000000001",
            })
        );
    }

    #[test]
    fn error_event_serializes_with_snake_case() {
        let json = serde_json::to_value(StreamingEvent::Error(StreamingErrorEvent {
            message_id: fixed_id(),
            error: "upstream timeout".into(),
        }))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "error",
                "message_id": "00000000-0000-0000-0000-000000000001",
                "error": "upstream timeout",
            })
        );
    }
}

#[cfg(test)]
mod id_validation_tests {
    use super::{TenantId, UserId};

    #[test]
    fn tenant_id_accepts_non_empty() {
        assert_eq!(TenantId::new("t").as_str(), "t");
        assert_eq!(TenantId::from(String::from("t")).as_str(), "t");
        assert_eq!(TenantId::from("t").as_str(), "t");
    }

    #[test]
    #[should_panic(expected = "TenantId must not be empty")]
    fn tenant_id_new_rejects_empty() {
        let _ = TenantId::new("");
    }

    #[test]
    #[should_panic(expected = "TenantId must not be empty")]
    fn tenant_id_from_string_rejects_empty() {
        let _ = TenantId::from(String::new());
    }

    #[test]
    #[should_panic(expected = "TenantId must not be empty")]
    fn tenant_id_from_str_rejects_empty() {
        let _ = TenantId::from("");
    }

    #[test]
    fn user_id_accepts_non_empty() {
        assert_eq!(UserId::new("u").as_str(), "u");
        assert_eq!(UserId::from(String::from("u")).as_str(), "u");
        assert_eq!(UserId::from("u").as_str(), "u");
    }

    #[test]
    #[should_panic(expected = "UserId must not be empty")]
    fn user_id_new_rejects_empty() {
        let _ = UserId::new("");
    }

    #[test]
    #[should_panic(expected = "UserId must not be empty")]
    fn user_id_from_string_rejects_empty() {
        let _ = UserId::from(String::new());
    }

    #[test]
    #[should_panic(expected = "UserId must not be empty")]
    fn user_id_from_str_rejects_empty() {
        let _ = UserId::from("");
    }
}
