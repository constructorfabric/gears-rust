use async_trait::async_trait;
use modkit_db::secure::DbTx;
use modkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::quota::{SettlementInput, SettlementOutcome};
use crate::domain::repos::QuotaUsageRepository;
use crate::domain::service::QuotaService;
use crate::domain::stream_events::QuotaWarning;

/// Type-erased settlement interface for `FinalizationService`.
///
/// Erases the `QuotaService<QR>` generic so that `FinalizationService` is
/// non-generic and can be shared via `Arc` into spawned task closures
/// without propagating repository type parameters.
///
/// Takes `&DbTx` (transaction) rather than generic `&impl DBRunner` because
/// finalization always runs within a transaction. This avoids the `Sized`
/// constraint issue with `&dyn DBRunner`.
///
/// See design D2: "Generic erasure via `QuotaSettler` trait".
#[async_trait]
pub trait QuotaSettler: Send + Sync {
    async fn settle_in_tx(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        input: SettlementInput,
    ) -> Result<SettlementOutcome, DomainError>;
}

#[async_trait]
impl<QR: QuotaUsageRepository + 'static> QuotaSettler for QuotaService<QR> {
    async fn settle_in_tx(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        input: SettlementInput,
    ) -> Result<SettlementOutcome, DomainError> {
        // DbTx implements DBRunner, so we can pass it to the generic settle().
        self.settle(tx, scope, input).await
    }
}

/// Type-erased quota warnings interface for `spawn_provider_task`.
///
/// Erases the `QuotaService<QR>` generic so that `FinalizationCtx` can hold
/// a reference without propagating repository type parameters.
#[async_trait]
pub trait QuotaWarningsProvider: Send + Sync {
    async fn get_quota_warnings(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<QuotaWarning>, DomainError>;
}

#[async_trait]
impl<QR: QuotaUsageRepository + 'static> QuotaWarningsProvider for QuotaService<QR> {
    async fn get_quota_warnings(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<QuotaWarning>, DomainError> {
        self.compute_quota_warnings(scope, tenant_id, user_id).await
    }
}
