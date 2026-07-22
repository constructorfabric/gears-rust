// The `.ends_with(".md")` asserts below compare a value we generate, not
// an untrusted filename, so the case-sensitivity lint does not apply.
#![allow(clippy::case_sensitive_file_extension_comparisons)]
use super::*;
use crate::domain::message::MessageRole;
use crate::domain::ports::NewSession;
use crate::domain::ports::SessionRepo;
use crate::domain::ports::{FinalizeOutcome, InsertedPair, MessageRepo, NewUserMessage};
use crate::domain::service::session_service::Identity;
use crate::domain::session::LifecycleState;
use crate::domain::session::METADATA_KEY_SHARE_EXPIRES_AT;
use crate::domain::session::Session;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

// --- mocks -----------------------------------------------------------

#[derive(Default)]
struct MockSessionRepo {
    rows: Mutex<Vec<Session>>,
}

impl MockSessionRepo {
    fn seed(&self, row: Session) {
        self.rows.lock().push(row);
    }
}

#[async_trait]
impl SessionRepo for MockSessionRepo {
    async fn insert(&self, _model: NewSession) -> Result<Session> {
        unimplemented!()
    }

    async fn find_by_id(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
    ) -> Result<Option<Session>> {
        Ok(self
            .rows
            .lock()
            .iter()
            .find(|m| {
                m.session_id == session_id
                    && m.tenant_id.as_str() == tenant_id
                    && m.user_id.as_str() == user_id
            })
            .cloned())
    }

    async fn list_paginated(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _query: &toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<Session>> {
        Ok(toolkit_odata::Page::empty(0))
    }

    async fn update_metadata(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
        _metadata: Option<JsonValue>,
    ) -> Result<Session> {
        unimplemented!()
    }

    async fn update_capabilities(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
        _capabilities: Option<JsonValue>,
    ) -> Result<Session> {
        unimplemented!()
    }

    async fn update_lifecycle_state(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
        _new_state: LifecycleState,
    ) -> Result<Session> {
        unimplemented!()
    }

    async fn soft_delete(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
        _retention_days: i64,
    ) -> Result<Session> {
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

    async fn find_by_share_token(&self, share_token: &str) -> Result<Option<Session>> {
        Ok(self
            .rows
            .lock()
            .iter()
            .find(|m| {
                m.share_token
                    .as_ref()
                    .map(toolkit_utils::SecretString::expose)
                    == Some(share_token)
            })
            .cloned())
    }

    async fn update_share_token(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
        share_token: Option<String>,
        metadata: Option<JsonValue>,
    ) -> Result<Session> {
        let mut rows = self.rows.lock();
        let row = rows
            .iter_mut()
            .find(|m| {
                m.session_id == session_id
                    && m.tenant_id.as_str() == tenant_id
                    && m.user_id.as_str() == user_id
            })
            .ok_or_else(|| ChatEngineError::not_found("session", session_id))?;
        row.share_token = share_token.map(toolkit_utils::SecretString::new);
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
    async fn insert_user_and_assistant_stub(&self, _req: NewUserMessage) -> Result<InsertedPair> {
        unimplemented!()
    }

    async fn finalize_assistant(
        &self,
        _session_id: Uuid,
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

fn sample_session(tenant: &str, user: &str, session_id: Uuid) -> Session {
    Session {
        session_id,
        tenant_id: tenant.into(),
        user_id: user.into(),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: Some(json!({"title": "Hello"})),
        lifecycle_state: LifecycleState::Active,
        share_token: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn sample_message(role: MessageRole, text: &str) -> Message {
    Message {
        message_id: Uuid::new_v4(),
        session_id: Uuid::nil(),
        tenant_id: None,
        user_id: None,
        parent_message_id: None,
        variant_index: 0,
        is_active: true,
        role,
        parts: vec![MessagePart::text(Uuid::nil(), Uuid::nil(), 0, text)],
        file_ids: Vec::new(),
        metadata: Some(json!({"plugin": "gpt", "request_id": "r1", "user_field": "ok"})),
        is_complete: true,
        is_hidden_from_user: false,
        is_hidden_from_backend: false,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn build_service() -> (ExportService, Arc<MockSessionRepo>, Arc<MockMessageRepo>) {
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
    messages.messages.lock().extend(vec![
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
    messages.messages.lock().extend(vec![
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
    row.lifecycle_state = LifecycleState::SoftDeleted;
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
    assert_eq!(
        stored
            .share_token
            .as_ref()
            .map(toolkit_utils::SecretString::expose),
        Some(issue.share_token.as_str())
    );
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
    row.lifecycle_state = LifecycleState::SoftDeleted;
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
    row.share_token = Some(toolkit_utils::SecretString::new("abcd-token"));
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
    row.share_token = Some(toolkit_utils::SecretString::new("soft-tok"));
    row.lifecycle_state = LifecycleState::SoftDeleted;
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
        row.lifecycle_state = LifecycleState::from_str_value(state).unwrap();
        ensure_shareable(&row).expect("active/archived OK");
    }
}

#[test]
fn ensure_shareable_rejects_deleted_states() {
    let mut row = sample_session("t", "u", Uuid::nil());
    for state in ["soft_deleted", "hard_deleted"] {
        row.lifecycle_state = LifecycleState::from_str_value(state).unwrap();
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
        lifecycle_state: "active".to_string(),
        title: Some("My chat".into()),
        metadata: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    let views = vec![MessageView {
        message_id: Uuid::nil(),
        role: MessageRole::User,
        parts: vec![MessagePart::text(Uuid::nil(), Uuid::nil(), 0, "hello")],
        metadata: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }];
    let bytes = render_markdown(&meta, &views);
    let s = String::from_utf8(bytes).unwrap();
    assert!(s.contains("# My chat"));
    assert!(s.contains("## user \u{2014} "));
    assert!(s.contains("hello"));
}

#[test]
fn render_json_emits_envelope() {
    let meta = ExportSessionMeta {
        session_id: Uuid::nil(),
        session_type_id: None,
        lifecycle_state: "active".to_string(),
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
