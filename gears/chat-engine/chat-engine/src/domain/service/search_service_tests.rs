use super::*;
use crate::infra::db::entity::session as session_entity;
use crate::infra::db::repo::session_repo::SessionRepo;
use async_trait::async_trait;
use serde_json::Value as JsonValue;
use time::{Duration, OffsetDateTime};

// ---------- Mock SessionRepo ----------

#[derive(Default)]
struct MockSessionRepo {
    sessions: Vec<session_entity::Model>,
}

impl MockSessionRepo {
    fn with(model: session_entity::Model) -> Self {
        Self {
            sessions: vec![model],
        }
    }
}

#[async_trait]
impl SessionRepo for MockSessionRepo {
    async fn insert(
        &self,
        _model: session_entity::ActiveModel,
    ) -> std::result::Result<session_entity::Model, ChatEngineError> {
        unimplemented!()
    }
    async fn find_by_id(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
    ) -> std::result::Result<Option<session_entity::Model>, ChatEngineError> {
        Ok(self
            .sessions
            .iter()
            .find(|s| {
                s.session_id == session_id && s.tenant_id == tenant_id && s.user_id == user_id
            })
            .cloned())
    }
    async fn list_paginated(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _query: &toolkit_odata::ODataQuery,
    ) -> std::result::Result<toolkit_odata::Page<session_entity::Model>, ChatEngineError> {
        unimplemented!()
    }
    async fn update_metadata(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
        _metadata: Option<JsonValue>,
    ) -> std::result::Result<session_entity::Model, ChatEngineError> {
        unimplemented!()
    }
    async fn update_capabilities(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
        _enabled_capabilities: Option<JsonValue>,
    ) -> std::result::Result<session_entity::Model, ChatEngineError> {
        unimplemented!()
    }
    async fn update_lifecycle_state(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
        _state: crate::domain::session::LifecycleState,
    ) -> std::result::Result<session_entity::Model, ChatEngineError> {
        unimplemented!()
    }
    async fn soft_delete(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
        _retention_days: i64,
    ) -> std::result::Result<session_entity::Model, ChatEngineError> {
        unimplemented!()
    }
    async fn hard_delete(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
    ) -> std::result::Result<bool, ChatEngineError> {
        unimplemented!()
    }
}

// ---------- Mock MessageRepo ----------

#[derive(Default)]
struct MockMessageRepo {
    messages: Vec<Message>,
}

impl MockMessageRepo {
    fn with(messages: Vec<Message>) -> Self {
        Self { messages }
    }
}

#[async_trait]
impl MessageRepo for MockMessageRepo {
    async fn insert_user_and_assistant_stub(
        &self,
        _req: crate::infra::db::repo::message_repo::NewUserMessage,
    ) -> std::result::Result<crate::infra::db::repo::message_repo::InsertedPair, ChatEngineError>
    {
        unimplemented!()
    }
    async fn finalize_assistant(
        &self,
        _session_id: Uuid,
        _assistant_message_id: Uuid,
        _outcome: crate::infra::db::repo::message_repo::FinalizeOutcome,
    ) -> std::result::Result<(), ChatEngineError> {
        unimplemented!()
    }
    async fn fetch_active_history(
        &self,
        session_id: Uuid,
        _depth: Option<u32>,
    ) -> std::result::Result<Vec<Message>, ChatEngineError> {
        Ok(self
            .messages
            .iter()
            .filter(|m| m.session_id == session_id)
            .cloned()
            .collect())
    }
    async fn find_message_in_session(
        &self,
        session_id: Uuid,
        message_id: Uuid,
    ) -> std::result::Result<Option<Message>, ChatEngineError> {
        Ok(self
            .messages
            .iter()
            .find(|m| m.session_id == session_id && m.message_id == message_id)
            .cloned())
    }
    async fn list_active_path(
        &self,
        session_id: Uuid,
    ) -> std::result::Result<Vec<Message>, ChatEngineError> {
        let mut out: Vec<Message> = self
            .messages
            .iter()
            .filter(|m| m.session_id == session_id)
            .cloned()
            .collect();
        out.sort_by_key(|m| m.created_at);
        Ok(out)
    }
}

