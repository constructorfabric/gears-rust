//! Approval units, review decisions, quorum policy and typed refusals.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

/// The state of one unit; `Approved`, `Rejected` and `Withdrawn` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitState {
    Pending,
    Approved,
    Rejected,
    Withdrawn,
}

impl UnitState {
    /// Returns the stable stored state name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Withdrawn => "withdrawn",
        }
    }
    /// Parses a stored name, returning `None` for an unknown value.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "withdrawn" => Some(Self::Withdrawn),
            _ => None,
        }
    }
}

/// A reviewer's vote on one generation of a unit, stored as `approve` or `reject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approve,
    Reject,
}

impl Verdict {
    /// Returns the stable stored verdict name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
        }
    }
    /// Parses a stored name, returning `None` for an unknown value.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(Self::Approve),
            "reject" => Some(Self::Reject),
            _ => None,
        }
    }
}

/// Quorum policy of a tenant: `'*'` is the default, a kind may override it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub default_quorum: u32,
    pub overrides: BTreeMap<String, u32>,
}

impl Policy {
    /// Returns the kind override or the tenant default quorum.
    #[must_use]
    pub fn quorum_for(&self, kind: &str) -> u32 {
        self.overrides
            .get(kind)
            .copied()
            .unwrap_or(self.default_quorum)
    }
}

/// One element of a unit with the diff the reviewer sees.
///
/// `after` is the **proposed business content only** — never a lock, version,
/// revision or lifecycle column. The fingerprint is computed over it, so a value
/// that the lock or the approval itself changes would make every unit stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRef {
    pub item_type: String,
    pub item_id: Uuid,
    /// Who authored the element; excluded from approving (spec §6).
    pub created_by: Uuid,
    pub before: Option<serde_json::Value>,
    /// Proposed business content only; never lock, version, revision or lifecycle columns.
    pub after: serde_json::Value,
}

/// One approval unit: the reviewed change of one aggregate (`ref_type`, `ref_id`), its state,
/// quorum and generation, the snapshot the reviewers see and the fingerprint of its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub kind: String,
    pub ref_type: String,
    pub ref_id: Uuid,
    pub state: UnitState,
    pub common_effective_date: Option<Date>,
    pub quorum_required: u32,
    /// Bumped on every stale refresh; decisions belong to a generation (spec §2.2).
    pub generation: i32,
    pub submitted_by: Uuid,
    pub submitted_at: OffsetDateTime,
    /// The submitter's own words on the unit, as sent (products P-D-219, pricing D-445): never
    /// content, so neither the snapshot nor its hash carries it, and a stale refresh keeps it.
    pub submit_note: Option<String>,
    pub decided_at: Option<OffsetDateTime>,
    pub decided_note: Option<String>,
    pub snapshot: serde_json::Value,
    pub snapshot_hash: String,
    /// Optimistic concurrency: every write is conditional on this value.
    pub version: i64,
}

/// One reviewer's vote on a unit in one generation; at most one per actor per generation. A
/// vote of an earlier generation is `stale` once the unit is refreshed and no longer counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub unit_id: Uuid,
    pub actor: Uuid,
    pub generation: i32,
    pub verdict: Verdict,
    pub note: Option<String>,
    pub at: OffsetDateTime,
    pub stale: bool,
}

/// The longest note a vote carries, in characters (Unicode scalar values): the engine refuses a
/// longer one with [`ApprovalError::NoteTooLong`] before any write (products P-D-219's limit for
/// a submitter's note).
pub const NOTE_MAX_CHARS: usize = 2000;

/// Every refusal the engine can produce; gears map `code()` to their wire codes.
#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("the actor authored or submitted this unit")]
    SodViolation,
    #[error("the unit is already decided")]
    AlreadyDecided,
    #[error("the actor already voted in this generation")]
    DuplicateVote,
    #[error("another writer changed the unit; retry")]
    Contended,
    #[error("{item_type} {item_id} is locked by another pending unit")]
    Locked { item_type: String, item_id: Uuid },
    #[error("only the submitter may withdraw")]
    NotSubmitter,
    #[error("a reject needs a note")]
    NoteRequired,
    #[error("a note is at most {NOTE_MAX_CHARS} characters")]
    NoteTooLong,
    #[error("unit {unit_id} not found")]
    UnitNotFound { unit_id: Uuid },
    #[error("no items to submit")]
    Empty,
    #[error("submit refused: {code} on {field} — {detail}")]
    InvalidSubmit {
        code: &'static str,
        field: String,
        detail: String,
    },
    #[error("apply refused: {code} — {detail}")]
    ApplyRefused { code: &'static str, detail: String },
    #[error("the vote names generation {seen}, the unit is at {current}")]
    GenerationMismatch { seen: i32, current: i32 },
    /// A database error a gear's retry loop must see typed (serialization failures, `SQLite` lock upgrades).
    #[error("database: {0}")]
    Db(#[from] sea_orm::DbErr),
    #[error("store: {0}")]
    Store(String),
}

impl ApprovalError {
    /// Returns the stable category code that gears map to wire errors.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SodViolation => "SOD_VIOLATION",
            Self::AlreadyDecided => "UNIT_ALREADY_DECIDED",
            Self::DuplicateVote => "DUPLICATE_VOTE",
            Self::Contended => "UNIT_CONTENDED",
            Self::Locked { .. } => "ROW_LOCKED_PENDING",
            Self::NotSubmitter => "NOT_SUBMITTER",
            Self::NoteRequired => "NOTE_REQUIRED",
            Self::NoteTooLong => "NOTE_TOO_LONG",
            Self::UnitNotFound { .. } => "UNIT_NOT_FOUND",
            Self::Empty | Self::InvalidSubmit { .. } => "VALIDATION",
            Self::ApplyRefused { .. } => "APPLY_REFUSED",
            Self::GenerationMismatch { .. } => "GENERATION_MISMATCH",
            Self::Db(_) => "DB",
            Self::Store(_) => "STORE",
        }
    }
}

impl ApprovalError {
    /// Returns the typed database error for the caller's transaction retry classifier.
    #[must_use]
    pub fn db_err(&self) -> Option<&sea_orm::DbErr> {
        if let Self::Db(error) = self {
            Some(error)
        } else {
            None
        }
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;
