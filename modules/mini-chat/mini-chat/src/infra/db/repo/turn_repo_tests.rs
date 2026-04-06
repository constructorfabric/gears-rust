use super::*;
use crate::domain::repos::{CreateTurnParams, TurnRepository as TurnRepoTrait};
use crate::domain::service::test_helpers::{inmem_db, insert_chat, mock_db_provider};
use crate::infra::db::entity::chat_turn::TurnState;
use modkit_security::AccessScope;
use uuid::Uuid;

/// Helper: backdate `last_progress_at` via `SeaORM` `update_many`.
async fn backdate_progress(runner: &impl modkit_db::secure::DBRunner, turn_id: Uuid) {
    let past = OffsetDateTime::now_utc() - time::Duration::seconds(600);
    let scope = AccessScope::allow_all();
    TurnEntity::update_many()
        .col_expr(
            Column::LastProgressAt,
            sea_orm::sea_query::Expr::value(Some(past)),
        )
        .filter(Column::Id.eq(turn_id))
        .secure()
        .scope_with(&scope)
        .exec(runner)
        .await
        .expect("backdate last_progress_at");
}

/// Helper: insert a running turn with standard params.
async fn setup_running_turn(
    db: &std::sync::Arc<modkit_db::DBProvider<modkit_db::DbError>>,
) -> (Uuid, Uuid, Uuid, Uuid) {
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();

    insert_chat(db, tenant_id, chat_id).await;

    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let repo = TurnRepository;
    repo.create_turn(
        &conn,
        &scope,
        CreateTurnParams {
            id: turn_id,
            tenant_id,
            chat_id,
            request_id,
            requester_type: "user".to_owned(),
            requester_user_id: Some(Uuid::new_v4()),
            reserve_tokens: Some(100),
            max_output_tokens_applied: Some(4096),
            reserved_credits_micro: Some(1000),
            policy_version_applied: Some(1),
            effective_model: Some("gpt-5.2".to_owned()),
            minimal_generation_floor_applied: Some(10),
            web_search_enabled: false,
        },
    )
    .await
    .expect("create turn");

    (tenant_id, chat_id, turn_id, request_id)
}

// ── Phase 1: create_turn_sets_last_progress_at ──

#[tokio::test]
async fn create_turn_sets_last_progress_at() {
    let db = mock_db_provider(inmem_db().await);
    let (_, chat_id, _, request_id) = setup_running_turn(&db).await;

    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let repo = TurnRepository;
    let turn = repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .unwrap()
        .expect("turn should exist");

    assert!(
        turn.last_progress_at.is_some(),
        "last_progress_at should be set on creation"
    );
    let elapsed = (OffsetDateTime::now_utc() - turn.last_progress_at.unwrap()).whole_seconds();
    assert!(
        elapsed.abs() < 5,
        "last_progress_at should be approximately now (delta={elapsed}s)"
    );
}

// ── Phase 2: update_progress_at ──

#[tokio::test]
async fn update_progress_at_updates_running_turn() {
    let db = mock_db_provider(inmem_db().await);
    let (_, chat_id, turn_id, request_id) = setup_running_turn(&db).await;

    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let repo = TurnRepository;

    // Record initial last_progress_at
    let before = repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .unwrap()
        .unwrap();
    let initial = before.last_progress_at.unwrap();

    // Briefly sleep to ensure timestamp changes
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let rows = repo
        .update_progress_at(&conn, &scope, turn_id)
        .await
        .unwrap();
    assert_eq!(rows, 1, "should update one row");

    let after = repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .unwrap()
        .unwrap();
    // SQLite may not advance subsecond, so >= is the strongest
    // portable check. The rows_affected == 1 assertion above proves
    // the UPDATE executed.
    assert!(
        after.last_progress_at.unwrap() >= initial,
        "last_progress_at must not go backwards"
    );
}

#[tokio::test]
async fn update_progress_at_noop_on_terminal() {
    let db = mock_db_provider(inmem_db().await);
    let (_, _, turn_id, _) = setup_running_turn(&db).await;

    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let repo = TurnRepository;

    // Finalize the turn to Failed
    repo.cas_update_state(
        &conn,
        &scope,
        crate::domain::repos::CasTerminalParams {
            turn_id,
            state: TurnState::Failed,
            error_code: Some("test".to_owned()),
            error_detail: None,
            assistant_message_id: None,
            provider_response_id: None,
        },
    )
    .await
    .unwrap();

    let rows = repo
        .update_progress_at(&conn, &scope, turn_id)
        .await
        .unwrap();
    assert_eq!(rows, 0, "should not update a terminal turn");
}

