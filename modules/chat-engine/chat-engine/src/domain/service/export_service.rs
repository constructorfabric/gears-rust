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

use modkit_macros::domain_model;
use serde_json::{Map, Value as JsonValue};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tracing::{info, instrument};
use uuid::Uuid;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::export::{
    ExportFormat, ExportSessionMeta, ExportStorage, ExportedSession, MessageView, ShareTokenIssue,
    SharedSessionView, generate_share_token,
};
use crate::domain::message::{Message, MessageRole};
use crate::domain::service::session_service::Identity;
use crate::domain::session::{
    LifecycleState, Session, get_share_expires_at, public_metadata, set_share_expires_at,
};
use crate::infra::db::entity::session as session_entity;
use crate::infra::db::repo::message_repo::MessageRepo;
use crate::infra::db::repo::session_repo::SessionRepo;

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
        let mut session: Session = row.into();
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

        let mut session: Session = row.into();
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

        let session: Session = row.into();

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

    async fn load_owned(
        &self,
        identity: &Identity,
        session_id: Uuid,
    ) -> Result<session_entity::Model> {
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

fn ensure_shareable(model: &session_entity::Model) -> Result<()> {
    let state = LifecycleState::from_str_value(&model.lifecycle_state).unwrap_or(LifecycleState::Active);
    if matches!(state, LifecycleState::Active | LifecycleState::Archived) {
        Ok(())
    } else {
        Err(ChatEngineError::conflict(format!(
            "session lifecycle '{}' does not allow share/export (expected active or archived)",
            state.as_str()
        )))
    }
}

fn build_session_meta(model: &session_entity::Model) -> ExportSessionMeta {
    // Reconstruct a Session view to reuse public_metadata + title helpers.
    let session: Session = model.clone().into();
    ExportSessionMeta {
        session_id: session.session_id,
        session_type_id: session.session_type_id,
        lifecycle_state: session.lifecycle_state.as_str().to_owned(),
        title: metadata_title(session.metadata.as_ref()),
        metadata: public_metadata(&session),
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
                content: m.content.clone(),
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
    let _ = writeln!(out, "# {title}");
    let _ = writeln!(out);
    if let Ok(ts) = meta.created_at.format(&Rfc3339) {
        let _ = writeln!(out, "_Exported from session {} (created {})._", meta.session_id, ts);
        let _ = writeln!(out);
    }

    for msg in messages {
        let ts = msg
            .created_at
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::from("?"));
        let _ = writeln!(out, "## {} — {}", role_label(&msg.role), ts);
        let _ = writeln!(out);
        let _ = writeln!(out, "{}", content_to_markdown_body(&msg.content));
        let _ = writeln!(out);
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

fn content_to_markdown_body(content: &JsonValue) -> String {
    // The SDK convention is `{ "text": "..." }`. Fall back to the raw
    // JSON representation so plugin-defined content shapes (content
    // parts, tool calls, …) survive the export without crashing the
    // renderer.
    if let Some(text) = content.get("text").and_then(|v| v.as_str()) {
        text.to_owned()
    } else {
        serde_json::to_string_pretty(content).unwrap_or_else(|_| String::from("<unserializable>"))
    }
}

fn build_storage_key(
    tenant_id: &str,
    session_id: Uuid,
    now: &OffsetDateTime,
    format: ExportFormat,
) -> String {
    let ts = now
        .unix_timestamp_nanos()
        .to_string();
    format!(
        "exports/{tenant_id}/{session_id}/{ts}.{ext}",
        ext = format.extension()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::message::MessageRole;
    use crate::domain::service::session_service::Identity;
    use crate::domain::session::METADATA_KEY_SHARE_EXPIRES_AT;
    use crate::infra::db::repo::message_repo::{
        FinalizeOutcome, InsertedPair, MessageRepo, NewUserMessage,
    };
    use crate::infra::db::repo::session_repo::{SessionPage, SessionRepo};
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use serde_json::json;
    use std::sync::Arc;

    // --- mocks -----------------------------------------------------------

    #[derive(Default)]
    struct MockSessionRepo {
        rows: Mutex<Vec<session_entity::Model>>,
    }

    impl MockSessionRepo {
        fn seed(&self, row: session_entity::Model) {
            self.rows.lock().push(row);
        }
    }

    #[async_trait]
    impl SessionRepo for MockSessionRepo {
        async fn insert(
            &self,
            _model: session_entity::ActiveModel,
        ) -> Result<session_entity::Model> {
            unimplemented!()
        }

        async fn find_by_id(
            &self,
            tenant_id: &str,
            user_id: &str,
            session_id: Uuid,
        ) -> Result<Option<session_entity::Model>> {
            Ok(self
                .rows
                .lock()
                .iter()
                .find(|m| {
                    m.session_id == session_id
                        && m.tenant_id == tenant_id
                        && m.user_id == user_id
                })
                .cloned())
        }

        async fn list_paginated(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _cursor: Option<&str>,
            _limit: u32,
        ) -> Result<SessionPage> {
            Ok(SessionPage {
                items: Vec::new(),
                next_cursor: None,
            })
        }

        async fn update_metadata(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
            _metadata: Option<JsonValue>,
        ) -> Result<session_entity::Model> {
            unimplemented!()
        }

        async fn update_capabilities(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
            _capabilities: Option<JsonValue>,
        ) -> Result<session_entity::Model> {
            unimplemented!()
        }

        async fn update_lifecycle_state(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
            _new_state: LifecycleState,
        ) -> Result<session_entity::Model> {
            unimplemented!()
        }

        async fn soft_delete(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
            _retention_days: i64,
        ) -> Result<session_entity::Model> {
            unimplemented!()
        }

        async fn hard_delete(
            &self,
            _tenant_id: &str,
            _user_id: &str,
            _session_id: Uuid,
        ) -> Result<bool> {
            Ok(false)
        }

        async fn find_by_share_token(
            &self,
            share_token: &str,
        ) -> Result<Option<session_entity::Model>> {
            Ok(self
                .rows
                .lock()
                .iter()
                .find(|m| m.share_token.as_deref() == Some(share_token))
                .cloned())
        }

        async fn update_share_token(
            &self,
            tenant_id: &str,
            user_id: &str,
            session_id: Uuid,
            share_token: Option<String>,
            metadata: Option<JsonValue>,
        ) -> Result<session_entity::Model> {
            let mut rows = self.rows.lock();
            let row = rows
                .iter_mut()
                .find(|m| {
                    m.session_id == session_id
                        && m.tenant_id == tenant_id
                        && m.user_id == user_id
                })
                .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;
            row.share_token = share_token;
            row.metadata = metadata;
            row.updated_at = OffsetDateTime::now_utc();
            Ok(row.clone())
        }
    }

    #[derive(Default)]
    struct MockMessageRepo {
        messages: Mutex<Vec<Message>>,
    }

    #[async_trait]
    impl MessageRepo for MockMessageRepo {
        async fn insert_user_and_assistant_stub(
            &self,
            _req: NewUserMessage,
        ) -> Result<InsertedPair> {
            unimplemented!()
        }

        async fn finalize_assistant(
            &self,
            _assistant_message_id: Uuid,
            _outcome: FinalizeOutcome,
        ) -> Result<()> {
            unimplemented!()
        }

        async fn fetch_active_history(
            &self,
            _session_id: Uuid,
            _depth: Option<u32>,
        ) -> Result<Vec<Message>> {
            Ok(self.messages.lock().clone())
        }

        async fn find_message_in_session(
            &self,
            _session_id: Uuid,
            _message_id: Uuid,
        ) -> Result<Option<Message>> {
            Ok(None)
        }

        async fn list_active_path(&self, _session_id: Uuid) -> Result<Vec<Message>> {
            Ok(self.messages.lock().clone())
        }
    }

    fn sample_session(tenant: &str, user: &str, session_id: Uuid) -> session_entity::Model {
        session_entity::Model {
            session_id,
            tenant_id: tenant.into(),
            user_id: user.into(),
            client_id: None,
            session_type_id: None,
            enabled_capabilities: None,
            metadata: Some(json!({"title": "Hello"})),
            lifecycle_state: "active".into(),
            share_token: None,
            deleted_at: None,
            scheduled_hard_delete_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn sample_message(role: MessageRole, text: &str) -> Message {
        Message {
            message_id: Uuid::new_v4(),
            session_id: Uuid::nil(),
            parent_message_id: None,
            variant_index: 0,
            is_active: true,
            role,
            content: json!({"text": text}),
            file_ids: Vec::new(),
            metadata: Some(json!({"plugin": "gpt", "request_id": "r1", "user_field": "ok"})),
            is_complete: true,
            is_hidden_from_user: false,
            is_hidden_from_backend: false,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn build_service() -> (
        ExportService,
        Arc<MockSessionRepo>,
        Arc<MockMessageRepo>,
    ) {
        let sessions = Arc::new(MockSessionRepo::default());
        let messages = Arc::new(MockMessageRepo::default());
        let storage = Arc::new(crate::domain::export::StubExportStorage);
        let service = ExportService::new(
            sessions.clone() as Arc<dyn SessionRepo>,
            messages.clone() as Arc<dyn MessageRepo>,
            storage as Arc<dyn ExportStorage>,
        )
        .with_share_urls(ShareUrlBuilder {
            base_url: "https://example.test".into(),
        });
        (service, sessions, messages)
    }

    fn identity() -> Identity {
        Identity::new("tenant-a", "user-a", None).unwrap()
    }

    #[tokio::test]
    async fn export_json_returns_envelope_with_active_path() {
        let (svc, sessions, messages) = build_service();
        let session_id = Uuid::new_v4();
        sessions.seed(sample_session("tenant-a", "user-a", session_id));
        messages
            .messages
            .lock()
            .extend(vec![
                sample_message(MessageRole::User, "hi"),
                sample_message(MessageRole::Assistant, "hello"),
            ]);

        let exported = svc
            .export(&identity(), session_id, ExportFormat::Json, false)
            .await
            .expect("export ok");
        assert_eq!(exported.format, ExportFormat::Json);
        assert_eq!(exported.message_count, 2);
        assert!(exported.download_url.starts_with("memory://exports/"));
        assert_eq!(exported.session.title.as_deref(), Some("Hello"));
        // include_plugin_metadata=false strips the plugin field.
        let plugin = &exported.messages[0]
            .metadata
            .as_ref()
            .and_then(|v| v.get("plugin"))
            .cloned();
        assert!(plugin.is_none());
    }

    #[tokio::test]
    async fn export_markdown_renders_role_headers() {
        let (svc, sessions, messages) = build_service();
        let session_id = Uuid::new_v4();
        sessions.seed(sample_session("tenant-a", "user-a", session_id));
        messages
            .messages
            .lock()
            .extend(vec![
                sample_message(MessageRole::User, "hi"),
                sample_message(MessageRole::Assistant, "hello"),
            ]);

        let exported = svc
            .export(&identity(), session_id, ExportFormat::Markdown, false)
            .await
            .expect("export ok");
        assert_eq!(exported.format, ExportFormat::Markdown);
        assert!(exported.download_url.ends_with(".md"));
    }

    #[tokio::test]
    async fn export_empty_session_still_succeeds() {
        let (svc, sessions, _messages) = build_service();
        let session_id = Uuid::new_v4();
        sessions.seed(sample_session("tenant-a", "user-a", session_id));

        let exported = svc
            .export(&identity(), session_id, ExportFormat::Json, true)
            .await
            .expect("empty export ok");
        assert_eq!(exported.message_count, 0);
    }

    #[tokio::test]
    async fn export_rejects_soft_deleted_session() {
        let (svc, sessions, _messages) = build_service();
        let session_id = Uuid::new_v4();
        let mut row = sample_session("tenant-a", "user-a", session_id);
        row.lifecycle_state = "soft_deleted".into();
        sessions.seed(row);

        let err = svc
            .export(&identity(), session_id, ExportFormat::Json, false)
            .await
            .unwrap_err();
        assert!(matches!(err, ChatEngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn export_not_found_when_session_missing() {
        let (svc, _sessions, _messages) = build_service();
        let err = svc
            .export(&identity(), Uuid::new_v4(), ExportFormat::Json, false)
            .await
            .unwrap_err();
        assert!(matches!(err, ChatEngineError::NotFound { .. }));
    }

    #[tokio::test]
    async fn create_share_persists_token_and_returns_url() {
        let (svc, sessions, _messages) = build_service();
        let session_id = Uuid::new_v4();
        sessions.seed(sample_session("tenant-a", "user-a", session_id));

        let issue = svc
            .create_share(&identity(), session_id, Some(24))
            .await
            .expect("share created");
        assert!(!issue.share_token.is_empty());
        assert!(issue.share_url.contains(&issue.share_token));
        assert!(issue.share_url.starts_with("https://example.test/share/"));
        assert!(issue.expires_at.is_some());

        // Persistence side effect.
        let stored = sessions
            .find_by_id("tenant-a", "user-a", session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.share_token.as_deref(), Some(issue.share_token.as_str()));
        let metadata_expires = stored
            .metadata
            .as_ref()
            .and_then(|v| v.get(METADATA_KEY_SHARE_EXPIRES_AT))
            .and_then(|v| v.as_str());
        assert!(metadata_expires.is_some());
    }

    #[tokio::test]
    async fn create_share_rejects_soft_deleted_session() {
        let (svc, sessions, _messages) = build_service();
        let session_id = Uuid::new_v4();
        let mut row = sample_session("tenant-a", "user-a", session_id);
        row.lifecycle_state = "soft_deleted".into();
        sessions.seed(row);

        let err = svc
            .create_share(&identity(), session_id, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ChatEngineError::Conflict { .. }));
    }

    #[tokio::test]
    async fn access_shared_returns_view_without_user_or_tenant() {
        let (svc, sessions, messages) = build_service();
        let session_id = Uuid::new_v4();
        sessions.seed(sample_session("tenant-a", "user-a", session_id));
        messages
            .messages
            .lock()
            .push(sample_message(MessageRole::Assistant, "hi there"));

        let issue = svc
            .create_share(&identity(), session_id, None)
            .await
            .unwrap();

        let view = svc.access_shared(&issue.share_token).await.expect("ok");
        assert!(view.read_only);
        assert_eq!(view.message_count, 1);
        assert_eq!(view.title.as_deref(), Some("Hello"));
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("user_id"));
        assert!(!json.contains("tenant_id"));
        assert!(!json.contains("share_token"));
    }

    #[tokio::test]
    async fn access_shared_returns_404_for_unknown_token() {
        let (svc, _sessions, _messages) = build_service();
        let err = svc.access_shared("nope-not-real").await.unwrap_err();
        assert!(matches!(err, ChatEngineError::NotFound { .. }));
    }

    #[tokio::test]
    async fn access_shared_returns_expired_for_past_expiry() {
        let (svc, sessions, _messages) = build_service();
        let session_id = Uuid::new_v4();
        let mut row = sample_session("tenant-a", "user-a", session_id);
        row.share_token = Some("abcd-token".into());
        // Build expired metadata manually.
        let past = (OffsetDateTime::now_utc() - time::Duration::hours(1))
            .format(&Rfc3339)
            .unwrap();
        row.metadata = Some(json!({
            "title": "Hello",
            "share_expires_at": past,
        }));
        sessions.seed(row);

        let err = svc.access_shared("abcd-token").await.unwrap_err();
        assert!(is_share_token_expired(&err));
    }

    #[tokio::test]
    async fn access_shared_returns_expired_for_soft_deleted_session() {
        let (svc, sessions, _messages) = build_service();
        let session_id = Uuid::new_v4();
        let mut row = sample_session("tenant-a", "user-a", session_id);
        row.share_token = Some("soft-tok".into());
        row.lifecycle_state = "soft_deleted".into();
        sessions.seed(row);

        let err = svc.access_shared("soft-tok").await.unwrap_err();
        assert!(is_share_token_expired(&err));
    }

    #[tokio::test]
    async fn revoke_share_clears_token_and_expires() {
        let (svc, sessions, _messages) = build_service();
        let session_id = Uuid::new_v4();
        sessions.seed(sample_session("tenant-a", "user-a", session_id));

        let issue = svc
            .create_share(&identity(), session_id, Some(1))
            .await
            .unwrap();
        assert!(!issue.share_token.is_empty());

        svc.revoke_share(&identity(), session_id).await.unwrap();

        let stored = sessions
            .find_by_id("tenant-a", "user-a", session_id)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.share_token.is_none());
        let metadata_expires = stored
            .metadata
            .as_ref()
            .and_then(|v| v.get(METADATA_KEY_SHARE_EXPIRES_AT));
        assert!(metadata_expires.is_none());
    }

    #[tokio::test]
    async fn revoke_share_is_idempotent_when_already_cleared() {
        let (svc, sessions, _messages) = build_service();
        let session_id = Uuid::new_v4();
        sessions.seed(sample_session("tenant-a", "user-a", session_id));

        svc.revoke_share(&identity(), session_id)
            .await
            .expect("idempotent no-op");
    }

    #[test]
    fn ensure_shareable_allows_active_and_archived() {
        let mut row = sample_session("t", "u", Uuid::nil());
        for state in ["active", "archived"] {
            row.lifecycle_state = state.into();
            ensure_shareable(&row).expect("active/archived OK");
        }
    }

    #[test]
    fn ensure_shareable_rejects_deleted_states() {
        let mut row = sample_session("t", "u", Uuid::nil());
        for state in ["soft_deleted", "hard_deleted"] {
            row.lifecycle_state = state.into();
            assert!(ensure_shareable(&row).is_err());
        }
    }

    #[test]
    fn strip_plugin_fields_keeps_user_keys() {
        let stripped = strip_plugin_fields(Some(json!({
            "plugin": "gpt",
            "model": "x",
            "title": "y",
            "custom": 1,
        })))
        .unwrap();
        let map = stripped.as_object().unwrap();
        assert!(!map.contains_key("plugin"));
        assert!(!map.contains_key("model"));
        assert!(map.contains_key("title"));
        assert!(map.contains_key("custom"));
    }

    #[test]
    fn strip_plugin_fields_returns_none_when_empty() {
        let stripped = strip_plugin_fields(Some(json!({
            "plugin": "gpt",
            "request_id": "abc",
        })));
        assert!(stripped.is_none());
    }

    #[test]
    fn share_url_builder_strips_trailing_slash() {
        let b = ShareUrlBuilder {
            base_url: "https://x.test/".into(),
        };
        assert_eq!(b.build("abc"), "https://x.test/share/abc");
    }

    #[test]
    fn build_storage_key_format() {
        let key = build_storage_key(
            "tenant-1",
            Uuid::nil(),
            &OffsetDateTime::UNIX_EPOCH,
            ExportFormat::Markdown,
        );
        assert!(key.starts_with("exports/tenant-1/00000000-0000-0000-0000-000000000000/"));
        assert!(key.ends_with(".md"));
    }

    #[test]
    fn render_markdown_includes_role_headers() {
        let meta = ExportSessionMeta {
            session_id: Uuid::nil(),
            session_type_id: None,
            lifecycle_state: "active".into(),
            title: Some("My chat".into()),
            metadata: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };
        let views = vec![MessageView {
            message_id: Uuid::nil(),
            role: MessageRole::User,
            content: json!({"text": "hello"}),
            metadata: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }];
        let bytes = render_markdown(&meta, &views);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("# My chat"));
        assert!(s.contains("## user — "));
        assert!(s.contains("hello"));
    }

    #[test]
    fn render_json_emits_envelope() {
        let meta = ExportSessionMeta {
            session_id: Uuid::nil(),
            session_type_id: None,
            lifecycle_state: "active".into(),
            title: None,
            metadata: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };
        let bytes = render_json(&meta, &[]).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("\"session\""));
        assert!(s.contains("\"messages\""));
    }
}
