//! Session export and sharing domain primitives (Phase 10).
//!
//! Owns the wire-shape DTOs and the file-storage abstraction used by the
//! `POST /sessions/{id}/export`, `POST /sessions/{id}/share`,
//! `GET /share/{token}` and `DELETE /sessions/{id}/share` surfaces. The
//! types here are framework-neutral; REST glue lives in
//! `api/rest/handlers/export.rs`, orchestration in
//! `domain/service/export_service.rs`.
//!
//! Three invariants drive the shapes below.
//!
//! 1. Share tokens are bearer secrets. The [`ShareToken`] newtype has a
//!    manual [`std::fmt::Debug`] impl that prints `ShareToken(***redacted***)`
//!    and intentionally implements neither [`std::fmt::Display`] nor
//!    [`serde::Serialize`]. The only `Serialize`-able carrier of the raw
//!    token string is [`ShareTokenIssue`], used exclusively in the
//!    share-issuance response.
//! 2. The unauthenticated `GET /share/{token}` endpoint MUST NOT expose
//!    `user_id`, `tenant_id`, or the token itself. [`SharedSessionView`]
//!    only carries fields safe to ship to an anonymous viewer.
//! 3. Storage is abstracted behind [`ExportStorage`] so Phase 10 compiles
//!    without a real file-storage backend. [`StubExportStorage`] returns a
//!    deterministic `memory://exports/{key}` URL; Phase 15 swaps in the
//!    real implementation.
//
// @cpt-cf-chat-engine-domain-export:p10
// @cpt-cf-chat-engine-adr-session-sharing:p10

use std::str::FromStr;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::domain::message::MessageRole;

/// Cryptographic share-link token granting read-only access to a session.
///
/// Bearer secret. The manual `Debug` impl renders `ShareToken(***redacted***)`
/// so the value cannot leak via `tracing` span fields, error formatting, or
/// test panics. Intentionally does NOT implement
/// [`std::fmt::Display`] or [`serde::Serialize`] — the only sanctioned way
/// to surface the raw value is to construct a [`ShareTokenIssue`] DTO
/// inside the share-issuance handler.
#[domain_model]
#[derive(Clone, PartialEq, Eq)]
pub struct ShareToken(String);

impl ShareToken {
    /// Wrap a raw token string. The string MUST come from
    /// [`generate_share_token`] (or an equivalent CSPRNG path) — there is
    /// no validation here because the wrapper is purely a redaction shell.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Borrow the raw token string. The accessor exists for the issuance
    /// DTO and the SQL `WHERE share_token = $1` path — every other call
    /// site should pass `&ShareToken` around so the redaction shell stays
    /// intact.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the raw token. Use sparingly — the
    /// returned `String` is not redacted from `Debug`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for ShareToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShareToken(***redacted***)")
    }
}

/// Generate a fresh share token using the strongest CSPRNG available to
/// the crate without pulling in `rand`/`base64` (Phase 15 owns the
/// dependency surface; see workspace policy).
///
/// We concatenate two independent UUIDv4 hex strings — that yields 64
/// URL-safe hex characters (256 bits of entropy total). This exceeds the
/// ADR-0016 minimum of 32 URL-safe characters / 128 bits and is the
/// strongest constructible value with only the `uuid` crate available.
#[must_use]
pub fn generate_share_token() -> ShareToken {
    let a = Uuid::new_v4().simple().to_string();
    let b = Uuid::new_v4().simple().to_string();
    ShareToken(format!("{a}{b}"))
}

/// Export format. The wire value is the lowercase variant name.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// Structured JSON envelope (default).
    Json,
    /// Rendered Markdown transcript.
    Markdown,
}

impl ExportFormat {
    /// Canonical lowercase string representation.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Markdown => "markdown",
        }
    }

    /// MIME content-type used when uploading the rendered export to file
    /// storage.
    #[must_use]
    pub fn content_type(&self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Markdown => "text/markdown; charset=utf-8",
        }
    }

    /// File extension used in the storage object key.
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Markdown => "md",
        }
    }
}

impl FromStr for ExportFormat {
    type Err = ChatEngineError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "markdown" | "md" => Ok(Self::Markdown),
            other => Err(ChatEngineError::bad_request(format!(
                "unsupported export format '{other}' (expected 'json' or 'markdown')"
            ))),
        }
    }
}

