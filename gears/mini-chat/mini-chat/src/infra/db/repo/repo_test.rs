use std::sync::Arc;

use sea_orm::ActiveEnum;
use toolkit_db::DBProvider;
use toolkit_db::odata::LimitCfg;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::repos::{
    CasCompleteParams, CasTerminalParams, CreateTurnParams, IncrementReserveParams,
    InsertAssistantMessageParams, InsertUserMessageParams, MessageRepository as _,
    QuotaUsageRepository as _, SettleParams, TurnRepository as _,
};
use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};
use crate::infra::db::entity::chat_turn::TurnState;
use crate::infra::db::entity::message::MessageRole;
use crate::infra::db::entity::quota_usage::PeriodType;
use crate::infra::db::repo::message_repo::MessageRepository;
use crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository;
use crate::infra::db::repo::turn_repo::TurnRepository;

type Db = Arc<DBProvider<toolkit_db::DbError>>;

// ── Helpers ──

fn scope() -> AccessScope {
    AccessScope::allow_all()
}

fn limit_cfg() -> LimitCfg {
    LimitCfg {
        default: 20,
        max: 100,
    }
}

async fn test_db() -> Db {
    mock_db_provider(inmem_db().await)
}

/// Insert a parent chat row (required by FK constraints on `chat_turns` and `messages`).
async fn insert_chat(db: &Db, tenant_id: Uuid, chat_id: Uuid) {
    use crate::infra::db::entity::chat::{ActiveModel, Entity as ChatEntity};
    use sea_orm::Set;
    use time::OffsetDateTime;
    use toolkit_db::secure::secure_insert;

    let now = OffsetDateTime::now_utc();
    let am = ActiveModel {
        id: Set(chat_id),
        tenant_id: Set(tenant_id),
        user_id: Set(Uuid::new_v4()),
        model: Set("gpt-5.2".to_owned()),
        title: Set(Some("test".to_owned())),
        is_temporary: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
    };
    let conn = db.conn().unwrap();
    secure_insert::<ChatEntity>(am, &scope(), &conn)
        .await
        .expect("insert chat");
}

fn default_turn_params(tenant_id: Uuid, chat_id: Uuid, request_id: Uuid) -> CreateTurnParams {
    CreateTurnParams {
        id: Uuid::new_v4(),
        tenant_id,
        chat_id,
        request_id,
        requester_type: "user".to_owned(),
        requester_user_id: Some(Uuid::new_v4()),
        reserve_tokens: None,
        max_output_tokens_applied: None,
        reserved_credits_micro: None,
        policy_version_applied: None,
        effective_model: None,
        minimal_generation_floor_applied: None,
        web_search_enabled: false,
    }
}

// ════════════════════════════════════════════════════════════════════
// 7.1 — Entity enum round-trip tests
// ════════════════════════════════════════════════════════════════════

#[test]
fn turn_state_to_value() {
    assert_eq!(TurnState::Running.into_value(), "running".to_owned());
    assert_eq!(TurnState::Completed.into_value(), "completed".to_owned());
    assert_eq!(TurnState::Failed.into_value(), "failed".to_owned());
    assert_eq!(TurnState::Cancelled.into_value(), "cancelled".to_owned());
}

#[test]
fn turn_state_try_from_value() {
    assert_eq!(
        TurnState::try_from_value(&"running".to_owned()).unwrap(),
        TurnState::Running,
    );
    assert_eq!(
        TurnState::try_from_value(&"completed".to_owned()).unwrap(),
        TurnState::Completed,
    );
    assert_eq!(
        TurnState::try_from_value(&"failed".to_owned()).unwrap(),
        TurnState::Failed,
    );
    assert_eq!(
        TurnState::try_from_value(&"cancelled".to_owned()).unwrap(),
        TurnState::Cancelled,
    );
    assert!(TurnState::try_from_value(&"bogus".to_owned()).is_err());
}

#[test]
fn message_role_to_value() {
    assert_eq!(MessageRole::User.into_value(), "user".to_owned());
    assert_eq!(MessageRole::Assistant.into_value(), "assistant".to_owned());
    assert_eq!(MessageRole::System.into_value(), "system".to_owned());
}

#[test]
fn message_role_try_from_value() {
    assert_eq!(
        MessageRole::try_from_value(&"user".to_owned()).unwrap(),
        MessageRole::User,
    );
    assert_eq!(
        MessageRole::try_from_value(&"assistant".to_owned()).unwrap(),
        MessageRole::Assistant,
    );
    assert_eq!(
        MessageRole::try_from_value(&"system".to_owned()).unwrap(),
        MessageRole::System,
    );
    assert!(MessageRole::try_from_value(&"bogus".to_owned()).is_err());
}

#[test]
fn period_type_to_value() {
    assert_eq!(PeriodType::Daily.into_value(), "daily".to_owned());
    assert_eq!(PeriodType::Monthly.into_value(), "monthly".to_owned());
}

#[test]
fn period_type_try_from_value() {
    assert_eq!(
        PeriodType::try_from_value(&"daily".to_owned()).unwrap(),
        PeriodType::Daily,
    );
    assert_eq!(
        PeriodType::try_from_value(&"monthly".to_owned()).unwrap(),
        PeriodType::Monthly,
    );
    assert!(PeriodType::try_from_value(&"bogus".to_owned()).is_err());
}

