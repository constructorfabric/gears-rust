//! What a caller can learn about a traced batch after the fact.
//!
//! The in-memory subscription is the normal way to hear about a completion, and
//! it dies with the process that held it. This is the durable fallback: the
//! trace row outlives both the messages it describes and the instance that
//! enqueued them, so a restarted process can still ask what became of work it
//! submitted before it died.

/// What a caller is told when its batch finishes.
///
/// Delivered once, to the instance that enqueued the batch, whichever instance
/// processed the entities.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TraceOutcome {
    /// The trace the caller supplied.
    pub trace: String,
    /// How many entities the batch held.
    pub entities: i64,
    /// How many of them were dead-lettered rather than delivered.
    pub failures: i64,
    /// Handler attempts spent on the batch's final entity.
    pub attempts: i64,
    /// When the last entity reached a terminal state.
    pub completed_at: chrono::DateTime<chrono::Utc>,
}

impl TraceOutcome {
    /// Whether every entity was delivered rather than dead-lettered.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.failures == 0
    }
}

/// Everything a caller can learn about a traced batch while it is in flight, over
/// one channel.
///
/// A subscription carries a single watch of this state. `Retrying` appears only
/// when the batch is *stuck* rather than merely slow - once a handler has retried
/// one of its entities - and reverts to `InFlight` when the batch moves again. A
/// batch that flows straight through is only ever `InFlight` then `Completed`,
/// which is why watching one costs nothing until something goes wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceState {
    /// Moving normally: every entity so far reached a terminal state without a
    /// handler having to retry the one now in flight.
    InFlight,
    /// A handler keeps failing one entity, so the batch is stuck retrying it.
    Retrying {
        /// How many entities the batch held.
        entities: i64,
        /// How many have not yet reached a terminal state.
        pending: i64,
        /// How many reached a terminal state by being dead-lettered.
        failures: i64,
        /// Handler attempts against the entity currently blocking the batch.
        attempts: i64,
        /// Why the blocking entity was last retried.
        last_error: Option<String>,
        /// When the batch first stopped making progress.
        retrying_since: chrono::DateTime<chrono::Utc>,
    },
    /// Every entity reached a terminal state. Terminal: the channel closes after
    /// this, and the durable answer stays available from `Outbox::trace_status`.
    Completed(TraceOutcome),
}

/// What an ack's countdown did to a trace.
///
/// Internal ack-path signal - `Unknown` only exists to encode a dialect that
/// cannot report the outcome without another read, so it is not part of the
/// crate's public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceAdvance {
    /// This advance drove the batch to zero, so it may be claimable.
    Completed,
    /// Entities remain, so there is nothing to claim.
    StillPending,
    /// The guard matched no row: somebody else's ack got there first.
    NotAffected,
    /// The dialect cannot report it without another read, so the caller must
    /// try the claim to find out.
    Unknown,
}

/// The state of one traced batch.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TraceStatus {
    /// The trace the caller supplied.
    pub trace: String,
    /// The queue the batch was enqueued to.
    pub queue: String,
    /// How many entities the batch held.
    pub entities: i64,
    /// How many have not yet reached a terminal state. Zero is completion.
    pub pending: i64,
    /// How many reached a terminal state by being dead-lettered.
    pub failures: i64,
    /// Handler attempts against the entity currently blocking the batch.
    pub attempts: i64,
    /// Why the blocking entity was last retried.
    pub last_error: Option<String>,
    /// When the batch first stopped making progress, cleared when it resumes.
    pub retrying_since: Option<chrono::DateTime<chrono::Utc>>,
    /// When the batch was enqueued.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When every entity had reached a terminal state.
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl TraceStatus {
    /// Whether every entity in the batch has reached a terminal state.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.pending == 0
    }

    /// Whether the batch is stuck retrying an entity rather than merely slow.
    #[must_use]
    pub const fn is_retrying(&self) -> bool {
        self.retrying_since.is_some()
    }
}
