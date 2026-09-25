//! Admission: the synchronous acceptance half (T7) and the request identity it
//! rests on.
//!
//! The split is SPEC §8.1's: **acceptance** runs in the caller's task, reads no
//! entity state, and either refuses synchronously or commits one operation row,
//! its items and an outbox message in a single transaction. **Admission** — the
//! worker — is a separate pass driven by that outbox message.
//!
//! Everything before the transaction is a pure function of the request and the
//! configuration. That is not a style choice: SPEC §8.1's ordering invariant is
//! that the policy gate precedes any existence lookup, so a refusal cannot probe
//! the namespace, and the cheapest way to keep that true is for the refusing code
//! to have no database in scope at all.

pub mod acceptance;
mod batch;
mod bounds;
mod deletion;
mod drift;
pub mod dry_run;
mod errors;
mod reasons;
mod unchanged;

pub mod fingerprint;
mod graph;
mod outcome;
pub mod refresh;
pub mod revision;
mod tuning;
pub mod unit;
pub mod vector;
pub mod worker;

pub use errors::{StoredFailure, UnreadableFailure};
pub use reasons::{AdmissionFailureReason, DeliveryFailure};

use serde_json::Value;
use toolkit_db::DbTx;
use toolkit_db::outbox::Wake;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::enums::{OperationKind, OperationStatus};

/// One candidate in a submitted request.
#[domain_model]
#[derive(Clone, Debug)]
pub struct Candidate {
    /// The identifier as authored. Canonicalized through `GtsId::try_new` during
    /// acceptance; a non-canonical spelling is refused rather than rewritten.
    pub gts_id: String,
    /// The authored document. Absent for a deletion.
    pub content: Option<Value>,
    /// The optimistic precondition. **`None` means must-not-exist**; `Some(0)` is
    /// refused, because the wire vocabulary spells must-not-exist as an absent
    /// field and a literal `0` is more likely a serialization accident than an
    /// intent (`database.sql`).
    pub expected_resource_version: Option<i64>,
    /// ADR-0004 `force`: waive one cross-minor compatibility check.
    pub force: bool,
}

/// The closed optimistic-precondition vocabulary used after acceptance.
///
/// REST spells creation as an absent `expected_resource_version`; storage spells
/// it as `0`. Neither representation crosses the domain pipeline: adapters map
/// them to and from this enum at their respective boundaries.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Precondition {
    MustNotExist,
    Version(i64),
}

impl Precondition {
    /// The stable integer included in the request fingerprint and persisted in
    /// `operation_item.expected_resource_version`.
    #[must_use]
    pub const fn stored_value(self) -> i64 {
        match self {
            Self::MustNotExist => 0,
            Self::Version(version) => version,
        }
    }

    /// The REST response spelling: absence means must-not-exist.
    #[must_use]
    pub const fn expected_resource_version(self) -> Option<i64> {
        match self {
            Self::MustNotExist => None,
            Self::Version(version) => Some(version),
        }
    }

    /// Validate the persisted closed vocabulary.
    #[must_use]
    pub const fn from_stored(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::MustNotExist),
            1.. => Some(Self::Version(value)),
            _ => None,
        }
    }
}

/// A submitted request, before acceptance.
#[domain_model]
#[derive(Clone, Debug)]
pub struct SubmitRequest {
    /// Mandatory. Absence is a synchronous refusal, not a generated key: a
    /// generated one would make every retry a fresh operation.
    ///
    /// `Option` rather than an empty-string sentinel: a transport that has no
    /// key to report says `None` in the type, and the one place that decides
    /// what absence means is [`acceptance::validate`]. After it,
    /// [`Validated::idempotency_key`] is a plain `String`, because by then the
    /// key exists and is non-empty.
    pub idempotency_key: Option<String>,
    pub kind: OperationKind,
    pub dry_run: bool,
    pub candidates: Vec<Candidate>,
}

/// What acceptance decided.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub operation_id: Uuid,
    /// `true` when this request resolved to an operation that already existed
    /// under its `Idempotency-Key` with a matching fingerprint.
    pub replayed: bool,
    /// The operation's status as of this call's return — `pending` for a fresh
    /// acceptance, the stored value for a replay.
    ///
    /// Carried rather than left to the caller to look up: the REST layer needs it for
    /// the receipt, and re-reading the row it has just written cost a second snapshot
    /// transaction plus a `"pending"` fallback for a `None` that cannot happen.
    pub status: OperationStatus,
}

impl Accepted {
    /// `true` when the operation will not change again. The REST layer answers `200`
    /// for a terminal replay and `202` otherwise (SPEC §8.1) — derived from
    /// [`Self::status`] rather than stored beside it, so the two cannot disagree.
    #[must_use]
    pub fn terminal(&self) -> bool {
        self.status == OperationStatus::Completed
    }
}

/// Errors from the admission outbox port: starting the pipeline, binding a
/// started pipeline to the dispatch, or enqueuing within a transaction.
///
/// The port lives in the domain, so its error type does too; the outbox
/// transport ([`OperationDispatch`]'s implementation) maps the underlying
/// `toolkit_db` failure into [`Backend`](Self::Backend).
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    /// The underlying outbox operation failed — starting it, building the record,
    /// or the transactional enqueue.
    #[error("the admission outbox operation failed: {0}")]
    Backend(#[from] toolkit_db::outbox::OutboxError),
    /// A pipeline is already attached to the dispatch.
    #[error("the admission dispatch is already bound to a running pipeline")]
    AlreadyBound,
    /// `enqueue` was called before the pipeline was bound, or after it stopped.
    #[error("the admission outbox is not running")]
    NotRunning,
}

/// How an accepted operation reaches the admission worker.
///
/// A port rather than a direct `Outbox` call, because an `Outbox` only exists
/// after `OutboxBuilder::start()` has spawned its processors — which is precisely
/// what T21 wires and what SPEC §13's *"no test may poll"* rule forbids a test
/// from doing. The transaction shape is the same either way: the message is
/// written by the same transaction as the operation, so a committed operation is
/// always dispatched and a rolled-back one never is.
///
/// The runner is the concrete [`DbTx`] rather than `&impl DBRunner`, so the trait
/// stays object-safe: acceptance always dispatches from inside its transaction,
/// so there is no second executor to be generic over.
#[async_trait::async_trait]
pub trait OperationDispatch: Send + Sync {
    /// Enqueue one operation UUID, returning the [`Wake`] for its rows.
    ///
    /// The payload carries the UUID and nothing else — candidate content must
    /// never enter an outbox or dead-letter payload (SPEC T21).
    ///
    /// The wake must be fired only *after* the acceptance transaction commits;
    /// acceptance holds it across the commit and fires it, or drops it unfired on
    /// rollback. The dispatch parks nothing across the commit boundary.
    ///
    /// # Errors
    /// [`OutboxError`] if the transport cannot enqueue; acceptance turns it into a
    /// refusal and the transaction rolls back, so nothing is half-accepted.
    async fn enqueue(&self, tx: &DbTx<'_>, operation_id: Uuid) -> Result<Wake, OutboxError>;
}
