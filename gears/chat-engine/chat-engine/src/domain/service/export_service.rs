//! Session export and sharing service (Phase 10).
//!
//! Orchestrates the four endpoints declared by the Phase 10 spec:
//!
//! 1. `POST /sessions/{id}/export` — render the active message path as
//!    JSON or Markdown, upload through [`ExportStorage`], return the
//!    [`ExportedSession`] envelope with the storage `download_url`.
//! 2. `POST /sessions/{id}/share` — generate a CSPRNG share token, write
//!    it to `sessions.share_token`, persist the optional
//!    `share_expires_at` into `metadata` (per ADR-0017), and return a
//!    [`ShareTokenIssue`] containing the raw token.
//! 3. `GET /share/{token}` (unauthenticated) — resolve the token via
//!    [`SessionRepo::find_by_share_token`], reject hard-deleted /
//!    soft-deleted sessions (404), reject expired tokens (410), then
//!    project the session into [`SharedSessionView`] with NO `user_id`,
//!    `tenant_id`, or `share_token` exposure.
//! 4. `DELETE /sessions/{id}/share` — clear the token column and the
//!    `share_expires_at` metadata key, atomically. Idempotent.
//!
//! Lifecycle preconditions: `share` and `export` require
//! `lifecycle_state ∈ {active, archived}` per ADR-0016 §Decision and the
//! Phase 10 Rules section. Soft-/hard-deleted sessions return
//! `409 Conflict`. The shared-view endpoint additionally folds
//! soft-deleted and expired states into `410 Gone` so revoked and expired
//! tokens are indistinguishable to anonymous viewers.
//!
//! Active-path traversal: every export and shared-view read calls
//! [`MessageRepo::list_active_path`] (Phase 5) so the message-tree
//! traversal is centralised in the repo layer.
//
// @cpt-cf-chat-engine-export-service:p10
// @cpt-cf-chat-engine-adr-session-sharing:p10
// @cpt-cf-chat-engine-adr-session-metadata:p10

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value as JsonValue};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit_macros::domain_model;
use tracing::{info, instrument};
use uuid::Uuid;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::export::{
    ExportFormat, ExportSessionMeta, ExportStorage, ExportedSession, MessageView, ShareTokenIssue,
    SharedSessionView, generate_share_token,
};
use crate::domain::message::{Message, MessagePart, MessagePartType, MessageRole};
use crate::domain::ports::MessageRepo;
use crate::domain::ports::SessionRepo;
use crate::domain::service::session_service::Identity;
use crate::domain::session::{
    LifecycleState, Session, get_share_expires_at, public_metadata, set_share_expires_at,
};

/// Reserved metadata key holding the (optional) user-friendly title.
/// Title is opaque to Chat Engine — we only read it to surface in the
/// export / shared-view envelopes.
pub const METADATA_KEY_TITLE: &str = "title";

/// Configuration for [`ExportService::create_share`]'s URL construction.
/// Service consumers (Phase 15 module wiring) inject this when assembling
/// the service so the share URL matches the public-facing host.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ShareUrlBuilder {
    /// Public base URL (e.g. `https://chat.example.com`). No trailing
    /// slash required; one is added between the base and the path.
    pub base_url: String,
}

impl ShareUrlBuilder {
    /// Build a share URL for the given token. The token is appended
    /// verbatim — callers MUST have already generated a CSPRNG value.
    #[must_use]
    pub fn build(&self, token: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/share/{token}")
    }
}

impl Default for ShareUrlBuilder {
    fn default() -> Self {
        // Fallback used by tests and the Phase 15 stub config. Real wiring
        // overrides this with the deployed public URL.
        Self {
            base_url: "http://localhost".into(),
        }
    }
}

/// Orchestration of export rendering, token issuance, anonymous shared
/// reads, and token revocation.
///
/// Cheap to clone — every field is an `Arc`. The service is generic over
/// the repo / storage traits so unit tests can swap mocks without
/// touching a database.
#[domain_model]
#[derive(Clone)]
pub struct ExportService {
    sessions: Arc<dyn SessionRepo>,
    messages: Arc<dyn MessageRepo>,
    storage: Arc<dyn ExportStorage>,
    share_urls: ShareUrlBuilder,
}

