use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{SummaryFrontier, ThreadSummaryModel};
use crate::infra::db::entity::thread_summary::{self, Column, Entity};

/// Repository for thread summary persistence operations.
pub struct ThreadSummaryRepository;

#[async_trait]
impl crate::domain::repos::ThreadSummaryRepository for ThreadSummaryRepository {
    async fn get_latest<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<ThreadSummaryModel>, DomainError> {
        let row = Entity::find()
            .filter(Column::ChatId.eq(chat_id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?;

        Ok(row.map(|r| ThreadSummaryModel {
            content: r.summary_text,
            frontier: SummaryFrontier {
                created_at: r.summarized_up_to_created_at,
                message_id: r.summarized_up_to_message_id,
            },
            token_estimate: r.token_estimate,
        }))
    }

    async fn upsert_with_cas<C: DBRunner>(
        &self,
        runner: &C,
        chat_id: Uuid,
        tenant_id: Uuid,
        expected_base_frontier: Option<&SummaryFrontier>,
        new_frontier: &SummaryFrontier,
        summary_text: &str,
        token_estimate: i32,
    ) -> Result<u64, DomainError> {
        let now = OffsetDateTime::now_utc();
        let scope = AccessScope::allow_all();

        match expected_base_frontier {
            None => {
                // First summary: INSERT via secure_insert.
                // UNIQUE(chat_id) acts as the CAS guard — if another handler
                // already inserted, the unique violation returns 0.
                let am = thread_summary::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    tenant_id: Set(tenant_id),
                    chat_id: Set(chat_id),
                    summary_text: Set(summary_text.to_owned()),
                    summarized_up_to_created_at: Set(new_frontier.created_at),
                    summarized_up_to_message_id: Set(new_frontier.message_id),
                    token_estimate: Set(token_estimate),
                    created_at: Set(now),
                    updated_at: Set(now),
                };

                match secure_insert::<Entity>(am, &scope, runner).await {
                    Ok(_) => Ok(1),
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("UNIQUE")
                            || msg.contains("unique")
                            || msg.contains("duplicate")
                        {
                            // Another handler already inserted — CAS lost.
                            Ok(0)
                        } else {
                            Err(DomainError::internal(format!("thread_summary insert: {e}")))
                        }
                    }
                }
            }
            Some(base) => {
                // Subsequent summary: UPDATE WHERE frontier matches (CAS guard).
                // Returns 0 if frontier was already advanced by another handler.
                let result = Entity::update_many()
                    .col_expr(Column::SummaryText, Expr::value(summary_text.to_owned()))
                    .col_expr(
                        Column::SummarizedUpToCreatedAt,
                        Expr::value(new_frontier.created_at),
                    )
                    .col_expr(
                        Column::SummarizedUpToMessageId,
                        Expr::value(new_frontier.message_id),
                    )
                    .col_expr(Column::TokenEstimate, Expr::value(token_estimate))
                    .col_expr(Column::UpdatedAt, Expr::value(now))
                    .filter(
                        Condition::all()
                            .add(Column::ChatId.eq(chat_id))
                            .add(Column::SummarizedUpToCreatedAt.eq(base.created_at))
                            .add(Column::SummarizedUpToMessageId.eq(base.message_id)),
                    )
                    .secure()
                    .scope_with(&scope)
                    .exec(runner)
                    .await?;

                Ok(result.rows_affected)
            }
        }
    }

    async fn delete_for_chat<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<u64, DomainError> {
        use crate::infra::db::entity::message::{Column as MessageColumn, Entity as MessageEntity};

        let result = Entity::delete_many()
            .filter(Column::ChatId.eq(chat_id))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        // Without the summary the compressed messages would drop out of the
        // context and out of the next summary for good.
        MessageEntity::update_many()
            .col_expr(MessageColumn::IsCompressed, Expr::value(false))
            .filter(MessageColumn::ChatId.eq(chat_id))
            .filter(MessageColumn::IsCompressed.eq(true))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected)
    }
}
