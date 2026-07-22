use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_utils::SecretString;
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
    #[serde(
        default,
        serialize_with = "toolkit_utils::secret_string::serialize_option_exposed"
    )]
    pub share_token: Option<SecretString>,
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
    /// Owning tenant, denormalized from the parent session so message-scoped
    /// queries and sharding don't require a join. When set, always equals the
    /// session's `tenant_id`. `None` only for legacy rows persisted before the
    /// column existed (not yet backfilled).
    #[serde(default)]
    pub tenant_id: Option<TenantId>,
    /// Author of this specific message (not the session owner). Set to the
    /// authenticated user for `user`-role messages; `None` for `assistant` /
    /// `system` messages (machine-generated, no human author) and un-backfilled
    /// legacy rows. Enables author attribution in multi-user / shared sessions.
    #[serde(default)]
    pub user_id: Option<UserId>,
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
    /// Ordered, typed body fragments. The parts in `number` order form the
    /// message body (replaces the former single `content` blob). Empty only
    /// for a freshly-created assistant stub before its text part is persisted.
    #[serde(default)]
    pub parts: Vec<MessagePart>,
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

/// Type discriminant for a [`MessagePart`]. The base set is fixed; plugin
/// vendors extend it via GTS without forking Chat Engine core.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessagePartType {
    /// Plain text: `{ text, title? }`.
    Text,
    /// Code block: `{ language, code }`.
    Code,
    /// One or more image references: `{ images: [{ image_id, mime_type?, .. }] }`.
    Images,
    /// One or more video references: `{ videos: [{ video_id, mime_type?, .. }] }`.
    Videos,
    /// Link preview cards: `{ links: [{ url, title?, .. }] }`.
    Links,
    /// Progress/status indicators: `{ statuses: [{ code, detail? }] }`.
    Statuses,
}

/// The wire / plugin shape of a message part before persistence: a `type`
/// plus its typed `content`. Chat Engine assigns `id` and `number` on persist
/// and returns a full [`MessagePart`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePartInput {
    /// Discriminates the `content` shape.
    #[serde(rename = "type")]
    pub part_type: MessagePartType,
    /// Typed payload; shape determined by `part_type`. Kept as JSON because the
    /// per-type shapes are plugin-extensible (validated structurally, not here).
    pub content: serde_json::Value,
    /// Document citations attached to this part (meaningful for `text` parts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_citations: Vec<FileCitation>,
    /// Web-page citations attached to this part (meaningful for `text` parts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_citations: Vec<LinkCitation>,
    /// Lightweight URL references attached to this part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<LinkReference>,
}

/// A persisted, ordered fragment of a message body.
///
/// A message owns one or more parts; the parts in `number` order are the
/// message body. Persisted in the `message_parts` table with a CASCADE foreign
/// key to `messages` (see DESIGN `cpt-cf-chat-engine-design-entity-message-part`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePart {
    /// Unique part identifier (primary key).
    pub id: Uuid,
    /// Message this part belongs to.
    pub message_id: Uuid,
    /// Discriminates the `content` shape.
    #[serde(rename = "type")]
    pub part_type: MessagePartType,
    /// Typed payload; shape determined by `part_type`.
    pub content: serde_json::Value,
    /// 0-based ordinal within the message; unique per message.
    pub number: u32,
    /// Document citations attached to this part (only `text` parts carry these).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_citations: Vec<FileCitation>,
    /// Web-page citations attached to this part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_citations: Vec<LinkCitation>,
    /// Lightweight URL references attached to this part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<LinkReference>,
}

impl MessagePart {
    /// Convenience constructor for a `text` part with the given body (no
    /// attached citations/references).
    #[must_use]
    pub fn text(id: Uuid, message_id: Uuid, number: u32, text: impl Into<String>) -> Self {
        Self {
            id,
            message_id,
            part_type: MessagePartType::Text,
            content: serde_json::json!({ "text": text.into() }),
            number,
            file_citations: Vec::new(),
            link_citations: Vec::new(),
            references: Vec::new(),
        }
    }
}

/// Per-marker source-location anchor, parallel to one entry in a citation's
/// `text_positions`. Forwarded verbatim from the plugin (see DESIGN
/// `cpt-cf-chat-engine-design-entity-citations`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextPositionAnchor {
    /// Zero-indexed start offset of the cited fragment in the source text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_start: Option<i64>,
    /// Exclusive end offset of the cited fragment in the source text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_end: Option<i64>,
    /// Verbatim cited text at the `[char_start..char_end]` slice.
    #[serde(default)]
    pub quote: String,
    /// Chunk identifier for this occurrence's source passage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    /// First ~200 chars of this occurrence's chunk, for hover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_preview: Option<String>,
}