impl ExportService {
    #[must_use]
    pub fn new(
        sessions: Arc<dyn SessionRepo>,
        messages: Arc<dyn MessageRepo>,
        storage: Arc<dyn ExportStorage>,
    ) -> Self {
        Self {
            sessions,
            messages,
            storage,
            share_urls: ShareUrlBuilder::default(),
        }
    }

    /// Override the share-URL builder used by [`Self::create_share`].
    /// Returns `self` for chained construction.
    #[must_use]
    pub fn with_share_urls(mut self, builder: ShareUrlBuilder) -> Self {
        self.share_urls = builder;
        self
    }

    // ---------------------------------------------------------------------
    // export
    // ---------------------------------------------------------------------

    /// Render and upload the export. Returns the [`ExportedSession`]
    /// envelope for JSON callers; Markdown callers consume `download_url`
    /// to fetch the rendered file (the envelope is still returned so
    /// every export path emits the same response shape).
    #[instrument(skip(self, identity), fields(session_id = %session_id, format = %format.as_str()))]
    pub async fn export(
        &self,
        identity: &Identity,
        session_id: Uuid,
        format: ExportFormat,
        include_plugin_metadata: bool,
    ) -> Result<ExportedSession> {
        let started = Instant::now();
        let session = self.load_owned(identity, session_id).await?;
        ensure_shareable(&session)?;

        let messages = self.messages.list_active_path(session_id).await?;
        let views = build_message_views(&messages, include_plugin_metadata);
        let session_meta = build_session_meta(&session);

        let bytes = match format {
            ExportFormat::Json => render_json(&session_meta, &views)?,
            ExportFormat::Markdown => render_markdown(&session_meta, &views),
        };
        let now = OffsetDateTime::now_utc();
        let key = build_storage_key(&identity.tenant_id, session_id, &now, format);
        let download_url = self
            .storage
            .upload(&key, bytes, format.content_type())
            .await
            .map_err(ChatEngineError::from)?;

        let message_count = views.len();
        let exported = ExportedSession {
            session: session_meta,
            messages: views,
            format,
            exported_at: now,
            download_url,
            message_count,
        };

        let duration_ms = started.elapsed().as_millis();
        info!(
            target: "chat_engine::export",
            session_id = %session_id,
            format = format.as_str(),
            message_count = message_count,
            duration_ms = duration_ms as u64,
            "export.completed"
        );

        Ok(exported)
    }

    // ---------------------------------------------------------------------
    // share lifecycle
    // ---------------------------------------------------------------------

    /// Generate a fresh share token, persist it on
    /// `sessions.share_token`, and write the optional `share_expires_at`
    /// into `session.metadata`. Re-issuing a share on a session that
    /// already has a token effectively revokes the old one (column
    /// approach, ADR-0016 §Consequences).
    #[instrument(skip(self, identity), fields(session_id = %session_id))]
    pub async fn create_share(
        &self,
        identity: &Identity,
        session_id: Uuid,
        expires_in_hours: Option<u32>,
    ) -> Result<ShareTokenIssue> {
        let row = self.load_owned(identity, session_id).await?;
        ensure_shareable(&row)?;

        let token = generate_share_token();
        let expires_at = expires_in_hours.map(|hours| {
            OffsetDateTime::now_utc() + time::Duration::seconds(i64::from(hours) * 3600)
        });

        // Materialize the metadata payload (with `share_expires_at` set
        // when an expiry was requested) ahead of the DB write. The
        // `share_token` column is supplied separately to the repo call.
        let mut session: Session = row;
        set_share_expires_at(&mut session, expires_at);

        let _persisted = self
            .sessions
            .update_share_token(
                &identity.tenant_id,
                &identity.user_id,
                session_id,
                Some(token.as_str().to_owned()),
                session.metadata.clone(),
            )
            .await?;

        info!(
            target: "chat_engine::export",
            session_id = %session_id,
            "share.created"
        );

        let share_url = self.share_urls.build(token.as_str());
        Ok(ShareTokenIssue {
            share_token: token.into_inner(),
            share_url,
            expires_at,
        })
    }

