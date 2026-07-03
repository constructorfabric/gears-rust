//! SDK default backends — the "implement cache only, get all four primitives"
//! guarantee (DESIGN §3.11, ADR-001, ADR-009).
//!
//! Three backends, each built on `Arc<dyn ClusterCacheBackend>`, derive the
//! remaining coordination primitives from cache operations so a plugin author
//! who implements only the cache obtains working leader election, distributed
//! lock, and service discovery:
//!
//! - [`CasBasedLeaderElectionBackend`] — compare-and-swap leadership over
//!   `put_if_absent` + `compare_and_swap` + `watch`, with TTL-bounded renewal.
//! - [`CasBasedDistributedLockBackend`] — TTL-bounded mutual exclusion over
//!   `put_if_absent` + conditional release, with TTL reaping of a crashed
//!   holder.
//! - [`CacheBasedServiceDiscoveryBackend`] — set-membership discovery over
//!   per-instance keys with a heartbeat TTL and a `watch_prefix` topology
//!   stream.
//!
//! # Consistency safety (ADR-009)
//!
//! The two consistency-sensitive backends (leader election, lock) expose a
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
//! Service discovery has a single infallible constructor: transient staleness
//! is acceptable for set-membership semantics, so it imposes no consistency
//! guard (ADR-009 §"Service-discovery backend does NOT follow the same rule").
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
//! before shutdown completes (`cpt-cf-clst-fr-shutdown-revoke`). All three
//! default backends therefore carry a [`tokio_util::sync::CancellationToken`]
//! and implement [`ShutdownRevoke`]; the wiring cancels each one:
//!
//! - leader election — the in-flight election tasks latch `Status(Lost)` then
//!   `Closed(Shutdown)` and exit (the wiring awaits those tasks);
//! - lock — an in-flight blocking `lock()` waiter returns `Err(Shutdown)` (no
//!   spawned task to await, since the waiter runs in the caller's future);
//! - service discovery — the in-flight watch translators send a terminal
//!   `Closed(Shutdown)` and exit (the wiring awaits those tasks).
//!
//! No remote release is performed: held claims, locks, and registrations lapse
//! via TTL per `cpt-cf-clst-fr-shutdown-ttl-cleanup`.

use async_trait::async_trait;

mod guard;
mod identity;

pub mod discovery;
pub mod leader;
pub mod lock;

#[cfg(test)]
mod test_cache;

#[cfg(test)]
mod observability_tests;

pub use discovery::CacheBasedServiceDiscoveryBackend;
pub use leader::CasBasedLeaderElectionBackend;
pub use lock::CasBasedDistributedLockBackend;

/// Cache-key namespace prefixes for the three default backends (ADR-001).
///
/// In an omit-primitive profile the wiring clones one cache `Arc` into all four
/// defaults, so they share a keyspace. Each default builds its keys from exactly
/// one of these prefixes, which is what keeps the per-primitive keyspaces from
/// overlapping: a service named `election` lands under `svc/election/...` and
/// can never collide with a leader claim at `election/...`.
pub(crate) const SVC_KEY_PREFIX: &str = "svc/";
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
