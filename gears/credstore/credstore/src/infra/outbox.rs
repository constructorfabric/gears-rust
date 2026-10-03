//! Platform transactional outbox wiring (ADR-0006, DESIGN section 6.3).
//!
//! The gear's one asynchronous side effect is the cleanup of the value
//! store: a transaction that learns some store content is dead enqueues a
//! task on the one queue [`QUEUE`], next to the row change (or write-intent
//! deletion) that made it known, and the handler below executes it. No
//! best-effort store call is ever relied on for cleanup.
//!
//! * `purge {tenant_id, record_id}` — `plugin.delete_key(key)`: the key
//!   will never hold a live value (record deleted, or a write's fresh key
//!   lost its row).
//! * `destroy {tenant_id, record_id, selector, version}` —
//!   `plugin.destroy(key, selector)` where the plugin declares
//!   `supports_destroy`, acknowledged without a call where it does not.
//!
//! Both are idempotent (a destroy on a purged key is success), so the outbox
//! may deliver them at least once; a transient error is retried, a malformed
//! payload is rejected. Every message of a key goes to the key's partition,
//! so tasks of one key run in order.
//!
//! The gear owns its own table family (prefix [`TABLE_PREFIX`]) so it never
//! shares outbox tables with another gear in the same database.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use credstore_sdk::{CredStoreError, DestroySelector, StoreKey, TenantId, ValueVersion};
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
use crate::domain::ports::metrics::{CleanupOp, CredStoreMetricsPort, Dep, DepOp, Outcome};
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::secret::model::CleanupTask;

/// Queue carrying every store-cleanup message (purge and destroy).
pub const QUEUE: &str = "credstore.store_cleanup";
/// Prefix of the gear's own outbox table family.
pub const TABLE_PREFIX: &str = "credstore_outbox";
/// Payload type of a purge message.
pub const PURGE_PAYLOAD_TYPE: &str = "application/vnd.credstore.key-purge.v1+json";
/// Payload type of a destroy message.
pub const DESTROY_PAYLOAD_TYPE: &str = "application/vnd.credstore.version-destroy.v1+json";
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

/// Which versions a destroy message removes (wire form of
/// [`DestroySelector`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DestroySelectorKind {
    /// Every version older than `version`.
    Below,
    /// Exactly `version`.
    Exactly,
}

/// Wire form of a destroy message: the record key, which versions, and the
/// provider's opaque version identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestroyMessage {
    pub tenant_id: Uuid,
    pub record_id: Uuid,
    pub selector: DestroySelectorKind,
    pub version: String,
}

impl DestroyMessage {
    fn new(k: &StoreKey, selector: &DestroySelector) -> Self {
        let (selector, version) = match selector {
            DestroySelector::Below(v) => (DestroySelectorKind::Below, v),
            DestroySelector::Exactly(v) => (DestroySelectorKind::Exactly, v),
        };
        Self {
            tenant_id: k.tenant_id.0,
            record_id: k.record_id,
            selector,
            version: version.0.clone(),
        }
    }

    fn into_parts(self) -> (StoreKey, DestroySelector) {
        let version = ValueVersion::new(self.version);
        let selector = match self.selector {
            DestroySelectorKind::Below => DestroySelector::Below(version),
            DestroySelectorKind::Exactly => DestroySelector::Exactly(version),
        };
        (
            StoreKey::new(TenantId(self.tenant_id), self.record_id),
            selector,
        )
    }
}

/// Port the repository uses to enqueue a store-cleanup task inside its
/// transaction.
#[async_trait]
pub trait CleanupEnqueuer: Send + Sync {
    /// Enqueues `task` on `runner` (the caller's open transaction) and
    /// returns the wake to fire after the commit.
    async fn enqueue(
        &self,
        runner: &(dyn DBRunner + Sync),
        task: &CleanupTask,
    ) -> Result<Wake, DomainError>;
}

/// [`CleanupEnqueuer`] over the platform outbox. The `Outbox` handle is set
/// once the pipeline starts (`serve`); enqueue before that is an internal
/// error (HTTP traffic only arrives after the gear reports ready).
#[derive(Default)]
pub struct OutboxCleanupEnqueuer {
    outbox: OnceLock<Arc<Outbox>>,
}

