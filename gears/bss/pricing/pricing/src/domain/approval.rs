//! The approval record's state machine — `cpt-cf-bss-pricing-state-approval`
//! (`design/05-governance.md` §4).
//!
//! [`ApprovalState::transition`] is the single source of truth for what is
//! legal, in the shape [`crate::domain::lifecycle::LifecycleState`] already has
//! and for the same reason: the `pricing_approval` trigger and the REST surface
//! both restate the machine in their own vocabulary, and if the legal edge set
//! lived in three places the one that drifted would be discovered as an approval
//! that decided twice.
//!
//! Legal edges, and nothing else:
//!
//! ```text
//! submitted -> approved     (inst-as-approve)
//! submitted -> rejected     (inst-as-reject)
//! submitted -> voided       (inst-as-void)
//! ```
//!
//! Everything else is refused, including every self-edge and every move out of
//! a decided state (`inst-as-immutable`). There is no re-open and no re-submit
//! **of a record**: §4 says a re-submit opens a *new* one, which is what keeps
//! `content_hash` a pin rather than a field.
//!
//! # The function is total, and it returns a reason rather than a bool
//!
//! A predicate answering `false` makes every refused edge the same event, and
//! two of the edges here are not the same event at all. Deciding an
//! already-decided or voided record is `APPROVAL_NOT_PENDING` (§5, 409) — a
//! caller raced another decision, or is acting on a stale list. Asking a
//! `submitted` record to stay `submitted` is a caller bug no surface can commit.
//! [`TransitionRefusal`] keeps them apart, and [`TransitionRefusal::code`]
//! answers `None` for the second.
//!
//! **`None` is deliberate and is not an omission.** The approve, reject and
//! withdraw routes each name a fixed outcome, so no request can ask for the
//! self-edge; minting a code for it would document an API a client cannot
//! provoke, which is the argument D-146 makes about the pin frontier's
//! regression and the reason that one carries no code either.
//! [`ApprovalDecision`] makes the same point in the type system — the outcome a
//! surface can ask for is one of three, and the self-edge is unrepresentable on
//! that path.
//!
//! **The argument holds only because the repository takes the decision.** With
//! [`approval_repo::decide`](crate::infra::storage::repo::approval_repo::decide)
//! taking a bare [`ApprovalState`] instead and folding every refusal into
//! `APPROVAL_NOT_PENDING`, the one path into the store can ask a pending record to
//! stay pending and be answered "approval X is submitted; only a submitted record
//! is decidable" — a 409 contradicting itself, for a refusal this module argues no
//! client can provoke. That would make the sentence above a fact about an unused
//! type. `approval_tests::no_decision_is_ever_refused_as_not_an_outcome` ranges
//! over the whole state x decision product and is what keeps it a fact about the
//! code.
//!
//! # What this file carries, and what its siblings do
//!
//! This file is the state machine and nothing else: it needs only the record's
//! own state, while the two-person rule's `submitter != approver` comparison,
//! the content pin, the approver's scope check and the materiality evaluator
//! each need a *subject* — a `PlanShape`, an authz claim set, a change set. A
//! state machine that reached for a plan would have to be re-stated by every
//! other subject `subject_kind` admits.
//!
//! They land beside it, as this module's earlier note said they would:
//!
//! - [`content_pin`] — `inst-ap-pin`'s digest over the submitted plan revision.
//! - [`decision`] — the whole decision-refusal vocabulary ([`DecisionRefusal`])
//!   and the pure judgement [`authorize_decision`], covering the two-person
//!   rule, the approver's scope, the mandatory reason and the pin's
//!   re-verification.
//!
//! Both are re-exported here, so `domain::approval::content_hash` and
//! `domain::approval::DecisionRefusal` are the names a surface reaches for. The
//! four wire codes `decision.rs` declares are re-exported beside them for the
//! same reason and one more: [`APPROVAL_NOT_PENDING`] already lives here, and a
//! consumer-visible vocabulary split across two paths is one a reader has to
//! know is split.
//!
//! The materiality evaluator is [`crate::domain::materiality`] — a fourth
//! subject, a change set, and therefore its own module for the reason above. It
//! carries §3's three fail-safe arms and, deliberately, none of its threshold
//! comparison; its module doc says why, and what the group that adds one owes.

