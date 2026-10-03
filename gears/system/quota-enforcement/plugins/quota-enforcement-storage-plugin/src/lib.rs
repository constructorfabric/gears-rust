//! Quota Enforcement storage plugin: foundation and Quota tables.
//!
//! Schema-version check, the three configuration tables with idempotent
//! seeding, the Quota tables with the four lifecycle primitives and the two
//! platform-plane reads, the enqueue-only notification outbox, and the
//! migrations that create them. See the crate README for why no
//! `QuotaEnforcementStoragePluginV1` client is published yet.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod test_support;

pub use domain::{
    Actor, ConsumptionStore, FoundationStore, QuotaStore, SeedReport, StoragePlugin, StoreError,
};
pub use gear::StoragePluginGear;
#[cfg(any(test, feature = "test-util"))]
pub use infra::outbox::start_undelivered_outbox;
pub use infra::outbox::{
    NOTIFICATION_PARTITIONS, NOTIFICATION_QUEUE, NotificationEnqueuer, NotificationOutbox,
    QeOutbox, SqlNotificationPipeline, start_notification_pipeline,
};
pub use infra::storage::{
    OUTBOX_TABLE_PREFIX, SqlConsumptionStore, SqlFoundationStore, SqlPolicyStore, SqlQuotaStore,
};
