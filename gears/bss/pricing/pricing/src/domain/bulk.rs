//! The bulk run's vocabulary — what kind of run it is and where it stands
//! (`design/12-operator-efficiency.md` §4, §6; D-260, D-267).
//!
//! # The states are here and the **edges are not**
//!
//! `pricing_bulk_operation`'s trigger is the state machine, on both engines, and
//! the entity says so: "the edges between them are the table's trigger, not a
//! caller's convention". A `may_move_to` on [`BulkState`] would be a second
//! spelling of a rule the store already enforces, and the two would drift the
//! first time a state is added — which has happened once already, `rejected`
//! arriving four migrations after the table.
//!
//! So this module carries the tokens and nothing else: a caller names a state,
//! the store decides whether the move is legal, and the refusal it raises is the
//! one message an operator sees.
//!
//! # An import can never be `awaiting_approval`
//!
//! A `CHECK` says so (`NOT (kind = 'import' AND state = 'awaiting_approval')`),
//! because D-137 makes a bulk import never material: D-118 pinned it to the draft
//! plane, and materiality is evaluated at a *publish* against a published
//! baseline. The state exists for mass repricing, whose rows are published.

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// Which bulk flow a run is.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BulkKind {
    /// §4's two-phase price import — draft-plane authoring (D-118).
    Import,
    /// §4's mass repricing run — published rows, through D-88's supersession
    /// units.
    Repricing,
}

impl BulkKind {
    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Repricing => "repricing",
        }
    }

    /// Read a stored token back.
    ///
    /// # Errors
    /// [`DomainError::InvalidRequest`] for a token no `CHECK` should have
    /// admitted — read at the boundary so a value the store holds and this crate
    /// does not never travels into a rule.
    pub fn parse(token: &str) -> Result<Self, DomainError> {
        match token {
            "import" => Ok(Self::Import),
            "repricing" => Ok(Self::Repricing),
            other => Err(DomainError::InvalidRequest(format!(
                "bulk operation kind: `{other}` is not `import` or `repricing`"
            ))),
        }
    }
}

/// How far one **selected row** of a repricing run got (`inst-mr-journal`).
///
/// Three states and not four: `not-attempted` is how a *report* renders a row the
/// journal still holds as `pending`, never a stored value (D-261). An aborted run
/// therefore leaves `pending` rows standing, which is what lets a re-drive tell
/// "never reached" from "decided".
///
/// **That holds for an ordinary failure and stops holding for the two the
/// apply's own code cannot see coming.** `infra::repricing::apply_run_in`'s
/// tail preserves it exactly for a plain storage `Err`: the run stays
/// `committing`, its unreached rows stay `pending`, and a second call retakes
/// the lock and carries on. A panic or a dropped future is a different case —
/// no code is left running to make that redrive-preserving choice at all —
/// and there `infra::repricing::RunLockGuard`'s `Drop` fallback marks every
/// straggler `failed` instead, so the run can still reach a terminal state
/// rather than sit `committing` forever with its lock held by nothing. The
/// distinction between "never reached" and "decided" survives that case only
/// in the row's free-text `failure_reason`, not in its `state`.
///
/// The edges and the finality of the two decided states are the table's trigger,
/// not this type's: a `match` here listing them would be the second spelling
/// [`BulkState`]'s own doc refuses for the same reason.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JournalState {
    /// Born here, at the run's row-set expansion.
    Pending,
    /// The apply committed a successor for this row, and the journal names it.
    Applied,
    /// This row did not apply — its own refusal, or the plan-level one it shares
    /// with every other row of its plan (D-134).
    Failed,
}

impl JournalState {
    /// The persisted token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Failed => "failed",
        }
    }

    /// A decided row never moves again, which the table's trigger enforces and
    /// this answers for a caller that has to choose whether to re-drive it.
    #[must_use]
    pub const fn is_decided(self) -> bool {
        matches!(self, Self::Applied | Self::Failed)
    }

    /// Read a stored token back.
    ///
    /// # Errors
    /// [`DomainError::InvalidRequest`] for a token no `CHECK` should have
    /// admitted, at the boundary, for [`BulkKind::parse`]'s reason.
    pub fn parse(token: &str) -> Result<Self, DomainError> {
        match token {
            "pending" => Ok(Self::Pending),
            "applied" => Ok(Self::Applied),
            "failed" => Ok(Self::Failed),
            other => Err(DomainError::InvalidRequest(format!(
                "repricing journal state: `{other}` is not `pending`, `applied` or `failed`"
            ))),
        }
    }
}

/// Where a run stands in §4's state machine.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BulkState {
    /// Born here. Phase 1 is running.
    Validating,
    /// Phase 1 refused the batch; terminal.
    ValidationFailed,
    /// A repricing run waiting on its batch approval. **Never an import**
    /// (D-137).
    AwaitingApproval,
    /// Phase 2 is running and the run holds its row locks.
    Committing,
    /// Every row committed; terminal.
    Completed,
    /// Phase 2 finished with at least one row conflicted; terminal, and a
    /// *success* — `inst-bk-phase2`'s "committed rows stand".
    CompletedWithConflicts,
    /// The batch approval was refused; terminal, and reachable only from
    /// [`BulkState::AwaitingApproval`] (D-267 — before it a rejected run was
    /// stranded there with no exit).
    Rejected,
}

impl BulkState {
    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Validating => "validating",
            Self::ValidationFailed => "validation_failed",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Committing => "committing",
            Self::Completed => "completed",
            Self::CompletedWithConflicts => "completed_with_conflicts",
            Self::Rejected => "rejected",
        }
    }

    /// Whether the run is over.
    ///
    /// The four the `CHECK` pairs with `completed_at IS NOT NULL`, which is what
    /// makes this a reading of the schema rather than a second opinion about it.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::ValidationFailed
                | Self::Completed
                | Self::CompletedWithConflicts
                | Self::Rejected
        )
    }

    /// Read a stored token back.
    ///
    /// # Errors
    /// [`DomainError::InvalidRequest`] for a token no `CHECK` should have
    /// admitted.
    pub fn parse(token: &str) -> Result<Self, DomainError> {
        match token {
            "validating" => Ok(Self::Validating),
            "validation_failed" => Ok(Self::ValidationFailed),
            "awaiting_approval" => Ok(Self::AwaitingApproval),
            "committing" => Ok(Self::Committing),
            "completed" => Ok(Self::Completed),
            "completed_with_conflicts" => Ok(Self::CompletedWithConflicts),
            "rejected" => Ok(Self::Rejected),
            other => Err(DomainError::InvalidRequest(format!(
                "bulk operation state: `{other}` is not one of the seven §4 states"
            ))),
        }
    }
}

#[cfg(test)]
#[path = "bulk_tests.rs"]
mod bulk_tests;
