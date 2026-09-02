//! The store-owned lease algebra the two cache-backed defaults share
//! (DESIGN.md, ADR-012).
//!
//! A lock and a leader claim are the *same* lease: a [`LeaseRecord`] under the
//! primitive's cache key, taken by insert-or-steal-if-lapsed, held by conditional
//! writes predicated on the [`LeaseToken`] the holder presents, and fenced so a
//! stale holder's predicate can never match again. Both defaults delegate here so
//! there is exactly one implementation of that algebra to reason about — and one
//! place where the CAS races are argued.
//!
//! # What this changes about liveness
//!
//! Expiry used to be *physical*: the lock entry carried the lease TTL as its cache
//! TTL, so the entry vanished when the lease lapsed and a waiter learned of it from
//! the watch's `Expired` event. Now expiry is **logical** — the stored `deadline`
//! is the authority, and the record outlives it by
//! [`FENCE_RETENTION_DEFAULT`](cluster_sdk::lease::FENCE_RETENTION_DEFAULT) so the
//! fence survives the lapse (§5.8.1).
//!
//! That has a consequence worth stating, because it is the one behavioural change
//! a caller can observe: **nothing happens in the store when a lease lapses**, so
//! there is no watch event at the deadline. A waiter that only listened to the
//! watch would sleep past a lease it could have taken. Both defaults therefore
//! wake themselves at the deadline, which
//! [`Acquisition::Contended`]'s `lapse_in` is for.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::cache::{CacheEntry, ClusterCacheBackend};
use cluster_sdk::error::ClusterError;
use cluster_sdk::lease::{FENCE_RETENTION_DEFAULT, LeaseClock, LeaseRecord, LeaseToken};

/// The fence a lease name starts at. Non-zero so that zero stays available as
/// "no lease held" for callers that need to name the absence of a claim.
const FIRST_FENCE: u64 = 1;

/// How many times [`CacheLeaseStore::try_acquire`] retries when the record it is
/// competing for disappears between the insert attempt and the read.
///
/// Each retry is driven by an *observed* race (a concurrent release or a physical
/// reap landing in that window), never by contention — a live lease returns
/// `Contended` on the first pass. Three is enough that a real acquisition is not
/// lost to one unlucky interleaving, and small enough that a pathological store
/// cannot spin the caller.
const ACQUIRE_ATTEMPTS: u32 = 3;

/// What a cache key holds when a lease operation looks at it.
enum Slot {
    /// Nothing there.
    Vacant,
    /// A lease record this build understands.
    Lease {
        record: LeaseRecord,
        /// Carried so a conditional write can guard on the exact version and
        /// bytes that were read.
        entry: CacheEntry,
    },
    /// A value that is not a lease record this build can read — a pre-lease
    /// holder marker, a later encoding revision, or something else entirely.
    ///
    /// Treated as an opaque foreign holder: never stolen, never rewritten, never
    /// deleted. It clears at its own physical TTL, which for every value cluster
    /// has ever written under these keys is bounded by the lease TTL that wrote it.
    Foreign,
}

/// The outcome of an acquisition attempt.
///
/// Not a `Result`, because losing a race for a lease is an ordinary outcome that
/// two callers report differently: the lock turns it into
/// [`ClusterError::LockContended`], the election into "I am a follower".
pub(super) enum Acquisition {
    /// The lease is now this owner's, under the returned token.
    Acquired(LeaseToken),
    /// Someone else holds a live lease.
    Contended {
        /// How long until the incumbent's lease lapses, when that was observable.
        ///
        /// `None` when the competing state was not a readable lease record (a
        /// foreign value, or a CAS lost to another stealer). A caller waiting for
        /// the lease must fall back to its own polling cadence rather than
        /// sleeping indefinitely.
        lapse_in: Option<Duration>,
    },
}