    /// Clear `share_token` and remove `share_expires_at` from metadata.
    /// Idempotent — revoking an already-cleared token returns `Ok(())`
    /// without touching the database (avoids a needless write).
    #[instrument(skip(self, identity), fields(session_id = %session_id))]
    pub async fn revoke_share(&self, identity: &Identity, session_id: Uuid) -> Result<()> {
        let row = self.load_owned(identity, session_id).await?;
        // Idempotent no-op when nothing to revoke.
        if row.share_token.is_none() {
            info!(
                target: "chat_engine::export",
                session_id = %session_id,
                "share.revoked (no-op)"
            );
            return Ok(());
        }

        let mut session: Session = row;
        set_share_expires_at(&mut session, None);

        self.sessions
            .update_share_token(
                &identity.tenant_id,
                &identity.user_id,
                session_id,
                None,
                session.metadata.clone(),
            )
            .await?;

        info!(
            target: "chat_engine::export",
            session_id = %session_id,
            "share.revoked"
        );
        Ok(())
    }

    /// Resolve a share token to a [`SharedSessionView`] without
    /// authentication. Hard-deleted sessions return 404. Expired tokens
    /// return 410 (mapped from `Conflict("expired")`). Missing tokens
    /// return 404 (mapped from `NotFound`). Per ADR-0016 expired and
    /// revoked tokens are indistinguishable to anonymous viewers — a
    /// revoked token has been removed from the column so the lookup
    /// surfaces 404, but soft-deleted sessions with an active token
    /// surface 410.
    #[instrument(skip(self, token), fields(token = "***redacted***"))]
    pub async fn access_shared(&self, token: &str) -> Result<SharedSessionView> {
        if token.is_empty() {
            return Err(ChatEngineError::not_found("share_token", "<empty>"));
        }
        let row = self
            .sessions
            .find_by_share_token(token)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("share_token", "***redacted***"))?;

        let session: Session = row;

        // Hard-deleted rows are excluded by find_by_share_token; treat
        // soft-deleted as expired so the response is indistinguishable
        // from a real expiration.
        if matches!(
            session.lifecycle_state,
            LifecycleState::SoftDeleted | LifecycleState::HardDeleted
        ) {
            return Err(token_expired());
        }

        if let Some(expires) = get_share_expires_at(&session)
            && expires < OffsetDateTime::now_utc()
        {
            return Err(token_expired());
        }

        let messages = self.messages.list_active_path(session.session_id).await?;
        let views = build_message_views(&messages, false);
        let title = metadata_title(session.metadata.as_ref());

        let session_id_for_log = session.session_id;
        let view = SharedSessionView {
            title,
            created_at: session.created_at,
            message_count: views.len(),
            messages: views,
            read_only: true,
        };

        info!(
            target: "chat_engine::export",
            session_id = %session_id_for_log,
            "share.accessed"
        );

        Ok(view)
    }

    // ---------------------------------------------------------------------
    // helpers
    // ---------------------------------------------------------------------

    async fn load_owned(&self, identity: &Identity, session_id: Uuid) -> Result<Session> {
        self.sessions
            .find_by_id(&identity.tenant_id, &identity.user_id, session_id)
            .await?
            .ok_or_else(|| ChatEngineError::not_found("session", session_id))
    }
}

// ---------------- free helpers ----------------

/// `Conflict { reason: "expired" }` carrier used by handlers to render
/// HTTP 410 Gone for an expired or soft-deleted shared session. The token
/// itself is intentionally NOT included in the reason string (per Phase
/// 10 Rules §Share Token Security).
fn token_expired() -> ChatEngineError {
    ChatEngineError::Conflict {
        reason: "share token expired".into(),
    }
}

/// Returns `true` when the chat engine error indicates an expired share
/// token. Used by the REST handler to map to HTTP 410.
#[must_use]
pub fn is_share_token_expired(err: &ChatEngineError) -> bool {
    matches!(err, ChatEngineError::Conflict { reason } if reason == "share token expired")
}

fn ensure_shareable(session: &Session) -> Result<()> {
    let state = session.lifecycle_state;
    if matches!(state, LifecycleState::Active | LifecycleState::Archived) {
        Ok(())
    } else {
        Err(ChatEngineError::conflict(format!(
            "session lifecycle '{}' does not allow share/export (expected active or archived)",
            state.as_str()
        )))
    }
}