#[test]
fn turn_state_is_terminal() {
    assert!(!TurnState::Running.is_terminal());
    assert!(TurnState::Completed.is_terminal());
    assert!(TurnState::Failed.is_terminal());
    assert!(TurnState::Cancelled.is_terminal());
}

// ════════════════════════════════════════════════════════════════════
// 7.2 — TurnRepository tests
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn create_turn_success() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();
    let request_id = Uuid::new_v4();
    let params = default_turn_params(tenant_id, chat_id, request_id);

    let turn = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create_turn");

    assert_eq!(turn.chat_id, chat_id);
    assert_eq!(turn.request_id, request_id);
    assert_eq!(turn.state, TurnState::Running);
    assert!(turn.completed_at.is_none());
}

#[tokio::test]
async fn create_turn_duplicate_request_id_rejected() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();
    let request_id = Uuid::new_v4();

    // First insert succeeds
    let params = default_turn_params(tenant_id, chat_id, request_id);
    repo.create_turn(&conn, &scope(), params)
        .await
        .expect("first create_turn");

    // Second insert with same (chat_id, request_id) fails
    let mut params2 = default_turn_params(tenant_id, chat_id, request_id);
    params2.id = Uuid::new_v4(); // different PK
    let err = repo
        .create_turn(&conn, &scope(), params2)
        .await
        .expect_err("duplicate should fail");

    // Should be a database constraint error
    assert!(
        format!("{err:?}").contains("UNIQUE") || format!("{err:?}").contains("Database"),
        "expected UNIQUE constraint error, got: {err:?}"
    );
}

#[tokio::test]
async fn find_by_chat_and_request_id_returns_turn() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();
    let request_id = Uuid::new_v4();

    let params = default_turn_params(tenant_id, chat_id, request_id);
    let created = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create_turn");

    let found = repo
        .find_by_chat_and_request_id(&conn, &scope(), chat_id, request_id)
        .await
        .expect("find")
        .expect("should exist");

    assert_eq!(found.id, created.id);
}

#[tokio::test]
async fn find_running_by_chat_id_finds_running_turn() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();

    let params = default_turn_params(tenant_id, chat_id, Uuid::new_v4());
    let created = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create_turn");

    let found = repo
        .find_running_by_chat_id(&conn, &scope(), chat_id)
        .await
        .expect("find_running")
        .expect("should exist");

    assert_eq!(found.id, created.id);
    assert_eq!(found.state, TurnState::Running);
}

#[tokio::test]
async fn find_running_returns_none_after_completion() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();

    let params = default_turn_params(tenant_id, chat_id, Uuid::new_v4());
    let created = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create_turn");

    // Transition to completed
    let rows = repo
        .cas_update_state(
            &conn,
            &scope(),
            CasTerminalParams {
                turn_id: created.id,
                state: TurnState::Completed,
                error_code: None,
                error_detail: None,
                assistant_message_id: None,
                provider_response_id: None,
            },
        )
        .await
        .expect("cas");
    assert_eq!(rows, 1);

    // No running turns found
    let found = repo
        .find_running_by_chat_id(&conn, &scope(), chat_id)
        .await
        .expect("find_running");
    assert!(found.is_none());
}

// ════════════════════════════════════════════════════════════════════
// 7.3 — TurnRepository CAS tests
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn cas_update_state_on_running_succeeds() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();
    let params = default_turn_params(tenant_id, chat_id, Uuid::new_v4());
    let turn = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create_turn");

    let rows = repo
        .cas_update_state(
            &conn,
            &scope(),
            CasTerminalParams {
                turn_id: turn.id,
                state: TurnState::Failed,
                error_code: Some("provider_error".to_owned()),
                error_detail: Some("timeout".to_owned()),
                assistant_message_id: None,
                provider_response_id: None,
            },
        )
        .await
        .expect("cas");
    assert_eq!(rows, 1, "CAS on running turn should affect 1 row");
}

#[tokio::test]
async fn cas_update_state_on_terminal_returns_zero() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();
    let params = default_turn_params(tenant_id, chat_id, Uuid::new_v4());
    let turn = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create_turn");

    // First CAS succeeds
    repo.cas_update_state(
        &conn,
        &scope(),
        CasTerminalParams {
            turn_id: turn.id,
            state: TurnState::Completed,
            error_code: None,
            error_detail: None,
            assistant_message_id: None,
            provider_response_id: None,
        },
    )
    .await
    .expect("first cas");

    // Second CAS on already-completed turn returns 0
    let rows = repo
        .cas_update_state(
            &conn,
            &scope(),
            CasTerminalParams {
                turn_id: turn.id,
                state: TurnState::Cancelled,
                error_code: None,
                error_detail: None,
                assistant_message_id: None,
                provider_response_id: None,
            },
        )
        .await
        .expect("second cas");
    assert_eq!(rows, 0, "CAS on terminal turn should affect 0 rows");
}

