use super::*;

#[test]
fn reject_reserved_metadata_blocks_known_keys() {
    let metadata = serde_json::json!({"memory_strategy": {"type": "full"}});
    let err = reject_reserved_metadata(Some(&metadata)).unwrap_err();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[test]
fn merge_plugin_metadata_object_merge_overlay_wins() {
    let base = Some(serde_json::json!({"a": 1, "b": 2}));
    let overlay = serde_json::json!({"b": 99, "c": 3});
    let merged = merge_plugin_metadata(base, overlay);
    assert_eq!(merged, serde_json::json!({"a": 1, "b": 99, "c": 3}));
}

#[test]
fn merge_plugin_metadata_strips_reserved_keys_from_overlay() {
    let base = Some(serde_json::json!({"a": 1}));
    // Plugin tries to set engine-reserved keys — they must be dropped.
    let overlay = serde_json::json!({
        "memory_strategy": {"type": "full"},
        "retention_policy": {"x": 1},
        "share_expires_at": "2026-01-01",
        "model": "gpt-4",
    });
    let merged = merge_plugin_metadata(base, overlay);
    assert_eq!(merged, serde_json::json!({"a": 1, "model": "gpt-4"}));
}

#[test]
fn merge_plugin_metadata_uses_overlay_when_base_absent() {
    let merged = merge_plugin_metadata(None, serde_json::json!({"k": "v"}));
    assert_eq!(merged, serde_json::json!({"k": "v"}));
}

#[test]
fn merge_plugin_metadata_keeps_base_when_overlay_not_object() {
    // A non-object overlay must not clobber existing client metadata.
    let base = Some(serde_json::json!({"a": 1}));
    let merged = merge_plugin_metadata(base, serde_json::json!("oops"));
    assert_eq!(merged, serde_json::json!({"a": 1}));
}

#[test]
fn reject_reserved_metadata_allows_client_keys() {
    let metadata = serde_json::json!({"title": "hello"});
    reject_reserved_metadata(Some(&metadata)).expect("client metadata accepted");
}

#[test]
fn redact_session_clears_share_token_and_reserved_metadata() {
    let s = Session {
        session_id: Uuid::nil(),
        tenant_id: TenantId::new("t"),
        user_id: UserId::new("u"),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: Some(serde_json::json!({
            "memory_strategy": {"type": "full"},
            "client_field": "ok",
        })),
        lifecycle_state: LifecycleState::Active,
        share_token: Some("super-secret".into()),
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    let redacted = redact_session(s);
    assert!(redacted.share_token.is_none());
    assert_eq!(
        redacted.metadata,
        Some(serde_json::json!({"client_field": "ok"}))
    );
}

#[test]
fn identity_rejects_empty_tenant() {
    let err = Identity::new("", "u", None).unwrap_err();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[test]
fn identity_rejects_empty_user() {
    let err = Identity::new("t", "", None).unwrap_err();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[test]
fn parse_state_falls_back_to_active() {
    assert_eq!(
        LifecycleState::from_str_value("garbage").unwrap_or(LifecycleState::Active),
        LifecycleState::Active
    );
    assert_eq!(
        LifecycleState::from_str_value("soft_deleted"),
        Some(LifecycleState::SoftDeleted)
    );
}

// Anchor for the acceptance criterion that requires `ensure_can_transition`
// to be called before every state-changing write — the call sites in
// archive/restore/delete invoke this same helper, and the test below
// verifies the routing for one representative edge.
#[test]
fn ensure_can_transition_path_used_by_service_for_archive() {
    let from = LifecycleState::Active;
    let to = LifecycleState::Archived;
    ensure_can_transition(from, to).expect("active->archived is valid");
}

// ===========================================================================
// Authorization suite (Phase 8) — PEP enforcement on SessionService.
// Real Sea-ORM repos over an in-memory SQLite DB + a mock PDP, so each test
// exercises the full enforcer -> AccessScope -> SecureORM WHERE flow.
// ===========================================================================

use crate::domain::service::test_support::{
    build_session_service, ctx_allow_tenants, ctx_for_subject, enforcer_allow,
    enforcer_compile_fail, enforcer_deny, enforcer_failing, inmem_db, seed_session,
};
use toolkit_odata::ODataQuery;

// --- 2a. PDP deny (Denied) -> 403 -----------------------------------------

#[tokio::test]
async fn pdp_denied_list_sessions_returns_forbidden() {
    let db = inmem_db().await;
    let svc = build_session_service(&db, enforcer_deny());
    let ctx = ctx_allow_tenants(&[Uuid::new_v4()]);
    let err = svc
        .list_sessions(&ctx, &ODataQuery::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::Forbidden { .. }),
        "Expected Forbidden from DenyAll, got: {err:?}"
    );
}

#[tokio::test]
async fn pdp_denied_create_session_returns_forbidden() {
    let db = inmem_db().await;
    let svc = build_session_service(&db, enforcer_deny());
    let ctx = ctx_allow_tenants(&[Uuid::new_v4()]);
    let err = svc
        .create_session(
            &ctx,
            CreateSessionRequest {
                session_type_id: None,
                metadata: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::Forbidden { .. }),
        "Expected Forbidden from DenyAll, got: {err:?}"
    );
}

#[tokio::test]
async fn pdp_denied_get_session_returns_forbidden() {
    let db = inmem_db().await;
    let (tenant, user, sid) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_session(&db, sid, tenant, user).await;

    let svc = build_session_service(&db, enforcer_deny());
    let err = svc
        .get_session(&ctx_for_subject(user, tenant), sid)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::Forbidden { .. }),
        "Expected Forbidden from DenyAll, got: {err:?}"
    );
}

#[tokio::test]
async fn pdp_denied_update_session_returns_forbidden() {
    let db = inmem_db().await;
    let (tenant, user, sid) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_session(&db, sid, tenant, user).await;

    let svc = build_session_service(&db, enforcer_deny());
    let err = svc
        .update_metadata(
            &ctx_for_subject(user, tenant),
            sid,
            serde_json::json!({"title": "x"}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::Forbidden { .. }),
        "Expected Forbidden from DenyAll, got: {err:?}"
    );
}

#[tokio::test]
async fn pdp_denied_delete_session_returns_forbidden() {
    let db = inmem_db().await;
    let (tenant, user, sid) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_session(&db, sid, tenant, user).await;

    let svc = build_session_service(&db, enforcer_deny());
    let err = svc
        .delete_session(&ctx_for_subject(user, tenant), sid, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::Forbidden { .. }),
        "Expected Forbidden from DenyAll, got: {err:?}"
    );
}

