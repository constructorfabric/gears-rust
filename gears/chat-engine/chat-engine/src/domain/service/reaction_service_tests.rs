use super::*;
use crate::domain::ports::NewSession;
use crate::domain::ports::NewSessionType;
use crate::domain::session::Session;
use crate::domain::session::SessionType;
use async_trait::async_trait;
use chat_engine_sdk::models::LifecycleState;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use time::OffsetDateTime;
use toolkit::ClientHub;
use uuid::Uuid;

use crate::domain::message::{Message, MessagePart, MessageRole};
use crate::domain::ports::PluginConfigRepo;
use crate::domain::ports::SessionRepo;
use crate::domain::ports::SessionTypeRepo;
use crate::domain::ports::{FinalizeOutcome, InsertedPair, MessageRepo, NewUserMessage};
use crate::domain::ports::{ReactionDeleteOutcome, ReactionRepo, ReactionUpsertOutcome};
use crate::domain::service::test_support;

// ----------------------------- Stubs ----------------------------------

struct StubSessionRepo {
    session: Mutex<Session>,
}

impl StubSessionRepo {
    fn new(session: Session) -> Arc<Self> {
        Arc::new(Self {
            session: Mutex::new(session),
        })
    }
}

#[async_trait]
impl SessionRepo for StubSessionRepo {
    async fn insert(&self, _m: NewSession) -> std::result::Result<Session, ChatEngineError> {
        Ok(self.session.lock().clone())
    }

    async fn find_by_id(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: Uuid,
    ) -> std::result::Result<Option<Session>, ChatEngineError> {
        let s = self.session.lock().clone();
        if s.tenant_id.as_str() == tenant_id
            && s.user_id.as_str() == user_id
            && s.session_id == session_id
        {
            Ok(Some(s))
        } else {
            Ok(None)
        }
    }

    async fn list_paginated(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _query: &toolkit_odata::ODataQuery,
    ) -> std::result::Result<toolkit_odata::Page<Session>, ChatEngineError> {
        Ok(toolkit_odata::Page::empty(0))
    }

    async fn update_metadata(
        &self,
        _t: &str,
        _u: &str,
        _i: Uuid,
        _m: Option<JsonValue>,
    ) -> std::result::Result<Session, ChatEngineError> {
        Ok(self.session.lock().clone())
    }

    async fn update_capabilities(
        &self,
        _t: &str,
        _u: &str,
        _i: Uuid,
        _c: Option<JsonValue>,
    ) -> std::result::Result<Session, ChatEngineError> {
        Ok(self.session.lock().clone())
    }

    async fn update_lifecycle_state(
        &self,
        _t: &str,
        _u: &str,
        _i: Uuid,
        _s: LifecycleState,
    ) -> std::result::Result<Session, ChatEngineError> {
        Ok(self.session.lock().clone())
    }

    async fn soft_delete(
        &self,
        _t: &str,
        _u: &str,
        _i: Uuid,
        _d: i64,
    ) -> std::result::Result<Session, ChatEngineError> {
        Ok(self.session.lock().clone())
    }

    async fn hard_delete(
        &self,
        _t: &str,
        _u: &str,
        _i: Uuid,
    ) -> std::result::Result<bool, ChatEngineError> {
        Ok(true)
    }

    // Scoped (Phase 4) prefetch used by authorize_session under a permissive
    // enforcer: ignore the scope, return the fixed session if the id matches.
    async fn find_by_id_scoped(
        &self,
        _scope: &AccessScope,
        session_id: Uuid,
    ) -> std::result::Result<Option<Session>, ChatEngineError> {
        let s = self.session.lock().clone();
        Ok((s.session_id == session_id).then_some(s))
    }
}

struct StubSessionTypeRepo;

#[async_trait]
impl SessionTypeRepo for StubSessionTypeRepo {
    async fn insert(
        &self,
        _m: NewSessionType,
    ) -> std::result::Result<SessionType, ChatEngineError> {
        unreachable!()
    }

    async fn find_by_id(
        &self,
        _id: Uuid,
    ) -> std::result::Result<Option<SessionType>, ChatEngineError> {
        Ok(None)
    }

    async fn list(&self) -> std::result::Result<Vec<SessionType>, ChatEngineError> {
        Ok(vec![])
    }
}

