//! SDK default backends — the "implement cache only, get all three primitives"
//! guarantee (DESIGN §3.11, ADR-001, ADR-009).
//!
//! Two backends, each built on `Arc<dyn ClusterCacheBackend>`, derive the
//! remaining coordination primitives from cache operations so a plugin author
//! who implements only the cache obtains working leader election and a
//! distributed lock:
//!
//! - [`CasBasedLeaderElectionBackend`] — compare-and-swap leadership over
//!   `put_if_absent` + `compare_and_swap` + `watch`, with TTL-bounded renewal.
//! - [`CasBasedDistributedLockBackend`] — TTL-bounded mutual exclusion over
//!   `put_if_absent` + conditional release, with a crashed holder's lock lapsing
//!   at its deadline.
//!
//! # Both are store-owned leases (DESIGN.md, ADR-012)
//!
//! A held lock and a leader claim are the same thing: a
//! [`LeaseRecord`](cluster_sdk::lease::LeaseRecord) — `{ owner, deadline, fence }` —
//! under the primitive's cache key, held by conditional writes predicated on the
//! [`LeaseToken`](cluster_sdk::lease::LeaseToken) the holder presents. Nothing
//! about a lease lives in the process that issued it, so any replica of the cluster
//! gear serves any lease operation and no process's death ends another's lease
//! (invariant I7). The algebra lives in one place, [`lease`], and both defaults
//! delegate to it.
//!
//! Expiry is therefore **logical**: the stored `deadline` decides, and the record
//! outlives it by `fence_retention` so the fence that keeps a stale holder out
//! survives the lapse. One consequence is worth knowing before reading either
//! backend — a lapsing lease writes nothing, so no watch event announces it, and
//! both defaults schedule their own wake-up at the incumbent's deadline instead.
//!
//! # Consistency safety (ADR-009)
//!
//! Both backends (leader election, lock) are consistency-sensitive and expose a
//! **constructor pair** implementing
//! `cpt-cf-clst-algo-sdk-default-backends-constructor-guard`:
//!
//! - `new(cache)` is default-safe: it returns
//!   [`ClusterError::InvalidConfig`](cluster_sdk::error::ClusterError::InvalidConfig)
//!   when the cache declares
//!   [`CacheConsistency::EventuallyConsistent`](cluster_sdk::cache::CacheConsistency),
//!   because their correctness depends on linearizable CAS.
//! - `new_allow_weak_consistency(cache)` always succeeds and emits a
//!   `tracing::warn!` acknowledging the split-brain risk, for the deployments
//!   that intentionally accept it (ADR-009 §"Why opt-in exists").
//!
//! # Background-task lifecycle
//!
//! None of the consumer handles/watches perform I/O on `Drop`. Each backend
//! drives its renewal / heartbeat / waiter logic from a background task that
//! self-terminates by **channel closure**: when the consumer drops the watch or
//! handle, the task observes the closed command/event channel (its `recv`
//! yields `None`, or a `send` fails), makes a best-effort claim release where
//! applicable, and exits.
//!
//! # Graceful-shutdown revocation (DESIGN §3.13)
//!
//! Channel closure covers *consumer-initiated* teardown. *Cluster-initiated*
//! graceful shutdown is the other direction: when the gear host stops the
//! cluster, every active coordination handle must observe a terminal shutdown
//! before shutdown completes (`cpt-cf-clst-fr-shutdown-revoke`). Both default
//! backends therefore carry a [`tokio_util::sync::CancellationToken`]
//! and implement [`ShutdownRevoke`]; the wiring cancels each one:
//!
//! - leader election — the in-flight election tasks latch `Status(Lost)` then
//!   `Closed(Shutdown)` and exit (the wiring awaits those tasks);
//! - lock — an in-flight blocking `lock()` waiter returns `Err(Shutdown)` (no
//!   spawned task to await, since the waiter runs in the caller's future).
//!
//! No remote release is performed: held claims and locks lapse at their stored
//! deadline per `cpt-cf-clst-fr-shutdown-ttl-cleanup`.

use async_trait::async_trait;

mod guard;
mod identity;
mod lease;

pub mod leader;
pub mod lock;

#[cfg(test)]
pub(crate) mod test_cache;

#[cfg(test)]
mod observability_tests;

pub use leader::CasBasedLeaderElectionBackend;
pub use lock::CasBasedDistributedLockBackend;

/// Cache-key namespace prefixes for the two default backends (ADR-001).
///
/// In an omit-primitive profile the wiring clones one cache `Arc` into both
/// defaults, so they share a keyspace. Each default builds its keys from exactly
/// one of these prefixes, which is what keeps the per-primitive keyspaces from
/// overlapping: a lock named `election` lands under `lock/election/...` and can
/// never collide with a leader claim at `election/...`.
///
/// These separate the two *defaults* from each other. What separates both from
/// consumer cache data is a different mechanism one layer down: the wiring hands
/// them a [`reserved_lease_cache`](cluster_sdk::reserved_lease_cache) view, so
/// the keys below compose under a prefix no cache request can name. Without it
/// these prefixes sat in the keyspace the cache API serves, and a caller could
/// read, forge, or reset a lease through `Cache.Get`/`Put`/`Delete`.
pub(crate) const ELECTION_KEY_PREFIX: &str = "election/";
pub(crate) const LOCK_KEY_PREFIX: &str = "lock/";

/// SDK-internal seam letting the cluster wiring revoke a default backend's
/// in-flight coordination during graceful shutdown (DESIGN §3.13).
///
/// This is **not** part of the plugin contract (`nfr-plugin-stability`): only the
/// wiring-owned SDK default backends implement it, and the wiring holds the
/// concrete handles it needs to call [`revoke`](ShutdownRevoke::revoke). Native
/// plugin backends manage their own shutdown through their plugin stop hook.
#[async_trait]
pub trait ShutdownRevoke: Send + Sync {
    /// Signals every in-flight task to surface a terminal shutdown and awaits
    /// their completion, so the caller knows revocation has finished (an active
    /// leader has observed `Status(Lost)`) before shutdown proceeds.
    async fn revoke(&self);
}
