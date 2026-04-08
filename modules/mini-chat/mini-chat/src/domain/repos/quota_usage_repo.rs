use async_trait::async_trait;
use modkit_db::secure::DBRunner;
use modkit_macros::domain_model;
use modkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::db::entity::quota_usage::{Model as QuotaUsageModel, PeriodType};

/// Parameters for reserving quota credits.
#[domain_model]
#[allow(dead_code)]
pub struct IncrementReserveParams {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub period_type: PeriodType,
    pub period_start: time::Date,
    pub bucket: String,
    pub amount_micro: i64,
}

/// Parameters for settling quota after turn completion.
#[domain_model]
#[allow(dead_code)]
pub struct SettleParams {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub period_type: PeriodType,
    pub period_start: time::Date,
    pub bucket: String,
    pub reserved_credits_micro: i64,
    pub actual_credits_micro: i64,
    /// Token telemetry — only applied on `total` bucket.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    /// Web search calls to increment — only applied on `total` bucket.
    pub web_search_calls: u32,
}

/// Repository trait for quota usage persistence operations.
#[async_trait]
#[allow(dead_code)]
pub trait QuotaUsageRepository: Send + Sync {
    /// Atomically increment `reserved_credits_micro` (UPSERT).
    async fn increment_reserve<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: IncrementReserveParams,
    ) -> Result<(), DomainError>;

    /// Atomic reserve-release + spend-commit for settlement.
    async fn settle<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SettleParams,
    ) -> Result<(), DomainError>;

    /// `SELECT` all `quota_usage` rows for a user across periods and buckets.
    async fn find_bucket_rows<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<QuotaUsageModel>, DomainError>;

    /// `SELECT` `quota_usage` rows with pessimistic locking (`FOR UPDATE` on Postgres,
    /// plain `SELECT` on `SQLite`). Filters by `period_types` and `period_starts`.
    async fn find_bucket_rows_for_update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
        period_types: &[PeriodType],
        period_starts: &[time::Date],
    ) -> Result<Vec<QuotaUsageModel>, DomainError>;

    /// Sum `web_search_calls` for a user's daily `total` bucket on the given date.
    /// Returns 0 if no row exists.
    async fn get_daily_web_search_calls<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
        period_start: time::Date,
    ) -> Result<u32, DomainError>;
}