fn build_session_meta(session: &Session) -> ExportSessionMeta {
    ExportSessionMeta {
        session_id: session.session_id,
        session_type_id: session.session_type_id,
        lifecycle_state: session.lifecycle_state.as_str().to_owned(),
        title: metadata_title(session.metadata.as_ref()),
        metadata: public_metadata(session),
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

fn metadata_title(metadata: Option<&JsonValue>) -> Option<String> {
    metadata
        .and_then(|v| v.as_object())
        .and_then(|map| map.get(METADATA_KEY_TITLE))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn build_message_views(messages: &[Message], include_plugin_metadata: bool) -> Vec<MessageView> {
    messages
        .iter()
        .filter(|m| !m.is_hidden_from_user)
        .map(|m| {
            let metadata = if include_plugin_metadata {
                m.metadata.clone()
            } else {
                strip_plugin_fields(m.metadata.clone())
            };
            MessageView {
                message_id: m.message_id,
                role: m.role.clone(),
                parts: m.parts.clone(),
                metadata,
                created_at: m.created_at,
            }
        })
        .collect()
}

/// Reserved-key filter for the OPTIONAL `include_plugin_metadata` flag.
/// When false we drop any per-message metadata that smells like
/// plugin/engine instrumentation so the exported transcript stays clean.
fn strip_plugin_fields(metadata: Option<JsonValue>) -> Option<JsonValue> {
    let Some(JsonValue::Object(map)) = metadata else {
        return metadata;
    };
    const PLUGIN_FIELDS: &[&str] = &[
        "plugin",
        "plugin_instance_id",
        "plugin_call_id",
        "request_id",
        "trace_id",
        "model",
        "finish_reason",
        "usage",
        "cancelled",
        "partial",
    ];
    let filtered: Map<String, JsonValue> = map
        .into_iter()
        .filter(|(k, _)| !PLUGIN_FIELDS.contains(&k.as_str()))
        .collect();
    if filtered.is_empty() {
        None
    } else {
        Some(JsonValue::Object(filtered))
    }
}

fn render_json(meta: &ExportSessionMeta, messages: &[MessageView]) -> Result<Vec<u8>> {
    let envelope = serde_json::json!({
        "session": meta,
        "messages": messages,
    });
    serde_json::to_vec_pretty(&envelope).map_err(|e| ChatEngineError::Internal {
        reason: format!("failed to serialize export: {e}"),
        source: Some(Box::new(e)),
    })
}

fn render_markdown(meta: &ExportSessionMeta, messages: &[MessageView]) -> Vec<u8> {
    let mut out = String::new();
    let title = meta.title.as_deref().unwrap_or("Session export");
    writeln!(out, "# {title}").ok();
    writeln!(out).ok();
    if let Ok(ts) = meta.created_at.format(&Rfc3339) {
        writeln!(
            out,
            "_Exported from session {} (created {})._",
            meta.session_id, ts
        )
        .ok();
        writeln!(out).ok();
    }

    for msg in messages {
        let ts = msg
            .created_at
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::from("?"));
        writeln!(out, "## {} — {}", role_label(&msg.role), ts).ok();
        writeln!(out).ok();
        writeln!(out, "{}", parts_to_markdown_body(&msg.parts)).ok();
        writeln!(out).ok();
    }

    out.into_bytes()
}

fn role_label(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
    }
}

fn parts_to_markdown_body(parts: &[MessagePart]) -> String {
    // Render each part in order: `text` parts as their body (SDK convention
    // `{ "text": "..." }`), other typed parts as pretty JSON so plugin-defined
    // shapes survive the export without crashing the renderer.
    let mut blocks: Vec<String> = Vec::with_capacity(parts.len());
    for p in parts {
        let block = if p.part_type == MessagePartType::Text {
            p.content
                .get("text")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .unwrap_or_default()
        } else {
            serde_json::to_string_pretty(&p.content)
                .unwrap_or_else(|_| String::from("<unserializable>"))
        };
        blocks.push(block);
    }
    blocks.join("\n\n")
}

fn build_storage_key(
    tenant_id: &str,
    session_id: Uuid,
    now: &OffsetDateTime,
    format: ExportFormat,
) -> String {
    let ts = now.unix_timestamp_nanos().to_string();
    format!(
        "exports/{tenant_id}/{session_id}/{ts}.{ext}",
        ext = format.extension()
    )
}

#[cfg(test)]
#[path = "export_service_tests.rs"]
mod export_service_tests;
