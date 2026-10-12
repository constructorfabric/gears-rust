//! Pure quorum and separation-of-duties rules for the current generation.

use crate::model::{ApprovalError, Decision, ItemRef, Unit, UnitState, Verdict};
use uuid::Uuid;

/// Outcome of an eligible approve vote, as computed by [`evaluate_approve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApproveStep {
    /// The quorum is not met yet; the vote pends.
    NeedMore {
        /// Counted approvals, this vote included.
        have: u32,
        /// Approvals the quorum requires.
        need: u32,
    },
    /// The vote meets the quorum; the unit's change applies.
    Apply,
}

/// Did `actor` already vote in the unit's **current** generation?
#[must_use]
pub fn already_voted(unit: &Unit, decisions: &[Decision], actor: Uuid) -> bool {
    decisions
        .iter()
        .any(|d| d.actor == actor && d.generation == unit.generation)
}

/// The approve votes the quorum counts on `unit`: of its current generation and not stale. A
/// reject, a stale vote and a vote of an earlier generation do not count. It depends on the unit
/// and its decisions only, so a reader that shows the count reads no item and names no actor.
#[must_use]
pub fn counted_approvals(unit: &Unit, decisions: &[Decision]) -> u32 {
    let counted = decisions
        .iter()
        .filter(|d| d.verdict == Verdict::Approve && !d.stale && d.generation == unit.generation)
        .count();
    u32::try_from(counted).unwrap_or(u32::MAX)
}

/// Why an actor may not approve a unit now: the three refusals of the engine's approve that
/// [`approve_eligibility`] judges. Each converts into the [`ApprovalError`] the engine answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApproveRefusal {
    /// The unit is decided: [`ApprovalError::AlreadyDecided`].
    AlreadyDecided,
    /// The actor submitted the unit or authored one of its items: [`ApprovalError::SodViolation`].
    SodViolation,
    /// The actor already voted in the current generation: [`ApprovalError::DuplicateVote`].
    DuplicateVote,
}
impl From<ApproveRefusal> for ApprovalError {
    fn from(refusal: ApproveRefusal) -> Self {
        match refusal {
            ApproveRefusal::AlreadyDecided => Self::AlreadyDecided,
            ApproveRefusal::SodViolation => Self::SodViolation,
            ApproveRefusal::DuplicateVote => Self::DuplicateVote,
        }
    }
}

/// Whether one actor may approve a unit, and the votes the quorum counts: the approve eligibility
/// of [`approve_eligibility`], the one rule the engine's approve and every reader judge by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApproveEligibility {
    /// [`counted_approvals`]: the approve votes of the unit's current generation that are not
    /// stale.
    pub approvals: u32,
    /// `None` when the actor may approve; else the refusal the engine answers its approve with,
    /// in this order: [`ApproveRefusal::AlreadyDecided`] for a terminal unit,
    /// [`ApproveRefusal::SodViolation`] for its submitter or an item author,
    /// [`ApproveRefusal::DuplicateVote`] for an actor who already voted in the current generation.
    pub refusal: Option<ApproveRefusal>,
}

/// The approve eligibility of `actor` on `unit`, from the unit, the authors of its stored items
/// (the current generation's: a stale refresh rewrites them; only who authored an item is
/// judged, so a reader may read the authors alone) and its decisions of every generation. Pure,
/// and the engine's own rule: [`crate::Engine::approve`] judges through it once it has loaded the
/// unit at the reviewer's generation, so a reader that shows whether its reader may approve
/// cannot drift from the vote door.
#[must_use]
pub fn approve_eligibility(
    unit: &Unit,
    authors: impl IntoIterator<Item = Uuid>,
    decisions: &[Decision],
    actor: Uuid,
) -> ApproveEligibility {
    let refusal = if unit.state != UnitState::Pending {
        Some(ApproveRefusal::AlreadyDecided)
    } else if actor == unit.submitted_by || authors.into_iter().any(|author| author == actor) {
        Some(ApproveRefusal::SodViolation)
    } else if already_voted(unit, decisions, actor) {
        Some(ApproveRefusal::DuplicateVote)
    } else {
        None
    };
    ApproveEligibility {
        approvals: counted_approvals(unit, decisions),
        refusal,
    }
}

/// What an approve vote by `actor` does, given the votes cast so far: [`approve_eligibility`]'s
/// refusal, else the vote pends or applies. Pure: the caller has loaded the unit, its stored
/// items and its decisions inside the transaction. Decisions must belong to this unit, with at
/// most one per actor per generation.
///
/// # Errors
/// Returns [`ApprovalError::AlreadyDecided`] for a terminal unit,
/// [`ApprovalError::SodViolation`] for its submitter or an item author, or
/// [`ApprovalError::DuplicateVote`] for an actor who already voted in this generation.
pub fn evaluate_approve(
    unit: &Unit,
    decisions: &[Decision],
    actor: Uuid,
    items: &[ItemRef],
) -> Result<ApproveStep, ApprovalError> {
    let judged = approve_eligibility(unit, items.iter().map(|i| i.created_by), decisions, actor);
    if let Some(refusal) = judged.refusal {
        return Err(refusal.into());
    }
    let have = judged.approvals.saturating_add(1);
    if have >= unit.quorum_required {
        Ok(ApproveStep::Apply)
    } else {
        Ok(ApproveStep::NeedMore {
            have,
            need: unit.quorum_required,
        })
    }
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod rules_tests;
