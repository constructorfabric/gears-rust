//! The decision guards, one at a time and then all at once.
//!
//! **Every refusal test first puts the world in the state where the guard it
//! names is what answers.** Five checks share one request, and four of them can
//! fire on a request built carelessly: a self-approval attempt whose reason is
//! missing is answered by whichever check runs first, and a test that asserted
//! `SELF_APPROVAL_FORBIDDEN` on such a request would be evidence about the
//! *order* rather than about the rule. So [`valid`] is a request every check
//! passes, and each refusal below moves exactly one thing.
//!
//! The order itself has its own tests, because it is a decision and not an
//! accident — see `a_self_approval_is_answered_before_the_missing_reason_is`.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::{DecisionBy, DecisionRefusal, DecisionRequest, WithdrawAuthority, authorize_decision};
use crate::domain::approval::{ApprovalDecision, ApprovalState};
use crate::domain::scope_key::Region;

const SUBMITTER: Uuid = Uuid::from_u128(0x5b_01);
const APPROVER: Uuid = Uuid::from_u128(0xab_01);

const PINNED: &[u8] = &[0xde, 0xad, 0xbe, 0xef];

fn pinned32() -> [u8; 32] {
    let mut out = [0_u8; 32];
    out[..4].copy_from_slice(PINNED);
    out
}

/// Every decision, each carrying an approver on the arms that have one.
///
/// Derived from [`ApprovalDecision::ALL`] rather than written out, so a fourth
/// decision added to the machine reaches the loops below instead of quietly
/// skipping them — which is the coverage `ALL` exists to give and a hand-written
/// triple would lose.
fn every_decision_by(approver: Uuid) -> Vec<DecisionBy> {
    ApprovalDecision::ALL
        .iter()
        .map(|decision| match decision {
            ApprovalDecision::Approve => DecisionBy::Approve(approver),
            ApprovalDecision::Reject => DecisionBy::Reject(approver),
            ApprovalDecision::Void => DecisionBy::Void(Some(approver)),
        })
        .collect()
}

fn regions(values: &[&str]) -> BTreeSet<Region> {
    values
        .iter()
        .map(|value| Region::new(value).expect("a non-blank region"))
        .collect()
}

/// A request every one of the five checks passes.
///
/// The world in which each refusal below is a fact about the guard it names.
/// Without it, a suite of refusals would pass against
/// `fn authorize_decision(_) -> Err(whatever)`.
struct World {
    approver_regions: BTreeSet<Region>,
    change_set_regions: BTreeSet<Region>,
    pinned: Vec<u8>,
}

impl World {
    fn new() -> Self {
        Self {
            approver_regions: regions(&["EU", "US"]),
            change_set_regions: regions(&["EU"]),
            pinned: pinned32().to_vec(),
        }
    }

    fn valid(&self) -> DecisionRequest<'_> {
        DecisionRequest {
            record_state: ApprovalState::Submitted,
            submitter_principal: SUBMITTER,
            decision: DecisionBy::Approve(APPROVER),
            reason: None,
            pinned_content_hash: &self.pinned,
            current_content_hash: Some(pinned32()),
            approver_regions: &self.approver_regions,
            change_set_regions: &self.change_set_regions,
            withdraw_authority: WithdrawAuthority::OwnUnitsOnly,
        }
    }
}

// ---------------------------------------------------------------------------
// The world in which the refusals mean something
// ---------------------------------------------------------------------------

/// An independent reviewer, in scope, approving unmoved content, is allowed.
#[test]
fn an_independent_reviewer_may_approve_the_content_they_were_shown() {
    let world = World::new();
    assert_eq!(authorize_decision(&world.valid()), Ok(()));
}