#[tokio::test]
async fn cas_update_completed_sets_assistant_message_id() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();
    let request_id = Uuid::new_v4();
    let params = default_turn_params(tenant_id, chat_id, request_id);
    let turn = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create_turn");

    // Insert an assistant message (required by FK on assistant_message_id)
    let msg_repo = MessageRepository::new(limit_cfg());
    let msg_id = Uuid::new_v4();
    msg_repo
        .insert_assistant_message(
            &conn,
            &scope(),
            InsertAssistantMessageParams {
                id: msg_id,
                tenant_id,
                chat_id,
                request_id,
                content: "response".to_owned(),
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_write_input_tokens: None,
                reasoning_tokens: None,
                model: None,
                provider_response_id: None,
            },
        )
        .await
        .expect("insert_assistant_msg");

    let rows = repo
        .cas_update_completed(
            &conn,
            &scope(),
            CasCompleteParams {
                turn_id: turn.id,
                assistant_message_id: msg_id,
                provider_response_id: Some("resp_123".to_owned()),
            },
        )
        .await
        .expect("cas_complete");
    assert_eq!(rows, 1);

    // Verify the turn was updated
    let found = repo
        .find_by_chat_and_request_id(&conn, &scope(), chat_id, request_id)
        .await
        .expect("find")
        .expect("should exist");
    assert_eq!(found.state, TurnState::Completed);
    assert_eq!(found.assistant_message_id, Some(msg_id));
    assert_eq!(found.provider_response_id.as_deref(), Some("resp_123"));
    assert!(found.completed_at.is_some());
}

// ════════════════════════════════════════════════════════════════════
// 7.2 (cont.) — soft_delete + find_latest_turn
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn soft_delete_hides_from_find_latest() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();

    // Create and complete a turn
    let params = default_turn_params(tenant_id, chat_id, Uuid::new_v4());
    let turn = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create");
    repo.cas_update_state(
        &conn,
        &scope(),
        CasTerminalParams {
            turn_id: turn.id,
            state: TurnState::Completed,
            error_code: None,
            error_detail: None,
            assistant_message_id: None,
            provider_response_id: None,
        },
    )
    .await
    .expect("complete");

    // Soft-delete it
    repo.soft_delete(&conn, &scope(), turn.id, None)
        .await
        .expect("soft_delete");

    // find_latest_turn should return None (deleted_at IS NULL filter)
    let latest = repo
        .find_latest_turn(&conn, &scope(), chat_id)
        .await
        .expect("find_latest");
    assert!(latest.is_none());
}

#[tokio::test]
async fn find_latest_turn_returns_most_recent() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();

    // Create first turn and complete it
    let params1 = default_turn_params(tenant_id, chat_id, Uuid::new_v4());
    let turn1 = repo
        .create_turn(&conn, &scope(), params1)
        .await
        .expect("create1");
    repo.cas_update_state(
        &conn,
        &scope(),
        CasTerminalParams {
            turn_id: turn1.id,
            state: TurnState::Completed,
            error_code: None,
            error_detail: None,
            assistant_message_id: None,
            provider_response_id: None,
        },
    )
    .await
    .expect("complete1");

    // Create second turn
    let params2 = default_turn_params(tenant_id, chat_id, Uuid::new_v4());
    let turn2 = repo
        .create_turn(&conn, &scope(), params2)
        .await
        .expect("create2");

    // find_latest should return the second turn (most recent started_at)
    let latest = repo
        .find_latest_turn(&conn, &scope(), chat_id)
        .await
        .expect("find_latest")
        .expect("should exist");
    assert_eq!(latest.id, turn2.id);
}

// ════════════════════════════════════════════════════════════════════
// 7.4 — MessageRepository tests
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn insert_user_message_round_trip() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let request_id = Uuid::new_v4();

    let msg = repo
        .insert_user_message(
            &conn,
            &scope(),
            InsertUserMessageParams {
                id: Uuid::new_v4(),
                tenant_id,
                chat_id,
                request_id,
                content: "hello world".to_owned(),
            },
        )
        .await
        .expect("insert_user");

    assert_eq!(msg.role, MessageRole::User);
    assert_eq!(msg.content, "hello world");
    assert_eq!(msg.chat_id, chat_id);
    assert_eq!(msg.request_id, Some(request_id));
}

#[tokio::test]
async fn insert_assistant_message_with_usage() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let request_id = Uuid::new_v4();

    let msg = repo
        .insert_assistant_message(
            &conn,
            &scope(),
            InsertAssistantMessageParams {
                id: Uuid::new_v4(),
                tenant_id,
                chat_id,
                request_id,
                content: "sure, here's the answer".to_owned(),
                input_tokens: Some(100),
                output_tokens: Some(50),
                cache_read_input_tokens: Some(42),
                cache_write_input_tokens: Some(17),
                reasoning_tokens: Some(88),
                model: Some("gpt-5.2".to_owned()),
                provider_response_id: Some("resp_abc".to_owned()),
            },
        )
        .await
        .expect("insert_assistant");

    assert_eq!(msg.role, MessageRole::Assistant);
    assert_eq!(msg.input_tokens, 100);
    assert_eq!(msg.output_tokens, 50);
    assert_eq!(msg.cache_read_input_tokens, 42);
    assert_eq!(msg.cache_write_input_tokens, 17);
    assert_eq!(msg.reasoning_tokens, 88);
    assert_eq!(msg.model.as_deref(), Some("gpt-5.2"));
}