/// The lease operations of §5.8.1 over an `Arc<dyn ClusterCacheBackend>`.
pub(super) struct CacheLeaseStore {
    cache: Arc<dyn ClusterCacheBackend>,
    clock: LeaseClock,
    /// How long a record outlives the lease it fenced, from the cluster gear's
    /// `fence_retention` key (§5.8.1).
    retention: Duration,
    /// Latches the first `ttl >= retention` acquisition so the warning is one per
    /// backend rather than one per acquisition — a hot lock would otherwise emit
    /// it at the acquisition rate, which is how a real signal becomes noise.
    ttl_warned: std::sync::atomic::AtomicBool,
    /// Source of the per-write [`LeaseRecord::nonce`]. A monotonic counter seeded
    /// per store, so every record this store encodes carries a distinct nonce and
    /// two separate writes can never present identical value bytes — the property
    /// the steal's value-guarded CAS depends on (§5.8.1, the version-reset
    /// caveat). Seeded randomly in production so two stores over one backing store
    /// do not produce colliding sequences; a test seeds it explicitly.
    nonce_counter: AtomicU64,
}

impl CacheLeaseStore {
    pub(super) fn new(cache: Arc<dyn ClusterCacheBackend>) -> Self {
        Self::with_retention(cache, FENCE_RETENTION_DEFAULT)
    }

    /// The store with an explicit retention window.
    ///
    /// The window is validated by the caller that read it from config
    /// (`cluster_sdk::lease::validate_fence_retention`), not here: this type is
    /// constructed on the wiring path where an error has somewhere to go, and a
    /// zero window reaching it means the validation was skipped rather than that
    /// the operator asked for one.
    pub(super) fn with_retention(cache: Arc<dyn ClusterCacheBackend>, retention: Duration) -> Self {
        use rand::RngExt as _;
        Self::with_retention_and_nonce_seed(cache, retention, rand::rng().random::<u64>())
    }

    /// The store with an explicit retention window *and* an explicit nonce seed.
    ///
    /// The seed is the starting value of the per-write nonce counter. Injecting
    /// it makes the nonce sequence deterministic for a test — two stores seeded
    /// alike reproduce one sequence (a forced collision), two seeded apart do not
    /// — which is how the nonce's collision-freedom is exercised without reaching
    /// for wall-clock or global RNG in the test path.
    pub(super) fn with_retention_and_nonce_seed(
        cache: Arc<dyn ClusterCacheBackend>,
        retention: Duration,
        nonce_seed: u64,
    ) -> Self {
        Self::with_parts(cache, retention, nonce_seed, LeaseClock::new())
    }

    /// The store with every injectable part explicit, including the
    /// [`LeaseClock`]. The private seam the public constructors and the test-only
    /// clock injector both route through.
    ///
    /// Production always passes [`LeaseClock::new`] (the pure wall clock). A test
    /// that must lapse a lease deadline under [`tokio::time::pause`] passes
    /// [`LeaseClock::virtual_clock`] via [`with_virtual_clock`](Self::with_virtual_clock),
    /// mirroring the `nonce_seed` injection: exactly the same reason — reaching
    /// past global, uncontrollable time in the test path — with the same shape.
    fn with_parts(
        cache: Arc<dyn ClusterCacheBackend>,
        retention: Duration,
        nonce_seed: u64,
        clock: LeaseClock,
    ) -> Self {
        Self {
            cache,
            clock,
            retention,
            ttl_warned: std::sync::atomic::AtomicBool::new(false),
            nonce_counter: AtomicU64::new(nonce_seed),
        }
    }