/// And a reject with a reason, and a withdraw, are allowed too — so the three
/// decision arms are each observable before anything below refuses one.
#[test]
fn a_reasoned_reject_and_a_withdraw_are_both_allowed() {
    let world = World::new();

    let mut reject = world.valid();
    reject.decision = DecisionBy::Reject(APPROVER);
    reject.reason = Some("margin below floor");
    assert_eq!(authorize_decision(&reject), Ok(()));

    let mut withdraw = world.valid();
    withdraw.decision = DecisionBy::Void(None);
    assert_eq!(authorize_decision(&withdraw), Ok(()));
}

// ---------------------------------------------------------------------------
// inst-tp-distinct
// ---------------------------------------------------------------------------

/// `inst-tp-distinct`: **identity, not role.**
///
/// The request is otherwise perfect — in scope, unmoved content, a decision the
/// machine allows — and the only thing moved is who is asking.
#[test]
fn the_submitter_may_not_approve_their_own_unit() {
    let world = World::new();
    let mut request = world.valid();
    request.decision = DecisionBy::Approve(SUBMITTER);
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::SelfApproval)
    );
}

/// The same principal cannot reject it either.
///
/// A reject is a decision, and `inst-as-reject` returns the plan to `draft`;
/// a submitter who could reject their own unit could unwind a review they had
/// invited, unaudited by the two-person rule.
#[test]
fn the_submitter_may_not_reject_their_own_unit_either() {
    let world = World::new();
    let mut request = world.valid();
    request.decision = DecisionBy::Reject(SUBMITTER);
    request.reason = Some("changed my mind");
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::SelfApproval)
    );
}

/// **Holding both roles changes nothing**, which is the whole content of G2's
/// "not roles".
///
/// The rule cannot see roles at all — there is no role in
/// [`DecisionRequest`] — and that is the executable form of the claim: a
/// principal granted both `plan × publish` and `approval × approve` by a custom
/// role reaches this function with exactly the same fields as anyone else, so
/// there is nothing a role grant could switch off.
#[test]
fn a_principal_holding_both_roles_is_still_one_principal() {
    let world = World::new();
    let both_roles = SUBMITTER;
    let mut request = world.valid();
    request.decision = DecisionBy::Approve(both_roles);
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::SelfApproval)
    );
}

/// The withdraw path is exempt, and that exemption is load-bearing rather than
/// lax: `inst-as-void`'s withdrawer **is** the submitter.
#[test]
fn a_submitter_may_withdraw_their_own_unit() {
    let world = World::new();
    let mut request = world.valid();
    request.decision = DecisionBy::Void(Some(SUBMITTER));
    assert_eq!(authorize_decision(&request), Ok(()));
}

/// And a principal who is **not** the submitter is refused, unless they hold
/// catalog authority.
///
/// Rule 3b is the one refusal this crate mints a code for, and it is asked of the
/// withdrawer rather than of the decider so the TOCTOU guard's principal-less
/// void stays exempt. A withdraw releases the canonical scope keys the unit held,
/// so closing someone else's unit is an authority act; without this pair the rule
/// and its `CatalogAuthority` exemption are exercised nowhere in the file that
/// owns the function.
#[test]
fn a_foreign_withdraw_is_refused_unless_the_caller_holds_catalog_authority() {
    let world = World::new();

    let mut foreign = world.valid();
    foreign.decision = DecisionBy::Void(Some(APPROVER));
    assert_eq!(
        authorize_decision(&foreign),
        Err(DecisionRefusal::ForeignWithdraw)
    );

    // The exemption, and it is what makes the case above a rule about authority
    // rather than about identity.
    let mut authorized = world.valid();
    authorized.decision = DecisionBy::Void(Some(APPROVER));
    authorized.withdraw_authority = WithdrawAuthority::CatalogAuthority;
    assert_eq!(authorize_decision(&authorized), Ok(()));
}

// ---------------------------------------------------------------------------
// inst-ap-scope
// ---------------------------------------------------------------------------

