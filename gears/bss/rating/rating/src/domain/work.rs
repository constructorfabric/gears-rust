//! The work queues a transaction enqueues into (DESIGN 14 §4.4): `rating.child_work` (partition
//! `hash(subscription_id)`) and `rating.fact_bind` (partition `hash(fact_id)`). A work item is a
//! notification only: its worker re-reads every decision from the rows, so it is enqueued in the
//! transaction that writes its cause and commits with it.

use async_trait::async_trait;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::child::ChildWork;
use crate::domain::error::DomainError;

/// A pending usage fact to bind again on `rating.fact_bind`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FactBindWork {
    /// The seller tenant.
    pub tenant_id: Uuid,
    /// The fact.
    pub fact_id: Uuid,
}

/// What wakes a queue's workers once the enqueueing transaction has committed. Woken earlier, a
/// worker could find nothing committed and miss the items until the outbox's own periodic sweep;
/// so the caller fires it after the commit, and an unfired wakeup only delays the work.
#[must_use = "fire the wakeup once the transaction has committed"]
#[domain_model]
#[derive(Default)]
pub struct Wakeup {
    fires: Vec<Box<dyn FnOnce() + Send>>,
}

impl Wakeup {
    /// A wakeup of `fire`.
    pub fn new(fire: impl FnOnce() + Send + 'static) -> Self {
        Self {
            fires: vec![Box::new(fire)],
        }
    }

    /// Adds `other`'s wakes to this one.
    pub fn merge(&mut self, other: Self) {
        self.fires.extend(other.fires);
    }

    /// Wakes the workers. Call it once the transaction has committed.
    pub fn fire(self) {
        for fire in self.fires {
            fire();
        }
    }
}

impl std::fmt::Debug for Wakeup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wakeup")
            .field("wakes", &self.fires.len())
            .finish()
    }
}

/// Enqueues work items inside the caller's transaction.
#[async_trait]
pub trait WorkQueue: Send + Sync {
    /// Enqueues one `rating.child_work` item per child.
    async fn enqueue_children(
        &self,
        tx: &DbTx<'_>,
        items: &[ChildWork],
    ) -> Result<Wakeup, DomainError>;

    /// Enqueues a `rating.fact_bind` item.
    async fn enqueue_fact_bind(
        &self,
        tx: &DbTx<'_>,
        item: FactBindWork,
    ) -> Result<Wakeup, DomainError>;
}