/// A citation into a retrieved document, attached to a `text` message part.
///
/// Supplied by the backend plugin and stored verbatim — Chat Engine does not
/// generate or interpret citations. `index` matches a `[N]` marker in the part
/// text (1-indexed), sharing one namespace with [`LinkCitation`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileCitation {
    /// Plugin-assigned id, unique per message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_id: Option<String>,
    /// Matches the `[N]` token in the part text (1-indexed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// Source document id.
    pub document_id: String,
    /// Source document name.
    pub document_name: String,
    /// Human-readable document title, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,
    /// Document source / venue, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Quoted text from the document.
    #[serde(default)]
    pub quote: String,
    /// Zero-indexed start offset into the source plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_start: Option<i64>,
    /// Exclusive end offset into the source plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_end: Option<i64>,
    /// Source chunk identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    /// First ~200 chars of the chunk, for hover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_preview: Option<String>,
    /// Full chunk body (text) or image URL (when `chunk_type = "image"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_content: Option<String>,
    /// Content type of the cited chunk: `"text"` (default) or `"image"`.
    #[serde(default = "default_chunk_type")]
    pub chunk_type: String,
    /// Source page number (1-indexed), when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<i32>,
    /// Video timestamp in seconds, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
    /// Highlighted spans within the cited chunk (plugin-defined shape).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlights: Vec<serde_json::Value>,
    /// `direct_quote` / `paraphrase` / `data_reference` / `methodology_reference`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_type: Option<String>,
    /// Character offsets in the part text where this citation's `[index]` marker
    /// appears. Pre-computed by the plugin; the engine forwards them verbatim.
    #[serde(default)]
    pub text_positions: Vec<u32>,
    /// Per-marker source anchors, parallel to `text_positions`.
    #[serde(default)]
    pub text_position_anchors: Vec<TextPositionAnchor>,
    /// Opaque plugin metadata; forwarded but not interpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

/// A citation into a web page, attached to a `text` message part. Shares the
/// `[N]` index namespace with [`FileCitation`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkCitation {
    /// Plugin-assigned id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_id: Option<String>,
    /// Matches the `[N]` token in the part text (1-indexed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// Cited page URL.
    pub url: String,
    /// Cited page title.
    pub title: String,
    /// Snippet / preview from the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_text: Option<String>,
    /// Favicon URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_url: Option<String>,
    /// Cited text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    /// Zero-indexed start offset into the source plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_start: Option<i64>,
    /// Exclusive end offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_end: Option<i64>,
    /// Citation kind label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_type: Option<String>,
    /// Character offsets in the part text where this citation's `[index]` marker
    /// appears. Plugin-provided; forwarded verbatim.
    #[serde(default)]
    pub text_positions: Vec<u32>,
}

/// A lightweight URL badge attached to a `text` message part (no quote/anchor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkReference {
    /// Reference title.
    #[serde(default)]
    pub title: String,
    /// Reference URL.
    pub url: String,
    /// Preview text.
    #[serde(default)]
    pub preview_text: String,
    /// Character offsets in the part text where the badge appears.
    #[serde(default)]
    pub position: Vec<u32>,
    /// Highlight spans for the preview (plugin-defined shape).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview_highlights: Vec<serde_json::Value>,
    /// Reference type: `"url"` (default) / `"document"` / `"internal"`.
    #[serde(default = "default_ref_type")]
    pub ref_type: String,
    /// Additional metadata (e.g. `entity_id` for document references).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_meta: Option<serde_json::Value>,
    /// Per-part ordinal so positional `[N]` → `refs[N-1]` is stable.
    #[serde(default)]
    pub idx: u32,
}

fn default_chunk_type() -> String {
    String::from("text")
}