/// `inst-ap-scope`: an EU-scoped reviewer cannot approve a US repricing.
#[test]
fn an_approver_whose_grant_misses_a_region_of_the_change_set_is_refused() {
    let mut world = World::new();
    world.approver_regions = regions(&["EU"]);
    world.change_set_regions = regions(&["EU", "US"]);
    assert_eq!(
        authorize_decision(&world.valid()),
        Err(DecisionRefusal::OutOfScope)
    );
}

/// One region missing out of many is enough — the rule is coverage of **every**
/// region, not overlap with any.
#[test]
fn covering_some_of_the_change_set_is_not_covering_it() {
    let mut world = World::new();
    world.approver_regions = regions(&["EU", "APAC"]);
    world.change_set_regions = regions(&["EU", "APAC", "US"]);
    assert_eq!(
        authorize_decision(&world.valid()),
        Err(DecisionRefusal::OutOfScope)
    );
}

/// A grant wider than the change set is fine; the rule is coverage, not equality.
#[test]
fn a_wider_grant_covers_a_narrower_change_set() {
    let mut world = World::new();
    world.approver_regions = regions(&["EU", "US", "APAC"]);
    world.change_set_regions = regions(&["EU"]);
    assert_eq!(authorize_decision(&world.valid()), Ok(()));
}

/// A change set touching no region at all — a pure plan-shape revision with no
/// price rows — is covered by any grant, including an empty one.
///
/// Stated as a test because the empty-set case is where a subset check and a
/// hand-written loop diverge, and because D-115 makes pure-shape revisions a
/// real and always-material change class.
#[test]
fn a_change_set_touching_no_region_is_covered_by_any_grant() {
    let mut world = World::new();
    world.approver_regions = BTreeSet::new();
    world.change_set_regions = BTreeSet::new();
    assert_eq!(authorize_decision(&world.valid()), Ok(()));
}

/// A withdraw is exempt: it decides nothing about the content.
#[test]
fn a_withdraw_is_not_scope_checked() {
    let mut world = World::new();
    world.approver_regions = BTreeSet::new();
    world.change_set_regions = regions(&["US"]);
    let mut request = world.valid();
    request.decision = DecisionBy::Void(None);
    assert_eq!(authorize_decision(&request), Ok(()));
}

// ---------------------------------------------------------------------------
// inst-as-reject's mandatory reason
// ---------------------------------------------------------------------------

#[test]
fn a_reject_without_a_reason_is_refused() {
    let world = World::new();
    let mut request = world.valid();
    request.decision = DecisionBy::Reject(APPROVER);
    request.reason = None;
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::ReasonRequired)
    );
}

/// Blank is absent. A single space satisfies `chk_pricing_approval_reason` at
/// the storage layer and tells an auditor nothing, which is the outcome the rule
/// exists to prevent — so the surface refuses what the CHECK cannot.
#[test]
fn a_blank_reason_is_no_reason() {
    let world = World::new();
    let mut request = world.valid();
    request.decision = DecisionBy::Reject(APPROVER);
    request.reason = Some("   \t\n ");
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::ReasonRequired)
    );
}

/// An approve needs none, and a withdraw needs none.
#[test]
fn only_a_reject_needs_a_reason() {
    let world = World::new();

    let approve = world.valid();
    assert_eq!(authorize_decision(&approve), Ok(()));

    let mut withdraw = world.valid();
    withdraw.decision = DecisionBy::Void(None);
    withdraw.reason = None;
    assert_eq!(authorize_decision(&withdraw), Ok(()));
}

// ---------------------------------------------------------------------------
// inst-ap-pin, the re-verification arm
// ---------------------------------------------------------------------------

#[test]
fn an_approve_of_content_that_moved_is_refused() {
    let world = World::new();
    let mut request = world.valid();
    request.current_content_hash = Some([0x11; 32]);
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::ContentMismatch)
    );
}