#[tokio::test]
async fn find_messages_by_chat_and_request_id() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let request_id = Uuid::new_v4();

    // Insert user + assistant messages for the same request
    repo.insert_user_message(
        &conn,
        &scope(),
        InsertUserMessageParams {
            id: Uuid::new_v4(),
            tenant_id,
            chat_id,
            request_id,
            content: "question".to_owned(),
        },
    )
    .await
    .expect("insert_user");

    repo.insert_assistant_message(
        &conn,
        &scope(),
        InsertAssistantMessageParams {
            id: Uuid::new_v4(),
            tenant_id,
            chat_id,
            request_id,
            content: "answer".to_owned(),
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            reasoning_tokens: None,
            model: None,
            provider_response_id: None,
        },
    )
    .await
    .expect("insert_assistant");

    let msgs = repo
        .find_by_chat_and_request_id(&conn, &scope(), chat_id, request_id)
        .await
        .expect("find");

    assert_eq!(msgs.len(), 2);
    assert!(msgs.iter().any(|m| m.role == MessageRole::User));
    assert!(msgs.iter().any(|m| m.role == MessageRole::Assistant));
}

#[tokio::test]
async fn duplicate_user_message_rejected() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let request_id = Uuid::new_v4();

    // First user message succeeds
    repo.insert_user_message(
        &conn,
        &scope(),
        InsertUserMessageParams {
            id: Uuid::new_v4(),
            tenant_id,
            chat_id,
            request_id,
            content: "hello".to_owned(),
        },
    )
    .await
    .expect("first insert");

    // Second user message with same (chat_id, request_id, role=user) fails
    let err = repo
        .insert_user_message(
            &conn,
            &scope(),
            InsertUserMessageParams {
                id: Uuid::new_v4(),
                tenant_id,
                chat_id,
                request_id,
                content: "duplicate".to_owned(),
            },
        )
        .await
        .expect_err("duplicate should fail");

    assert!(
        format!("{err:?}").contains("UNIQUE") || format!("{err:?}").contains("Database"),
        "expected UNIQUE constraint error, got: {err:?}"
    );
}

// ════════════════════════════════════════════════════════════════════
// 7.5 — QuotaUsageRepository tests
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn increment_reserve_creates_row_on_first_call() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let repo = QuotaUsageRepository;
    let conn = db.conn().unwrap();

    repo.increment_reserve(
        &conn,
        &scope(),
        IncrementReserveParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start: time::Date::from_calendar_date(2026, time::Month::March, 5).unwrap(),
            bucket: "total".to_owned(),
            amount_micro: 1000,
        },
    )
    .await
    .expect("increment_reserve");

    let rows = repo
        .find_bucket_rows(&conn, &scope(), tenant_id, user_id)
        .await
        .expect("find");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reserved_credits_micro, 1000);
    assert_eq!(rows[0].spent_credits_micro, 0);
}

#[tokio::test]
async fn increment_reserve_upserts_on_second_call() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let repo = QuotaUsageRepository;
    let conn = db.conn().unwrap();

    let period_start = time::Date::from_calendar_date(2026, time::Month::March, 5).unwrap();

    repo.increment_reserve(
        &conn,
        &scope(),
        IncrementReserveParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            amount_micro: 1000,
        },
    )
    .await
    .expect("first");

    // Second call with same key should increment, not insert new row
    repo.increment_reserve(
        &conn,
        &scope(),
        IncrementReserveParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            amount_micro: 500,
        },
    )
    .await
    .expect("second");

    let rows = repo
        .find_bucket_rows(&conn, &scope(), tenant_id, user_id)
        .await
        .expect("find");
    assert_eq!(rows.len(), 1, "should be single row (upsert)");
    assert_eq!(rows[0].reserved_credits_micro, 1500); // 1000 + 500
}

#[tokio::test]
async fn settle_decrements_reserved_increments_spent() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let repo = QuotaUsageRepository;
    let conn = db.conn().unwrap();
    let period_start = time::Date::from_calendar_date(2026, time::Month::March, 5).unwrap();

    // Reserve first
    repo.increment_reserve(
        &conn,
        &scope(),
        IncrementReserveParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            amount_micro: 2000,
        },
    )
    .await
    .expect("reserve");

    // Settle: release 2000 reserved, commit 1500 spent
    repo.settle(
        &conn,
        &scope(),
        SettleParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            reserved_credits_micro: 2000,
            actual_credits_micro: 1500,
            input_tokens: Some(100),
            output_tokens: Some(50),
            web_search_calls: 0,
            code_interpreter_calls: 0,
        },
    )
    .await
    .expect("settle");

    let rows = repo
        .find_bucket_rows(&conn, &scope(), tenant_id, user_id)
        .await
        .expect("find");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reserved_credits_micro, 0); // 2000 - 2000
    assert_eq!(rows[0].spent_credits_micro, 1500);
    assert_eq!(rows[0].calls, 1);
    assert_eq!(rows[0].input_tokens, 100);
    assert_eq!(rows[0].output_tokens, 50);
}

#[tokio::test]
async fn settle_non_total_bucket_skips_token_telemetry() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let repo = QuotaUsageRepository;
    let conn = db.conn().unwrap();
    let period_start = time::Date::from_calendar_date(2026, time::Month::March, 5).unwrap();

    repo.increment_reserve(
        &conn,
        &scope(),
        IncrementReserveParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Monthly,
            period_start,
            bucket: "model:gpt-5.2".to_owned(),
            amount_micro: 1000,
        },
    )
    .await
    .expect("reserve");

    // Settle on non-total bucket — tokens should NOT be updated
    repo.settle(
        &conn,
        &scope(),
        SettleParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Monthly,
            period_start,
            bucket: "model:gpt-5.2".to_owned(),
            reserved_credits_micro: 1000,
            actual_credits_micro: 800,
            input_tokens: Some(999),
            output_tokens: Some(999),
            web_search_calls: 0,
            code_interpreter_calls: 0,
        },
    )
    .await
    .expect("settle");

    let rows = repo
        .find_bucket_rows(&conn, &scope(), tenant_id, user_id)
        .await
        .expect("find");
    assert_eq!(rows[0].spent_credits_micro, 800);
    assert_eq!(
        rows[0].input_tokens, 0,
        "non-total bucket: tokens not updated"
    );
    assert_eq!(
        rows[0].output_tokens, 0,
        "non-total bucket: tokens not updated"
    );
}

