//! Store-owned leases — the record, the token, and the clock they are compared
//! against (DESIGN.md, ADR-012).
//!
//! **A lease is a record in the backing store, fenced by a token the client
//! presents.** No process vouches for it, so no process's death ends one and any
//! replica serves any lease operation (invariant I7). That reduces `renew`,
//! `release` and `resign` to *conditional writes predicated on state the store
//! already holds*:
//!
//! | Element | Definition |
//! |---|---|
//! | [`LeaseRecord`] | What the store holds: `owner`, `deadline`, `fence` |
//! | [`LeaseToken`] | What the client holds and presents: `(name, owner, fence)`. It is the whole of the authority — there is no lookup table behind it |
//! | Liveness | The stored `deadline`. A holder that stops renewing lapses; nothing has to notice it is gone |
//!
//! # Why the fence lives in the value
//!
//! `CacheEntry::version` is monotonic per key *while the key exists*, and a TTL
//! reap deletes the key — the standalone plugin then writes `version: 1` on the
//! next insert. A lease that lapsed, was reaped and was re-acquired by the **same**
//! owner would hand the old token a matching predicate again. So the fence is a
//! field of the record, the record is CAS'd rather than deleted-and-reinserted on
//! a steal, and the record's *physical* expiry is `deadline + fence_retention`
//! ([`FENCE_RETENTION_DEFAULT`]) so the counter outlives the lease it fenced.
//!
//! The guarantee that buys, stated exactly: **a fence value is never reused for a
//! given lease name within `fence_retention` of that lease lapsing.** It is not
//! global monotonicity, so the fence is not exposed on
//! [`LockGuard`](crate::lock::LockGuard) — promising a third-party resource a
//! monotonic token needs a source that can honour it across the retention window
//! (§5.8.1).
//!
//! # This encoding is not the wire
//!
//! [`LeaseRecord::encode`] is how the *cache-backed default backends* store a
//! lease in an opaque cache value. It is internal to cluster: invariant I12
//! governs `cluster.v1` and `proto.lock.toml`, not this. What it shares with the
//! wire is the discipline — see [`LeaseRecord::encode`] for the versioning rule a
//! change here must follow, because two cluster replicas at different versions do
//! read one another's records.
//!
//! A native backend that has columns (the Postgres lock's `owner` / `expires_at` /
//! `fence`) stores the same three fields natively and never touches this codec.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a lease record outlives the lease it fenced, so the `fence` counter
/// survives a lapse (§5.8.1).
///
/// An hour — orders of magnitude above any plausible lease TTL, and cheap: one
/// small record per lease *name*, not per acquisition. Operator-overridable
/// through the cluster gear's `fence_retention` key, which reaches the two
/// cache-backed defaults; a native backend holding its own fence takes its own
/// option (see [`validate_fence_retention`]).
pub const FENCE_RETENTION_DEFAULT: Duration = Duration::from_hours(1);

/// Rejects a fence-retention window that would defeat the point of having one.
///
/// # What this can and cannot check
///
/// §5.8.1 asks for more than this: *"the backends refuse to start"* when
/// `fence_retention` is shorter than the longest configured lease TTL. **There is
/// no configured lease TTL anywhere in the tree to compare against** — a TTL is a
/// per-call argument to `lock(name, ttl)` and to a leader-election claim, so the
/// longest one in use is not knowable until it is used, and by then a startup
/// check has long returned.
///
/// So the check is split, and the halves land where the data actually is:
///
/// - **Here, at startup**: zero is rejected. A zero window is not a short
///   retention, it is the absence of one — the record's physical expiry collapses
///   back onto the lease deadline and the fence resets on the next reap, which is
///   the precise defect §5.8.1 exists to close.
/// - **At acquisition**, in the backend: a lease taken with `ttl >= retention`
///   warns, naming both durations. That is the real form of "shorter than the
///   longest lease TTL", checked against a TTL that exists rather than one that
///   was configured. It warns rather than failing because denying coordination
///   service is a worse outcome than a narrowed fence guarantee, and because the
///   caller passing the TTL is not the operator who set the window.
///
/// # Errors
/// [`ClusterError::InvalidConfig`](crate::error::ClusterError::InvalidConfig)
/// naming the key when `retention` is zero.
pub fn validate_fence_retention(retention: Duration) -> Result<(), crate::error::ClusterError> {
    if retention.is_zero() {
        return Err(crate::error::ClusterError::InvalidConfig {
            reason: "fence_retention must be greater than zero: it is how long a lease record \
                     outlives the lease it fenced, so a zero window lets the fence counter reset \
                     on the next reap and a stale holder's token match a lease it no longer holds"
                .to_owned(),
        });
    }
    Ok(())
}

