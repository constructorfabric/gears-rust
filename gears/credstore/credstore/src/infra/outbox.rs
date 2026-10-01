//! Platform transactional outbox wiring (ADR-0006, DESIGN section 6.3).
//!
//! The one asynchronous side effect of the gear is the purge of a deleted
//! record's store key: the delete transaction enqueues `purge {tenant_id,
//! record_id}` next to the row delete, and the handler below calls
//! `plugin.delete_key` and lets the outbox retry on any error (at least once;
//! `delete_key` is idempotent and the key belongs to a record id nothing will
//! reuse). Writes never use the outbox.
//!
//! The gear owns its own table family (prefix [`TABLE_PREFIX`]) so it never
//! shares outbox tables with another gear in the same database.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use credstore_sdk::{StoreKey, TenantId};
use serde::{Deserialize, Serialize};
use toolkit_db::Db;
use toolkit_db::outbox::{
    LeasedMessageHandler, MessageResult, Outbox, OutboxHandle, OutboxMessage, Partitions, Record,
    Wake, outbox_migrations_with_prefix,
};
use toolkit_db::secure::DBRunner;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{CredStoreMetricsPort, Dep, DepOp, Outcome};
use crate::domain::ports::plugin::PluginSelector;

/// Queue carrying key-purge messages.
pub const QUEUE: &str = "credstore.key_purge";
/// Prefix of the gear's own outbox table family.
pub const TABLE_PREFIX: &str = "credstore_outbox";
/// Payload type of a purge message.
pub const PURGE_PAYLOAD_TYPE: &str = "application/vnd.credstore.key-purge.v1+json";
const PARTITIONS: u16 = 4;

/// Outbox migrations for the gear's table family.
///
/// # Errors
/// Never fails for the fixed [`TABLE_PREFIX`]; the `Result` mirrors the
/// platform signature.
pub fn migrations() -> Result<Vec<Box<dyn sea_orm_migration::MigrationTrait>>, DomainError> {
    outbox_migrations_with_prefix(TABLE_PREFIX)
        .map_err(|e| DomainError::internal(format!("credstore outbox migrations: {e}")))
}

/// Wire form of a purge message: the record key, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeMessage {
    pub tenant_id: Uuid,
    pub record_id: Uuid,
}

impl From<&StoreKey> for PurgeMessage {
    fn from(k: &StoreKey) -> Self {
        Self {
            tenant_id: k.tenant_id.0,
            record_id: k.record_id,
        }
    }
}

/// Port the repository uses to enqueue a key purge inside its transaction.
#[async_trait]
pub trait PurgeEnqueuer: Send + Sync {
    /// Enqueues `purge(key)` on `runner` (the caller's open transaction) and
    /// returns the wake to fire after the commit.
    async fn enqueue_purge(
        &self,
        runner: &(dyn DBRunner + Sync),
        key: &StoreKey,
    ) -> Result<Wake, DomainError>;
}

/// [`PurgeEnqueuer`] over the platform outbox. The `Outbox` handle is set
/// once the pipeline starts (`serve`); enqueue before that is an internal
/// error (HTTP traffic only arrives after the gear reports ready).
#[derive(Default)]
pub struct OutboxPurgeEnqueuer {
    outbox: OnceLock<Arc<Outbox>>,
}

impl OutboxPurgeEnqueuer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs the started outbox. A second call is ignored.
    pub fn set_outbox(&self, outbox: Arc<Outbox>) {
        drop(self.outbox.set(outbox));
    }
}

fn partition_for(key: &StoreKey) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        (key.record_id.as_u128() % u128::from(PARTITIONS)) as u32
    }
}

#[async_trait]
impl PurgeEnqueuer for OutboxPurgeEnqueuer {
    async fn enqueue_purge(
        &self,
        runner: &(dyn DBRunner + Sync),
        key: &StoreKey,
    ) -> Result<Wake, DomainError> {
        let outbox = self
            .outbox
            .get()
            .ok_or_else(|| DomainError::internal("credstore outbox is not started"))?;
        let payload = serde_json::to_vec(&PurgeMessage::from(key))
            .map_err(|e| DomainError::internal(format!("serialize purge message: {e}")))?;
        let record = Record::to(QUEUE, partition_for(key))
            .payload(payload, PURGE_PAYLOAD_TYPE)
            .build()
            .map_err(|e| DomainError::internal(format!("build purge message: {e}")))?;
        outbox
            .enqueue(runner, record)
            .await
            .map_err(|e| DomainError::internal(format!("enqueue purge message: {e}")))
    }
}

/// Outbox handler: `plugin.delete_key(key)`, retried by the outbox on error.
pub struct PurgeHandler {
    plugins: Arc<dyn PluginSelector>,
    metrics: Arc<dyn CredStoreMetricsPort>,
}

impl PurgeHandler {
    #[must_use]
    pub fn new(plugins: Arc<dyn PluginSelector>, metrics: Arc<dyn CredStoreMetricsPort>) -> Self {
        Self { plugins, metrics }
    }

    fn service_ctx() -> Option<SecurityContext> {
        SecurityContext::builder()
            .subject_id(Uuid::nil())
            .subject_tenant_id(Uuid::nil())
            .build()
            .ok()
    }

    fn failed(&self) -> MessageResult {
        self.metrics.outbox_purge_failed();
        MessageResult::Retry
    }
}

#[async_trait]
impl LeasedMessageHandler for PurgeHandler {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        let purge: PurgeMessage = match serde_json::from_slice(&msg.payload) {
            Ok(p) => p,
            Err(e) => return MessageResult::Reject(format!("bad purge payload: {e}")),
        };
        let key = StoreKey::new(TenantId(purge.tenant_id), purge.record_id);
        let Some(ctx) = Self::service_ctx() else {
            tracing::error!("credstore outbox: cannot build the service context");
            return self.failed();
        };
        let plugin = match self.plugins.resolve().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(err = %e, "credstore outbox: no plugin for key purge; will retry");
                return self.failed();
            }
        };
        let t0 = std::time::Instant::now();
        let result = plugin.delete_key(&ctx, &key).await;
        let secs = t0.elapsed().as_secs_f64();
        let outcome = if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Error
        };
        self.metrics
            .dependency(Dep::Plugin, DepOp::PluginDeleteKey, outcome, secs);
        match result {
            Ok(()) => MessageResult::Ok,
            Err(e) => {
                // The plugin's error text is not logged: it is not curated
                // for the credstore boundary.
                tracing::warn!(
                    kind = error_kind(&e),
                    attempts = msg.attempts,
                    "credstore outbox: delete_key failed; will retry"
                );
                self.failed()
            }
        }
    }
}

fn error_kind(e: &credstore_sdk::CredStoreError) -> &'static str {
    if e.is_unavailable() {
        "unavailable"
    } else {
        "error"
    }
}

/// Builds and starts the gear's outbox pipeline and installs it into the
/// enqueuer.
///
/// # Errors
/// Returns the outbox start-up error text.
pub async fn start(
    db: Db,
    enqueuer: &OutboxPurgeEnqueuer,
    handler: PurgeHandler,
) -> anyhow::Result<OutboxHandle> {
    let handle = Outbox::builder(db)
        .table_prefix(TABLE_PREFIX)
        .map_err(|e| anyhow::anyhow!("credstore outbox prefix: {e}"))?
        .queue(QUEUE, Partitions::of(PARTITIONS))
        .leased(handler)
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("credstore outbox start: {e}"))?;
    enqueuer.set_outbox(Arc::clone(handle.outbox()));
    Ok(handle)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "outbox_tests.rs"]
mod outbox_tests;
