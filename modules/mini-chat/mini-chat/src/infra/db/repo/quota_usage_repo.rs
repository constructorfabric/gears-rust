use async_trait::async_trait;
use modkit_db::secure::{
    DBRunner, SecureEntityExt, SecureInsertExt, SecureOnConflict, SecureUpdateExt,
};
use modkit_security::AccessScope;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveEnum, ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect, Set};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{IncrementReserveParams, SettleParams};
use crate::infra::db::entity::quota_usage::{
    ActiveModel, Column, Entity as QuotaUsageEntity, Model as QuotaUsageModel, PeriodType,
};

pub struct QuotaUsageRepository;

#[async_trait]
impl crate::domain::repos::QuotaUsageRepository for QuotaUsageRepository {
    async fn increment_reserve<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: IncrementReserveParams,
    ) -> Result<(), DomainError> {
        let now = OffsetDateTime::now_utc();
        let id = Uuid::new_v4();

        let am = ActiveModel {
            id: Set(id),
            tenant_id: Set(params.tenant_id),
            user_id: Set(params.user_id),
            period_type: Set(params.period_type),
            period_start: Set(params.period_start),
            bucket: Set(params.bucket),
            spent_credits_micro: Set(0),
            reserved_credits_micro: Set(params.amount_micro),
            calls: Set(0),
            input_tokens: Set(0),
            output_tokens: Set(0),
            file_search_calls: Set(0),
            web_search_calls: Set(0),
            rag_retrieval_calls: Set(0),
            image_inputs: Set(0),
            image_upload_bytes: Set(0),
            updated_at: Set(now),
        };

        // ON CONFLICT: increment reserved_credits_micro and refresh updated_at.
        let on_conflict = SecureOnConflict::<QuotaUsageEntity>::columns([
            Column::TenantId,
            Column::UserId,
            Column::PeriodType,
            Column::PeriodStart,
            Column::Bucket,
        ])
        .value(
            Column::ReservedCreditsMicro,
            Expr::col((QuotaUsageEntity, Column::ReservedCreditsMicro))
                .add(Expr::value(params.amount_micro)),
        )?
        .value(Column::UpdatedAt, Expr::value(now))?;

        QuotaUsageEntity::insert(am)
            .secure()
            .scope_unchecked(scope)?
            .on_conflict(on_conflict)
            .exec(runner)
            .await?;

        Ok(())
    }

    async fn settle<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SettleParams,
    ) -> Result<(), DomainError> {
        let now = OffsetDateTime::now_utc();

        // Determine if token/web-search telemetry should be updated (only for `total` bucket).
        let is_total = params.bucket == "total";
        let (input_delta, output_delta) = if is_total {
            (
                params.input_tokens.unwrap_or(0),
                params.output_tokens.unwrap_or(0),
            )
        } else {
            (0, 0)
        };
        let web_search_delta = if is_total { params.web_search_calls } else { 0 };

        let mut update = QuotaUsageEntity::update_many()
            .col_expr(
                Column::ReservedCreditsMicro,
                Expr::col(Column::ReservedCreditsMicro)
                    .sub(Expr::value(params.reserved_credits_micro)),
            )
            .col_expr(
                Column::SpentCreditsMicro,
                Expr::col(Column::SpentCreditsMicro).add(Expr::value(params.actual_credits_micro)),
            )
            .col_expr(
                Column::Calls,
                Expr::col(Column::Calls).add(Expr::value(1i32)),
            )
            .col_expr(
                Column::InputTokens,
                Expr::col(Column::InputTokens).add(Expr::value(input_delta)),
            )
            .col_expr(
                Column::OutputTokens,
                Expr::col(Column::OutputTokens).add(Expr::value(output_delta)),
            )
            .col_expr(Column::UpdatedAt, Expr::value(now));

        if web_search_delta > 0 {
            update = update.col_expr(
                Column::WebSearchCalls,
                Expr::col(Column::WebSearchCalls).add(Expr::value(web_search_delta.cast_signed())),
            );
        }

        update
            .filter(
                Condition::all()
                    .add(Column::TenantId.eq(params.tenant_id))
                    .add(Column::UserId.eq(params.user_id))
                    .add(Column::PeriodType.eq(params.period_type.into_value()))
                    .add(Column::PeriodStart.eq(params.period_start))
                    .add(Column::Bucket.eq(params.bucket)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;

        Ok(())
    }

    async fn find_bucket_rows<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<QuotaUsageModel>, DomainError> {
        Ok(QuotaUsageEntity::find()
            .filter(
                Condition::all()
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::UserId.eq(user_id)),
            )
            .secure()
            .scope_with(scope)
            .all(runner)
            .await?)
    }

    async fn find_bucket_rows_for_update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
        period_types: &[PeriodType],
        period_starts: &[time::Date],
    ) -> Result<Vec<QuotaUsageModel>, DomainError> {
        let period_type_values: Vec<_> = period_types
            .iter()
            .map(|pt| pt.clone().into_value())
            .collect();

        let base_query = QuotaUsageEntity::find().filter(
            Condition::all()
                .add(Column::TenantId.eq(tenant_id))
                .add(Column::UserId.eq(user_id))
                .add(Column::PeriodType.is_in(period_type_values))
                .add(Column::PeriodStart.is_in(period_starts.iter().copied())),
        );

        // FOR UPDATE on Postgres for pessimistic locking.
        // SeaORM omits the FOR UPDATE clause for SQLite backend since it's not supported.
        // SQLite has implicit table-level locking within a transaction.
        let query = base_query.lock(sea_orm::sea_query::LockType::Update);

        Ok(query.secure().scope_with(scope).all(runner).await?)
    }

    async fn get_daily_web_search_calls<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
        period_start: time::Date,
    ) -> Result<u32, DomainError> {
        let row = QuotaUsageEntity::find()
            .filter(
                Condition::all()
                    .add(Column::TenantId.eq(tenant_id))
                    .add(Column::UserId.eq(user_id))
                    .add(Column::PeriodType.eq(PeriodType::Daily.into_value()))
                    .add(Column::PeriodStart.eq(period_start))
                    .add(Column::Bucket.eq("total")),
            )
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?;

        Ok(row.map_or(0, |r| {
            if r.web_search_calls < 0 {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    user_id = %user_id,
                    web_search_calls = r.web_search_calls,
                    "negative web_search_calls detected, clamping to 0"
                );
                0
            } else {
                r.web_search_calls.cast_unsigned()
            }
        }))
    }
}