/// The whole authority for a lease-keyed operation (§5.8.1).
///
/// Held by the client, presented on `renew` / `release` / `resign`, and turned
/// into a row predicate by whichever replica serves the call. Opaque above the
/// backend seam: no facade surfaces it, and in particular
/// [`LockGuard`](crate::lock::LockGuard) cannot carry one — its fields are
/// private and its only constructor is
/// [`LockGuard::channel`](crate::lock::LockGuard::channel), so the token lives in
/// the guard task's closure instead (§6.5).
///
/// # Two `LeaseToken` types, deliberately
///
/// This is the SDK-native, **serde-free** form, and it is the one the
/// plugin-facing backend traits name. [`dto::LeaseToken`](crate::dto::LeaseToken)
/// is its wire mirror, carrying the `Serialize`/`JsonSchema` derives the contract
/// projection needs. Keeping them apart is what stops
/// `cpt-cf-clst-constraint-no-serde` — and the `*Api`/`*Backend` split that
/// invariant I11 rests on — from leaking serde into every plugin, and it follows
/// the same convention as [`CacheFeatures`](crate::cache::CacheFeatures) versus
/// [`WireCacheFeatures`](crate::dto::WireCacheFeatures). `From` impls in both
/// directions live beside the DTO.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LeaseToken {
    /// Lock name or election name — the lease's identity within the profile.
    /// Unprefixed: the name the consumer used, not the backend's cache key.
    pub name: String,
    /// The holder's identity. Two holders never share one.
    pub owner: String,
    /// Bumped on every acquisition of `name`, including a steal-on-expiry, so a
    /// stale holder's predicate can never match again.
    pub fence: u64,
    /// When this claim's most recently written lease deadline falls, in the
    /// renewal task's own [`tokio::time::Instant`] domain — **not** on the wire
    /// and **not** part of the token's identity (see [`matches`] below and the
    /// hand-written equality note).
    ///
    /// This is the deadline authority (§7.3, invariant I8): a renewal task holds
    /// leadership only while `now < deadline`, independent of how few renewals it
    /// has missed, so a half-open feed that neither answers nor errors cannot keep
    /// a claim past one TTL. It is armed on every successful acquire/renew and is
    /// `None` on a token that has not yet been used to hold a live claim (a
    /// follower's zero token, a token minted by [`new`](Self::new), or one
    /// reconstructed from its wire mirror — the wire carries no deadline because
    /// the deadline is a replica-local liveness bound, not a shared record field
    /// like [`LeaseRecord::deadline_ms`]).
    ///
    /// [`matches`]: LeaseRecord::matches
    pub deadline: Option<tokio::time::Instant>,
}

impl LeaseToken {
    /// Mints the token for a fresh acquisition, with no deadline armed yet — the
    /// renewal task arms it once the claim is confirmed held.
    #[must_use]
    pub fn new(name: impl Into<String>, owner: impl Into<String>, fence: u64) -> Self {
        Self {
            name: name.into(),
            owner: owner.into(),
            fence,
            deadline: None,
        }
    }

