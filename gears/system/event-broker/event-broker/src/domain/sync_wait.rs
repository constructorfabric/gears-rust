//! The synchronous-publish wait seam (`DESIGN.md:1335`, feature 0007 §4.1).
//!
//! A `Prefer: wait=N` publish holds until the backend stores its events. Ingest
//! mints a trace (a UUID, never on the wire), subscribes to it *before* the
//! enqueue commits, and enqueues the admitted rows under that trace in the same
//! transaction. The outbox's own trace machinery
//! (`toolkit_db::outbox::Outbox::subscribe`) counts the batch down as each row
//! is acked and delivers the completion to the instance that submitted it -
//! in-process when that instance drains the rows itself, through the outbox
//! notifier when another instance does. Single-instance and cluster run the
//! identical path, and `trace_status` is the durable fallback, so nothing here
//! reimplements cross-instance delivery.
//!
//! This module is just the domain seam over that machinery: [`SyncWaitRegistry`]
//! registers interest in a trace, and the [`SyncWait`] it returns is awaited
//! once the enqueue has committed. The infra implementation lives on
//! `infra::storage::Storage`, which holds the `Arc<Outbox>`.

use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

/// Registers interest in a synchronous-publish trace, before its outbox rows
/// are enqueued.
///
/// A completion cannot precede the enqueue's commit, so subscribing first means
/// nothing can be missed; the returned [`SyncWait`] is awaited after the commit.
/// Implemented by `infra::storage::Storage` over
/// [`Outbox::subscribe`](toolkit_db::outbox::Outbox::subscribe).
pub trait SyncWaitRegistry: Send + Sync {
    /// Subscribe to `trace` before its rows are enqueued. `trace` must be unique
    /// per publish (a fresh UUID), the trace machinery's own requirement.
    fn subscribe(&self, trace: Uuid) -> Box<dyn SyncWait>;
}

/// A live subscription to one sync-publish trace, awaited after the enqueue has
/// committed.
#[async_trait]
pub trait SyncWait: Send {
    /// Hold until every traced row has been stored (`true`), or `wait` elapses
    /// or this instance can no longer answer (`false` - the publish gets the
    /// honest `202`). `wait` is already clamped to the broker's configured
    /// ceiling by the caller.
    async fn stored(self: Box<Self>, wait: Duration) -> bool;
}
