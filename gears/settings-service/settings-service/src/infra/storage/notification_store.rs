// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-delivery:p1
//! `NotificationStore` over the settings database: what the notification half
//! of the Activation Publisher reads and writes. A read or a stamp is one
//! statement on its own connection; a call of silence and the forgetting of
//! one subscription are each one transaction, writing an instance's state
//! before its subscription — the order every writer of both keeps — and taking
//! no key's mutation lock.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::audit::{ActorTenant, AuditOperation, AuditRecord, AuditSink};
use crate::domain::activation::subscriptions::ActivationRepository;
use crate::domain::activation::{
    ActivationCounts, ActivationStatus, DueRow, InstanceKey, NotificationStore, Silenced,
    SubscriberTenant,
};
use crate::domain::error::DomainError;
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::audit_store::AuditStore;
use crate::infra::storage::clock::now;

/// The store, over the gear's database provider.
pub struct DbNotificationStore {
    db: Arc<DBProvider<DbError>>,
}

impl DbNotificationStore {
    /// Over this database.
    #[must_use]
    pub fn new(db: Arc<DBProvider<DbError>>) -> Self {
        Self { db }
    }

    fn conn(&self) -> Result<toolkit_db::DbConn<'_>, DomainError> {
        self.db
            .conn()
            .map_err(|e| DomainError::dependency_unavailable("database", "open a connection", e))
    }
}

/// `age` before now. An age beyond the clock's range (the configuration and
/// the constants bound it far below) saturates to the epoch.
fn before_now(age: Duration) -> OffsetDateTime {
    time::Duration::try_from(age)
        .ok()
        .and_then(|span| now().checked_sub(span))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// Why a transaction of the store failed: a statement of its body, already
/// mapped by the repository or the sink, or the transaction itself.
enum TxError {
    Statement(DomainError),
    Transaction(DbError),
}

impl From<DbError> for TxError {
    fn from(err: DbError) -> Self {
        Self::Transaction(err)
    }
}

impl TxError {
    fn into_domain(self, operation: &'static str) -> DomainError {
        match self {
            Self::Statement(err) => err,
            Self::Transaction(err) => {
                DomainError::dependency_unavailable("database", operation, err)
            }
        }
    }
}

/// The body of a call of silence, in its transaction.
async fn silence_in<C: DBRunner>(
    tx: &C,
    row: &DueRow,
    at: OffsetDateTime,
) -> Result<Option<Silenced>, DomainError> {
    let instance = row.instance_key();
    // The state first, guarded on the `change_ref` the scan read and on its
    // still being `awaiting`: a report that settled it or a change that
    // replaced it meanwhile leaves the guard unmatched, and the transaction
    // ends with no other statement.
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-23
    if !ActivationRepo
        .call_silent(tx, &instance, row.change.change_ref, at)
        .await?
    {
        return Ok(None);
    }
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-23
    // Then the subscription, unless already silent: the day before forgetting
    // counts from the first call of silence for the key.
    ActivationRepo.mark_silent(tx, &instance, at).await?;
    #[cfg(any(test, feature = "integration"))]
    crate::infra::storage::activation_hold::reached(
        crate::infra::storage::activation_hold::Step::SilentRecord,
        &row.change.key,
    )
    .await;
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-25
    // The record of the call, under the change it answers: the service's
    // judgement, no applied value and no detail. The label is the
    // subscription's at the time of the call.
    let label = ActivationRepo
        .find(tx, &instance)
        .await?
        .and_then(|subscription| subscription.instance_label);
    let tenant = Uuid::parse_str(&row.pair.tenant).map_err(|err| DomainError::Internal {
        diagnostic: format!("activation state carries a tenant that is not a UUID: {err}"),
    })?;
    let record = AuditRecord::new(
        &row.change.key,
        Some(tenant),
        &row.pair.subscriber,
        ActorTenant::machine(),
        AuditOperation::Activation,
        Uuid::new_v4().to_string(),
    )
    .by_module()
    .with_change_ref(row.change.change_ref)
    .with_instance(row.instance_id, label)
    .silent();
    AuditStore
        .append(tx, &AccessScope::allow_all(), record)
        .await?;
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-25
    // The account's transition, judged in the call's transaction over the
    // subscriber's other states of the key at the tenant: `incomplete` is new
    // unless one of them was already `silent` or `failed`.
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-26
    let others = ActivationRepo
        .states_of(tx, &row.change.key, &row.pair.tenant)
        .await?;
    let incomplete = !others.iter().any(|state| {
        state.subscriber == row.pair.subscriber
            && state.instance_id != row.instance_id
            && matches!(
                state.status,
                ActivationStatus::Silent | ActivationStatus::Failed
            )
    });
    Ok(Some(Silenced { incomplete }))
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-26
}

#[async_trait]
impl NotificationStore for DbNotificationStore {
    async fn due_of(
        &self,
        pair: &SubscriberTenant,
        interval: Duration,
    ) -> Result<Vec<DueRow>, DomainError> {
        ActivationRepo
            .due_of(&self.conn()?, pair, before_now(interval))
            .await
    }

    async fn stamp_due(&self, rows: &[DueRow]) -> Result<u64, DomainError> {
        if rows.is_empty() {
            return Ok(0);
        }
        ActivationRepo.stamp_due(&self.conn()?, rows, now()).await
    }

    async fn due(&self, interval: Duration) -> Result<Vec<DueRow>, DomainError> {
        // A state is due when it was never published or last published at least
        // `interval` ago.
        ActivationRepo
            .due(&self.conn()?, before_now(interval))
            .await
    }

    async fn silence(&self, row: &DueRow) -> Result<Option<Silenced>, DomainError> {
        let at = now();
        let row = row.clone();
        self.db
            .db()
            .transaction_ref_mapped::<_, Option<Silenced>, TxError>(move |tx| {
                Box::pin(async move { silence_in(tx, &row, at).await.map_err(TxError::Statement) })
            })
            .await
            .map_err(|err| err.into_domain("run a call of silence"))
    }

    async fn silent_past(&self, age: Duration) -> Result<Vec<InstanceKey>, DomainError> {
        Ok(ActivationRepo
            .forgettable(&self.conn()?, before_now(age))
            .await?
            .iter()
            .map(crate::domain::activation::Subscription::instance_key)
            .collect())
    }

    async fn forget_silent(
        &self,
        instance: &InstanceKey,
        age: Duration,
    ) -> Result<bool, DomainError> {
        let cutoff = before_now(age);
        let instance = instance.clone();
        self.db
            .db()
            .transaction_ref_mapped::<_, bool, TxError>(move |tx| {
                Box::pin(async move {
                    ActivationRepo
                        .forget(tx, &instance, cutoff)
                        .await
                        .map_err(TxError::Statement)
                })
            })
            .await
            .map_err(|err| err.into_domain("forget a silent subscription"))
    }

    async fn sweep_orphans(&self) -> Result<u64, DomainError> {
        ActivationRepo.sweep_orphans(&self.conn()?).await
    }

    async fn counts(&self) -> Result<ActivationCounts, DomainError> {
        ActivationRepo.counts(&self.conn()?).await
    }
}

#[cfg(test)]
#[path = "notification_store_tests.rs"]
mod notification_store_tests;