use std::fmt;

use toolkit_macros::domain_model;

pub mod content_pin;
pub mod decision;

pub use content_pin::content_hash;
pub use decision::{
    APPROVAL_CONTENT_MISMATCH, DecisionBy, DecisionRefusal, DecisionRequest, REASON_REQUIRED,
    REGION_SCOPE_DENIED, SELF_APPROVAL_FORBIDDEN, WITHDRAW_FORBIDDEN, WithdrawAuthority,
    authorize_decision,
};

/// A decision was asked of a record that is no longer pending (§5, 409).
///
/// Declared in `design/05-governance.md` §5's problem-response list, so this is
/// a code the design set names rather than one raised here.
pub const APPROVAL_NOT_PENDING: &str = "APPROVAL_NOT_PENDING";

/// A second change unit was submitted over a subject a pending unit holds
/// (409).
///
/// Declared by [`07-pricewindow-linkage.md`](../../../docs/design/07-pricewindow-linkage.md)
/// §5's problem-response list and by `inst-co-single-pending` — *"at most one
/// pending approval unit **of any kind** may hold a canonical scope key … a
/// second submit touching a held key while one is `submitted` returns 409"* —
/// so this is a code the design set names rather than one raised here.
///
/// It lives beside [`APPROVAL_NOT_PENDING`] rather than in a Slice-7 module for
/// the reason that module does not exist: the rule is about a **pending approval
/// unit**, which is this store's row, and the surface that first raises it is
/// this slice's submit. A later slice that pends a window or an overlay
/// references this constant rather than declaring a second one.
pub const PENDING_CHANGE_UNIT_EXISTS: &str = "PENDING_CHANGE_UNIT_EXISTS";

/// The state of one approval record.
#[domain_model]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ApprovalState {
    /// Opened by a material change unit and awaiting an independent reviewer.
    /// The only state whose record may still move, and the state that pins the
    /// unit's scope key through `PENDING_CHANGE_UNIT_EXISTS`.
    #[default]
    Submitted,
    /// An independent `FinanceReviewer` approved it; the publish may continue.
    /// **Terminal.**
    Approved,
    /// The reviewer refused it, with a mandatory reason. **Terminal** — the
    /// subject returns to its pre-submit state and a re-submit opens a new
    /// record.
    Rejected,
    /// Withdrawn by the submitter or a `CatalogAdmin`, or voided by the TOCTOU
    /// guard when the pinned subject mutated post-submit. **Terminal**, and the
    /// state that frees the pinned scope key.
    Voided,
}

