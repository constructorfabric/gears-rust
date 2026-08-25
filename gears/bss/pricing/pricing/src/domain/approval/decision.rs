//! Everything a decision must satisfy before the store is touched —
//! `cpt-cf-bss-pricing-algo-two-person` (`design/05-governance.md` §3), the pin
//! re-verification of `inst-ap-pin`, and the approver-scope rule
//! `inst-ap-scope`.
//!
//! One entry point, [`authorize_decision`], and it is **pure**: it takes the
//! record's own facts and the request's, and answers with a
//! [`DecisionRefusal`] or nothing. It reads no clock, no database and no claim
//! resolver. That is what lets the whole rule set be a table test rather than a
//! choreography, and it is why the audited-denial writer sits one layer up in
//! [`crate::infra::approval`] — writing a record is not a judgement.
//!
//! # The order of the checks is a decision, and the argument is the audit
//!
//! ```text
//! 1. NotPending      2. SelfApproval      3. OutOfScope
//! 3b. ForeignWithdraw
//! 4. ReasonRequired  5. ContentMismatch
//! ```
//!
//! **`NotPending` first** because it is the operative fact about the record
//! rather than about the request: a caller holding a decided record is told
//! *that*, whatever they asked for. [`ApprovalState::transition`] already
//! resolves its own refusals in that order and this keeps the two agreeing.
//!
//! **`SelfApproval` before `ReasonRequired`**, and this one is load-bearing.
//! `inst-tp-selfaudit` requires the attempt to be **written to the audit log**.
//! A submitter rejecting their own unit with no reason would, under the other
//! order, be answered `REASON_REQUIRED` — a well-formedness complaint that
//! invites them to retry with a reason and lands **no** attempted-violation
//! record. The self-approval attempt would be swallowed by a check about the
//! shape of the request. So the two audited rules come first.
//!
//! **`ContentMismatch` last** because it is the only check whose input the
//! caller cannot see: it needs the subject re-derived from the store. Answering
//! it ahead of the identity rules would tell a principal who may not decide at
//! all that the content moved, which is a fact about a change set they were
//! never entitled to.
//!
//! # What each decision is actually checked for
//!
//! | | `Approve` | `Reject` | `Void` |
//! |---|---|---|---|
//! | `NotPending` | yes | yes | yes |
//! | `SelfApproval` | yes | yes | **no** |
//! | `OutOfScope` | yes | yes | **no** |
//! | `ForeignWithdraw` | no | no | **yes, when a principal withdraws** |
//! | `ReasonRequired` | no | yes | no |
//! | `ContentMismatch` | yes | **no** | **no** |
//!
//! Three of those cells are choices rather than readings, and each is argued:
//!
//! - **`Void` is exempt from the *distinctness* rules and bound by its own.** A
//!   withdraw's decider *is* the submitter (`inst-as-void`: "the submitter (or a
//!   `CatalogAdmin`) explicitly withdraws it"), and the TOCTOU void has no human
//!   decider at all. A distinctness rule applied here would make the withdraw
//!   path — the one §4 added precisely so a pending unit is not an indefinite
//!   lock on a scope key — unreachable by the person it is for. The exemption is
//!   read off [`DecisionBy::approver`] rather than off a decision match: the two
//!   arms that exercise review authority are the two that carry an approver, and
//!   the rules ask for one instead of asking which decision this is.
//!
//!   **That exemption swallowed the other half of `inst-as-void` for as long as
//!   it stood alone.** The instruction names *who* may withdraw, and because
//!   `approver()` is `None` on every void, no rule here read the withdrawer at
//!   all: any principal the gate admitted could close any `submitted` unit and
//!   release the scope keys it held. `ForeignWithdraw` (3b) is that half, and
//!   [`WithdrawAuthority`] records why its second clause is a proxy.
//! - **`Void` is exempt from the pin.** Voiding is what a moved subject *causes*;
//!   refusing to void because the subject moved would leave the record pinned
//!   open by the very event that should close it.
//! - **`Reject` is checked for scope, and §2 step 4a's literal says "approve".**
//!   **Reported as a divergence, not absorbed.** A reject is the same principal
//!   deciding the same pinned change set, and it is not the harmless direction:
//!   `inst-as-reject` returns the plan to `draft`, so an EU-scoped reviewer
//!   rejecting a US repricing unwinds work they were denied the authority to
//!   approve. The guard is widened rather than narrowed, so the failure mode of
//!   being wrong about this is a refusal an operator can see, not a decision
//!   nobody was entitled to make.
//!
//! **`Reject` is *not* checked against the pin, matching §2 step 2a's literal**
//! ("approve re-verifies the pinned content hash"). A reject of content that has
//! since moved refuses a plan revision that no longer exists in that form, and
//! the subject returns to `draft` either way; there is nothing the mismatch
//! would protect.
//!
//! # `region` here is the **pricing** axis, and never the authz-region claim
//!
//! `inst-rb-region` and S4 C5 are explicit: pricing `region` is a commercial
//! axis, deliberately decoupled from the identity provider's region claim. So
//! [`DecisionRequest::approver_regions`] is the approver's grant **expressed in
//! pricing regions**, and the change set's regions come off the pinned rows'
//! scope keys. Conflating the two would let a reviewer whose `IdP` tenant happens
//! to sit in one deployment region approve pricing for another.
//!
//! **What no document fixes, and what this therefore cannot do:** nothing in the
//! design set says how a principal's pricing-region grant is *transported* —
//! which claim, constraint or resource property carries it.
//! `inst-rb-preview-scope` says the preview grant carries an explicit
//! pricing-region set and stops there. So this module takes the set as a
//! parameter and the surface that mounts the route owes the plumbing; the gap is
//! reported rather than closed by minting a property name here.
//!
//! # Brand is **not** checked, and that is not an omission
//!
//! §2 step 4a says "every region/brand touched by the pinned change set". For a
//! `plan_revision` subject there is no brand to touch: brand is not a price-row
//! field (S4 `inst-tx-brand`), and `inst-rb-preview-scope` says in as many words
//! that brand "has **no selector** on the base-price preview surface" and
//! applies only where a brand-scoped artifact — a brand-scoped `PriceOverlay` —
//! is the object. This gear authors no overlays. A brand check here would range
//! over the empty set and read as coverage.