// ---------- Fixtures ----------

fn fixture_session(tenant: &str, user: &str, id: Uuid) -> session_entity::Model {
    session_entity::Model {
        session_id: id,
        tenant_id: tenant.to_string(),
        user_id: user.to_string(),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: Some(serde_json::json!({
            "title": "Test Session",
            "tags": ["alpha", "beta"]
        })),
        lifecycle_state: "active".to_string(),
        share_token: None,
        deleted_at: None,
        scheduled_hard_delete_at: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn fixture_message(
    session_id: Uuid,
    role: MessageRole,
    text: &str,
    offset_secs: i64,
    hidden: bool,
) -> Message {
    Message {
        message_id: Uuid::new_v4(),
        session_id,
        tenant_id: None,
        user_id: None,
        parent_message_id: None,
        variant_index: 0,
        is_active: true,
        role,
        parts: vec![MessagePart::text(Uuid::nil(), Uuid::nil(), 0, text)],
        file_ids: vec![],
        metadata: None,
        is_complete: true,
        is_hidden_from_user: hidden,
        is_hidden_from_backend: false,
        created_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(offset_secs),
        updated_at: OffsetDateTime::UNIX_EPOCH + Duration::seconds(offset_secs),
    }
}

fn identity() -> Identity {
    Identity::new("tenant-a", "user-1", None).unwrap()
}

fn make_service(session: session_entity::Model, messages: Vec<Message>) -> SearchService {
    let sessions = Arc::new(MockSessionRepo::with(session.clone()));
    let message_repo = Arc::new(MockMessageRepo::with(messages.clone()));

    let mut backend = InMemorySearchBackend::new();
    for m in messages {
        backend.push(
            SearchScopeFilter::new(
                session.tenant_id.clone(),
                session.user_id.clone(),
                Some(session.session_id),
            ),
            m,
        );
    }
    let backend = Arc::new(backend);
    SearchService::new(sessions, message_repo, backend)
}

// ---------- Tests ----------

#[test]
fn parse_query_empty_returns_query_required() {
    let err = parse_search_query("").unwrap_err();
    assert!(matches!(err, SearchError::QueryRequired));
    let err = parse_search_query("   ").unwrap_err();
    assert!(matches!(err, SearchError::QueryRequired));
}

#[test]
fn parse_query_over_length_returns_query_too_long() {
    let raw: String = "a".repeat(MAX_QUERY_LENGTH + 1);
    let err = parse_search_query(&raw).unwrap_err();
    assert!(matches!(err, SearchError::QueryTooLong));
}

#[test]
fn parse_query_only_operators_treated_as_empty() {
    let err = parse_search_query("&|!()").unwrap_err();
    assert!(matches!(err, SearchError::QueryRequired));
}

#[test]
fn parse_query_accepts_normal_input() {
    let parsed = parse_search_query("Hello World").unwrap();
    assert_eq!(parsed.raw, "Hello World");
    assert_eq!(parsed.tsquery, "Hello World");
}

#[tokio::test]
async fn empty_query_returns_400_via_chat_engine_error() {
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    let svc = make_service(session, vec![]);
    let result = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some(String::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(result, ChatEngineError::BadRequest { .. }));
}

#[tokio::test]
async fn over_length_query_returns_400() {
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    let svc = make_service(session, vec![]);
    let q = "a".repeat(MAX_QUERY_LENGTH + 1);
    let result = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some(q),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    match result {
        ChatEngineError::BadRequest { reason } => {
            assert!(reason.contains("too long"), "got: {reason}");
        }
        other => panic!("expected BadRequest, got {other:?}"),
    }
}

#[tokio::test]
async fn unowned_session_returns_404() {
    let session_id = Uuid::new_v4();
    // Session is owned by a different user.
    let session = fixture_session("tenant-a", "someone-else", session_id);
    let svc = make_service(session, vec![]);
    let result = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("hello".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(result, ChatEngineError::NotFound { .. }));
}

#[tokio::test]
async fn hidden_messages_excluded_from_results() {
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    let messages = vec![
        fixture_message(
            session_id,
            MessageRole::User,
            "find me hidden secret",
            0,
            true,
        ),
        fixture_message(session_id, MessageRole::User, "find me", 1, false),
    ];
    let svc = make_service(session, messages);
    let page = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("find me".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.total_count, 1);
}

#[tokio::test]
async fn tenant_scoping_blocks_cross_tenant_results() {
    // Backend stores a message for tenant-b — caller is tenant-a.
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    // Mock storage to inject a cross-tenant row.
    let foreign_session = Uuid::new_v4();
    let foreign_msg = fixture_message(
        foreign_session,
        MessageRole::User,
        "find me everywhere",
        0,
        false,
    );
    let sessions = Arc::new(MockSessionRepo::with(session.clone()));
    let mr = Arc::new(MockMessageRepo::with(vec![foreign_msg.clone()]));

    let mut backend = InMemorySearchBackend::new();
    backend.push(
        SearchScopeFilter::new("tenant-b", "user-9", Some(foreign_session)),
        foreign_msg,
    );
    let backend = Arc::new(backend);
    let svc = SearchService::new(sessions, mr, backend);

    let page = svc
        .search_across_sessions(
            &identity(),
            &SearchQuery {
                q: Some("find me".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 0);
    assert_eq!(page.total_count, 0);
}

#[tokio::test]
async fn pagination_caps_per_page_at_max() {
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    let mut messages = Vec::new();
    for i in 0..80 {
        messages.push(fixture_message(
            session_id,
            MessageRole::User,
            "needle haystack",
            i,
            false,
        ));
    }
    let svc = make_service(session, messages);
    let page = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("needle".into()),
                top: Some(1000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.per_page, crate::domain::search::MAX_PAGE_SIZE);
    assert_eq!(
        page.items.len(),
        crate::domain::search::MAX_PAGE_SIZE as usize
    );
    assert!(page.next_cursor.is_some());
}

#[tokio::test]
async fn context_window_populated() {
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    let messages = vec![
        fixture_message(session_id, MessageRole::User, "before-1", 0, false),
        fixture_message(session_id, MessageRole::Assistant, "needle here", 1, false),
        fixture_message(session_id, MessageRole::User, "after-1", 2, false),
    ];
    let svc = make_service(session, messages);
    let page = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("needle".into()),
                context_radius: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let ctx = &page.items[0].context_messages;
    assert_eq!(ctx.len(), 2);
    // First context message is the "before-1" (chronologically earliest).
    assert!(message_text(&ctx[0].parts).contains("before-1"));
    assert!(message_text(&ctx[1].parts).contains("after-1"));
}

#[tokio::test]
async fn cross_session_results_attach_session_metadata() {
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    let messages = vec![fixture_message(
        session_id,
        MessageRole::User,
        "needle haystack",
        0,
        false,
    )];
    let svc = make_service(session, messages);
    let page = svc
        .search_across_sessions(
            &identity(),
            &SearchQuery {
                q: Some("needle".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let meta = page.items[0]
        .session_metadata
        .as_ref()
        .expect("cross-session result must carry session metadata");
    assert_eq!(meta.title.as_deref(), Some("Test Session"));
    assert_eq!(meta.tags, vec!["alpha".to_string(), "beta".to_string()]);
}

#[tokio::test]
async fn malformed_cursor_returns_400() {
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    let svc = make_service(session, vec![]);
    let err = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("needle".into()),
                cursor: Some("not-a-cursor".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[tokio::test]
async fn cursor_pages_advance_strictly_past_prior_page() {
    // Pre-fix this test would have failed: the backend's cursor
    // application dropped only the single row whose id matched the
    // cursor, so page 2 returned the first page again (minus that
    // row). With the keyset fix, page 2 must be strictly older than
    // page 1's last row under the `(created_at DESC, message_id
    // DESC)` ordering.
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    // 5 distinct matches with monotonically increasing created_at.
    let messages: Vec<Message> = (0..5)
        .map(|i| {
            fixture_message(
                session_id,
                MessageRole::User,
                &format!("needle row {i}"),
                i64::from(i),
                false,
            )
        })
        .collect();
    let svc = make_service(session, messages);

    // Page size 2 → expect three pages: 2 + 2 + 1.
    let page1 = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("needle".into()),
                top: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page1.items.len(), 2, "page 1 size");
    let cursor1 = page1
        .next_cursor
        .clone()
        .expect("page 1 must surface a cursor when more rows exist");

    let page2 = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("needle".into()),
                top: Some(2),
                cursor: Some(cursor1.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page2.items.len(), 2, "page 2 size");

    // Strict disjointness: page 2 must NOT replay any page 1 ids.
    let p1_ids: std::collections::HashSet<Uuid> =
        page1.items.iter().map(|r| r.message_id).collect();
    for r in &page2.items {
        assert!(
            !p1_ids.contains(&r.message_id),
            "page 2 leaked page-1 row {} — cursor skip is broken",
            r.message_id,
        );
    }

    // Sort-order invariant: every page-2 row's created_at must be
    // strictly older than page 1's last row (or equal with a
    // smaller message_id) — the DESC keyset advance condition.
    let p1_last = page1.items.last().expect("page 1 non-empty");
    let p1_last_msg = svc
        .messages
        .find_message_in_session(session_id, p1_last.message_id)
        .await
        .unwrap()
        .expect("page 1 last message present in repo");
    for r in &page2.items {
        let r_msg = svc
            .messages
            .find_message_in_session(session_id, r.message_id)
            .await
            .unwrap()
            .expect("page 2 row present in repo");
        assert!(
            r_msg.created_at < p1_last_msg.created_at
                || (r_msg.created_at == p1_last_msg.created_at
                    && r_msg.message_id < p1_last_msg.message_id),
            "page 2 row {} is not strictly older than page 1's last row {} \
             under DESC ordering",
            r_msg.message_id,
            p1_last_msg.message_id,
        );
    }

    // Third (final) page: one row left, no further cursor.
    let cursor2 = page2
        .next_cursor
        .clone()
        .expect("page 2 must surface a cursor when more rows exist");
    let page3 = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("needle".into()),
                top: Some(2),
                cursor: Some(cursor2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page3.items.len(), 1, "page 3 carries the final row");
    assert!(page3.next_cursor.is_none(), "no cursor past the final row");

    // Total ids across all three pages: every input row, exactly once.
    let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for r in page1.items.iter().chain(&page2.items).chain(&page3.items) {
        assert!(
            seen.insert(r.message_id),
            "id {} appeared twice",
            r.message_id
        );
    }
    assert_eq!(seen.len(), 5, "all 5 rows surfaced across the pages");
}

#[tokio::test]
async fn legacy_cursor_without_created_at_still_advances() {
    // Cursors minted by the pre-fix encoder lack the `:t:<unix_ns>`
    // tail. The backend falls back to a position-based skip so
    // clients holding an in-flight legacy cursor at the cutover are
    // still able to advance instead of looping.
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    let messages: Vec<Message> = (0..4)
        .map(|i| {
            fixture_message(
                session_id,
                MessageRole::User,
                &format!("needle row {i}"),
                i64::from(i),
                false,
            )
        })
        .collect();
    // Snapshot ids in expected sort order (DESC).
    let mut snapshot = messages.clone();
    snapshot.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.message_id.cmp(&a.message_id))
    });
    let svc = make_service(session, messages);

    // Hand-craft a legacy cursor (no `:t:` tail) pointing at the
    // 2nd row in DESC order — page 2 should start from the 3rd.
    let cursor_target = snapshot[1].message_id;
    let legacy_cursor = format!("r:0:m:{cursor_target}");

    let page = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("needle".into()),
                top: Some(10),
                cursor: Some(legacy_cursor),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Expect the 3rd + 4th rows in DESC order.
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].message_id, snapshot[2].message_id);
    assert_eq!(page.items[1].message_id, snapshot[3].message_id);
}

#[tokio::test]
async fn session_scoped_results_omit_session_metadata() {
    let session_id = Uuid::new_v4();
    let session = fixture_session("tenant-a", "user-1", session_id);
    let messages = vec![fixture_message(
        session_id,
        MessageRole::User,
        "needle",
        0,
        false,
    )];
    let svc = make_service(session, messages);
    let page = svc
        .search_in_session(
            &identity(),
            session_id,
            &SearchQuery {
                q: Some("needle".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].session_metadata.is_none());
}