    /// A store whose lease clock tracks [`tokio::time::pause`]/`advance`, for a
    /// test that fast-forwards a TTL under virtual time (§5.8.1). Production never
    /// calls this — [`new`](Self::new)/[`with_retention`](Self::with_retention) use
    /// the pure wall clock, which cannot diverge across a wall step.
    ///
    /// `pub(crate)` and un-gated (not `#[cfg(test)]`) because the gear's
    /// integration conformance suite (`cluster/tests/conformance.rs`) builds
    /// lease-bearing backends outside the crate's unit-test cfg and must still
    /// lapse leases under `TimeControl::Virtual`; it reaches this through the
    /// backends' `with_virtual_clock` builders. The store type is itself
    /// crate-internal, so this is not public API.
    pub(crate) fn with_virtual_clock(
        cache: Arc<dyn ClusterCacheBackend>,
        retention: Duration,
        nonce_seed: u64,
    ) -> Self {
        Self::with_parts(cache, retention, nonce_seed, LeaseClock::virtual_clock())
    }

    /// The next per-write nonce, distinct from every other this store hands out.
    pub(super) fn next_nonce(&self) -> u64 {
        self.nonce_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// The underlying cache, for the callers that also `watch` it.
    pub(super) fn cache(&self) -> &Arc<dyn ClusterCacheBackend> {
        &self.cache
    }

    /// The fence-retention window this store was built with, so a rebuild (e.g.
    /// the test-only virtual-clock swap) can preserve it.
    pub(crate) fn retention(&self) -> Duration {
        self.retention
    }

    /// The physical TTL a record with lease duration `ttl` is written under: the
    /// lease itself plus the retention window that keeps its fence alive (§5.8.1).
    ///
    /// This is the whole of what the plugins' TTL reapers need to know about
    /// retention, and it is why neither of them changed for item `L3`: both sweep
    /// on the expiry they were *given*, so folding the window into the stored TTL
    /// makes "the reaper must skip records inside the retention window"
    /// structurally true instead of a rule a reaper could get wrong. A native
    /// backend with its own deadline column has no such luck - see the Postgres
    /// lock reaper, which has to subtract the window itself.
    fn physical_ttl(&self, ttl: Duration) -> Ttl {
        Ttl::Of(ttl.saturating_add(self.retention))
    }

    /// Warns once when a lease is taken for longer than the window meant to
    /// outlive it.
    ///
    /// This is §5.8.1's "shorter than the longest lease TTL in use" check, made at
    /// the only point where a lease TTL exists (see
    /// `cluster_sdk::lease::validate_fence_retention` for why it cannot be a
    /// startup check). What it costs when ignored is stated rather than implied:
    /// the fence guarantee narrows from "never reused within `retention` of the
    /// lapse" to "not reused for `retention`", which is shorter than the lease it
    /// is fencing - so a holder that was wedged for longer than the window can
    /// come back with a token that matches again.
    fn warn_if_ttl_exceeds_retention(&self, name: &str, ttl: Duration) {
        if ttl < self.retention
            || self
                .ttl_warned
                .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        tracing::warn!(
            lease.name = name,
            lease.ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX),
            fence_retention_ms = u64::try_from(self.retention.as_millis()).unwrap_or(u64::MAX),
            "lease TTL is at least the fence retention window: a record cannot outlive the lease \
             it fenced, so the fence may be reused while a stale holder's token is still within \
             its own TTL. Raise the cluster gear's fence_retention above the longest lease TTL in \
             use (DESIGN.md). Warned once per backend."
        );
    }

    /// Reads `key` and classifies what is there.
    async fn slot(&self, key: &str) -> Result<Slot, ClusterError> {
        let Some(entry) = self.cache.get(key).await? else {
            return Ok(Slot::Vacant);
        };
        match LeaseRecord::decode(&entry.value) {
            Some(record) => Ok(Slot::Lease { record, entry }),
            None => Ok(Slot::Foreign),
        }
    }