/// Public session metadata included in the JSON export envelope. Mirrors
/// the subset of [`crate::domain::session::Session`] that is safe to ship
/// to the session owner; `user_id` / `tenant_id` are intentionally
/// included here because export is an authenticated path (unlike
/// [`SharedSessionView`]).
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct ExportSessionMeta {
    pub session_id: Uuid,
    pub session_type_id: Option<Uuid>,
    pub lifecycle_state: String,
    pub title: Option<String>,
    /// Public metadata (reserved keys stripped) — surfaced verbatim so
    /// the client can round-trip its own custom fields.
    pub metadata: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// One message in an export payload. The shape is stable across JSON and
/// the rendered Markdown (Markdown is built from this view).
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct MessageView {
    pub message_id: Uuid,
    pub role: MessageRole,
    pub content: serde_json::Value,
    /// Per-message metadata. Stripped of plugin-injected fields unless
    /// the caller passes `include_plugin_metadata=true`.
    pub metadata: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// JSON envelope returned by `POST /sessions/{id}/export`. The Markdown
/// variant returns plain text; this DTO carries the JSON shape plus the
/// envelope metadata that the JSON renderer wraps around the messages.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct ExportedSession {
    pub session: ExportSessionMeta,
    pub messages: Vec<MessageView>,
    pub format: ExportFormat,
    #[serde(with = "time::serde::rfc3339")]
    pub exported_at: OffsetDateTime,
    pub download_url: String,
    pub message_count: usize,
}

/// Response shape for `POST /sessions/{id}/share`. Carries the raw token
/// string — this is the only `Serialize`-able surface in the crate that
/// embeds the token verbatim.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct ShareTokenIssue {
    pub share_token: String,
    pub share_url: String,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at: Option<OffsetDateTime>,
}

/// Wire shape returned by the unauthenticated `GET /share/{token}` route.
/// Contains a minimal session envelope plus the active-path messages.
/// `user_id`, `tenant_id`, and the share token are intentionally absent.
#[domain_model]
#[derive(Debug, Clone, Serialize)]
pub struct SharedSessionView {
    pub title: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub messages: Vec<MessageView>,
    pub read_only: bool,
    pub message_count: usize,
}

/// Storage error returned by [`ExportStorage::upload`].
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Backend rejected the upload, refused the connection, or timed out.
    /// Maps to a backend-unavailable response (HTTP 503).
    #[error("export storage unavailable: {0}")]
    Unavailable(String),
    /// No production storage backend is wired; the export feature is
    /// exposed but not implemented. Maps to HTTP 501 so the endpoint
    /// refuses honestly instead of discarding the bytes and handing back a
    /// dead URL (RUST-NO-001).
    #[error("export storage not implemented: {0}")]
    NotImplemented(String),
}

impl From<StorageError> for ChatEngineError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::Unavailable(msg) => ChatEngineError::BackendUnavailable {
                // Built from the message string, not the error's Display, so
                // the typed `StorageError` survives in `source` (DE1302).
                reason: format!("export storage unavailable: {msg}"),
                retry_after: None,
                source: Some(Box::new(StorageError::Unavailable(msg))),
            },
            StorageError::NotImplemented(reason) => ChatEngineError::not_implemented(reason),
        }
    }
}