// ── Phase 3: find_orphan_candidates ──

#[tokio::test]
async fn find_orphan_candidates_returns_stale_running() {
    let db = mock_db_provider(inmem_db().await);
    let (_, _, turn_id, _) = setup_running_turn(&db).await;

    let conn = db.conn().unwrap();
    backdate_progress(&conn, turn_id).await;

    let repo = TurnRepository;
    let candidates = repo.find_orphan_candidates(&conn, 60, 100).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, turn_id);
}

#[tokio::test]
async fn find_orphan_candidates_excludes_recent_progress() {
    let db = mock_db_provider(inmem_db().await);
    let _ = setup_running_turn(&db).await;

    let conn = db.conn().unwrap();
    let repo = TurnRepository;
    let candidates = repo.find_orphan_candidates(&conn, 60, 100).await.unwrap();
    assert!(
        candidates.is_empty(),
        "recent turn should not be an orphan candidate"
    );
}

#[tokio::test]
async fn find_orphan_candidates_excludes_terminal() {
    let db = mock_db_provider(inmem_db().await);
    let (_, _, turn_id, _) = setup_running_turn(&db).await;

    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let repo = TurnRepository;

    // Finalize to Failed
    repo.cas_update_state(
        &conn,
        &scope,
        crate::domain::repos::CasTerminalParams {
            turn_id,
            state: TurnState::Failed,
            error_code: Some("test".to_owned()),
            error_detail: None,
            assistant_message_id: None,
            provider_response_id: None,
        },
    )
    .await
    .unwrap();

    // Make it look old
    backdate_progress(&conn, turn_id).await;

    let candidates = repo.find_orphan_candidates(&conn, 60, 100).await.unwrap();
    assert!(
        candidates.is_empty(),
        "terminal turn should not be an orphan candidate"
    );
}

// ── Phase 3: cas_finalize_orphan ──

#[tokio::test]
async fn cas_finalize_orphan_transitions_to_failed() {
    let db = mock_db_provider(inmem_db().await);
    let (_, chat_id, turn_id, request_id) = setup_running_turn(&db).await;

    let conn = db.conn().unwrap();
    // Make it stale
    backdate_progress(&conn, turn_id).await;

    let repo = TurnRepository;
    let rows = repo.cas_finalize_orphan(&conn, turn_id, 60).await.unwrap();
    assert_eq!(rows, 1, "CAS should succeed for stale running turn");

    let scope = AccessScope::allow_all();
    let turn = repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .unwrap()
        .expect("turn should exist");
    assert_eq!(turn.state, TurnState::Failed);
    assert_eq!(turn.error_code.as_deref(), Some("orphan_timeout"));
    assert!(turn.completed_at.is_some(), "completed_at should be set");
}

#[tokio::test]
async fn cas_finalize_orphan_noop_if_progress_renewed() {
    let db = mock_db_provider(inmem_db().await);
    let (_, _, turn_id, _) = setup_running_turn(&db).await;

    // last_progress_at is fresh (just created) — CAS should fail
    let conn = db.conn().unwrap();
    let repo = TurnRepository;
    let rows = repo.cas_finalize_orphan(&conn, turn_id, 60).await.unwrap();
    assert_eq!(
        rows, 0,
        "CAS should fail when progress was renewed (P1 safety invariant)"
    );
}

#[tokio::test]
async fn cas_finalize_orphan_noop_if_already_terminal() {
    let db = mock_db_provider(inmem_db().await);
    let (_, _, turn_id, _) = setup_running_turn(&db).await;

    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let repo = TurnRepository;

    // Finalize normally first
    repo.cas_update_state(
        &conn,
        &scope,
        crate::domain::repos::CasTerminalParams {
            turn_id,
            state: TurnState::Failed,
            error_code: Some("test".to_owned()),
            error_detail: None,
            assistant_message_id: None,
            provider_response_id: None,
        },
    )
    .await
    .unwrap();

    // Make it look old
    backdate_progress(&conn, turn_id).await;

    let rows = repo.cas_finalize_orphan(&conn, turn_id, 60).await.unwrap();
    assert_eq!(rows, 0, "CAS should fail for already terminal turn");
}