    /// Arms the claim's deadline to `deadline`, returning the token so it can be
    /// stored in one move (the renewal task calls this on every successful
    /// acquire/renew).
    #[must_use]
    pub fn with_deadline(mut self, deadline: tokio::time::Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Whether this claim's armed deadline has passed at `now`. A token with no
    /// armed deadline is never expired — it is not holding a live claim.
    #[must_use]
    pub fn is_expired(&self, now: tokio::time::Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
}

/// The lease as the store holds it: who owns it, when it lapses, and the fence
/// that outlives it (§5.8.1).
///
/// The cache-backed defaults serialise this into the cache value under the
/// primitive's key; a native backend holds the same three fields in columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRecord {
    /// The holder's identity, matched against [`LeaseToken::owner`].
    pub owner: String,
    /// When the lease lapses, in milliseconds since the Unix epoch on the
    /// **server** clock ([`LeaseClock`]). Absolute rather than a duration so the
    /// replica evaluating it needs no memory of the one that wrote it.
    pub deadline_ms: u64,
    /// The fence, matched against [`LeaseToken::fence`].
    pub fence: u64,
    /// A per-write value that makes two *distinct* records for one lease name
    /// impossible to confuse by their bytes alone.
    ///
    /// It is **not** part of the [`matches`](Self::matches) predicate — the
    /// owner and fence decide authority. It exists solely so a value-guarded
    /// conditional write (a steal `compare_and_swap` guarded on the exact bytes
    /// that were read) cannot be aliased by a *different* record that happens to
    /// share owner, fence and deadline. `CacheEntry::version` resets to `1` on a
    /// delete-and-recreate, so a version guard could match a successor's fresh
    /// claim; guarding on the bytes closes that, and this nonce guarantees the
    /// bytes of two separate writes are never equal even when every other field
    /// coincides. The writer (`CacheLeaseStore`) stamps a fresh value on every
    /// record it encodes.
    pub nonce: u64,
}

/// The first byte of every encoded [`LeaseRecord`], and the length of the fixed
/// header before the owner.
const MAGIC: &[u8; 4] = b"CLSL";
/// The encoding revision this build writes and the only one it reads.
///
/// Bumped `1` → `2` when the per-write [`nonce`](LeaseRecord::nonce) was added.
/// A reader of a v1 record gets `None` from [`LeaseRecord::decode`] and treats
/// it as an opaque foreign record, which is the accepted no-migration behaviour
/// (this encoding is not live in any production store).
const VERSION: u8 = 2;
/// `MAGIC` + version + `fence` + `deadline_ms` + `nonce`.
const HEADER_LEN: usize = 4 + 1 + 8 + 8 + 8;

impl LeaseRecord {
    /// `true` while the lease has not lapsed. The comparison is strict, so a
    /// record whose deadline is exactly `now_ms` is already lapsed.
    #[must_use]
    pub fn is_live(&self, now_ms: u64) -> bool {
        self.deadline_ms > now_ms
    }

    /// `true` when `token` is authority over *this* record — the predicate every
    /// `renew` / `release` / `resign` is conditioned on, alongside
    /// [`is_live`](Self::is_live) where the operation requires a live lease.
    ///
    /// Both fields matter: `owner` keeps one holder from operating on another's
    /// lease, and `fence` keeps a *previous* holder from operating on the lease
    /// that superseded its own.
    #[must_use]
    pub fn matches(&self, token: &LeaseToken) -> bool {
        self.owner == token.owner && self.fence == token.fence
    }

    /// Serialises the record into an opaque cache value.
    ///
    /// # Layout
    ///
    /// | Bytes | Field |
    /// |---|---|
    /// | `0..4` | magic `CLSL` |
    /// | `4` | version (`2`) |
    /// | `5..13` | `fence`, `u64` big-endian |
    /// | `13..21` | `deadline_ms`, `u64` big-endian |
    /// | `21..29` | `nonce`, `u64` big-endian |
    /// | `29..` | `owner`, UTF-8, to the end of the value |
    ///
    /// Big-endian because these values are read by operators out of hex dumps and
    /// `psql` far more often than they are read fast.
    ///
    /// # Changing this encoding
    ///
    /// The owner is last precisely so `encode` cannot fail on a long owner, which
    /// means a new field costs a **version bump**: bytes appended after the owner
    /// would be indistinguishable from it. A reader of an unrecognised version
    /// gets `None` from [`decode`](Self::decode) and treats the record as a
    /// foreign holder's — safe, but it means a mixed-version fleet stops sharing
    /// leases for one lease name until the older readers are gone. Roll readers
    /// before writers.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.owner.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&self.fence.to_be_bytes());
        out.extend_from_slice(&self.deadline_ms.to_be_bytes());
        out.extend_from_slice(&self.nonce.to_be_bytes());
        out.extend_from_slice(self.owner.as_bytes());
        out
    }