#[tokio::test]
async fn settle_increments_web_search_calls_on_total_bucket() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let repo = QuotaUsageRepository;
    let conn = db.conn().unwrap();
    let period_start = time::Date::from_calendar_date(2026, time::Month::March, 5).unwrap();

    repo.increment_reserve(
        &conn,
        &scope(),
        IncrementReserveParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            amount_micro: 2000,
        },
    )
    .await
    .expect("reserve");

    // Settle with 2 web search calls
    repo.settle(
        &conn,
        &scope(),
        SettleParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            reserved_credits_micro: 2000,
            actual_credits_micro: 1500,
            input_tokens: Some(100),
            output_tokens: Some(50),
            web_search_calls: 2,
            code_interpreter_calls: 0,
        },
    )
    .await
    .expect("settle");

    let rows = repo
        .find_bucket_rows(&conn, &scope(), tenant_id, user_id)
        .await
        .expect("find");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].web_search_calls, 2);
}

#[tokio::test]
async fn settle_zero_web_search_calls_unchanged() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let repo = QuotaUsageRepository;
    let conn = db.conn().unwrap();
    let period_start = time::Date::from_calendar_date(2026, time::Month::March, 5).unwrap();

    repo.increment_reserve(
        &conn,
        &scope(),
        IncrementReserveParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            amount_micro: 2000,
        },
    )
    .await
    .expect("reserve");

    // Settle with 0 web search calls — column should stay at 0
    repo.settle(
        &conn,
        &scope(),
        SettleParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            reserved_credits_micro: 2000,
            actual_credits_micro: 1000,
            input_tokens: Some(50),
            output_tokens: Some(25),
            web_search_calls: 0,
            code_interpreter_calls: 0,
        },
    )
    .await
    .expect("settle");

    let rows = repo
        .find_bucket_rows(&conn, &scope(), tenant_id, user_id)
        .await
        .expect("find");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].web_search_calls, 0);
}

#[tokio::test]
async fn settle_increments_code_interpreter_calls_on_total_bucket() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let repo = QuotaUsageRepository;
    let conn = db.conn().unwrap();
    let period_start = time::Date::from_calendar_date(2026, time::Month::March, 5).unwrap();

    repo.increment_reserve(
        &conn,
        &scope(),
        IncrementReserveParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            amount_micro: 2000,
        },
    )
    .await
    .expect("reserve");

    // Settle with 3 code interpreter calls
    repo.settle(
        &conn,
        &scope(),
        SettleParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start,
            bucket: "total".to_owned(),
            reserved_credits_micro: 2000,
            actual_credits_micro: 1500,
            input_tokens: Some(100),
            output_tokens: Some(50),
            web_search_calls: 0,
            code_interpreter_calls: 3,
        },
    )
    .await
    .expect("settle");

    let rows = repo
        .find_bucket_rows(&conn, &scope(), tenant_id, user_id)
        .await
        .expect("find");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].code_interpreter_calls, 3);
}

#[tokio::test]
async fn get_daily_code_interpreter_calls_returns_sum() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let repo = QuotaUsageRepository;
    let conn = db.conn().unwrap();
    let today = time::Date::from_calendar_date(2026, time::Month::March, 5).unwrap();

    // Create and settle with code_interpreter_calls
    repo.increment_reserve(
        &conn,
        &scope(),
        IncrementReserveParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start: today,
            bucket: "total".to_owned(),
            amount_micro: 1000,
        },
    )
    .await
    .expect("reserve");

    repo.settle(
        &conn,
        &scope(),
        SettleParams {
            tenant_id,
            user_id,
            period_type: PeriodType::Daily,
            period_start: today,
            bucket: "total".to_owned(),
            reserved_credits_micro: 1000,
            actual_credits_micro: 1000,
            input_tokens: Some(10),
            output_tokens: Some(5),
            web_search_calls: 0,
            code_interpreter_calls: 7,
        },
    )
    .await
    .expect("settle");

    let count = repo
        .get_daily_code_interpreter_calls(&conn, &scope(), tenant_id, user_id, today)
        .await
        .expect("get daily ci calls");
    assert_eq!(count, 7);
}

#[tokio::test]
async fn find_bucket_rows_returns_all_period_buckets() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let repo = QuotaUsageRepository;
    let conn = db.conn().unwrap();
    let period_start = time::Date::from_calendar_date(2026, time::Month::March, 5).unwrap();

    // Insert three different buckets
    for bucket in ["total", "model:gpt-5.2", "model:gpt-5-mini"] {
        repo.increment_reserve(
            &conn,
            &scope(),
            IncrementReserveParams {
                tenant_id,
                user_id,
                period_type: PeriodType::Daily,
                period_start,
                bucket: bucket.to_owned(),
                amount_micro: 100,
            },
        )
        .await
        .expect("reserve");
    }

    let rows = repo
        .find_bucket_rows(&conn, &scope(), tenant_id, user_id)
        .await
        .expect("find");
    assert_eq!(rows.len(), 3);
}