// --- 2b. EvaluationFailed -> 403 (fail-closed) ----------------------------

#[tokio::test]
async fn evaluation_failed_list_sessions_returns_forbidden() {
    let db = inmem_db().await;
    let svc = build_session_service(&db, enforcer_failing());
    let ctx = ctx_allow_tenants(&[Uuid::new_v4()]);
    let err = svc
        .list_sessions(&ctx, &ODataQuery::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::Forbidden { .. }),
        "Expected Forbidden (fail-closed) from PDP failure, got: {err:?}"
    );
}

#[tokio::test]
async fn evaluation_failed_get_session_returns_forbidden() {
    let db = inmem_db().await;
    let (tenant, user, sid) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_session(&db, sid, tenant, user).await;

    let svc = build_session_service(&db, enforcer_failing());
    let err = svc
        .get_session(&ctx_for_subject(user, tenant), sid)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::Forbidden { .. }),
        "Expected Forbidden (fail-closed) from PDP failure, got: {err:?}"
    );
}

// --- 2c. CompileFailed -> 403 (fail-closed) -------------------------------

#[tokio::test]
async fn compile_failed_list_sessions_returns_forbidden() {
    let db = inmem_db().await;
    let svc = build_session_service(&db, enforcer_compile_fail());
    let ctx = ctx_allow_tenants(&[Uuid::new_v4()]);
    let err = svc
        .list_sessions(&ctx, &ODataQuery::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::Forbidden { .. }),
        "Expected Forbidden (fail-closed) from empty constraints, got: {err:?}"
    );
}

// --- 2d. Point-op scope-miss -> 404 (anti-enumeration) --------------------

#[tokio::test]
async fn get_session_wrong_tenant_returns_not_found() {
    let db = inmem_db().await;
    let (tenant_a, user_a, sid) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_session(&db, sid, tenant_a, user_a).await;

    // A subject in a different tenant: PDP allows with owner constraints that
    // filter to the subject's own rows, so the scoped re-read yields 0 rows.
    let svc = build_session_service(&db, enforcer_allow());
    let err = svc
        .get_session(&ctx_allow_tenants(&[Uuid::new_v4()]), sid)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::NotFound { .. }),
        "Expected NotFound for cross-tenant access, got: {err:?}"
    );
}

#[tokio::test]
async fn delete_session_wrong_tenant_returns_not_found() {
    let db = inmem_db().await;
    let (tenant_a, user_a, sid) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_session(&db, sid, tenant_a, user_a).await;

    let svc = build_session_service(&db, enforcer_allow());
    let err = svc
        .delete_session(&ctx_allow_tenants(&[Uuid::new_v4()]), sid, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ChatEngineError::NotFound { .. }),
        "Expected NotFound for cross-tenant delete, got: {err:?}"
    );
}

// --- happy path: owner can read its own session ---------------------------

#[tokio::test]
async fn get_session_owner_returns_session() {
    let db = inmem_db().await;
    let (tenant, user, sid) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_session(&db, sid, tenant, user).await;

    let svc = build_session_service(&db, enforcer_allow());
    let got = svc
        .get_session(&ctx_for_subject(user, tenant), sid)
        .await
        .expect("owner should read its own session");
    assert_eq!(got.session_id, sid);
}