    /// Insert-or-steal-if-lapsed (§5.8.1).
    ///
    /// Three writes cover every case, and which one runs is decided by the store
    /// rather than by a read this caller trusts:
    ///
    /// 1. `put_if_absent` takes a name nobody holds, at [`FIRST_FENCE`].
    /// 2. If something is there and its lease is **live**, this is contention —
    ///    including when the live lease is this same owner's, which makes
    ///    a re-entrant `try_lock` contend as it always has.
    /// 3. If something is there and its lease has **lapsed**, the record is
    ///    CAS'd — not deleted and reinserted — to this owner at `fence + 1`. The
    ///    CAS is what makes the steal safe: it is guarded on the exact bytes that
    ///    were read (never the cache version, which resets to 1 on a
    ///    delete-and-recreate), so two stealers race, one wins, and the loser —
    ///    or any writer that moved the record between the read and the swap —
    ///    sees a conflict rather than silently sharing the lease.
    ///
    /// # Errors
    /// Returns [`ClusterError`] if a cache operation fails. Contention is not an
    /// error — see [`Acquisition`].
    pub(super) async fn try_acquire(
        &self,
        key: &str,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<Acquisition, ClusterError> {
        self.warn_if_ttl_exceeds_retention(name, ttl);
        for _attempt in 0..ACQUIRE_ATTEMPTS {
            let fresh = LeaseRecord {
                owner: owner.to_owned(),
                deadline_ms: self.clock.deadline_after(ttl),
                fence: FIRST_FENCE,
                nonce: self.next_nonce(),
            };
            if self
                .cache
                .put_if_absent(PutRequest {
                    key,
                    value: &fresh.encode(),
                    ttl: self.physical_ttl(ttl),
                })
                .await?
                .is_some()
            {
                return Ok(Acquisition::Acquired(LeaseToken::new(
                    name,
                    owner,
                    FIRST_FENCE,
                )));
            }
            match self.slot(key).await? {
                // The record went away between the insert attempt and the read (a
                // release, or a physical reap of a long-lapsed record). Nothing was
                // observed to be held, so fall through and try to take it again.
                Slot::Vacant => {}
                Slot::Foreign => return Ok(Acquisition::Contended { lapse_in: None }),
                Slot::Lease { record, entry } => {
                    if record.is_live(self.clock.now_millis()) {
                        return Ok(Acquisition::Contended {
                            lapse_in: self.clock.remaining_until(record.deadline_ms),
                        });
                    }
                    let fence = record.fence.saturating_add(1);
                    let stolen = LeaseRecord {
                        owner: owner.to_owned(),
                        deadline_ms: self.clock.deadline_after(ttl),
                        fence,
                        nonce: self.next_nonce(),
                    };
                    // Guard on the exact bytes that were read, not `entry.version`.
                    // The version is not unique across a delete-and-recreate — a
                    // fresh `put_if_absent` resets it to 1 — so a version guard can
                    // match a successor's *live* claim taken between this read and
                    // this write, handing two owners one lease. The value guard,
                    // together with the per-write nonce that keeps two records'
                    // bytes distinct, makes that impossible. Still an in-place CAS,
                    // not a delete-and-reinsert, so the fence is never reset.
                    return match self
                        .cache
                        .compare_and_swap_value(
                            key,
                            &entry.value,
                            &stolen.encode(),
                            self.physical_ttl(ttl),
                        )
                        .await
                    {
                        Ok(_entry) => {
                            Ok(Acquisition::Acquired(LeaseToken::new(name, owner, fence)))
                        }
                        // Another stealer got there first, so its lease is live now.
                        Err(ClusterError::CasConflict { .. }) => {
                            Ok(Acquisition::Contended { lapse_in: None })
                        }
                        Err(err) => Err(err),
                    };
                }
            }
        }
        // Every attempt found the record gone and then lost the re-insert. The
        // name is being fought over hard enough that reporting contention is the
        // honest answer.
        Ok(Acquisition::Contended { lapse_in: None })
    }

    /// Extends the lease `token` is authority over to `ttl` from now.
    ///
    /// The predicate is `(owner, fence, deadline > now)` over the stored record, so
    /// the answer is a property of the record and identical on every replica (I7).
    /// The `deadline` is **reset** to `ttl` from now, not added to what remains —
    /// the cache exposes no remaining-TTL read, so the consumer-facing
    /// method is `renew` and not `extend`.
    ///
    /// # Errors
    /// - [`ClusterError::LockExpired`] when nothing matches: lapsed, stolen, or
    ///   never this owner's. All three are indistinguishable and all three mean
    ///   the caller must stop acting as the holder (§6.9).
    /// - Any other [`ClusterError`] the cache raises.
    pub(super) async fn renew(
        &self,
        key: &str,
        token: &LeaseToken,
        ttl: Duration,
    ) -> Result<(), ClusterError> {
        let Slot::Lease { record, entry } = self.slot(key).await? else {
            return Err(expired(token));
        };
        if !record.matches(token) || !record.is_live(self.clock.now_millis()) {
            return Err(expired(token));
        }
        let renewed = LeaseRecord {
            owner: record.owner,
            deadline_ms: self.clock.deadline_after(ttl),
            fence: record.fence,
            nonce: self.next_nonce(),
        };
        match self
            .cache
            .compare_and_swap(
                key,
                entry.version,
                &renewed.encode(),
                self.physical_ttl(ttl),
            )
            .await
        {
            Ok(_entry) => Ok(()),
            // The record changed under the read: a concurrent steal won, so this
            // holder no longer holds the lease it just proved it held.
            Err(ClusterError::CasConflict { .. }) => Err(expired(token)),
            Err(err) => Err(err),
        }
    }

    /// Releases the lease `token` is authority over.
    ///
    /// Liveness is deliberately **not** part of the predicate: a lapsed record that
    /// still bears this owner and fence is still this holder's, and removing it
    /// frees the name immediately instead of making the next acquirer steal it.
    /// A record the token does not match belongs to a successor and is left alone.
    ///
    /// The delete is guarded on the exact bytes that were read, so a concurrent
    /// steal or renew turns it into a no-op rather than removing state that moved
    /// under it. In the cache-backed defaults renew and release are issued by one
    /// task in sequence, so the only writer that can win that race is a foreign one.
    ///
    /// # Errors
    /// Returns [`ClusterError`] if a cache operation fails. **Nothing to release is
    /// not an error** — absence, a foreign record and a fenced-out token all return
    /// `Ok` (§6.10).
    pub(super) async fn release(&self, key: &str, token: &LeaseToken) -> Result<(), ClusterError> {
        let Slot::Lease { record, entry } = self.slot(key).await? else {
            return Ok(());
        };
        if !record.matches(token) {
            return Ok(());
        }
        let _deleted = self.cache.compare_and_delete(key, &entry.value).await?;
        Ok(())
    }

    /// The record at `key`, for the callers that reconcile against stored state
    /// rather than acting on it (the election's watch-driven path).
    ///
    /// # Errors
    /// Returns [`ClusterError`] if the cache read fails.
    pub(super) async fn read(&self, key: &str) -> Result<Option<LeaseRecord>, ClusterError> {
        Ok(match self.slot(key).await? {
            Slot::Lease { record, .. } => Some(record),
            Slot::Vacant | Slot::Foreign => None,
        })
    }

    /// `true` while `record` has not lapsed on this store's clock.
    pub(super) fn is_live(&self, record: &LeaseRecord) -> bool {
        record.is_live(self.clock.now_millis())
    }

    /// How long until `record` lapses, or `None` if it already has.
    pub(super) fn lapse_in(&self, record: &LeaseRecord) -> Option<Duration> {
        self.clock.remaining_until(record.deadline_ms)
    }
}

/// The one error every failed lease predicate produces, naming the lease the
/// caller thought it held.
fn expired(token: &LeaseToken) -> ClusterError {
    ClusterError::LockExpired {
        name: token.name.clone(),
    }
}

#[cfg(test)]
#[path = "lease_tests.rs"]
mod lease_tests;