// ════════════════════════════════════════════════════════════════════
// 8.1 — CAS mutual exclusion integration test
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn cas_mutual_exclusion_exactly_one_winner() {
    use crate::infra::db::entity::chat_turn::TurnState;

    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();

    let params = default_turn_params(tenant_id, chat_id, Uuid::new_v4());
    let turn = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create_turn");

    // Two concurrent CAS attempts on the same running turn.
    // With SQLite (max_conns=1) these serialize, but the CAS semantics are correct:
    // the second one sees `state != running` because the first already transitioned.
    let s1 = scope();
    let s2 = scope();
    let (r1, r2) = tokio::join!(
        repo.cas_update_state(
            &conn,
            &s1,
            CasTerminalParams {
                turn_id: turn.id,
                state: TurnState::Completed,
                error_code: None,
                error_detail: None,
                assistant_message_id: None,
                provider_response_id: None,
            },
        ),
        repo.cas_update_state(
            &conn,
            &s2,
            CasTerminalParams {
                turn_id: turn.id,
                state: TurnState::Failed,
                error_code: Some("timeout".to_owned()),
                error_detail: None,
                assistant_message_id: None,
                provider_response_id: None,
            },
        ),
    );

    let rows1 = r1.expect("cas1");
    let rows2 = r2.expect("cas2");

    // Exactly one should succeed (1 row), the other should fail (0 rows)
    assert_eq!(
        rows1 + rows2,
        1,
        "exactly one CAS should win: got {rows1} + {rows2}"
    );
}

#[tokio::test]
async fn create_turn_persists_web_search_enabled() {
    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = TurnRepository;
    let conn = db.conn().unwrap();

    // Create with web_search_enabled = true
    let request_id = Uuid::new_v4();
    let mut params = default_turn_params(tenant_id, chat_id, request_id);
    params.web_search_enabled = true;

    let turn = repo
        .create_turn(&conn, &scope(), params)
        .await
        .expect("create_turn");
    assert!(
        turn.web_search_enabled,
        "web_search_enabled should be true on insert"
    );

    // Read back via find_by_chat_and_request_id
    let found = repo
        .find_by_chat_and_request_id(&conn, &scope(), chat_id, request_id)
        .await
        .expect("find turn")
        .expect("turn should exist");
    assert!(
        found.web_search_enabled,
        "web_search_enabled should survive round-trip"
    );

    // Create another turn (different chat) with web_search_enabled = false (default)
    let chat_id2 = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id2).await;
    let request_id2 = Uuid::new_v4();
    let params2 = default_turn_params(tenant_id, chat_id2, request_id2);
    let turn2 = repo
        .create_turn(&conn, &scope(), params2)
        .await
        .expect("create_turn2");
    assert!(
        !turn2.web_search_enabled,
        "web_search_enabled should default to false"
    );
}

#[test]
fn odata_client_errors_keep_the_odata_error() {
    use crate::domain::error::DomainError;
    use crate::infra::db::repo::odata_err;

    for e in [
        toolkit_odata::Error::InvalidFilter("unknown field".to_owned()),
        toolkit_odata::Error::InvalidOrderByField("nope".to_owned()),
        toolkit_odata::Error::InvalidCursor,
        toolkit_odata::Error::FilterMismatch,
        toolkit_odata::Error::OrderMismatch,
        toolkit_odata::Error::InvalidLimit,
        toolkit_odata::Error::OrderWithCursor,
        toolkit_odata::Error::CursorInvalidBase64,
        toolkit_odata::Error::CursorInvalidJson,
        toolkit_odata::Error::CursorInvalidVersion,
        toolkit_odata::Error::CursorInvalidKeys,
        toolkit_odata::Error::CursorInvalidFields,
        toolkit_odata::Error::CursorInvalidDirection,
    ] {
        assert!(
            matches!(odata_err(e.clone()), DomainError::OData(_)),
            "{e} must be a client error"
        );
    }
    for e in [
        toolkit_odata::Error::Db("boom".to_owned()),
        toolkit_odata::Error::ParsingUnavailable("no parser"),
    ] {
        assert!(
            matches!(odata_err(e.clone()), DomainError::Database { .. }),
            "{e} must be a database error"
        );
    }
}

#[tokio::test]
async fn touch_activity_bumps_chat_updated_at() {
    use crate::domain::repos::ChatRepository as _;
    use crate::infra::db::repo::chat_repo::ChatRepository;

    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = ChatRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let before = repo.get(&conn, &scope(), chat_id).await.unwrap().unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(repo.touch_activity(&conn, &scope(), chat_id).await.unwrap());

    let after = repo.get(&conn, &scope(), chat_id).await.unwrap().unwrap();
    assert!(after.updated_at > before.updated_at);
}