impl OutboxCleanupEnqueuer {
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
impl CleanupEnqueuer for OutboxCleanupEnqueuer {
    async fn enqueue(
        &self,
        runner: &(dyn DBRunner + Sync),
        task: &CleanupTask,
    ) -> Result<Wake, DomainError> {
        let outbox = self
            .outbox
            .get()
            .ok_or_else(|| DomainError::internal("credstore outbox is not started"))?;
        let (payload, payload_type) = match task {
            CleanupTask::Purge(key) => (
                serde_json::to_vec(&PurgeMessage::from(key)),
                PURGE_PAYLOAD_TYPE,
            ),
            CleanupTask::Destroy { key, selector } => (
                serde_json::to_vec(&DestroyMessage::new(key, selector)),
                DESTROY_PAYLOAD_TYPE,
            ),
        };
        let payload = payload
            .map_err(|e| DomainError::internal(format!("serialize cleanup message: {e}")))?;
        // One partition per key: every task of a key is delivered in order.
        let record = Record::to(QUEUE, partition_for(task.key()))
            .payload(payload, payload_type)
            .build()
            .map_err(|e| DomainError::internal(format!("build cleanup message: {e}")))?;
        outbox
            .enqueue(runner, record)
            .await
            .map_err(|e| DomainError::internal(format!("enqueue cleanup message: {e}")))
    }
}

/// Outbox handler: executes one cleanup task against the plugin, retried by
/// the outbox on any error.
pub struct CleanupHandler {
    plugins: Arc<dyn PluginSelector>,
    metrics: Arc<dyn CredStoreMetricsPort>,
}

impl CleanupHandler {
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

    fn failed(&self, op: CleanupOp) -> MessageResult {
        self.metrics.store_cleanup_failed(op);
        MessageResult::Retry
    }

    /// Records the plugin call as a dependency and maps its result.
    fn finish(
        &self,
        op: CleanupOp,
        dep_op: DepOp,
        result: Result<(), CredStoreError>,
        secs: f64,
        attempts: i16,
    ) -> MessageResult {
        let outcome = if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Error
        };
        self.metrics.dependency(Dep::Plugin, dep_op, outcome, secs);
        match result {
            Ok(()) => MessageResult::Ok,
            Err(e) => {
                // The plugin's error text is not logged: it is not curated
                // for the credstore boundary.
                tracing::warn!(
                    op = op.as_str(),
                    kind = error_kind(&e),
                    attempts,
                    "credstore outbox: store cleanup failed; will retry"
                );
                self.failed(op)
            }
        }
    }

    async fn run(&self, task: CleanupTask, attempts: i16) -> MessageResult {
        let op = task.op();
        let Some(ctx) = Self::service_ctx() else {
            tracing::error!("credstore outbox: cannot build the service context");
            return self.failed(op);
        };
        let plugin = match self.plugins.resolve().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    op = op.as_str(),
                    err = %e,
                    "credstore outbox: no plugin for store cleanup; will retry"
                );
                return self.failed(op);
            }
        };
        match task {
            CleanupTask::Purge(key) => {
                let t0 = std::time::Instant::now();
                let result = plugin.delete_key(&ctx, &key).await;
                self.finish(
                    op,
                    DepOp::PluginDeleteKey,
                    result,
                    t0.elapsed().as_secs_f64(),
                    attempts,
                )
            }
            CleanupTask::Destroy { key, selector } => {
                // Enqueued only for destroy-capable plugins, but the plugin
                // can change between enqueue and delivery: a plugin without
                // `destroy` is acknowledged without a call.
                if !plugin.supports_destroy() {
                    return MessageResult::Ok;
                }
                let t0 = std::time::Instant::now();
                let result = plugin.destroy(&ctx, &key, selector).await;
                self.finish(
                    op,
                    DepOp::PluginDestroy,
                    result,
                    t0.elapsed().as_secs_f64(),
                    attempts,
                )
            }
        }
    }
}

#[async_trait]
impl LeasedMessageHandler for CleanupHandler {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        let task = match msg.payload_type.as_str() {
            PURGE_PAYLOAD_TYPE => match serde_json::from_slice::<PurgeMessage>(&msg.payload) {
                Ok(p) => CleanupTask::Purge(StoreKey::new(TenantId(p.tenant_id), p.record_id)),
                Err(e) => return MessageResult::Reject(format!("bad purge payload: {e}")),
            },
            DESTROY_PAYLOAD_TYPE => match serde_json::from_slice::<DestroyMessage>(&msg.payload) {
                Ok(d) => {
                    let (key, selector) = d.into_parts();
                    CleanupTask::Destroy { key, selector }
                }
                Err(e) => return MessageResult::Reject(format!("bad destroy payload: {e}")),
            },
            other => {
                return MessageResult::Reject(format!("unknown cleanup payload type: {other}"));
            }
        };
        self.run(task, msg.attempts).await
    }
}

fn error_kind(e: &CredStoreError) -> &'static str {
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
    enqueuer: &OutboxCleanupEnqueuer,
    handler: CleanupHandler,
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