/// A subject that has vanished is the strongest form of "not what you saw".
///
/// Deliberately not a not-found: the approval record is right there, and telling
/// the reviewer it is missing would send them looking for the wrong thing.
#[test]
fn an_approve_of_a_subject_that_no_longer_exists_is_a_mismatch() {
    let world = World::new();
    let mut request = world.valid();
    request.current_content_hash = None;
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::ContentMismatch)
    );
}

/// A one-byte difference is a mismatch. The pin is equality, not a prefix.
#[test]
fn one_byte_of_difference_is_a_mismatch() {
    let world = World::new();
    let mut moved = pinned32();
    moved[31] ^= 0x01;
    let mut request = world.valid();
    request.current_content_hash = Some(moved);
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::ContentMismatch)
    );
}

/// A reject is not pin-checked; §2 step 2a puts the re-verification on approve,
/// and the subject returns to `draft` either way.
#[test]
fn a_reject_of_content_that_moved_is_allowed() {
    let world = World::new();
    let mut request = world.valid();
    request.decision = DecisionBy::Reject(APPROVER);
    request.reason = Some("margin below floor");
    request.current_content_hash = Some([0x11; 32]);
    assert_eq!(authorize_decision(&request), Ok(()));
}

// ---------------------------------------------------------------------------
// inst-as-immutable
// ---------------------------------------------------------------------------

/// Every decided state refuses every decision.
#[test]
fn no_decision_reaches_a_record_that_is_no_longer_pending() {
    let world = World::new();
    for state in ApprovalState::ALL.iter().filter(|s| !s.is_pending()) {
        for decision in every_decision_by(APPROVER) {
            let mut request = world.valid();
            request.record_state = *state;
            request.decision = decision;
            request.reason = Some("margin below floor");
            assert_eq!(
                authorize_decision(&request),
                Err(DecisionRefusal::NotPending),
                "a {state} record took a {decision:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The order, which is itself a decision
// ---------------------------------------------------------------------------

/// **The order test that `inst-tp-selfaudit` depends on.**
///
/// A submitter rejecting their own unit with no reason must be told
/// `SELF_APPROVAL_FORBIDDEN`, not `REASON_REQUIRED`. Only the first of those is
/// an audited violation, so the other order would let a self-approval attempt be
/// swallowed by a complaint about the shape of the request, leaving no
/// attempted-violation record at all.
#[test]
fn a_self_approval_is_answered_before_the_missing_reason_is() {
    let world = World::new();
    let mut request = world.valid();
    request.decision = DecisionBy::Reject(SUBMITTER);
    request.reason = None;
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::SelfApproval)
    );
}

/// A decided record answers `APPROVAL_NOT_PENDING` even when the caller is also
/// the submitter: the operative fact is that there is nothing left to decide.
#[test]
fn pendingness_is_answered_before_identity_is() {
    let world = World::new();
    let mut request = world.valid();
    request.record_state = ApprovalState::Approved;
    request.decision = DecisionBy::Approve(SUBMITTER);
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::NotPending)
    );
}

/// A principal who may not decide at all is not told that the content moved.
///
/// The mismatch is a fact about a change set they were never entitled to see.
#[test]
fn identity_is_answered_before_the_pin_is() {
    let world = World::new();
    let mut request = world.valid();
    request.decision = DecisionBy::Approve(SUBMITTER);
    request.current_content_hash = Some([0x11; 32]);
    assert_eq!(
        authorize_decision(&request),
        Err(DecisionRefusal::SelfApproval)
    );
}

// ---------------------------------------------------------------------------
// The vocabulary
// ---------------------------------------------------------------------------