    /// Parses a cache value written by [`encode`](Self::encode).
    ///
    /// `None` means "not a lease record this build understands" — too short, wrong
    /// magic, an unrecognised version, or a non-UTF-8 owner. Every one of those is
    /// a value cluster did not write (or wrote at a later version), so callers
    /// treat `None` as an **opaque foreign record**: they neither steal it nor
    /// rewrite it, and it clears at its own physical TTL. Deleting a value we
    /// cannot read would be the one unrecoverable choice.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER_LEN || &bytes[0..4] != MAGIC || bytes[4] != VERSION {
            return None;
        }
        // Both slices are exactly 8 bytes by construction of `HEADER_LEN`, so the
        // array conversions cannot fail.
        let fence = u64::from_be_bytes(bytes[5..13].try_into().ok()?);
        let deadline_ms = u64::from_be_bytes(bytes[13..21].try_into().ok()?);
        let nonce = u64::from_be_bytes(bytes[21..HEADER_LEN].try_into().ok()?);
        let owner = std::str::from_utf8(&bytes[HEADER_LEN..]).ok()?.to_owned();
        Some(Self {
            owner,
            deadline_ms,
            fence,
            nonce,
        })
    }
}

/// The clock a lease `deadline` is written and compared against.
///
/// # Production is the wall clock, and only the wall clock
///
/// A lease `deadline` has to be *shared*: the replica serving a `renew` compares
/// it without having seen the acquire, so it must be wall-clock-absolute rather
/// than a process-local [`Instant`](std::time::Instant).
///
/// [`LeaseClock::new`] (and [`Default`]) is the production clock —
/// [`LeaseClock::System`]. [`now_millis`](Self::now_millis) reads
/// [`SystemTime::now`] fresh on every call, with **no anchor and no monotonic
/// component**. That is the fix, not an incidental detail: a clock that keeps no
/// per-process anchor has no state a wall-clock step can bias, so two replicas
/// can never disagree about lease time across a `chronyc makestep`, a VM snapshot
/// restore or a live migration. They agree to within their NTP skew, which is the
/// bound §5.8.1 already accepts.
///
/// # Why a virtual mode exists — and why it is test-only
///
/// Cluster's time-sensitive tests — and the conformance suite's
/// [`TimeControl::Virtual`] — fast-forward TTLs with [`tokio::time::pause`] +
/// `advance`, which moves tokio's clock and leaves `SystemTime` exactly where it
/// was. A lease keyed to the wall clock alone would never lapse under virtual
/// time, and every TTL scenario would have to become a real-time sleep.
///
/// So [`LeaseClock::virtual_clock`] builds a **test-injected** clock —
/// [`LeaseClock::Virtual`] — whose [`now_millis`](Self::now_millis) is the wall
/// clock **plus whatever virtual time the runtime has fast-forwarded past real
/// time** (see [`hybrid_now_millis`]):
///
/// ```text
/// virtual_extra = (tokio_now - anchor_instant) saturating- (wall_now - anchor_wall)
/// now_millis    = wall_now + virtual_extra
/// ```
///
/// Under a paused runtime no real time passes, so `virtual_extra` is the amount
/// advanced and a TTL deadline lapses as the test drives the clock forward.
///
/// **This mode is a hybrid of wall and monotonic time and must never reach
/// production.** A backward wall step of Δ shrinks `wall_elapsed` by Δ, so
/// `virtual_extra` becomes Δ and *stays there* — the clock reports the pre-step
/// timeline forever, while a clock anchored after the step reports the corrected
/// one, and the two disagree by Δ permanently. When Δ exceeds a TTL, the biased
/// replica sees every peer's lease as lapsed and steals it, breaking the "identical
/// on every replica" property invariant I7 rests on. That is precisely why the
/// production clock is pure wall: the hybrid is injected only through
/// [`virtual_clock`](Self::virtual_clock), never by [`new`](Self::new), so a
/// production `LeaseClock` cannot be a hybrid by construction.
///
/// [`TimeControl::Virtual`]: https://docs.rs/cf-gears-cluster-conformance
#[derive(Debug)]
pub enum LeaseClock {
    /// The production clock: the wall clock, read fresh each call, with no
    /// anchor and no virtual component. Built by [`new`](Self::new)/[`Default`].
    System,
    /// A **test-injected** wall/monotonic hybrid that tracks
    /// [`tokio::time::pause`]/`advance`, so TTL deadlines lapse under virtual
    /// time. Built only by [`virtual_clock`](Self::virtual_clock); never
    /// constructed on a production path (see the type docs for the divergence it
    /// causes across a backward wall step).
    Virtual {
        /// Wall-clock milliseconds when this clock was created.
        anchor_wall_ms: u64,
        /// The runtime clock's reading at the same moment.
        anchor: tokio::time::Instant,
    },
}

