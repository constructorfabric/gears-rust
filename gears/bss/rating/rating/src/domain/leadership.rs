//! Advisory leadership (feature 01 §3.4, ADR-0008, DESIGN §3.8).
//!
//! Some work has one active worker: the reader of each usage feed source, the scheduler and wake
//! scanner per shard, the recovery sweeps, the reconciler, the replay verifier and the periodic
//! jobs. Rating takes that leadership through the [`LeaseProvider`] port, backed by the Cluster
//! `LeaderElectionV1` facade scoped `bss-rating` (`infra::cluster`).
//!
//! Leadership is **advisory**: it saves duplicate work and nothing more. Correctness comes from
//! conditional database writes — cursor CAS, state CAS, unique keys — so a stale leader's write
//! fails. No lease fence is stored or compared (Cluster ADR-012), and no non-cluster I/O runs
//! under a lock guard (Cluster ADR-002): feed reads, Pricing calls and database transactions run
//! as the leader, never inside a `DistributedLockV1` critical section.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The scope every Rating leadership name is composed under (`bss-rating/<leaf>`).
pub const LEADERSHIP_SCOPE: &str = "bss-rating";

/// A Rating leadership (ADR-0008 "Names").
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeadershipName {
    /// The single active reader of a usage feed source, by the UUID of its canonical `source_id`
    /// (without the `usage:` prefix: the Cluster SDK's leaf rule refuses `:`).
    UsageFeed(Uuid),
    /// The window scheduler and wake scanner of one shard.
    Scheduler(u16),
    /// The recovery sweeps of one tenant shard.
    Recovery(u16),
    /// The reconciler of one tenant shard.
    Reconcile(u16),
    /// The replay verifier of one tenant shard.
    Verify(u16),
    /// The exception sweeper (every 15 minutes).
    ExceptionSweep,
    /// The daily retention purge.
    Purge,
}

impl LeadershipName {
    /// The leaf name passed to `elect` after `.scoped("bss-rating")`.
    #[must_use]
    pub fn leaf(&self) -> String {
        match self {
            Self::UsageFeed(source) => format!("usage-feed-{}", source.as_hyphenated()),
            Self::Scheduler(shard) => format!("scheduler-{shard}"),
            Self::Recovery(shard) => format!("recovery-{shard}"),
            Self::Reconcile(shard) => format!("reconcile-{shard}"),
            Self::Verify(shard) => format!("verify-{shard}"),
            Self::ExceptionSweep => "exception-sweep".to_owned(),
            Self::Purge => "purge".to_owned(),
        }
    }

    /// The composed name the Cluster backend sees.
    #[must_use]
    pub fn scoped(&self) -> String {
        format!("{LEADERSHIP_SCOPE}/{}", self.leaf())
    }
}

impl fmt::Display for LeadershipName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.scoped())
    }
}

/// The future of one leadership term's work.
pub type LeaderWorkFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// The work run while this instance leads. It receives a token that is cancelled when the
/// leadership is lost or the gear stops, and must stop after its current transaction.
pub type LeaderWork = Arc<dyn Fn(CancellationToken) -> LeaderWorkFuture + Send + Sync>;

/// Rating-local leadership port (ADR-0008). Backed by the Cluster `LeaderElectionV1` facade; a
/// transitional `bss-coord` adapter may sit behind it only if R-27 approves one.
#[async_trait]
pub trait LeaseProvider: Send + Sync {
    /// Joins the election `name` and runs `work` during every term this instance leads, until
    /// `shutdown` fires: the work starts on election and is cancelled on loss.
    ///
    /// # Errors
    ///
    /// [`DomainError::Coordination`] when the election cannot be joined or closes.
    async fn run_while_leader(
        &self,
        name: LeadershipName,
        shutdown: CancellationToken,
        work: LeaderWork,
    ) -> Result<(), DomainError>;
}

#[cfg(test)]
#[path = "leadership_tests.rs"]
mod tests;