/// `ALL` is the order [`authorize_decision`] tests them in, which is what its
/// own doc promises a reader of one is reading in the other.
///
/// The sequence is read off the numbered rules in that function — 1 pendingness,
/// 2 the two-person rule, 3 the approver's scope, 3b the withdrawer's identity,
/// 4 the reject's reason, 5 the pin — rather than derived from the array, so a
/// rule that moves and an array that does not have to be reconciled here.
///
/// It is pinned because nothing else can hold the claim: three of the six are
/// mutually exclusive by the decision they apply to (a void carries no reason
/// and no approve pin), so no pair of them is orderable by a request, and the
/// array carried `ForeignWithdraw` in sixth place — two rules past where 3b runs
/// — for as long as the rule existed.
#[test]
fn the_roster_is_in_the_order_the_judge_applies_them() {
    assert_eq!(
        DecisionRefusal::ALL,
        &[
            DecisionRefusal::NotPending,
            DecisionRefusal::SelfApproval,
            DecisionRefusal::OutOfScope,
            DecisionRefusal::ForeignWithdraw,
            DecisionRefusal::ReasonRequired,
            DecisionRefusal::ContentMismatch,
        ]
    );
}

/// Every refusal carries a distinct code, and every code is one §5 declares —
/// **except one, named here so the exception cannot be silent.**
///
/// The literals are repeated here on purpose: this is the test that would fail
/// if somebody renamed a constant, and a test that read the constant would
/// happily rename with it.
///
/// `ForeignWithdraw` is the exception and it is listed in its own table below
/// rather than folded into the first: §5's problem-response list declares no code
/// for a withdraw refused on the withdrawer's identity, because the design set
/// does not contemplate the refusal existing — `inst-as-void` states the identity
/// rule and the endpoint map gates the route on `approval × approve`, and nothing
/// reconciles the two. Splitting the tables is what keeps "every code is one §5
/// declares" a true sentence about the first one.
#[test]
fn every_refusal_carries_the_code_the_design_set_names() {
    /// Minted by this crate, not read off §5. See the doc above.
    const MINTED: [(DecisionRefusal, &str); 1] =
        [(DecisionRefusal::ForeignWithdraw, "WITHDRAW_FORBIDDEN")];

    let expected = [
        (DecisionRefusal::NotPending, "APPROVAL_NOT_PENDING"),
        (DecisionRefusal::SelfApproval, "SELF_APPROVAL_FORBIDDEN"),
        (DecisionRefusal::OutOfScope, "REGION_SCOPE_DENIED"),
        (DecisionRefusal::ReasonRequired, "REASON_REQUIRED"),
        (
            DecisionRefusal::ContentMismatch,
            "APPROVAL_CONTENT_MISMATCH",
        ),
    ];
    assert_eq!(
        expected.len() + MINTED.len(),
        DecisionRefusal::ALL.len(),
        "a refusal was added without a code"
    );
    for (refusal, code) in expected.into_iter().chain(MINTED) {
        assert_eq!(refusal.code(), code);
        assert!(
            DecisionRefusal::ALL.contains(&refusal),
            "{refusal:?} is missing from ALL"
        );
    }

    let mut codes: Vec<&str> = DecisionRefusal::ALL.iter().map(|r| r.code()).collect();
    codes.sort_unstable();
    let distinct = codes.len();
    codes.dedup();
    assert_eq!(distinct, codes.len(), "two refusals share one code");
}

/// Exactly the three **authority** refusals are audited violations.
///
/// The other three are races an entitled reviewer loses, or a malformed request;
/// recording them as attempted violations would bury the ones that matter under
/// an impatient client's retries.
///
/// `ForeignWithdraw` joined the audited set with the rule itself: closing a review
/// that is not yours releases the canonical scope keys the unit held, so a refused
/// attempt is somebody reaching for an authority they do not have — which is the
/// criterion this predicate's own doc states, and the same one `SelfApproval`
/// meets.
#[test]
fn only_the_authority_refusals_are_audited_violations() {
    let audited: Vec<DecisionRefusal> = DecisionRefusal::ALL
        .iter()
        .copied()
        .filter(|refusal| refusal.is_an_audited_violation())
        .collect();
    assert_eq!(
        audited,
        vec![
            DecisionRefusal::SelfApproval,
            DecisionRefusal::OutOfScope,
            DecisionRefusal::ForeignWithdraw
        ]
    );
}
