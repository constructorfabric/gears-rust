// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-delivery:p1
//! `NotificationStore` over the settings database: what the notification half
//! of the Activation Publisher reads and writes, one statement each on its own
//! connection.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_db::{DBProvider, DbError};

use crate::domain::activation::{
    ActivationCounts, DueRow, NotificationStore, NotifiedChange, SubscriberTenant,
};
use crate::domain::error::DomainError;
use crate::infra::storage::activation_repo::ActivationRepo;
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

#[async_trait]
impl NotificationStore for DbNotificationStore {
    async fn awaiting_of(
        &self,
        pair: &SubscriberTenant,
    ) -> Result<Vec<NotifiedChange>, DomainError> {
        ActivationRepo.awaiting_of(&self.conn()?, pair).await
    }

    async fn stamp(
        &self,
        pair: &SubscriberTenant,
        changes: &[NotifiedChange],
    ) -> Result<u64, DomainError> {
        ActivationRepo
            .stamp(&self.conn()?, pair, changes, now())
            .await
    }

    async fn due(&self, interval: Duration) -> Result<Vec<DueRow>, DomainError> {
        // A state is due when it was never published or last published more than
        // `interval` ago.
        // An interval beyond the clock's range (the configuration bounds it far
        // below) saturates to the epoch: only never-published states are due.
        let cutoff = time::Duration::try_from(interval)
            .ok()
            .and_then(|span| now().checked_sub(span))
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
        ActivationRepo.due(&self.conn()?, cutoff).await
    }

    async fn sweep_orphans(&self) -> Result<u64, DomainError> {
        ActivationRepo.sweep_orphans(&self.conn()?).await
    }

    async fn counts(&self) -> Result<ActivationCounts, DomainError> {
        ActivationRepo.counts(&self.conn()?).await
    }
}