struct StubMessageRepo {
    message: Mutex<Option<Message>>,
}

impl StubMessageRepo {
    fn assistant(session_id: Uuid, message_id: Uuid) -> Arc<Self> {
        let now = OffsetDateTime::now_utc();
        let msg = Message {
            message_id,
            session_id,
            tenant_id: None,
            user_id: None,
            parent_message_id: None,
            variant_index: 0,
            is_active: true,
            role: MessageRole::Assistant,
            parts: vec![MessagePart::text(Uuid::nil(), Uuid::nil(), 0, "hi")],
            file_ids: vec![],
            metadata: None,
            is_complete: true,
            is_hidden_from_user: false,
            is_hidden_from_backend: false,
            created_at: now,
            updated_at: now,
        };
        Arc::new(Self {
            message: Mutex::new(Some(msg)),
        })
    }

    fn user(session_id: Uuid, message_id: Uuid) -> Arc<Self> {
        let now = OffsetDateTime::now_utc();
        let msg = Message {
            message_id,
            session_id,
            tenant_id: None,
            user_id: None,
            parent_message_id: None,
            variant_index: 0,
            is_active: true,
            role: MessageRole::User,
            parts: vec![MessagePart::text(Uuid::nil(), Uuid::nil(), 0, "hi")],
            file_ids: vec![],
            metadata: None,
            is_complete: true,
            is_hidden_from_user: false,
            is_hidden_from_backend: false,
            created_at: now,
            updated_at: now,
        };
        Arc::new(Self {
            message: Mutex::new(Some(msg)),
        })
    }
}

#[async_trait]
impl MessageRepo for StubMessageRepo {
    async fn insert_user_and_assistant_stub(
        &self,
        _req: NewUserMessage,
    ) -> std::result::Result<InsertedPair, ChatEngineError> {
        unreachable!()
    }

    async fn finalize_assistant(
        &self,
        _session_id: Uuid,
        _id: Uuid,
        _outcome: FinalizeOutcome,
    ) -> std::result::Result<(), ChatEngineError> {
        unreachable!()
    }

    async fn fetch_active_history(
        &self,
        _s: Uuid,
        _d: Option<u32>,
    ) -> std::result::Result<Vec<Message>, ChatEngineError> {
        Ok(vec![])
    }

    async fn find_message_in_session(
        &self,
        session_id: Uuid,
        message_id: Uuid,
    ) -> std::result::Result<Option<Message>, ChatEngineError> {
        let m = self.message.lock().clone();
        Ok(m.filter(|msg| msg.session_id == session_id && msg.message_id == message_id))
    }
}

#[derive(Default)]
struct StubReactionRepo {
    upsert_calls: AtomicUsize,
    delete_calls: AtomicUsize,
    list_returns: Mutex<Vec<MessageReaction>>,
}

#[async_trait]
impl ReactionRepo for StubReactionRepo {
    async fn get_by_pk(
        &self,
        _message_id: Uuid,
        _user_id: &str,
    ) -> std::result::Result<Option<MessageReaction>, ChatEngineError> {
        Ok(None)
    }

    async fn upsert(
        &self,
        message_id: Uuid,
        user_id: &str,
        reaction_type: ReactionType,
    ) -> std::result::Result<ReactionUpsertOutcome, ChatEngineError> {
        self.upsert_calls.fetch_add(1, Ordering::SeqCst);
        let now = OffsetDateTime::now_utc();
        Ok(ReactionUpsertOutcome {
            reaction: MessageReaction {
                message_id,
                user_id: user_id.to_owned(),
                reaction_type,
                created_at: now,
                updated_at: now,
            },
            previous_reaction_type: None,
        })
    }

    async fn delete(
        &self,
        _message_id: Uuid,
        _user_id: &str,
    ) -> std::result::Result<ReactionDeleteOutcome, ChatEngineError> {
        self.delete_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ReactionDeleteOutcome {
            applied: true,
            previous_reaction_type: Some(ReactionType::Like),
        })
    }

    async fn list_by_message(
        &self,
        _message_id: Uuid,
    ) -> std::result::Result<Vec<MessageReaction>, ChatEngineError> {
        Ok(self.list_returns.lock().clone())
    }

    // Scoped (Phase 4) read used by list_reactions under a permissive
    // enforcer: ignore the scope, return the seeded reactions.
    async fn list_by_message_scoped(
        &self,
        _scope: &AccessScope,
        _message_id: Uuid,
    ) -> std::result::Result<Vec<MessageReaction>, ChatEngineError> {
        Ok(self.list_returns.lock().clone())
    }
}

