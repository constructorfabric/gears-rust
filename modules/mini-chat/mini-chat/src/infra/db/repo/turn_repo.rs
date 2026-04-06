use async_trait::async_trait;
use modkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use modkit_security::AccessScope;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QuerySelect, Set,
    sea_query::LockType,
};
use time::OffsetDateTime;
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
#[path = "turn_repo_tests.rs"]
mod tests;
