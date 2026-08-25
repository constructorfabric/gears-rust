use async_trait::async_trait;
use sea_orm::ExprTrait;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QuerySelect, Set,
    sea_query::LockType,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{CasCompleteParams, CasTerminalParams, CreateTurnParams, ToolCallType};
use crate::infra::db::entity::chat_turn::{
    ActiveModel, Column, Entity as TurnEntity, Model as TurnModel, TurnState,
};

pub struct TurnRepository;

#[async_trait]
impl crate::domain::repos::TurnRepository for TurnRepository {
    async fn create_turn<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: CreateTurnParams,
    ) -> Result<TurnModel, DomainError> {
        let now = OffsetDateTime::now_utc();
        let am = ActiveModel {
            id: Set(params.id),
            tenant_id: Set(params.tenant_id),
            chat_id: Set(params.chat_id),
            request_id: Set(params.request_id),
            requester_type: Set(params.requester_type),
            requester_user_id: Set(params.requester_user_id),
            state: Set(TurnState::Running),
            provider_name: Set(None),
            provider_response_id: Set(None),
            assistant_message_id: Set(None),
            error_code: Set(None),
            error_detail: Set(None),
            reserve_tokens: Set(params.reserve_tokens),
            max_output_tokens_applied: Set(params.max_output_tokens_applied),
            reserved_credits_micro: Set(params.reserved_credits_micro),
            policy_version_applied: Set(params.policy_version_applied),
            effective_model: Set(params.effective_model),
            minimal_generation_floor_applied: Set(params.minimal_generation_floor_applied),
            web_search_enabled: Set(params.web_search_enabled),
            web_search_completed_count: Set(0),
            code_interpreter_completed_count: Set(0),
            file_search_completed_count: Set(0),
            deleted_at: Set(None),
            replaced_by_request_id: Set(None),
            started_at: Set(now),
            last_progress_at: Set(Some(now)),
            completed_at: Set(None),
            updated_at: Set(now),
        };
        Ok(secure_insert::<TurnEntity>(am, scope, runner).await?)
    }

    async fn find_by_chat_and_request_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        request_id: Uuid,
    ) -> Result<Option<TurnModel>, DomainError> {
        Ok(TurnEntity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::RequestId.eq(request_id)),
            )
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?)
    }

    async fn find_running_by_chat_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<TurnModel>, DomainError> {
        Ok(TurnEntity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::State.eq(TurnState::Running))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?)
    }

    async fn cas_update_state<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: CasTerminalParams,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let mut update = TurnEntity::update_many()
            .col_expr(Column::State, Expr::value(params.state.into_value()))
            .col_expr(Column::ErrorCode, Expr::value(params.error_code))
            .col_expr(Column::ErrorDetail, Expr::value(params.error_detail))
            .col_expr(Column::CompletedAt, Expr::value(now))
            .col_expr(Column::UpdatedAt, Expr::value(now));

        // For completed turns, set assistant_message_id and provider_response_id
        // within the same CAS UPDATE (content durability invariant).
        if let Some(msg_id) = params.assistant_message_id {
            update = update.col_expr(Column::AssistantMessageId, Expr::value(Some(msg_id)));
        }
        if params.provider_response_id.is_some() {
            update = update.col_expr(
                Column::ProviderResponseId,
                Expr::value(params.provider_response_id),
            );
        }

        let result = update
            .filter(
                Condition::all()
                    .add(Column::Id.eq(params.turn_id))
                    .add(Column::State.eq(TurnState::Running)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected)
    }

    async fn cas_update_completed<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: CasCompleteParams,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let result = TurnEntity::update_many()
            .col_expr(
                Column::State,
                Expr::value(TurnState::Completed.into_value()),
            )
            .col_expr(
                Column::AssistantMessageId,
                Expr::value(Some(params.assistant_message_id)),
            )
            .col_expr(
                Column::ProviderResponseId,
                Expr::value(params.provider_response_id),
            )
            .col_expr(Column::CompletedAt, Expr::value(now))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(params.turn_id))
                    .add(Column::State.eq(TurnState::Running)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected)
    }

    async fn set_assistant_message_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        turn_id: Uuid,
        assistant_message_id: Uuid,
    ) -> Result<(), DomainError> {
        let now = OffsetDateTime::now_utc();
        let result = TurnEntity::update_many()
            .col_expr(
                Column::AssistantMessageId,
                Expr::value(Some(assistant_message_id)),
            )
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(Column::Id.eq(turn_id))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        if result.rows_affected == 0 {
            return Err(DomainError::internal(format!(
                "set_assistant_message_id: turn {turn_id} not found"
            )));
        }
        Ok(())
    }

    async fn soft_delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        turn_id: Uuid,
        replaced_by_request_id: Option<Uuid>,
    ) -> Result<(), DomainError> {
        let now = OffsetDateTime::now_utc();
        TurnEntity::update_many()
            .col_expr(Column::DeletedAt, Expr::value(Some(now)))
            .col_expr(
                Column::ReplacedByRequestId,
                Expr::value(replaced_by_request_id),
            )
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(Column::Id.eq(turn_id))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(())
    }

    async fn update_progress_at<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        turn_id: Uuid,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let result = TurnEntity::update_many()
            .col_expr(Column::LastProgressAt, Expr::value(Some(now)))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(turn_id))
                    .add(Column::State.eq(TurnState::Running)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected)
    }

    async fn find_orphan_candidates<C: DBRunner>(
        &self,
        runner: &C,
        timeout_secs: u64,
        limit: u32,
    ) -> Result<Vec<TurnModel>, DomainError> {
        let cutoff =
            OffsetDateTime::now_utc() - time::Duration::seconds(timeout_secs.cast_signed());
        let scope = AccessScope::allow_all();
        Ok(TurnEntity::find()
            .filter(
                Condition::all()
                    .add(Column::State.eq(TurnState::Running))
                    .add(Column::DeletedAt.is_null())
                    .add(Column::LastProgressAt.is_not_null())
                    .add(Column::LastProgressAt.lte(cutoff)),
            )
            .secure()
            .scope_with(&scope)
            .order_by(Column::LastProgressAt, Order::Asc)
            .limit(u64::from(limit))
            .all(runner)
            .await?)
    }

    async fn cas_finalize_orphan<C: DBRunner>(
        &self,
        runner: &C,
        turn_id: Uuid,
        timeout_secs: u64,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let cutoff = now - time::Duration::seconds(timeout_secs.cast_signed());
        let scope = AccessScope::allow_all();
        let result = TurnEntity::update_many()
            .col_expr(Column::State, Expr::value(TurnState::Failed.into_value()))
            .col_expr(
                Column::ErrorCode,
                Expr::value(Some("orphan_timeout".to_owned())),
            )
            .col_expr(Column::CompletedAt, Expr::value(now))
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(turn_id))
                    .add(Column::State.eq(TurnState::Running))
                    .add(Column::DeletedAt.is_null())
                    .add(Column::LastProgressAt.lte(cutoff)),
            )
            .secure()
            .scope_with(&scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected)
    }

    async fn find_latest_turn<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<TurnModel>, DomainError> {
        Ok(TurnEntity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .order_by(Column::StartedAt, Order::Desc)
            .one(runner)
            .await?)
    }

    async fn find_latest_for_update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<TurnModel>, DomainError> {
        Ok(TurnEntity::find()
            .filter(
                Condition::all()
                    .add(Column::ChatId.eq(chat_id))
                    .add(Column::DeletedAt.is_null()),
            )
            .lock(LockType::Update)
            .secure()
            .scope_with(scope)
            .order_by(Column::StartedAt, Order::Desc)
            .order_by(Column::Id, Order::Desc)
            .one(runner)
            .await?)
    }

    async fn update_preflight_fields<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: crate::domain::repos::UpdatePreflightParams,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let result = TurnEntity::update_many()
            .col_expr(
                Column::ReserveTokens,
                Expr::value(Some(params.reserve_tokens)),
            )
            .col_expr(
                Column::MaxOutputTokensApplied,
                Expr::value(Some(params.max_output_tokens_applied)),
            )
            .col_expr(
                Column::ReservedCreditsMicro,
                Expr::value(Some(params.reserved_credits_micro)),
            )
            .col_expr(
                Column::PolicyVersionApplied,
                Expr::value(Some(params.policy_version_applied)),
            )
            .col_expr(
                Column::EffectiveModel,
                Expr::value(Some(params.effective_model)),
            )
            .col_expr(
                Column::MinimalGenerationFloorApplied,
                Expr::value(Some(params.minimal_generation_floor_applied)),
            )
            .col_expr(Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(params.turn_id))
                    .add(Column::State.eq(TurnState::Running)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected)
    }

    async fn increment_tool_calls<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        turn_id: Uuid,
        tool: ToolCallType,
    ) -> Result<(), DomainError> {
        let col = match tool {
            ToolCallType::WebSearch => Column::WebSearchCompletedCount,
            ToolCallType::CodeInterpreter => Column::CodeInterpreterCompletedCount,
            ToolCallType::FileSearch => Column::FileSearchCompletedCount,
        };
        TurnEntity::update_many()
            .col_expr(col, Expr::col(col).add(1i32))
            .filter(
                Condition::all()
                    .add(Column::Id.eq(turn_id))
                    .add(Column::State.eq(TurnState::Running))
                    .add(Column::DeletedAt.is_null()),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::repos::{CreateTurnParams, TurnRepository as TurnRepoTrait};
    use crate::domain::service::test_helpers::{inmem_db, insert_chat, mock_db_provider};
    use crate::infra::db::entity::chat_turn::TurnState;
    use toolkit_security::AccessScope;
    use uuid::Uuid;

    /// Helper: backdate `last_progress_at` via `SeaORM` `update_many`.
    async fn backdate_progress(runner: &impl toolkit_db::secure::DBRunner, turn_id: Uuid) {
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
        db: &std::sync::Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
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

    // ── Phase 4: increment_tool_calls ──

    #[tokio::test]
    async fn create_turn_zeros_tool_counts() {
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

        assert_eq!(turn.web_search_completed_count, 0);
        assert_eq!(turn.code_interpreter_completed_count, 0);
    }

    #[tokio::test]
    async fn increment_tool_calls_web_search() {
        let db = mock_db_provider(inmem_db().await);
        let (_, chat_id, turn_id, request_id) = setup_running_turn(&db).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::allow_all();
        let repo = TurnRepository;

        repo.increment_tool_calls(&conn, &scope, turn_id, ToolCallType::WebSearch)
            .await
            .expect("increment should succeed");

        let turn = repo
            .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
            .await
            .unwrap()
            .expect("turn should exist");

        assert_eq!(turn.web_search_completed_count, 1);
        assert_eq!(
            turn.code_interpreter_completed_count, 0,
            "unrelated counter must not change"
        );
    }

    #[tokio::test]
    async fn increment_tool_calls_code_interpreter() {
        let db = mock_db_provider(inmem_db().await);
        let (_, chat_id, turn_id, request_id) = setup_running_turn(&db).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::allow_all();
        let repo = TurnRepository;

        repo.increment_tool_calls(&conn, &scope, turn_id, ToolCallType::CodeInterpreter)
            .await
            .expect("increment should succeed");

        let turn = repo
            .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
            .await
            .unwrap()
            .expect("turn should exist");

        assert_eq!(turn.code_interpreter_completed_count, 1);
        assert_eq!(
            turn.web_search_completed_count, 0,
            "unrelated counter must not change"
        );
    }

    #[tokio::test]
    async fn increment_tool_calls_accumulates() {
        let db = mock_db_provider(inmem_db().await);
        let (_, chat_id, turn_id, request_id) = setup_running_turn(&db).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::allow_all();
        let repo = TurnRepository;

        for _ in 0..3 {
            repo.increment_tool_calls(&conn, &scope, turn_id, ToolCallType::WebSearch)
                .await
                .expect("increment should succeed");
        }
        repo.increment_tool_calls(&conn, &scope, turn_id, ToolCallType::CodeInterpreter)
            .await
            .expect("increment should succeed");

        let turn = repo
            .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
            .await
            .unwrap()
            .expect("turn should exist");

        assert_eq!(turn.web_search_completed_count, 3);
        assert_eq!(turn.code_interpreter_completed_count, 1);
    }
}