struct StubPluginConfigRepo;

#[async_trait]
impl PluginConfigRepo for StubPluginConfigRepo {
    async fn find(
        &self,
        _p: &str,
        _s: Uuid,
    ) -> std::result::Result<Option<JsonValue>, ChatEngineError> {
        Ok(None)
    }

    async fn upsert(
        &self,
        _p: &str,
        _s: Uuid,
        _c: JsonValue,
    ) -> std::result::Result<(), ChatEngineError> {
        Ok(())
    }

    async fn delete(&self, _p: &str, _s: Uuid) -> std::result::Result<(), ChatEngineError> {
        Ok(())
    }
}

fn make_session(
    tenant_id: &str,
    user_id: &str,
    session_id: Uuid,
    enabled_capabilities: Option<JsonValue>,
) -> Session {
    let now = OffsetDateTime::now_utc();
    Session {
        session_id,
        tenant_id: tenant_id.into(),
        user_id: user_id.into(),
        client_id: None,
        session_type_id: None,
        enabled_capabilities,
        metadata: None,
        lifecycle_state: LifecycleState::Active,
        share_token: None,
        created_at: now,
        updated_at: now,
    }
}

fn plugin_service() -> PluginService {
    PluginService::new(Arc::new(ClientHub::new()), Arc::new(StubPluginConfigRepo))
}

fn make_service(
    sessions: Arc<dyn SessionRepo>,
    messages: Arc<dyn MessageRepo>,
    reactions: Arc<dyn ReactionRepo>,
) -> ReactionService {
    make_service_with_enforcer(sessions, messages, reactions, test_support::enforcer_allow())
}

fn make_service_with_enforcer(
    sessions: Arc<dyn SessionRepo>,
    messages: Arc<dyn MessageRepo>,
    reactions: Arc<dyn ReactionRepo>,
    enforcer: PolicyEnforcer,
) -> ReactionService {
    ReactionService::new(
        sessions,
        Arc::new(StubSessionTypeRepo),
        messages,
        reactions,
        plugin_service(),
        enforcer,
    )
}

fn make_ctx() -> SecurityContext {
    test_support::ctx_allow_tenants(&[Uuid::new_v4()])
}

// --------------------------- Unit tests -------------------------------