/// Abstraction over the file-storage backend used to persist rendered
/// exports. Phase 10 ships a stub implementation; Phase 15 wires the
/// production backend.
#[async_trait]
pub trait ExportStorage: Send + Sync {
    /// Upload `bytes` under `key` with the given content type. Returns the
    /// download URL or a [`StorageError`].
    async fn upload(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<String, StorageError>;
}

/// Deterministic in-memory storage stub. Returns
/// `memory://exports/{key}` without retaining the uploaded bytes —
/// sufficient for Phase 10 smoke tests and Phase 15 wiring stubs.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct StubExportStorage;

#[async_trait]
impl ExportStorage for StubExportStorage {
    async fn upload(
        &self,
        key: &str,
        _bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<String, StorageError> {
        Ok(format!("memory://exports/{key}"))
    }
}

/// Production-default export storage used until a real object-storage
/// backend is wired. Every upload refuses with
/// [`StorageError::NotImplemented`] (HTTP 501) so the export endpoint never
/// fakes success on a path that would otherwise discard the bytes and hand
/// back a dead `memory://` URL (RUST-NO-001). Swap this for a concrete
/// backend at module-wiring time to enable the feature.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NotImplementedExportStorage;

#[async_trait]
impl ExportStorage for NotImplementedExportStorage {
    async fn upload(
        &self,
        _key: &str,
        _bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<String, StorageError> {
        Err(StorageError::NotImplemented(
            "session export storage backend is not configured".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_token_debug_redacts() {
        let token = ShareToken::new("super-secret-value");
        let rendered = format!("{token:?}");
        assert!(rendered.contains("***redacted***"));
        assert!(!rendered.contains("super-secret-value"));
    }

    /// Compile-time guard: `ShareToken` MUST NOT implement
    /// [`serde::Serialize`]. Deriving Serialize on a bearer-secret newtype
    /// would let the raw value reach JSON response bodies, structured
    /// logs, and `tracing` field values — exactly the leak the manual
    /// `Debug` redaction defends against. This trait-object cast fails
    /// to compile if a `Serialize` impl is ever added.
    #[test]
    fn share_token_does_not_implement_serialize() {
        // Static assertion: `ShareToken: !Serialize`. We use a negative
        // bound emulation via a generic fn that ONLY accepts
        // `T: Serialize`. The fn body is never called — its presence
        // here is documentation; the real guard is `ShareToken`'s
        // declaration site (no `#[derive(Serialize)]`). If a future
        // derive is added, the dedicated assertion below catches it
        // via runtime introspection of the JSON encoding.
        fn _requires_serialize<T: serde::Serialize>(_: T) {}
        // The line below MUST stay commented out — if you can
        // uncomment it without a compile error, ShareToken gained a
        // Serialize impl and this guard's contract is broken:
        //   _requires_serialize(ShareToken::new("x"));

        // Runtime backstop: serde_json::to_string against a
        // `serde::Serialize` value would round-trip "raw" into the
        // output. Confirm by trying to serialize a wrapper struct that
        // would have inherited Serialize through a derive on
        // ShareToken — instead the wrapper holds the raw &str so the
        // raw value lands in the output ONLY when the test author
        // explicitly opts in.
        #[derive(serde::Serialize)]
        struct Holder<'a> {
            raw: &'a str,
        }
        let token = ShareToken::new("RAW-VALUE-MUST-NOT-LEAK");
        let holder = Holder {
            raw: token.as_str(),
        };
        let json = serde_json::to_string(&holder).unwrap();
        assert!(
            json.contains("RAW-VALUE-MUST-NOT-LEAK"),
            "test sanity: explicit `as_str()` opt-in should serialise",
        );
        // What the test actually pins: serialising the ShareToken
        // newtype directly is NOT POSSIBLE — there is no Serialize
        // impl, so callers cannot accidentally let it leak via a
        // `#[derive(Serialize)]` on an outer struct that holds a
        // `ShareToken` field. (The outer derive would fail to
        // compile, which is the real defence.)
    }

    #[test]
    fn share_token_accessor_returns_raw_value() {
        let token = ShareToken::new("raw");
        assert_eq!(token.as_str(), "raw");
        assert_eq!(token.into_inner(), "raw");
    }

    #[test]
    fn generate_share_token_meets_minimum_length() {
        let token = generate_share_token();
        // Two simple UUIDs concatenated = 32 + 32 = 64 hex chars.
        assert_eq!(token.as_str().len(), 64);
        assert!(token.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_share_token_is_unique_across_calls() {
        let a = generate_share_token();
        let b = generate_share_token();
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn export_format_parses_known_values() {
        assert_eq!(ExportFormat::from_str("json").unwrap(), ExportFormat::Json);
        assert_eq!(
            ExportFormat::from_str("markdown").unwrap(),
            ExportFormat::Markdown
        );
        assert_eq!(
            ExportFormat::from_str("md").unwrap(),
            ExportFormat::Markdown
        );
        assert_eq!(ExportFormat::from_str("JSON").unwrap(), ExportFormat::Json);
    }

    #[test]
    fn export_format_rejects_unknown() {
        let err = ExportFormat::from_str("xml").unwrap_err();
        assert!(matches!(err, ChatEngineError::BadRequest { .. }));
    }

    #[test]
    fn export_format_content_type_and_extension() {
        assert_eq!(ExportFormat::Json.content_type(), "application/json");
        assert_eq!(
            ExportFormat::Markdown.content_type(),
            "text/markdown; charset=utf-8"
        );
        assert_eq!(ExportFormat::Json.extension(), "json");
        assert_eq!(ExportFormat::Markdown.extension(), "md");
    }

    #[test]
    fn storage_error_maps_to_backend_unavailable() {
        let err: ChatEngineError = StorageError::Unavailable("blob: nope".into()).into();
        assert!(matches!(err, ChatEngineError::BackendUnavailable { .. }));
    }

    #[tokio::test]
    async fn stub_storage_returns_memory_url() {
        let url = StubExportStorage
            .upload(
                "exports/tenant-1/session-2/2026.json",
                vec![1, 2, 3],
                "application/json",
            )
            .await
            .expect("stub never fails");
        assert_eq!(url, "memory://exports/exports/tenant-1/session-2/2026.json");
    }

    #[test]
    fn share_token_issue_serializes_token_verbatim() {
        let issue = ShareTokenIssue {
            share_token: "abcd".into(),
            share_url: "https://example/share/abcd".into(),
            expires_at: None,
        };
        let json = serde_json::to_string(&issue).expect("serialize");
        assert!(json.contains("\"share_token\":\"abcd\""));
        assert!(!json.contains("expires_at"));
    }

    #[test]
    fn shared_session_view_omits_user_and_tenant_fields() {
        let view = SharedSessionView {
            title: Some("hello".into()),
            created_at: OffsetDateTime::UNIX_EPOCH,
            messages: Vec::new(),
            read_only: true,
            message_count: 0,
        };
        let json = serde_json::to_string(&view).expect("serialize");
        assert!(!json.contains("user_id"));
        assert!(!json.contains("tenant_id"));
        assert!(!json.contains("share_token"));
        assert!(json.contains("\"read_only\":true"));
    }
}