use std::collections::BTreeSet;

use toolkit_macros::domain_model;

use crate::domain::approval::{ApprovalDecision, ApprovalState, TransitionRefusal};
use crate::domain::scope_key::Region;

/// The submitter tried to decide their own unit (`inst-tp-distinct`, §5, 403).
pub const SELF_APPROVAL_FORBIDDEN: &str = "SELF_APPROVAL_FORBIDDEN";

/// The pinned content no longer matches at decision time (`inst-ap-pin`, §5,
/// 409).
pub const APPROVAL_CONTENT_MISMATCH: &str = "APPROVAL_CONTENT_MISMATCH";

/// A reject arrived without its mandatory reason (`inst-as-reject`, §5).
///
/// §5 types it 422; the canonical error family has no 422 category, so it
/// reaches the wire as a **400** carrying this code — the Foundation §3.3 rule
/// this crate applies to every architectural 422, and the reason
/// `no_operation_declares_a_422` exists.
pub const REASON_REQUIRED: &str = "REASON_REQUIRED";

/// The approver's pricing-region grant does not cover the pinned change set
/// (`inst-ap-scope`, §5, 403).
pub const REGION_SCOPE_DENIED: &str = "REGION_SCOPE_DENIED";

/// Rule code for a withdraw by a principal who is neither the unit's submitter
/// nor a catalog authority (`inst-as-void`).
///
/// **§5 declares no code for this refusal**, because the design set does not
/// contemplate the refusal existing: `inst-as-void` states the identity rule and
/// the endpoint map gates the route on `approval × approve`, and nothing
/// reconciles the two. The code is this crate's, in the shape of its five
/// neighbours, and the divergence is reported rather than smoothed over — see
/// [`WithdrawAuthority`].
pub const WITHDRAW_FORBIDDEN: &str = "WITHDRAW_FORBIDDEN";