#[tokio::test]
async fn set_reaction_returns_409_when_feedback_capability_missing() {
    let session_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    let session = make_session(
        "t",
        "u",
        session_id,
        Some(serde_json::json!([{ "name": "model", "value": "gpt-4" }])),
    );
    let svc = make_service(
        StubSessionRepo::new(session),
        StubMessageRepo::assistant(session_id, message_id),
        Arc::new(StubReactionRepo::default()),
    );

    let err = svc
        .set_reaction(&make_ctx(),session_id, message_id, ReactionType::Like)
        .await
        .expect_err("capability gate must reject");
    match err {
        ChatEngineError::Conflict { reason } => {
            assert!(reason.contains("feedback"), "reason mentions capability");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn set_reaction_upserts_when_capability_enabled() {
    let session_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    let session = make_session(
        "t",
        "u",
        session_id,
        Some(serde_json::json!([{ "name": "feedback", "value": true }])),
    );
    let reactions = Arc::new(StubReactionRepo::default());
    let svc = make_service(
        StubSessionRepo::new(session),
        StubMessageRepo::assistant(session_id, message_id),
        reactions.clone(),
    );

    let (resp, mutation) = svc
        .set_reaction(&make_ctx(),session_id, message_id, ReactionType::Like)
        .await
        .expect("ok");
    assert_eq!(resp.message_id, message_id);
    assert_eq!(resp.reaction_type, ReactionType::Like);
    assert!(resp.applied);
    assert_eq!(reactions.upsert_calls.load(Ordering::SeqCst), 1);
    assert_eq!(mutation.reaction_type, ReactionType::Like);
}

#[tokio::test]
async fn set_reaction_deletes_on_none_with_applied_true() {
    let session_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    let session = make_session(
        "t",
        "u",
        session_id,
        Some(serde_json::json!([{ "name": "feedback", "value": true }])),
    );
    let reactions = Arc::new(StubReactionRepo::default());
    let svc = make_service(
        StubSessionRepo::new(session),
        StubMessageRepo::assistant(session_id, message_id),
        reactions.clone(),
    );

    let (resp, mutation) = svc
        .set_reaction(&make_ctx(),session_id, message_id, ReactionType::None)
        .await
        .expect("ok");
    assert_eq!(resp.reaction_type, ReactionType::None);
    assert!(resp.applied);
    assert_eq!(reactions.delete_calls.load(Ordering::SeqCst), 1);
    assert_eq!(mutation.previous_reaction_type, Some(ReactionType::Like));
}

#[tokio::test]
async fn set_reaction_returns_404_on_unknown_session() {
    let session_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    let session = make_session(
        "t",
        "u",
        session_id,
        Some(serde_json::json!([{ "name": "feedback", "value": true }])),
    );
    let svc = make_service(
        StubSessionRepo::new(session),
        StubMessageRepo::assistant(session_id, message_id),
        Arc::new(StubReactionRepo::default()),
    );

    // authorize_session prefetches via `find_by_id_scoped`; an id the repo
    // does not hold resolves to None and collapses to a 404.
    let err = svc
        .set_reaction(&make_ctx(), Uuid::new_v4(), message_id, ReactionType::Like)
        .await
        .expect_err("missing session must be 404");
    assert!(matches!(
        err,
        ChatEngineError::NotFound {
            resource: "session",
            ..
        }
    ));
}

#[tokio::test]
async fn set_reaction_returns_400_on_non_assistant_target() {
    let session_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    let session = make_session(
        "t",
        "u",
        session_id,
        Some(serde_json::json!([{ "name": "feedback", "value": true }])),
    );
    let svc = make_service(
        StubSessionRepo::new(session),
        StubMessageRepo::user(session_id, message_id),
        Arc::new(StubReactionRepo::default()),
    );

    let err = svc
        .set_reaction(&make_ctx(),session_id, message_id, ReactionType::Like)
        .await
        .expect_err("user-message target must be rejected");
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[tokio::test]
async fn list_reactions_bypasses_capability_gate() {
    let session_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    // No feedback capability — the read path must still succeed.
    let session = make_session(
        "t",
        "u",
        session_id,
        Some(serde_json::json!([{ "name": "model", "value": "gpt-4" }])),
    );
    let svc = make_service(
        StubSessionRepo::new(session),
        StubMessageRepo::assistant(session_id, message_id),
        Arc::new(StubReactionRepo::default()),
    );

    let listing = svc
        .list_reactions(&make_ctx(),session_id, message_id)
        .await
        .expect("ok");
    assert_eq!(listing.message_id, message_id);
    assert!(listing.reactions.is_empty());
}

#[tokio::test]
async fn list_reactions_unknown_message_returns_empty() {
    let session_id = Uuid::new_v4();
    let session = make_session(
        "t",
        "u",
        session_id,
        Some(serde_json::json!([{ "name": "feedback", "value": true }])),
    );
    let svc = make_service(
        StubSessionRepo::new(session),
        StubMessageRepo::assistant(session_id, Uuid::new_v4()),
        Arc::new(StubReactionRepo::default()),
    );

    // The scoped read path no longer probes message existence: it lists the
    // caller's owned reactions and returns an empty set (anti-enumeration)
    // rather than leaking a 404 for an unknown/foreign message id.
    let unknown_message = Uuid::new_v4();
    let listing = svc
        .list_reactions(&make_ctx(), session_id, unknown_message)
        .await
        .expect("unknown message lists empty, not 404");
    assert_eq!(listing.message_id, unknown_message);
    assert!(listing.reactions.is_empty());
}

#[test]
fn ensure_feedback_capability_passes_when_present() {
    let now = OffsetDateTime::now_utc();
    let session = Session {
        session_id: Uuid::nil(),
        tenant_id: "t".to_string().into(),
        user_id: "u".to_string().into(),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: Some(serde_json::json!([
            { "name": "model", "value": "gpt-4" },
            { "name": "feedback", "value": true },
        ])),
        metadata: None,
        lifecycle_state: LifecycleState::Active,
        share_token: None,
        created_at: now,
        updated_at: now,
    };
    ensure_feedback_capability(&session).expect("passes");
}

#[test]
fn ensure_feedback_capability_rejects_when_array_missing() {
    let now = OffsetDateTime::now_utc();
    let session = Session {
        session_id: Uuid::nil(),
        tenant_id: "t".to_string().into(),
        user_id: "u".to_string().into(),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: None,
        lifecycle_state: LifecycleState::Active,
        share_token: None,
        created_at: now,
        updated_at: now,
    };
    let err = ensure_feedback_capability(&session).unwrap_err();
    assert!(matches!(err, ChatEngineError::Conflict { .. }));
}

// --------------------------- Authz (PEP) tests ------------------------
//
// The reaction mutation path gates on the parent SESSION (UPDATE); the read
// path gates on REACTION (LIST). All enforcer failure modes — an explicit
// PDP deny, an evaluation error, or a policy compile error — fail closed to
// `Forbidden` (HTTP 403) via `From<EnforcerError>`.
// @cpt-cf-chat-engine-interface-pep

fn authz_fixture(
    enforcer: PolicyEnforcer,
) -> (ReactionService, Uuid, Uuid) {
    let session_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    let session = make_session(
        "t",
        "u",
        session_id,
        Some(serde_json::json!([{ "name": "feedback", "value": true }])),
    );
    let svc = make_service_with_enforcer(
        StubSessionRepo::new(session),
        StubMessageRepo::assistant(session_id, message_id),
        Arc::new(StubReactionRepo::default()),
        enforcer,
    );
    (svc, session_id, message_id)
}

#[tokio::test]
async fn set_reaction_pdp_denied_returns_forbidden() {
    let (svc, session_id, message_id) = authz_fixture(test_support::enforcer_deny());
    let res = svc
        .set_reaction(&make_ctx(), session_id, message_id, ReactionType::Like)
        .await;
    assert!(matches!(res, Err(ChatEngineError::Forbidden { .. })));
}

#[tokio::test]
async fn delete_reaction_pdp_denied_returns_forbidden() {
    // ReactionType::None routes through the delete branch — still gated on
    // the parent SESSION UPDATE, so a PDP deny fails closed to 403.
    let (svc, session_id, message_id) = authz_fixture(test_support::enforcer_deny());
    let res = svc
        .set_reaction(&make_ctx(), session_id, message_id, ReactionType::None)
        .await;
    assert!(matches!(res, Err(ChatEngineError::Forbidden { .. })));
}

#[tokio::test]
async fn list_reactions_pdp_denied_returns_forbidden() {
    let (svc, session_id, message_id) = authz_fixture(test_support::enforcer_deny());
    let res = svc.list_reactions(&make_ctx(), session_id, message_id).await;
    assert!(matches!(res, Err(ChatEngineError::Forbidden { .. })));
}

#[tokio::test]
async fn list_reactions_evaluation_failed_returns_forbidden() {
    let (svc, session_id, message_id) = authz_fixture(test_support::enforcer_failing());
    let res = svc.list_reactions(&make_ctx(), session_id, message_id).await;
    assert!(matches!(res, Err(ChatEngineError::Forbidden { .. })));
}

#[tokio::test]
async fn list_reactions_compile_failed_returns_forbidden() {
    let (svc, session_id, message_id) = authz_fixture(test_support::enforcer_compile_fail());
    let res = svc.list_reactions(&make_ctx(), session_id, message_id).await;
    assert!(matches!(res, Err(ChatEngineError::Forbidden { .. })));
}

#[tokio::test]
async fn list_reactions_real_db_scoped_read_returns_empty() {
    // Exercise the real secure-ORM read path (`list_by_message_scoped` over
    // in-memory SQLite): a tenant with no reactions gets an empty, non-error
    // listing — the PDP owner constraints compile into a live query that
    // executes end to end.
    let db = test_support::inmem_db().await;
    let svc = test_support::build_reaction_service(&db, test_support::enforcer_allow());
    let ctx = test_support::ctx_allow_tenants(&[Uuid::new_v4()]);

    let message_id = Uuid::new_v4();
    let listing = svc
        .list_reactions(&ctx, Uuid::new_v4(), message_id)
        .await
        .expect("empty scoped read succeeds");
    assert_eq!(listing.message_id, message_id);
    assert!(listing.reactions.is_empty());
}