impl LeaseClock {
    /// The production clock: pure wall clock, no anchor, no virtual component.
    #[must_use]
    pub fn new() -> Self {
        Self::System
    }

    /// A **test-injected** clock that tracks [`tokio::time::pause`]/`advance` so
    /// TTL deadlines lapse under virtual time (see the type docs). Anchored to the
    /// current wall and runtime readings.
    ///
    /// Test-only. It is a wall/monotonic hybrid that diverges permanently across a
    /// backward wall step, so it must never be wired into a production store: the
    /// production constructors ([`new`](Self::new)/[`Default`]) never build it, and
    /// the cache-backed lease store injects it only through a test constructor.
    #[must_use]
    pub fn virtual_clock() -> Self {
        Self::Virtual {
            anchor_wall_ms: wall_millis(),
            anchor: tokio::time::Instant::now(),
        }
    }

    /// The current lease time in milliseconds since the Unix epoch.
    #[must_use]
    pub fn now_millis(&self) -> u64 {
        match self {
            Self::System => wall_millis(),
            Self::Virtual {
                anchor_wall_ms,
                anchor,
            } => {
                let runtime_elapsed = tokio::time::Instant::now()
                    .saturating_duration_since(*anchor)
                    .as_millis();
                hybrid_now_millis(wall_millis(), *anchor_wall_ms, runtime_elapsed)
            }
        }
    }

    /// The absolute deadline `ttl` from now, saturating rather than wrapping so an
    /// absurd TTL yields a deadline that never lapses instead of one already past.
    #[must_use]
    pub fn deadline_after(&self, ttl: Duration) -> u64 {
        self.now_millis()
            .saturating_add(u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX))
    }

    /// How long until `deadline_ms`, or `None` if it has already passed.
    #[must_use]
    pub fn remaining_until(&self, deadline_ms: u64) -> Option<Duration> {
        deadline_ms
            .checked_sub(self.now_millis())
            .filter(|remaining| *remaining > 0)
            .map(Duration::from_millis)
    }
}

impl Default for LeaseClock {
    fn default() -> Self {
        Self::new()
    }
}

/// The virtual-clock arithmetic: the wall clock plus whatever virtual time the
/// runtime has advanced past real time (see [`LeaseClock::Virtual`]).
///
/// Extracted as a pure function so the property that motivates the whole split —
/// that this arithmetic diverges permanently across a backward wall step while the
/// production wall clock does not — is testable without a controllable
/// `SystemTime`. It is only ever fed real readings by [`LeaseClock::now_millis`]
/// in the [`Virtual`](LeaseClock::Virtual) arm.
fn hybrid_now_millis(wall_now_ms: u64, anchor_wall_ms: u64, runtime_elapsed_ms: u128) -> u64 {
    let wall_elapsed = u128::from(wall_now_ms.saturating_sub(anchor_wall_ms));
    let virtual_extra = runtime_elapsed_ms.saturating_sub(wall_elapsed);
    wall_now_ms.saturating_add(u64::try_from(virtual_extra).unwrap_or(u64::MAX))
}

/// Wall-clock milliseconds since the Unix epoch, saturating for the pre-epoch and
/// post-`u64` clocks that cannot occur on a host running this code.
fn wall_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
#[path = "lease_tests.rs"]
mod lease_tests;