/// Whether a withdrawing principal carries authority over units that are not
/// theirs (`inst-as-void`'s "or a `CatalogAdmin`").
///
/// [`RegionGrant`](crate::infra::approval::RegionGrant)'s shape and its reason:
/// an authority the domain must judge but cannot itself establish, transported
/// by the surface that can.
///
/// # This is a proxy, and the proxy is recorded rather than hidden
///
/// `inst-as-void` names a **role** — `CatalogAdmin`. Nothing reaching this layer
/// can answer a question about roles: `SecurityContext` exposes a subject, a
/// tenant and token scopes, and the PDP answers `resource × action` decisions.
/// So [`Self::CatalogAuthority`] is set from `plan × publish`, which is the
/// authority `CatalogAdmin` and `FinanceManager` actually carry and which no
/// `FinanceReviewer` holds.
///
/// **What that buys, stated exactly.** Without it the identity half of
/// `inst-as-void` was enforced at neither layer: `authorize_decision`'s rules 2
/// and 3 live inside `if let Some(approver)`, and `approver()` is `None` on every
/// void, so what the code implemented was *any principal holding `approval ×
/// approve` may void any `submitted` unit of the tenant*. A withdraw is not
/// cosmetic — it moves the unit out of `submitted`, which releases the canonical
/// scope keys it held and re-opens them to whoever wants them. So a
/// `FinanceReviewer` who could not approve a change could nonetheless close
/// another principal's review of it and free its keys.
///
/// **What it does not buy.** A `FinanceManager` can still withdraw a
/// `CatalogAdmin`'s unit and vice versa; the proxy is coarser than the role. That
/// is a strictly smaller hole than the one it replaces, and closing it needs the
/// design set to reconcile `inst-as-void`'s identity rule with the `approval ×
/// approve` gate the endpoint map assigns the route — under the default matrix
/// the only principal who can reach `withdraw` at all is a `FinanceReviewer`, who
/// is neither of the two the transition names. `api::rest::approvals`' module doc
/// reports that contradiction; this type is where it bites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WithdrawAuthority {
    /// The caller holds `plan × publish` in the tenant — the expressible proxy
    /// for `CatalogAdmin`, so units that are not theirs are theirs to close.
    CatalogAuthority,
    /// The caller holds no catalog authority: only the units they submitted.
    OwnUnitsOnly,
}

/// Why a decision was refused.
///
/// Six variants. **Five carry a code the design set already names** in §5's
/// problem-response list; [`DecisionRefusal::ForeignWithdraw`] is the exception
/// and is minted here, because §5 does not contemplate the refusal existing —
/// see [`WITHDRAW_FORBIDDEN`] and [`WithdrawAuthority`].
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecisionRefusal {
    /// Two distinct **principals**, not two roles: one human holding both
    /// `plan × publish` and `approval × approve` still cannot sign their own
    /// change (`inst-tp-distinct`, G2 of §1.6).
    #[error("the submitter of an approval unit may not decide it")]
    SelfApproval,
    /// The subject moved between submission and decision, so the reviewer would
    /// be approving content they never saw (`inst-ap-pin`).
    #[error("the pinned content no longer matches the subject")]
    ContentMismatch,
    /// The record has already been decided or voided (`inst-as-immutable`).
    #[error("only a submitted record can be decided")]
    NotPending,
    /// A reject with no reason (`inst-as-reject`).
    #[error("a rejection carries a mandatory reason")]
    ReasonRequired,
    /// The approver's pricing-region grant does not cover every region the
    /// pinned change set touches (`inst-ap-scope`).
    #[error("the approver's scope does not cover every region of the pinned change set")]
    OutOfScope,
    /// A withdraw by a principal who is neither the unit's submitter nor a
    /// catalog authority (`inst-as-void`).
    ///
    /// An **authority** refusal: the caller tried to close a review that was not
    /// theirs and free the scope keys it held.
    #[error("only the submitter or a catalog authority may withdraw an approval unit")]
    ForeignWithdraw,
}

impl DecisionRefusal {
    /// Every refusal, stable order — the order [`authorize_decision`] tests
    /// them in, so a reader of one is reading the other.
    ///
    /// [`Self::ForeignWithdraw`] sits fourth because rule **3b** runs there, after
    /// the approver's scope and before the reject's reason. Listing it last — the
    /// position a newly added rule arrives in — makes the sentence above false about
    /// the one entry a reader is most likely to check, and nothing else notices: a
    /// void carries neither a reason nor an approve's pin, so only the doc can be
    /// wrong and only the doc can say so.
    pub const ALL: &'static [Self] = &[
        Self::NotPending,
        Self::SelfApproval,
        Self::OutOfScope,
        Self::ForeignWithdraw,
        Self::ReasonRequired,
        Self::ContentMismatch,
    ];

    /// The wire code §5 declares for this refusal.
    ///
    /// Total, unlike [`TransitionRefusal::code`]: every refusal a *surface* can
    /// provoke has a code, and the one refusal that has none —
    /// [`TransitionRefusal::NotAnOutcome`] — is unrepresentable on this path
    /// because [`ApprovalDecision`] names no self-edge.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::SelfApproval => SELF_APPROVAL_FORBIDDEN,
            Self::ContentMismatch => APPROVAL_CONTENT_MISMATCH,
            Self::NotPending => crate::domain::approval::APPROVAL_NOT_PENDING,
            Self::ReasonRequired => REASON_REQUIRED,
            Self::OutOfScope => REGION_SCOPE_DENIED,
            Self::ForeignWithdraw => WITHDRAW_FORBIDDEN,
        }
    }

    /// Must the attempt be written to `pricing_audit_log`?
    ///
    /// `inst-tp-selfaudit` binds the self-approval refusal to a record, and
    /// `inst-rb-audit` binds every denied attempt to one. Both of those are
    /// **authority** refusals: somebody tried to exercise an authority they do
    /// not have, and that is the security event.
    ///
    /// The other three are not. `NotPending` and `ContentMismatch` are races an
    /// entitled reviewer loses, and `ReasonRequired` is a malformed request; a
    /// trail that recorded them as attempted violations would bury the two that
    /// matter under the noise of an impatient client's retries, which is the
    /// same as not recording them.
    #[must_use]
    pub const fn is_an_audited_violation(self) -> bool {
        matches!(
            self,
            Self::SelfApproval | Self::OutOfScope | Self::ForeignWithdraw
        )
    }
}