impl ApprovalState {
    /// Every state, stable order.
    pub const ALL: &'static [Self] = &[
        Self::Submitted,
        Self::Approved,
        Self::Rejected,
        Self::Voided,
    ];

    /// The persisted / wire token.
    ///
    /// These four literals are exactly what `chk_pricing_approval_state`
    /// admits. A token renamed on one side only is a row the other side can
    /// neither write nor read.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Voided => "voided",
        }
    }

    /// Read a stored token back into the machine, or `None` if it is not one of
    /// ours.
    ///
    /// `None` rather than a default: an unrecognised token means the table was
    /// written around the CHECK, and defaulting it to `submitted` would make an
    /// unreadable record look like a live approval unit.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == token)
    }

    /// Is this record still awaiting a decision?
    ///
    /// The single reading of pendingness in the crate. `decided_at IS NULL` says
    /// the same thing in the store — `chk_pricing_approval_decided_at` makes the
    /// two the same fact — and nothing reconstructs it from the null, so the two
    /// spellings cannot disagree.
    #[must_use]
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::Submitted)
    }

    /// Must a record in this state carry a `reason`?
    ///
    /// Only a reject (`inst-as-reject`), which is what
    /// `chk_pricing_approval_reason` enforces physically. A reason on an approve
    /// or a withdraw is permitted and never required.
    #[must_use]
    pub const fn requires_reason(self) -> bool {
        matches!(self, Self::Rejected)
    }

    /// Must a record in this state name its `approver_principal`?
    ///
    /// An approve and a reject do; a void does **not**, and the exception is
    /// load-bearing rather than lax. A TOCTOU void has no human decider at all,
    /// and a withdraw's decider is the submitter — whom
    /// `chk_pricing_approval_distinct_principals` forbids in that column, so
    /// requiring an approver on a void would make the withdraw path unstorable.
    #[must_use]
    pub const fn requires_approver(self) -> bool {
        matches!(self, Self::Approved | Self::Rejected)
    }

    /// Is a move from `self` to `next` legal?
    #[must_use]
    pub const fn can_transition(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Submitted,
                Self::Approved | Self::Rejected | Self::Voided
            )
        )
    }

    /// Assert a transition, refusing every edge §4 does not sanction.
    ///
    /// Total over the whole 4x4 product: every pair is either legal or carries a
    /// named reason.
    ///
    /// # Errors
    /// [`TransitionRefusal::NotPending`] when the record has already been
    /// decided or voided — the arm that reaches the wire as
    /// [`APPROVAL_NOT_PENDING`]. [`TransitionRefusal::NotAnOutcome`] when a
    /// pending record is asked to stay pending, which no surface can request;
    /// see the module doc for why it carries no code.
    pub const fn transition(self, next: Self) -> Result<(), TransitionRefusal> {
        if self.can_transition(next) {
            return Ok(());
        }
        // Pendingness is tested first because it is the operative fact: a caller
        // holding a decided record is told *that*, whatever it asked for.
        if !self.is_pending() {
            return Err(TransitionRefusal::NotPending { from: self });
        }
        Err(TransitionRefusal::NotAnOutcome { to: next })
    }

    /// Apply a surface's decision, yielding the state it lands in.
    ///
    /// The entry point the three POST routes use. It cannot produce
    /// [`TransitionRefusal::NotAnOutcome`], because [`ApprovalDecision`] has no
    /// value that names `submitted`.
    ///
    /// # Errors
    /// [`TransitionRefusal::NotPending`] when the record has already been
    /// decided or voided.
    pub const fn decide(self, decision: ApprovalDecision) -> Result<Self, TransitionRefusal> {
        let outcome = decision.outcome();
        match self.transition(outcome) {
            Ok(()) => Ok(outcome),
            Err(refusal) => Err(refusal),
        }
    }
}

impl fmt::Display for ApprovalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a surface can ask of a pending record.
///
/// Three values, one per route (`approve`, `reject`, `withdraw`), and the type
/// exists so that "leave it submitted" is unrepresentable on the path a request
/// travels rather than merely refused at the end of it.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ApprovalDecision {
    /// An independent reviewer agrees (`inst-as-approve`).
    Approve,
    /// The reviewer refuses, with a mandatory reason (`inst-as-reject`).
    Reject,
    /// The submitter or a `CatalogAdmin` withdraws it, or the TOCTOU guard voids
    /// it (`inst-as-void`).
    Void,
}

impl ApprovalDecision {
    /// Every decision, stable order.
    pub const ALL: &'static [Self] = &[Self::Approve, Self::Reject, Self::Void];

    /// The state this decision lands the record in.
    #[must_use]
    pub const fn outcome(self) -> ApprovalState {
        match self {
            Self::Approve => ApprovalState::Approved,
            Self::Reject => ApprovalState::Rejected,
            Self::Void => ApprovalState::Voided,
        }
    }
}

/// Why the machine refused a move.
///
/// Two variants and only one of them has a wire code; see the module doc.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransitionRefusal {
    /// The record has already been decided or voided (`inst-as-immutable`).
    #[error("approval record is {from}; only a submitted record can be decided")]
    NotPending {
        /// The state the record is actually in.
        from: ApprovalState,
    },
    /// A pending record was asked to move to a state that is not a decision —
    /// in practice, to stay `submitted`.
    #[error("approval state {to} is not a decision; a record is approved, rejected or voided")]
    NotAnOutcome {
        /// The state that was asked for.
        to: ApprovalState,
    },
}

impl TransitionRefusal {
    /// The wire code this refusal renders as, when the design set names one.
    ///
    /// `None` for [`TransitionRefusal::NotAnOutcome`], deliberately: no surface
    /// can provoke it, and a code declared for an unreachable refusal documents
    /// an API a client cannot call.
    #[must_use]
    pub const fn code(self) -> Option<&'static str> {
        match self {
            Self::NotPending { .. } => Some(APPROVAL_NOT_PENDING),
            Self::NotAnOutcome { .. } => None,
        }
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod approval_tests;