fn default_ref_type() -> String {
    String::from("url")
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
    /// A partial text content chunk; multiple `Chunk`s concatenate to the full
    /// text of the assistant's primary `text` part.
    Chunk(StreamingChunkEvent),
    /// Transient progress indicator (e.g. `thinking`, `analyzing`). Streamed to
    /// the client but **not** persisted with the message.
    Status(StreamingStatusEvent),
    /// Emits a complete typed message part (image/video/link/code/…) to append
    /// to the message document. Persisted with the message on finalize.
    Part(StreamingPartEvent),
    /// Mid-stream citations/references attached to a part (vs. the batch set on
    /// `Complete`). Persisted with that part on finalize.
    Citation(StreamingCitationEvent),
    /// Opaque assistant-message state patch; merged into the message metadata.
    State(StreamingStateEvent),
    /// Session-scoped metadata patch; merged into the owning session's metadata.
    SessionMeta(StreamingSessionMetaEvent),
    /// Tool-invocation trace (e.g. file search); recorded in message metadata.
    Tool(StreamingToolEvent),
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
    /// Document citations for the completed assistant `text` part. Persisted
    /// with the text part on finalize (see FR-023).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_citations: Vec<FileCitation>,
    /// Web-page citations for the completed assistant `text` part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_citations: Vec<LinkCitation>,
    /// URL references for the completed assistant `text` part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<LinkReference>,
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

/// Transient progress indicator (`StreamingEvent::Status`). Streamed to the
/// client for live feedback but not persisted with the finalized message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingStatusEvent {
    /// ID of the assistant message this status pertains to.
    pub message_id: Uuid,
    /// Machine-readable status code (e.g. `thinking`, `analyzing`, `searching`).
    pub code: String,
    /// Optional human-readable detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Emits a complete typed message part mid-stream (`StreamingEvent::Part`):
/// images, videos, links, code, or an additional text part. The part is
/// appended to the message document and persisted on finalize.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingPartEvent {
    /// ID of the assistant message this part belongs to.
    pub message_id: Uuid,
    /// The typed part to append (type + content + optional citations).
    pub part: MessagePartInput,
}

/// Mid-stream citations/references attached to a part
/// (`StreamingEvent::Citation`). Unlike the batch set carried on `Complete`,
/// these arrive incrementally; they target the part at `part_number`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingCitationEvent {
    /// ID of the assistant message these citations belong to.
    pub message_id: Uuid,
    /// Target part index (defaults to the primary text part, `0`).
    #[serde(default)]
    pub part_number: i32,
    /// Document citations to append to the target part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_citations: Vec<FileCitation>,
    /// Web-page citations to append to the target part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_citations: Vec<LinkCitation>,
    /// URL references to append to the target part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<LinkReference>,
}

/// Opaque assistant-message state patch (`StreamingEvent::State`); merged into
/// the finalized message's metadata under `state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingStateEvent {
    /// ID of the assistant message whose state changed.
    pub message_id: Uuid,
    /// Arbitrary state object the client maintains alongside the document.
    pub state: serde_json::Value,
}

/// Session-scoped metadata patch (`StreamingEvent::SessionMeta`); shallow-merged
/// into the owning session's `metadata` while the message streams.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingSessionMetaEvent {
    /// ID of the assistant message that triggered the session update.
    pub message_id: Uuid,
    /// Partial session metadata to shallow-merge into `session.metadata`.
    pub patch: serde_json::Value,
}

/// Tool-invocation trace (`StreamingEvent::Tool`), e.g. a file search the
/// plugin ran; appended to the finalized message's metadata under `tools`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamingToolEvent {
    /// ID of the assistant message this tool call belongs to.
    pub message_id: Uuid,
    /// Tool identifier (e.g. `file_search`).
    pub tool: String,
    /// Tool-specific payload (query, results, status).
    pub payload: serde_json::Value,
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
            file_citations: vec![],
            link_citations: vec![],
            references: vec![],
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
            file_citations: vec![],
            link_citations: vec![],
            references: vec![],
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
        drop(TenantId::new(""));
    }

    #[test]
    #[should_panic(expected = "TenantId must not be empty")]
    fn tenant_id_from_string_rejects_empty() {
        drop(TenantId::from(String::new()));
    }

    #[test]
    #[should_panic(expected = "TenantId must not be empty")]
    fn tenant_id_from_str_rejects_empty() {
        drop(TenantId::from(""));
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
        drop(UserId::new(""));
    }

    #[test]
    #[should_panic(expected = "UserId must not be empty")]
    fn user_id_from_string_rejects_empty() {
        drop(UserId::from(String::new()));
    }

    #[test]
    #[should_panic(expected = "UserId must not be empty")]
    fn user_id_from_str_rejects_empty() {
        drop(UserId::from(""));
    }
}