/// What is being asked of a unit, **carrying the identity that asks it**.
///
/// One value and not a decision beside an `Option<Uuid>`, because the pairing is
/// what the two-person rule is *about*. A separate pair is a hole:
/// `approver_principal == Some(submitter)` reads `None` as "somebody other than
/// the submitter", so an approve naming nobody slips the distinctness rule, slips
/// the scope rule with it, and reaches `chk_pricing_approval_approver` — a 500
/// for a request no rule has judged.
/// Refusing it would have needed a code for "an approve with no approver", and
/// §5 names none; making it unrepresentable needs no code at all.
///
/// The obligation moves to whichever surface mounts the route, which is where it
/// belongs: an `Approve` cannot be constructed without a principal, so a surface
/// with no authenticated identity cannot pose one.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionBy {
    /// An approve, by the principal named.
    Approve(uuid::Uuid),
    /// A reject, by the principal named.
    Reject(uuid::Uuid),
    /// A withdraw or the machine-driven TOCTOU void.
    ///
    /// The `Option` is not the same absence the two arms above forbid: a void
    /// **has** no approver by rule rather than by omission. `Some` is a human
    /// withdraw — `inst-as-void`'s withdrawer is the submitter or a
    /// `CatalogAdmin` — and `None` is the guard closing a unit whose subject
    /// moved, which no principal asked for.
    Void(Option<uuid::Uuid>),
}

impl DecisionBy {
    /// The state-machine edge this asks for, identity dropped.
    #[must_use]
    pub const fn decision(self) -> ApprovalDecision {
        match self {
            Self::Approve(_) => ApprovalDecision::Approve,
            Self::Reject(_) => ApprovalDecision::Reject,
            Self::Void(_) => ApprovalDecision::Void,
        }
    }

    /// Whoever is acting, for the record and for the audit trail.
    ///
    /// `None` **only** on a machine-driven void. Distinct from
    /// [`DecisionBy::approver`]: a human withdrawer is a decider and is not an
    /// approver, and conflating the two is what would put the submitter's own
    /// withdraw in front of the distinctness rule.
    #[must_use]
    pub const fn decider(self) -> Option<uuid::Uuid> {
        match self {
            Self::Approve(principal) | Self::Reject(principal) => Some(principal),
            Self::Void(principal) => principal,
        }
    }

    /// The principal the two-person rule and the scope rule judge.
    ///
    /// `Some` exactly on the two arms that exercise review authority, so those
    /// two rules are written as "if there is an approver" rather than as a
    /// decision match that a fourth decision could fall through.
    #[must_use]
    pub const fn approver(self) -> Option<uuid::Uuid> {
        match self {
            Self::Approve(principal) | Self::Reject(principal) => Some(principal),
            Self::Void(_) => None,
        }
    }
}