#[tokio::test]
async fn delete_stale_placeholder_keeps_fresh_and_populated_rows() {
    use crate::domain::repos::{InsertVectorStoreParams, VectorStoreRepository as _};
    use crate::infra::db::repo::vector_store_repo::VectorStoreRepository;
    use time::{Duration, OffsetDateTime};

    let db = test_db().await;
    let conn = db.conn().unwrap();
    let repo = VectorStoreRepository;
    let tenant_id = Uuid::new_v4();

    let mut rows = Vec::new();
    for _ in 0..2 {
        let chat_id = Uuid::new_v4();
        insert_chat(&db, tenant_id, chat_id).await;
        let row = repo
            .insert(
                &conn,
                &scope(),
                InsertVectorStoreParams {
                    id: Uuid::new_v4(),
                    tenant_id,
                    chat_id,
                    provider: "openai".to_owned(),
                },
            )
            .await
            .unwrap();
        rows.push(row);
    }
    let (placeholder, populated) = (&rows[0], &rows[1]);
    repo.cas_set_vector_store_id(&conn, &scope(), populated.id, "vs_populated")
        .await
        .unwrap();

    let past = OffsetDateTime::now_utc() - Duration::seconds(120);
    let future = OffsetDateTime::now_utc() + Duration::seconds(1);

    // Created after the cutoff: not stale yet.
    let n = repo
        .delete_stale_placeholder(&conn, &scope(), placeholder.id, past)
        .await
        .unwrap();
    assert_eq!(n, 0);
    // The creator set the ID: the row stays even though it is old enough.
    let n = repo
        .delete_stale_placeholder(&conn, &scope(), populated.id, future)
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert!(
        repo.find_by_chat(&conn, &scope(), populated.chat_id)
            .await
            .unwrap()
            .is_some()
    );
    // NULL and old enough: reclaimed.
    let n = repo
        .delete_stale_placeholder(&conn, &scope(), placeholder.id, future)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn touch_activity_is_false_for_soft_deleted_chat() {
    use crate::domain::repos::ChatRepository as _;
    use crate::infra::db::repo::chat_repo::ChatRepository;

    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;

    let repo = ChatRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    assert!(repo.soft_delete(&conn, &scope(), chat_id).await.unwrap());
    assert!(!repo.touch_activity(&conn, &scope(), chat_id).await.unwrap());
}

#[tokio::test]
async fn attachment_ids_for_message_skips_deleted_and_other_chat() {
    use crate::domain::repos::MessageAttachmentRepository as _;
    use crate::domain::service::test_helpers::{
        InsertTestAttachmentParams, insert_test_attachment, insert_test_message,
        insert_test_message_attachment,
    };
    use crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository;

    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let other_chat_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;
    insert_chat(&db, tenant_id, other_chat_id).await;
    insert_test_message(&db, tenant_id, chat_id, message_id).await;

    let live = insert_test_attachment(
        &db,
        InsertTestAttachmentParams::ready_document(tenant_id, chat_id),
    )
    .await;
    let deleted = insert_test_attachment(
        &db,
        InsertTestAttachmentParams {
            deleted_at: Some(time::OffsetDateTime::now_utc()),
            ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
        },
    )
    .await;
    insert_test_message_attachment(&db, tenant_id, chat_id, message_id, live).await;
    insert_test_message_attachment(&db, tenant_id, chat_id, message_id, deleted).await;

    let repo = MessageAttachmentRepository;
    let conn = db.conn().unwrap();
    let ids = repo
        .attachment_ids_for_message(&conn, &scope(), chat_id, message_id)
        .await
        .unwrap();
    assert_eq!(ids, vec![live], "soft-deleted attachment must be excluded");

    let ids = repo
        .attachment_ids_for_message(&conn, &scope(), other_chat_id, message_id)
        .await
        .unwrap();
    assert!(ids.is_empty(), "links of another chat must be excluded");
}

#[tokio::test]
async fn summary_frontier_excludes_the_finalized_turn() {
    use crate::domain::repos::MessageRepository as _;

    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;
    let repo = MessageRepository::new(limit_cfg());
    let conn = db.conn().unwrap();

    let mut last_assistant_ids = Vec::new();
    let request_ids = [Uuid::new_v4(), Uuid::new_v4()];
    for request_id in request_ids {
        repo.insert_user_message(
            &conn,
            &scope(),
            InsertUserMessageParams {
                id: Uuid::new_v4(),
                tenant_id,
                chat_id,
                request_id,
                content: "question".to_owned(),
            },
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let assistant = repo
            .insert_assistant_message(
                &conn,
                &scope(),
                InsertAssistantMessageParams {
                    id: Uuid::new_v4(),
                    tenant_id,
                    chat_id,
                    request_id,
                    content: "answer".to_owned(),
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_input_tokens: None,
                    cache_write_input_tokens: None,
                    reasoning_tokens: None,
                    model: None,
                    provider_response_id: None,
                },
            )
            .await
            .unwrap();
        last_assistant_ids.push(assistant.id);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // Finalizing the second turn: the frontier is the first turn's answer.
    let frontier = repo
        .find_latest_message_before_turn(&conn, &scope(), chat_id, request_ids[1])
        .await
        .unwrap()
        .expect("first turn is summarizable");
    assert_eq!(frontier.message_id, last_assistant_ids[0]);

    // Finalizing the first (and only earlier) turn: nothing to summarize.
    let only_turn_chat = Uuid::new_v4();
    insert_chat(&db, tenant_id, only_turn_chat).await;
    let none = repo
        .find_latest_message_before_turn(&conn, &scope(), only_turn_chat, request_ids[0])
        .await
        .unwrap();
    assert!(none.is_none());
}

/// The effective list order always ends with `id`: after the default key
/// when the client sets no `$orderby`, after the client's keys otherwise. A
/// cursor keeps its own order.
#[test]
fn list_order_always_ends_with_id() {
    use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, SortDir};
    let fields = |q: &ODataQuery| {
        q.order
            .0
            .iter()
            .map(|k| {
                format!(
                    "{}{}",
                    if k.dir == SortDir::Desc { "-" } else { "+" },
                    k.field
                )
            })
            .collect::<Vec<_>>()
    };
    let default = ("updated_at", SortDir::Desc);

    let q = crate::infra::db::repo::with_id_tiebreaker(&ODataQuery::new(), default, SortDir::Desc);
    assert_eq!(fields(&q), ["-updated_at", "-id"]);

    let by_title = ODataQuery::new().with_order(ODataOrderBy(vec![OrderKey {
        field: "title".to_owned(),
        dir: SortDir::Asc,
    }]));
    let q = crate::infra::db::repo::with_id_tiebreaker(&by_title, default, SortDir::Desc);
    assert_eq!(fields(&q), ["+title", "-updated_at", "-id"]);
}

/// Walks every page of `list_page` with the given limit, following
/// `next_cursor`, and returns the ids in page order.
async fn page_all_chats(db: &Db, limit: u64) -> Vec<Uuid> {
    use crate::domain::repos::ChatRepository as _;
    use crate::infra::db::repo::chat_repo::ChatRepository;
    use toolkit_odata::{CursorV1, ODataQuery};

    let repo = ChatRepository::new(limit_cfg());
    let conn = db.conn().unwrap();
    let mut ids = Vec::new();
    let mut query = ODataQuery::new().with_limit(limit);
    for _ in 0..20 {
        let page = repo.list_page(&conn, &scope(), &query).await.unwrap();
        ids.extend(page.items.iter().map(|c| c.id));
        let Some(next) = page.page_info.next_cursor else {
            return ids;
        };
        query = ODataQuery::new()
            .with_limit(limit)
            .with_cursor(CursorV1::decode(&next).unwrap());
    }
    panic!("pagination did not terminate");
}

/// Chats tied on `updated_at` are paged in `id` order, each exactly once.
#[tokio::test]
async fn chat_pages_with_tied_updated_at_return_every_row_once() {
    use crate::infra::db::entity::chat::{ActiveModel, Entity as ChatEntity};
    use sea_orm::Set;
    use toolkit_db::secure::secure_insert;

    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let ts = time::macros::datetime!(2026-01-01 00:00:00 UTC);
    let mut expected: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
    let conn = db.conn().unwrap();
    for id in &expected {
        let am = ActiveModel {
            id: Set(*id),
            tenant_id: Set(tenant_id),
            user_id: Set(Uuid::new_v4()),
            model: Set("gpt-5.2".to_owned()),
            title: Set(Some("same".to_owned())),
            is_temporary: Set(false),
            created_at: Set(ts),
            updated_at: Set(ts),
            deleted_at: Set(None),
        };
        secure_insert::<ChatEntity>(am, &scope(), &conn)
            .await
            .expect("insert chat");
    }
    // Default order: updated_at desc, then id desc.
    expected.sort_unstable_by(|a, b| b.cmp(a));

    for limit in [1, 2] {
        assert_eq!(page_all_chats(&db, limit).await, expected, "limit {limit}");
    }
}

/// Messages tied on `created_at` are paged in `id` order, each exactly once.
#[tokio::test]
async fn message_pages_with_tied_created_at_return_every_row_once() {
    use crate::infra::db::entity::message::{ActiveModel, Entity as MessageEntity};
    use sea_orm::Set;
    use toolkit_db::secure::secure_insert;
    use toolkit_odata::{CursorV1, ODataQuery};

    let db = test_db().await;
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    insert_chat(&db, tenant_id, chat_id).await;
    let ts = time::macros::datetime!(2026-01-01 00:00:00 UTC);
    let conn = db.conn().unwrap();
    let mut expected: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
    for id in &expected {
        let am = ActiveModel {
            id: Set(*id),
            tenant_id: Set(tenant_id),
            chat_id: Set(chat_id),
            request_id: Set(Some(Uuid::new_v4())),
            role: Set(MessageRole::User),
            content: Set("same".to_owned()),
            content_type: Set("text".to_owned()),
            token_estimate: Set(0),
            provider_response_id: Set(None),
            request_kind: Set(Some("chat".to_owned())),
            features_used: Set(serde_json::json!([])),
            input_tokens: Set(0),
            output_tokens: Set(0),
            cache_read_input_tokens: Set(0),
            cache_write_input_tokens: Set(0),
            reasoning_tokens: Set(0),
            model: Set(None),
            is_compressed: Set(false),
            created_at: Set(ts),
            deleted_at: Set(None),
        };
        secure_insert::<MessageEntity>(am, &scope(), &conn)
            .await
            .expect("insert message");
    }
    // Default order: created_at asc, then id asc.
    expected.sort_unstable();

    let repo = MessageRepository::new(limit_cfg());
    for limit in [1, 2] {
        let mut ids = Vec::new();
        let mut query = ODataQuery::new().with_limit(limit);
        let mut pages = 0;
        loop {
            let page = repo
                .list_by_chat(&conn, &scope(), chat_id, &query)
                .await
                .unwrap();
            ids.extend(page.items.iter().map(|m| m.id));
            let Some(next) = page.page_info.next_cursor else {
                break;
            };
            pages += 1;
            assert!(pages < 20, "pagination did not terminate");
            query = ODataQuery::new()
                .with_limit(limit)
                .with_cursor(CursorV1::decode(&next).unwrap());
        }
        assert_eq!(ids, expected, "limit {limit}");
    }
}