/// Everything [`authorize_decision`] judges, gathered by the caller.
///
/// A struct rather than seven positional arguments because two of the fields
/// are byte slices and two are region sets, and a call site that transposed a
/// pair would compile.
#[domain_model]
#[derive(Clone, Debug)]
pub struct DecisionRequest<'a> {
    /// Where the record stands **as read**, and what `NotPending` is about.
    pub record_state: ApprovalState,
    /// Who opened the unit.
    pub submitter_principal: uuid::Uuid,
    /// What is being asked of it, and by whom.
    pub decision: DecisionBy,
    /// The reason supplied, if any.
    pub reason: Option<&'a str>,
    /// The digest pinned at submission, as the store holds it.
    pub pinned_content_hash: &'a [u8],
    /// The digest of the subject **as it stands now**. `None` when the subject
    /// could not be re-derived at all — see [`authorize_decision`]'s note on why
    /// that is a mismatch and not a not-found.
    pub current_content_hash: Option<[u8; 32]>,
    /// The pricing regions the approver's grant covers. See the module doc: this
    /// is the commercial axis, never the authz-region claim.
    pub approver_regions: &'a BTreeSet<Region>,
    /// The pricing regions the pinned change set touches.
    pub change_set_regions: &'a BTreeSet<Region>,
    /// Whether the caller may close a unit that is not theirs (`inst-as-void`).
    ///
    /// Read only on the `Void` arm. See [`WithdrawAuthority`] for why this is a
    /// transported proxy rather than the role the instruction names.
    pub withdraw_authority: WithdrawAuthority,
}

/// Judge one decision. `Ok(())` means the store may be told.
///
/// # Errors
/// The first [`DecisionRefusal`] that applies, in the order the module doc
/// fixes. First and not all of them: a refusal here is a single fact about
/// authority or about the world, not an authoring report to remediate in one
/// pass — the aggregate-report shape belongs to the validation pipeline, whose
/// subject is a plan a human is editing.
///
/// A `current_content_hash` of `None` is [`DecisionRefusal::ContentMismatch`]
/// and deliberately not a not-found: the subject having vanished between
/// submission and approval is the strongest form of "what you saw is no longer
/// there", and answering 404 would tell the reviewer their *approval record* was
/// missing when it is sitting in front of them.
pub fn authorize_decision(request: &DecisionRequest<'_>) -> Result<(), DecisionRefusal> {
    // 1. Pendingness, through the machine rather than beside it, so the two
    //    cannot disagree about which states are decidable.
    match request.record_state.decide(request.decision.decision()) {
        Ok(_) => {}
        Err(TransitionRefusal::NotPending { .. } | TransitionRefusal::NotAnOutcome { .. }) => {
            return Err(DecisionRefusal::NotPending);
        }
    }

    // 2 and 3 are the two rules about **review authority**, and they run exactly
    // when there is an approver to judge — which is a property of the value
    // rather than a decision match, so a `None` cannot pass for "somebody else".
    if let Some(approver) = request.decision.approver() {
        // 2. The two-person rule — identity, never role.
        if approver == request.submitter_principal {
            return Err(DecisionRefusal::SelfApproval);
        }

        // 3. The approver's reach over the pinned change set.
        if !request
            .change_set_regions
            .is_subset(request.approver_regions)
        {
            return Err(DecisionRefusal::OutOfScope);
        }
    }

    // 3b. The withdrawer's identity — `inst-as-void`'s other half, and the half
    //     no other layer enforces.
    //
    //     It cannot live in the block above, and that is exactly how it came to be
    //     missing: rules 2 and 3 run `if let Some(approver)`, and `approver()` is
    //     `None` on every void, so a withdraw skipped both and no other check
    //     looked at who was asking. What the code implemented was "any principal
    //     the gate let through may close any submitted unit of the tenant" — and a
    //     withdraw releases the canonical scope keys the unit held, so that is an
    //     authority act and not a cosmetic one.
    //
    //     Asked of the **withdrawer** rather than of `decider()`, because the void
    //     the TOCTOU guard performs carries no principal at all and must stay
    //     exempt: it is the machine closing a unit whose subject moved, which is
    //     the rule this one is about, not a violation of it.
    if let DecisionBy::Void(Some(withdrawer)) = request.decision
        && withdrawer != request.submitter_principal
        && request.withdraw_authority != WithdrawAuthority::CatalogAuthority
    {
        return Err(DecisionRefusal::ForeignWithdraw);
    }

    // 4. The reject's mandatory reason. Blank is absent: a reason column holding
    //    a space satisfies `chk_pricing_approval_reason` and tells an auditor
    //    nothing, which is the outcome the rule exists to prevent.
    if matches!(request.decision, DecisionBy::Reject(_))
        && request.reason.is_none_or(|reason| reason.trim().is_empty())
    {
        return Err(DecisionRefusal::ReasonRequired);
    }

    // 5. The pin.
    if matches!(request.decision, DecisionBy::Approve(_))
        && request
            .current_content_hash
            .as_ref()
            .map(<[u8; 32]>::as_slice)
            != Some(request.pinned_content_hash)
    {
        return Err(DecisionRefusal::ContentMismatch);
    }

    Ok(())
}

#[cfg(test)]
#[path = "decision_tests.rs"]
mod decision_tests;
