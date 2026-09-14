//! The `RepoError` -> `DomainError` ladder, arm by arm.
//!
//! `repo_failure` is the single place a storage failure becomes a rejection the
//! rest of the system reasons about, so a wrong arm here is a wrong HTTP status
//! and a wrong wire code everywhere at once. Each case below pins the *category*
//! a refusal lands in, which is the part a consumer branches on.

use super::{RepoError, names_the_policy_guard, repo_failure};
use crate::domain::concurrency::{RowVersion, require_match};
use crate::domain::error::DomainError;
use crate::domain::validation::ValidationReport;

#[test]
fn an_absent_row_and_a_foreign_tenants_row_give_the_same_404() {
    // The repository already collapses the two cases; the ladder must not
    // re-open them. Subject and id survive verbatim so the response names what
    // the caller asked for, not what the store happens to call it.
    let err = RepoError::NotFound {
        subject: "plan revision".to_owned(),
        id: "3f2a/1".to_owned(),
    };

    assert_eq!(
        repo_failure(&err),
        DomainError::NotFound {
            subject: "plan revision".to_owned(),
            id: "3f2a/1".to_owned(),
        }
    );
}

#[test]
fn a_stale_write_is_a_retryable_conflict_naming_both_versions() {
    // 409 with both numbers: the caller refetches and retries, and an operator
    // reading the log can tell a caller that never refreshed from a bulk run
    // colliding with interactive editing.
    let err = RepoError::StaleRowVersion {
        subject: "plan revision".to_owned(),
        id: "3f2a/1".to_owned(),
        current: 7,
        submitted: 4,
    };

    let DomainError::StaleVersion(detail) = repo_failure(&err) else {
        panic!("a stale row version must map to STALE_VERSION");
    };
    assert!(detail.contains("current 7"), "got: {detail}");
    assert!(detail.contains("submitted 4"), "got: {detail}");
    assert!(detail.contains("plan revision 3f2a/1"), "got: {detail}");
}

#[test]
fn the_store_and_the_pre_check_spell_one_collision_one_way() {
    // The pre-check refuses from a version it has already read; the store
    // refuses from the row itself. They are two code paths reporting one
    // event, and an operator correlating a log line with a 409 body has to see
    // the same numbers in the same clause — otherwise a runbook grows two
    // entries for one failure. The store's line adds a `{subject} {id}:`
    // prefix, which is the only difference this asserts is allowed.
    let from_pre_check =
        require_match(RowVersion::new(7), RowVersion::new(4)).expect_err("7 != 4 is stale");
    let DomainError::StaleVersion(pre_check) = from_pre_check else {
        panic!("require_match must refuse with STALE_VERSION");
    };

    let DomainError::StaleVersion(from_store) = repo_failure(&RepoError::StaleRowVersion {
        subject: "plan revision".to_owned(),
        id: "3f2a/1".to_owned(),
        current: 7,
        submitted: 4,
    }) else {
        panic!("a stale row version must refuse with STALE_VERSION");
    };

    assert!(
        from_store.ends_with(&pre_check),
        "the store said {from_store:?}, the pre-check said {pre_check:?}"
    );
}

#[test]
fn editing_a_frozen_revision_is_forbidden_state_and_never_a_conflict() {
    // The distinction is the point of the variant. A conflict invites a retry;
    // no retry will make a published revision editable, and a caller told
    // "conflict" would loop until an operator read the logs.
    let err = RepoError::NotDraft {
        subject: "plan revision".to_owned(),
        id: "3f2a/0".to_owned(),
        state: "published".to_owned(),
    };

    let DomainError::LifecycleForbidden(detail) = repo_failure(&err) else {
        panic!("a frozen revision must map to LIFECYCLE_FORBIDDEN");
    };
    assert!(detail.contains("published"), "got: {detail}");
}

#[test]
fn a_plan_that_can_never_be_superseded_is_refused_in_its_own_words() {
    // Its own code as well as its own sentence (D-146). The caller asked for a
    // successor, not to edit content, so `NotDraft`'s "only draft content is
    // mutable" would answer a question nobody asked and would name as the remedy
    // — open the next revision — the very operation being refused. Sharing
    // `LIFECYCLE_FORBIDDEN` left the sentence carrying that distinction alone,
    // which is exactly the prose a consumer cannot branch on.
    let err = RepoError::NoSuccessorRevision {
        plan_id: "3f2a".to_owned(),
        state: "retired".to_owned(),
    };

    let DomainError::PlanRetiredNoSuccessor(detail) = repo_failure(&err) else {
        panic!("a plan that can never be superseded must map to PLAN_RETIRED_NO_SUCCESSOR");
    };
    assert!(detail.contains("3f2a"), "got: {detail}");
    assert!(detail.contains("retired"), "got: {detail}");
    assert!(
        detail.contains("never be superseded"),
        "the refusal must name the ground it actually rests on, got: {detail}"
    );
    assert!(
        !detail.contains("only draft content is mutable"),
        "the content-mutability sentence is the one this variant exists to \
         stop being told, got: {detail}"
    );
}

#[test]
fn a_row_that_is_not_its_keys_current_one_is_refused_in_its_own_words() {
    // D-146's reasoning, applied to the supersession commit's predecessor swap:
    // the caller is not editing draft content, so `NotDraft`'s "only draft content
    // is mutable" would name as the remedy an operation nobody attempted. The two
    // states this variant can actually report want different next actions —
    // `superseded` means recompose against the key's new current row, `draft`
    // means publish it rather than supersede it — and neither of them is "open the
    // next revision".
    //
    // **The code is still shared with `NotDraft`, and that is a known gap rather
    // than a decision.** D-146's own lesson is that a differing remedy wants a
    // differing code, not only a differing sentence; the design set declares no
    // error code for this refusal (S7 §5's list carries ~15 codes and the only
    // supersession one among them is `SUPERSESSION_INSTANT_PASSED`;
    // `SUPERSESSION_UNIT_MISMATCH` is S3 §5's), and inventing one here would
    // be designing the wire contract ahead of the route that renders it. The
    // sentence carries the distinction until then, which is exactly the prose a
    // consumer cannot branch on — recorded against D-195 rather than left here.
    let err = RepoError::NotSupersedable {
        subject: "price".to_owned(),
        id: "3f2a".to_owned(),
        state: "superseded".to_owned(),
    };

    let DomainError::LifecycleForbidden(detail) = repo_failure(&err) else {
        panic!("a row that is not current must map to LIFECYCLE_FORBIDDEN for now");
    };
    assert!(detail.contains("superseded"), "got: {detail}");
    assert!(
        detail.contains("only a published row is supersedable"),
        "the ground the refusal rests on has to be in the sentence while it is all \
         there is, got: {detail}"
    );
    assert!(
        !detail.contains("only draft content is mutable"),
        "the content-mutability sentence is one this variant exists to stop being \
         told, got: {detail}"
    );
}

#[test]
fn a_second_open_draft_is_a_conflict_and_names_the_one_that_exists() {
    // A plan has at most one concurrently editable shape. The refusal points at
    // the revision holding the slot, so the caller edits it instead of guessing.
    // It is a uniqueness conflict on that slot rather than a transition the
    // state machine denies (D-146), which is why it leaves the lifecycle
    // category entirely.
    let err = RepoError::OpenDraftExists {
        plan_id: "3f2a".to_owned(),
        revision: 2,
    };

    let DomainError::OpenDraftRevisionExists(detail) = repo_failure(&err) else {
        panic!("a second open draft must map to OPEN_DRAFT_REVISION_EXISTS");
    };
    assert!(detail.contains("revision 2"), "got: {detail}");
}

#[test]
fn a_retired_plan_and_a_taken_draft_slot_are_told_apart_without_reading_prose() {
    // The test that would have passed before D-146 and must fail after it: both
    // refusals used to arrive as `LIFECYCLE_FORBIDDEN`, so a consumer branching
    // on the domain vocabulary saw one answer where the operator has two
    // actions. "Stop, this plan is retired" ends the attempt; "you already hold
    // an open draft at revision 2" sends the operator somewhere real.
    let retired = repo_failure(&RepoError::NoSuccessorRevision {
        plan_id: "3f2a".to_owned(),
        state: "retired".to_owned(),
    });
    let occupied = repo_failure(&RepoError::OpenDraftExists {
        plan_id: "3f2a".to_owned(),
        revision: 2,
    });

    assert_ne!(
        std::mem::discriminant(&retired),
        std::mem::discriminant(&occupied),
        "one code for two operator actions is the collapse D-146 exists to undo"
    );
    assert!(
        !matches!(retired, DomainError::LifecycleForbidden(_)),
        "the retired plan keeps the code it was narrowed into: {retired:?}"
    );
    assert!(
        !matches!(occupied, DomainError::LifecycleForbidden(_)),
        "and so does the occupied draft slot: {occupied:?}"
    );
    // What `LIFECYCLE_FORBIDDEN` keeps is the refusals with no alternative
    // action to describe: a frozen revision, and the projector's own frontier.
    assert!(matches!(
        repo_failure(&RepoError::NotDraft {
            subject: "plan revision".to_owned(),
            id: "3f2a/0".to_owned(),
            state: "published".to_owned(),
        }),
        DomainError::LifecycleForbidden(_)
    ));
    assert!(matches!(
        repo_failure(&RepoError::FrontierRegression {
            tenant: "7e11".to_owned(),
            current: 9,
            requested: 8,
        }),
        DomainError::LifecycleForbidden(_)
    ));
}

#[test]
fn an_occupied_scope_key_keeps_its_rendering_verbatim() {
    // The ten-axis rendering is the whole diagnostic: re-wrapping it in
    // another sentence, or dropping an axis, would report a collision between
    // rows that do not actually share a key.
    let key = "3f2a|USD|EU|base|9c1|all_subscriptions|recurring|none|none|none";
    let err = RepoError::DuplicateScopeKey(key.to_owned());

    assert_eq!(
        repo_failure(&err),
        DomainError::DuplicateScopeKey(key.to_owned())
    );
}

#[test]
fn a_taken_phase_id_reaches_the_domain_naming_the_id_and_what_holds_it() {
    // D-340. Unlike its neighbour above, this arm carries the **whole**
    // `Display` rather than the bare id: the id alone is not the diagnostic, the
    // clause saying a revision holds each id at most once is — and that clause is
    // precisely the half the `500` this replaced withheld. So the assertion is on
    // both, not on the class.
    let phase = "1e256059-2956-4b3e-8acd-898eb43a1ff7";
    let err = RepoError::PhaseIdInUse(phase.to_owned());

    let DomainError::PhaseIdInUse(detail) = repo_failure(&err) else {
        panic!("a taken phase id is its own refusal, not a storage fault");
    };
    assert!(detail.contains(phase), "{detail}");
    assert!(detail.contains("at most once"), "{detail}");
}

#[test]
fn a_reused_key_carrying_a_different_payload_is_its_own_refusal() {
    // Not a stale-version conflict, which would invite the one retry that can
    // never succeed: the stored response answers a different request, so
    // replaying it reports work nobody asked for and re-executing breaks the
    // at-most-once promise. The key and the operation survive into the detail
    // so an operator can find the caller that reused it.
    let err = RepoError::IdempotencyPayloadMismatch {
        operation: "create_price".to_owned(),
        client_key: "ck-9f2a".to_owned(),
    };

    let DomainError::IdempotencyPayloadMismatch(detail) = repo_failure(&err) else {
        panic!("a reused key with a different payload must map to IDEMPOTENCY_PAYLOAD_MISMATCH");
    };
    assert!(detail.contains("ck-9f2a"), "got: {detail}");
    assert!(detail.contains("create_price"), "got: {detail}");
}

#[test]
fn a_quantity_no_column_can_hold_is_the_callers_mistake_not_the_stores() {
    // The distinction this arm draws is the whole reason it is not
    // `CorruptRow`: a `package_size` past the `bigint` range arrived on a
    // request and the author can change it, so it is a 400-class rejection. The
    // identical mismatch read back *out* of the column would mean the table had
    // been written around, and that is an internal fault nobody can reshape a
    // request to avoid.
    let err = RepoError::ValueOutOfRange {
        field: "package_size".to_owned(),
        value: "18446744073709551615".to_owned(),
    };

    let DomainError::InvalidRequest(detail) = repo_failure(&err) else {
        panic!("an unstorable authored value must map to INVALID_REQUEST");
    };
    assert!(detail.contains("package_size"), "got: {detail}");
    assert!(detail.contains("18446744073709551615"), "got: {detail}");
}

#[test]
fn a_horizon_on_a_class_that_may_not_carry_one_carries_its_own_code() {
    // The arm that exists because a physical CHECK was reaching callers as a
    // driver error: `grandfather_until` is ordinary draft content, and pairing
    // it with the wrong eligibility class used to become `RepoError::Db` and
    // therefore `Internal` — a 500 for a request whose author only has to clear
    // one field. D-147 states the pairing as a rule, so the refusal no longer
    // borrows the generic bad-request answer, which named no field and told a
    // consumer nothing about which one to clear. The class is named so the
    // sentence says which key refused.
    let err = RepoError::GrandfatherHorizonOffClass {
        eligibility: "all_subscriptions".to_owned(),
    };

    let DomainError::GrandfatherUntilForbidden(detail) = repo_failure(&err) else {
        panic!("a horizon off its class must map to GRANDFATHER_UNTIL_FORBIDDEN");
    };
    assert!(detail.contains("all_subscriptions"), "got: {detail}");
    assert!(detail.contains("existing_grandfathered"), "got: {detail}");
    // Still not an internal fault, which is the property the arm was added for.
    assert!(!matches!(repo_failure(&err), DomainError::Internal(_)));
}

#[test]
fn a_row_whose_line_disagrees_with_its_key_carries_the_axis_rules_own_code() {
    // The arm this fold's own `WindowOverlap` comment describes, found a second
    // time: the store's suites asserted the `RepoError` and stopped, so nothing
    // observed what it maps to and corrupting this line to `Internal` left the
    // whole crate green while an author whose row's meter disagrees with its key
    // read a 500 for a request they could fix by editing one field.
    //
    // It is **not** a dead arm — the three producers are live doors:
    // `price_repo::resolve_authored_usage_line` on create (`prepare_draft`) and
    // on bulk import (`api::rest::bulk_imports::rows_of`, which folds through
    // this very function), and `check_update_keeps_the_line` on `PATCH`. The
    // usage line is authored on the *content* view, so a caller can state one
    // the key does not agree with on every one of them.
    //
    // Both halves of the disagreement are named: the pair is only wrong
    // *together*, and the remedy is to move one of them to the other.
    let err = RepoError::UsageLineDisagrees {
        key_line: "cloudlets/eu-west".to_owned(),
        row_line: "egress_gb/eu-west".to_owned(),
    };

    let DomainError::UsageLineAxisMismatch(detail) = repo_failure(&err) else {
        panic!("a disagreeing usage line must map to USAGE_LINE_AXIS_MISMATCH");
    };
    assert!(detail.contains("cloudlets/eu-west"), "got: {detail}");
    assert!(detail.contains("egress_gb/eu-west"), "got: {detail}");
    // The property the arm exists for, stated separately: §4.1 declares this a
    // refusal the author can act on, not a fault of the store.
    assert!(!matches!(repo_failure(&err), DomainError::Internal(_)));
}

#[test]
fn an_instant_below_the_quantum_is_refused_where_it_would_have_been_stored() {
    // `timestamptz` holds microseconds, so nothing downstream refuses this: the
    // value persists and is then matched for equality against an instant another
    // gear produced at the quantum (D-144). The field is named because clearing
    // or rounding one value is the whole remedy.
    let err = RepoError::TimestampPrecisionExceeded {
        field: "grandfatherUntil".to_owned(),
        value: "2026-08-02T12:00:00.000500+00:00".to_owned(),
    };

    let DomainError::TimestampPrecisionExceeded(detail) = repo_failure(&err) else {
        panic!("a sub-millisecond instant must map to TIMESTAMP_PRECISION_EXCEEDED");
    };
    assert!(detail.contains("grandfatherUntil"), "got: {detail}");
    assert!(
        detail.contains("2026-08-02T12:00:00.000500"),
        "got: {detail}"
    );
}

#[test]
fn a_duplicate_meeting_an_unanswered_claim_is_its_own_refusal() {
    // Not the payload mismatch beside it, and not a fabricated reply: nothing
    // has been told to anybody yet, so the only honest answer is that the key is
    // held and a retry will find it either answered or mismatching (D-143). The
    // key and the operation survive so an operator can find the caller holding
    // it.
    let err = RepoError::IdempotencyKeyInFlight {
        operation: "create_price".to_owned(),
        client_key: "ck-9f2a".to_owned(),
    };

    let DomainError::IdempotencyKeyInFlight(detail) = repo_failure(&err) else {
        panic!("an unanswered claim must map to IDEMPOTENCY_KEY_IN_FLIGHT");
    };
    assert!(detail.contains("ck-9f2a"), "got: {detail}");
    assert!(detail.contains("create_price"), "got: {detail}");
}

#[test]
fn an_overlap_on_a_canonical_scope_key_carries_the_window_code() {
    // `WINDOW_OVERLAP` (§5, 409) has a schema object of its own,
    // `excl_pricing_price_window_no_overlap` — an `EXCLUDE USING gist` on Postgres, a
    // trigger pair on `SQLite` — and §6 still puts the check inside every mutation
    // because the key is ten columns of `pricing_price`. So this arm is no longer
    // the *only* thing between the repository's refusal and the wire: it is what
    // the uncontended pre-check raises, while `window_repo::overlap_or` raises the
    // same variant for the loser of a race. Both render through here, which is why
    // all three parts of the sentence still have to survive — an
    // operator needs the key to know which of a plan's keys refused, the requested
    // interval to see what they composed, and the conflicting window to act on.
    let err = RepoError::WindowOverlap {
        key: "plan=91a1|currency=USD|region=EU|charge_kind=recurring".to_owned(),
        requested: "[2099-09-15T00:00:00+00:00, 2099-09-25T00:00:00+00:00)".to_owned(),
        conflicting: "window 0000-07 at [2099-09-10T00:00:00+00:00, \
                      2099-09-20T00:00:00+00:00)"
            .to_owned(),
    };

    let DomainError::WindowOverlap(detail) = repo_failure(&err) else {
        panic!("an overlap on a canonical scope key must map to WINDOW_OVERLAP");
    };
    assert!(detail.contains("charge_kind=recurring"), "got: {detail}");
    assert!(detail.contains("2099-09-25"), "got: {detail}");
    assert!(detail.contains("window 0000-07"), "got: {detail}");
    // And emphatically not a conflict-free internal fault: before the arm existed
    // the caller read a 500 for a request whose whole remedy is a different interval.
    assert!(!matches!(repo_failure(&err), DomainError::Internal(_)));
}

#[test]
fn a_refused_window_edge_lands_with_the_other_refused_lifecycle_edges() {
    // **It mints no code**, and that is the assertion rather than an omission: §5
    // names a code for every window refusal an operator can *provoke* — a
    // cancellation of an active window, a start in the past, a mutation of history —
    // and an unsanctioned edge is not one of them. What reaches here is a sweep whose
    // ordering assumption broke, or an edge whose §4 condition has not arrived, so it
    // renders as the refused lifecycle edge it is (`LIFECYCLE_FORBIDDEN`, 400) and
    // carries the state and the boundary in its sentence.
    let err = RepoError::WindowStateForbidden {
        window_id: "0000-21".to_owned(),
        state: "scheduled".to_owned(),
        attempted: "the transition to active at 2099-09-02T00:00:00+00:00, before its \
                    effective_from 2099-09-10T00:00:00+00:00"
            .to_owned(),
    };

    let DomainError::LifecycleForbidden(detail) = repo_failure(&err) else {
        panic!("a refused window edge must map to LIFECYCLE_FORBIDDEN");
    };
    assert!(detail.contains("scheduled"), "got: {detail}");
    assert!(detail.contains("2099-09-10"), "got: {detail}");
    // The sibling window refusal the store *does* answer with a code of its own, so
    // the two are told apart by the mapping rather than by reading the sentence.
    let historical = RepoError::WindowHistorical {
        window_id: "0000-18".to_owned(),
        frozen: "it is cancelled, and an expired or cancelled window is immutable history"
            .to_owned(),
    };
    assert!(matches!(
        repo_failure(&historical),
        DomainError::WindowHistoricalImmutable(_)
    ));
}

#[test]
fn a_same_group_membership_collision_carries_the_membership_overlap_code() {
    // `MEMBERSHIP_OVERLAP` (§5, 409) — the narrower of D-09's two codes, for a
    // collision within one group. `group_membership_repo::refuse_overlap` is the
    // only producer, `window_repo`'s own `WindowOverlap` situation one table
    // over: no schema object names this arm, so it is the only thing between the
    // repository's refusal and the wire.
    let err = RepoError::MembershipOverlap {
        payer_tenant_id: "0000da01-0000-0000-0000-000000000000".to_owned(),
        requested: "[2099-09-05T00:00:00+00:00, 2099-09-15T00:00:00+00:00)".to_owned(),
        conflicting: "membership 0000-b0-01 in group groupA at [2099-09-01T00:00:00+00:00, \
                      2099-09-10T00:00:00+00:00)"
            .to_owned(),
    };

    let DomainError::MembershipOverlap(detail) = repo_failure(&err) else {
        panic!("a same-group membership collision must map to MEMBERSHIP_OVERLAP");
    };
    assert!(detail.contains("0000da01"), "got: {detail}");
    assert!(detail.contains("2099-09-15"), "got: {detail}");
    assert!(detail.contains("membership 0000-b0-01"), "got: {detail}");
    // Emphatically not a conflict-free internal fault, `WindowOverlap`'s own
    // guard: before the arm existed the caller read a 500 for a request whose
    // whole remedy is a different interval.
    assert!(!matches!(repo_failure(&err), DomainError::Internal(_)));
}

#[test]
fn a_cross_group_membership_collision_carries_the_membership_conflict_code() {
    // `MEMBERSHIP_CONFLICT` (§5, 409, D-09) — the case D-09 is actually about: a
    // payer's interval in one group colliding with an interval in a *different*
    // group. Deliberately its own arm and not merged with `MembershipOverlap`:
    // §5 types the two codes separately because the remedies differ, and a
    // consumer branching on the wire code needs the distinction the mapping is
    // what carries it through.
    let err = RepoError::MembershipConflict {
        payer_tenant_id: "0000da01-0000-0000-0000-000000000000".to_owned(),
        requested: "[2099-09-05T00:00:00+00:00, 2099-09-15T00:00:00+00:00)".to_owned(),
        conflicting: "membership 0000-b0-01 in group groupA at [2099-09-01T00:00:00+00:00, \
                      2099-09-10T00:00:00+00:00)"
            .to_owned(),
    };

    let DomainError::MembershipConflict(detail) = repo_failure(&err) else {
        panic!("a cross-group membership collision must map to MEMBERSHIP_CONFLICT");
    };
    assert!(detail.contains("0000da01"), "got: {detail}");
    assert!(detail.contains("2099-09-15"), "got: {detail}");
    assert!(detail.contains("membership 0000-b0-01"), "got: {detail}");
    assert!(!matches!(repo_failure(&err), DomainError::Internal(_)));
    // And distinguishable from its same-group sibling by category alone — a
    // consumer must be able to tell the two apart without reading the sentence.
    let sibling = RepoError::MembershipOverlap {
        payer_tenant_id: "0000da01-0000-0000-0000-000000000000".to_owned(),
        requested: "[2099-09-05T00:00:00+00:00, 2099-09-15T00:00:00+00:00)".to_owned(),
        conflicting: "membership 0000-b0-01 in group groupA at [2099-09-01T00:00:00+00:00, \
                      2099-09-10T00:00:00+00:00)"
            .to_owned(),
    };
    assert!(!matches!(
        repo_failure(&sibling),
        DomainError::MembershipConflict(_)
    ));
}

#[test]
fn an_empty_membership_interval_is_the_callers_mistake_and_keeps_its_rendering() {
    // The application half of `chk_pricing_group_membership_interval`, and §5
    // declares no membership code for it either — `WindowIntervalEmpty`'s own
    // situation, one plane over, so this rides `INVALID_REQUEST` rather than
    // minting one.
    let err = RepoError::MembershipIntervalEmpty {
        requested: "[2099-09-20T00:00:00+00:00, 2099-09-20T00:00:00+00:00)".to_owned(),
    };

    let DomainError::InvalidRequest(detail) = repo_failure(&err) else {
        panic!("an empty membership interval must map to INVALID_REQUEST");
    };
    assert_eq!(
        detail,
        "pricing repo: the membership interval [2099-09-20T00:00:00+00:00, \
         2099-09-20T00:00:00+00:00) is empty; effective_to must be strictly after \
         effective_from"
    );

    // The merge with `ValueOutOfRange` / `WindowIntervalEmpty` is deliberate and
    // this is what makes it honest: one category, because all three are an
    // authored value the request can be reshaped around — and three
    // distinguishable sentences, so a consumer is not asked to act on a
    // distinction the mapping refused to draw.
    let sibling = RepoError::WindowIntervalEmpty {
        requested: "[2099-09-20T00:00:00+00:00, 2099-09-20T00:00:00+00:00)".to_owned(),
    };
    let DomainError::InvalidRequest(sibling_detail) = repo_failure(&sibling) else {
        panic!("the merged sibling arm must land in the same category");
    };
    assert_ne!(detail, sibling_detail);
}

#[test]
fn an_empty_window_interval_is_the_callers_mistake_and_keeps_its_rendering() {
    // The third of the three window arms, and the one the commit that closed the
    // class missed: corrupting it to `Internal` left the whole suite green at 1127.
    // §5 declares eight window codes and none of them covers `effectiveTo <=
    // effectiveFrom`, so this rides `INVALID_REQUEST` — a value the gear cannot
    // interpret — rather than minting a ninth.
    let err = RepoError::WindowIntervalEmpty {
        requested: "[2099-09-20T00:00:00+00:00, 2099-09-20T00:00:00+00:00)".to_owned(),
    };

    let DomainError::InvalidRequest(detail) = repo_failure(&err) else {
        panic!("an empty window interval must map to INVALID_REQUEST");
    };
    // **By equality**, which pins two things a containment check would not: that the
    // half-open rendering reaches the caller intact — the bracket asymmetry is what
    // says *why* two equal instants are not a span — and that the arm carries the
    // repository's own sentence rather than a generic one.
    assert_eq!(
        detail,
        "pricing repo: the window interval [2099-09-20T00:00:00+00:00, \
         2099-09-20T00:00:00+00:00) is empty; effective_to must be strictly after \
         effective_from"
    );

    // The merge with `ValueOutOfRange` is deliberate and this is what makes it
    // honest: one category, because both are an authored value the request can be
    // reshaped around — and two distinguishable sentences, so a consumer is not
    // asked to act on a distinction the mapping refused to draw.
    let sibling = RepoError::ValueOutOfRange {
        field: "package_size".to_owned(),
        value: "18446744073709551615".to_owned(),
    };
    let DomainError::InvalidRequest(sibling_detail) = repo_failure(&sibling) else {
        panic!("the merged sibling arm must land in the same category");
    };
    assert_ne!(detail, sibling_detail);
}

#[test]
fn a_driver_fault_and_a_corrupt_reading_stay_internal() {
    // Neither is a caller mistake and neither is reshapeable into a request
    // that would succeed, which is exactly the line between a bad request and
    // an internal fault.
    for err in [
        RepoError::Db("connection reset".to_owned()),
        RepoError::CorruptRow("lifecycle_state holds 'pending'".to_owned()),
    ] {
        assert!(matches!(repo_failure(&err), DomainError::Internal(_)));
    }
}

#[test]
fn a_key_the_register_refused_a_second_hold_on_is_a_pending_unit_conflict() {
    // **Both halves of the arm, and this is the half that had none.**
    // `uq_pricing_approval_key_pending` is what closes the two-writer race
    // `ApprovalService::submit`'s check cannot see, so its refusal reaches a caller
    // only through `repo_failure` — and the only assertion on it was a `#[ignore]`
    // Postgres race, which means a corruption of this arm to `Internal` would have
    // left the whole non-ignored suite green and turned an invariant conflict into a
    // 500 nobody can act on. That defect class has been found twice in this crate.
    //
    // It must land on the **same** code as the in-transaction check, which is
    // `inst-co-single-pending`'s own: a caller must not be told to do two different
    // things about one condition depending on which of two transactions committed
    // first.
    let err = RepoError::PendingKeyHeld {
        key: "3f2a|EUR|eu|base|9c1|all_subscriptions|recurring|none|none|none".to_owned(),
    };

    let DomainError::PendingChangeUnitExists(detail) = repo_failure(&err) else {
        panic!("a held key must map to PENDING_CHANGE_UNIT_EXISTS, never to Internal");
    };
    // The key is carried through, because it is what the operator acts on: which
    // unit to decide or withdraw is found by the key it holds.
    assert!(detail.contains("3f2a|EUR|eu"), "got: {detail}");
    // And the remedy is named, which is what separates this from a bare conflict.
    assert!(detail.contains("withdraw"), "got: {detail}");

    // **The convergence, asserted rather than claimed.** The check path builds its
    // own sentence (`infra::approval::refuse_held_key`) and this one comes off a
    // `thiserror` display, so the two bodies can only be held together by a test.
    // What they share is what a caller reads and acts on: the key, and the same
    // closing instruction. What they cannot share is the holding unit — at the moment
    // the index fires, the transaction that inserted the winning row may not have
    // committed, so there is no unit this path is entitled to name — and the
    // comparison is written against the *shared* clause for that reason rather than
    // against the whole string.
    assert!(
        detail.ends_with("decide it, or withdraw it to free the key"),
        "the constraint path must close with the check path's own instruction: {detail}"
    );
}

#[test]
fn a_tenants_second_open_policy_proposal_is_the_same_conflict_as_a_held_key() {
    // **The mint guard's arm (D-192 clause (2)), and it had none.**
    // `uq_pricing_approval_policy_pending` closes the two-writer race
    // `open_policy_unit`'s read-then-write check cannot see, so its refusal reaches a
    // caller only through `repo_failure` - and a corruption of this arm to `Internal`
    // would turn an invariant conflict into a 500 nobody can act on, which is the
    // defect class this file's sibling case above records having been found twice.
    let err = RepoError::PendingPolicyUnitHeld {
        tenant_id: "b0b0cafe-0000-0000-0000-000000000001".to_owned(),
    };

    let DomainError::PendingChangeUnitExists(detail) = repo_failure(&err) else {
        panic!("a second open proposal must map to PENDING_CHANGE_UNIT_EXISTS, never to Internal");
    };
    // The tenant is carried through: this is the one path that cannot name the unit,
    // so the subject of the sentence is all the refusal has to identify itself by.
    assert!(detail.contains("b0b0cafe"), "got: {detail}");

    // **The convergence with the check path, asserted rather than claimed**, exactly as
    // the held-key case asserts its own. `infra::approval::open_policy_unit` builds its
    // sentence by hand and this one comes off a `thiserror` display, so only a test
    // holds them together - and what they must share is the closing instruction, which
    // is what a caller acts on. The check path names the holding unit and this one
    // cannot, so the comparison is against the shared clause and not the whole string.
    assert!(
        detail.ends_with("decide it, or withdraw it to propose another"),
        "the constraint path must close with the check path's own instruction: {detail}"
    );
}

#[test]
fn the_mint_guard_is_told_from_the_primary_key_by_the_name_each_backend_prints() {
    // `policy_guard_or_contention` reads the driver's message, which this crate
    // otherwise refuses to do, and its doc argues why this is the narrow case. What
    // that argument rests on is *these three strings*, so they are pinned here rather
    // than left to a Postgres suite nobody runs on a laptop: two renderings of the
    // guard, one per backend, and the primary key's rendering which must not match.
    //
    // Both are captured from a real driver - the `SQLite` pair by running the two
    // colliding inserts against the mirror, the Postgres one from that server's
    // documented `unique_violation` message - and a rename of the index that forgot
    // this function reddens here.
    assert!(
        names_the_policy_guard(
            "database error: Execution Error: error returned from database: duplicate key value \
             violates unique constraint \"uq_pricing_approval_policy_pending\""
        ),
        "Postgres names the index"
    );
    assert!(
        names_the_policy_guard(
            "database error: Execution Error: error returned from database: (code: 2067) UNIQUE \
             constraint failed: pricing_approval.tenant_id"
        ),
        "`SQLite` names the indexed columns instead"
    );
    assert!(
        !names_the_policy_guard(
            "database error: Execution Error: error returned from database: (code: 1555) UNIQUE \
             constraint failed: pricing_approval.approval_id"
        ),
        "and the primary key is the other index on this table, whose loser is told to retry"
    );
    // The qualified form is what is matched, so another table's `tenant_id` cannot be
    // read as this rule. `pricing_approval_key`'s own register is the near miss that
    // makes this worth a line: its partial unique index leads on `tenant_id` too.
    assert!(
        !names_the_policy_guard(
            "database error: Execution Error: error returned from database: (code: 2067) UNIQUE \
             constraint failed: pricing_approval_key.tenant_id, pricing_approval_key.scope_key"
        ),
        "the register one plane over is not the mint guard"
    );
}

// ---------------------------------------------------------------------------
// `contention_or_db`'s roster, derived rather than transcribed.
// ---------------------------------------------------------------------------

/// Every `.rs` file under `src`, recursively, that is not a test module.
///
/// `approval_repo_tests::production_sources`' construction and for its reason: a
/// scan that reads one directory level is a guard a reorganisation switches off
/// silently.
fn production_sources() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("the crate's own source tree") {
            let path = entry.expect("a readable dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("_tests.rs"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The file stems that call [`contention_or_db`], read off the source.
///
/// The definition and the one delegation in `storage.rs` are excluded: neither is
/// a site that *decides* a unique violation is a contention.
fn modules_calling_contention_or_db() -> std::collections::BTreeSet<String> {
    production_sources()
        .into_iter()
        .filter(|path| {
            path.file_stem().and_then(|s| s.to_str()) != Some("storage")
                && std::fs::read_to_string(path)
                    .expect("a readable source file")
                    .contains("contention_or_db(")
        })
        .map(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .expect("a utf-8 file stem")
                .to_owned()
        })
        .collect()
}

/// The two unique violations this crate deliberately answers with something
/// **other** than a contention still do.
///
/// [`contention_or_db`]'s doc used to enumerate its call sites, and the
/// enumeration said five while there were sixteen — so it answered "500 by
/// omission" for the taxonomy, policy, migration and membership writes, all four
/// of which are 409. The roster is not the part worth guarding, because it grows
/// with every serialization point the gear adds; **the exclusions are**, because
/// an exclusion that decays reads exactly like a site somebody forgot.
///
/// Derived from the source rather than transcribed, for the reason the deleted
/// enumeration demonstrates. The floor is what stops a scan that silently matched
/// nothing from asserting an empty roster and passing.
#[test]
fn the_two_deliberate_exclusions_still_hold() {
    let callers = modules_calling_contention_or_db();

    assert!(
        callers.len() >= 8,
        "the scan found {} callers, which is fewer than this crate had when the scan was \
         written — a broken scan asserts the exclusions vacuously: {callers:?}",
        callers.len()
    );

    assert!(
        !callers.contains("price_repo"),
        "a canonical-key collision is DUPLICATE_SCOPE_KEY, which names the key and its \
         occupant: a caller-fixable authoring fault, not a race a retry resolves"
    );
    assert!(
        !callers.contains("idempotency_repo"),
        "a lost idempotency claim is a replay, whose remedy is to return the first caller's \
         answer: the CAS owns it, and folding it into ConcurrentMutation would tell a client \
         to retry a request that has already been answered"
    );
}

// ---------------------------------------------------------------------------
// The ladder, exhaustively: every arm, not only the arms somebody wrote a case
// for.
// ---------------------------------------------------------------------------

/// The [`DomainError`] class each [`RepoError`] variant is contracted to land
/// in, transcribed in [`repo_failure`]'s own order so the two read side by side.
///
/// Only the returned value's **discriminant** is read; its payload is a
/// placeholder. The point is the class, which is what a consumer branches on and
/// what decides the status.
///
/// The `match` carries no `_` arm on purpose: that is the whole compiler-forced
/// half of this census. A variant added to `RepoError`, or an arm re-pointed in
/// `repo_failure`, cannot compile past here without an author stating where it is
/// meant to land — which is what the per-case tests above cannot do, because a
/// variant nobody wrote a case for is a variant they are silent about.
fn declared_class(err: &RepoError) -> DomainError {
    use DomainError as D;
    use RepoError as R;
    let d = || "detail".to_owned();
    match err {
        // -- 500: the store, and a value no `CHECK` should have admitted.
        R::Db(_) | R::CorruptRow(_) => D::Internal(d()),

        // -- Every refused state-machine edge, in one arm.
        R::MigrationStateForbidden { .. }
        | R::FrontierRegression { .. }
        | R::NotDraft { .. }
        | R::NotSupersedable { .. }
        | R::WindowStateForbidden { .. } => D::LifecycleForbidden(d()),

        R::NoSuccessorRevision { .. } => D::PlanRetiredNoSuccessor(d()),
        R::OutboxEventAlreadyEnqueued { .. } => D::OutboxEventAlreadyEnqueued(d()),

        // -- The one editable slot is taken, twice over: a plan's draft revision
        // and an overlay's. Both name a revision the operator can go and edit.
        R::OpenDraftExists { .. } | R::OverlayOpenDraftExists { .. } => {
            D::OpenDraftRevisionExists(d())
        }

        R::NotFound { .. } => D::NotFound {
            subject: d(),
            id: d(),
        },
        R::StaleRowVersion { .. } => D::StaleVersion(d()),
        R::DuplicateScopeKey(_) => D::DuplicateScopeKey(d()),
        R::PhaseIdInUse(_) => D::PhaseIdInUse(d()),
        R::DuplicateBundleOnPlan { .. } => D::BundleExistsOnPlan(d()),
        R::OverlayPrecedenceHeld => D::PrecedenceDuplicate(d()),

        // -- The two publish rules the store re-raises on its own doors. They
        // keep the *rule's* code, which is why they are not one arm: an
        // unresolved rounding policy mints `ROUNDING_POLICY_UNRESOLVED`, while an
        // unresolved tax category is reported the way publish rules are — a
        // report naming the row.
        R::RoundingPolicyUnresolved { .. } => D::RoundingPolicyUnresolved(d()),
        R::TaxCategoryUnresolved { .. } => D::ValidationFailed(ValidationReport::default()),

        R::IdempotencyPayloadMismatch { .. } => D::IdempotencyPayloadMismatch(d()),
        R::IdempotencyKeyInFlight { .. } => D::IdempotencyKeyInFlight(d()),

        // -- One situation from three sides: a row under a bulk lock, a run that
        // could not establish its locks, and a plain lost race.
        R::BulkRowLocked { .. } | R::BulkLocksHeld { .. } | R::ConcurrentMutation { .. } => {
            D::ConcurrentMutation(d())
        }

        // -- Not in the arm above: a bulk lock ends with its run and tells the
        // caller to retry, where a window binds its price row for the rest of time.
        R::PriceWindowScheduled { .. } => D::PriceWindowScheduled(d()),

        // -- A value arriving on a request that no column will hold, in four
        // shapes; the caller can reshape all four.
        R::ValueOutOfRange { .. }
        | R::WindowIntervalEmpty { .. }
        | R::MembershipIntervalEmpty { .. }
        | R::MembershipHistorical { .. } => D::InvalidRequest(d()),

        R::GrandfatherHorizonOffClass { .. } => D::GrandfatherUntilForbidden(d()),
        R::UsageLineDisagrees { .. } => D::UsageLineAxisMismatch(d()),
        R::TimestampPrecisionExceeded { .. } => D::TimestampPrecisionExceeded(d()),
        R::ApprovalNotPending { .. } => D::ApprovalNotPending(d()),

        // -- The register's constraint and the mint guard's loser: one condition,
        // one remedy, whichever transaction committed first.
        R::PendingKeyHeld { .. } | R::PendingPolicyUnitHeld { .. } => {
            D::PendingChangeUnitExists(d())
        }

        R::WindowOverlap { .. } => D::WindowOverlap(d()),
        R::WindowHistorical { .. } => D::WindowHistoricalImmutable(d()),
        R::MembershipOverlap { .. } => D::MembershipOverlap(d()),
        R::MembershipConflict { .. } => D::MembershipConflict(d()),
    }
}

/// The placeholder every sample in the roster below carries. The census reads the
/// roster for its *variants*; only [`declared_class`]'s discriminant is compared,
/// so no payload here is ever the subject of an assertion.
///
/// A free function and not a closure: the samples are written at the
/// [`repo_error_roster`] invocation, and a `macro_rules!` body cannot lend a local
/// binding to tokens its caller wrote.
fn detail() -> String {
    "detail".to_owned()
}

/// Declares the census roster **and** the exhaustive `match` that forces it to be
/// complete, from one list.
///
/// One source for both halves, which is what makes the roster unevadable. A variant
/// added to [`RepoError`] leaves [`variant_name`]'s generated `match` non-exhaustive,
/// so the crate does not compile until the variant is written into the list below —
/// and because the arms are expanded rather than written, there is no `_` an author
/// can reach for to silence it.
///
/// A hand-maintained roster beside a hand-maintained count costs exactly the gap the
/// count claims to close: `declared_class`'s own exhaustive `match` forces a new
/// variant to be *classified*, but a roster short by one asserts the ladder vacuously
/// for the arm nobody looked at, and a count edited in the same breath as the variant
/// says nothing at all.
///
/// Each entry is `Variant` followed by its constructor arguments — `(..)` for a tuple
/// variant, `{ .. }` for a struct one, `{}` for a unit one — so the value and the
/// `match` arm are built from the same identifier and cannot name different variants.
macro_rules! repo_error_roster {
    ($($variant:ident $args:tt),+ $(,)?) => {
        /// One value of every [`RepoError`] variant, in [`declared_class`]'s order.
        fn one_of_every_repo_error() -> Vec<RepoError> {
            vec![$(RepoError::$variant $args),+]
        }

        /// The variant a sample is, for the census's own failure messages — and the
        /// exhaustive `match` that is the roster's completeness guard.
        ///
        /// The diagnostic a missing variant earns here is `E0004`, naming the variant
        /// that is not covered. The fix is to write it into [`repo_error_roster`]'s
        /// list with a sample payload, and to give it an arm in [`declared_class`],
        /// which reports the same error for the same reason.
        fn variant_name(err: &RepoError) -> &'static str {
            match err {
                $(RepoError::$variant { .. } => stringify!($variant),)+
            }
        }
    };
}

repo_error_roster! {
    Db(detail()),
    OutboxEventAlreadyEnqueued {
        dedup_key: detail(),
    },
    MigrationStateForbidden {
        migration_id: detail(),
        state: "scheduled".to_owned(),
        attempted: "the move to completed".to_owned(),
    },
    CorruptRow(detail()),
    FrontierRegression {
        tenant: detail(),
        current: 7,
        requested: 4,
    },
    NotDraft {
        subject: detail(),
        id: detail(),
        state: "published".to_owned(),
    },
    NotSupersedable {
        subject: detail(),
        id: detail(),
        state: "draft".to_owned(),
    },
    WindowStateForbidden {
        window_id: detail(),
        state: "cancelled".to_owned(),
        attempted: "activation".to_owned(),
    },
    NoSuccessorRevision {
        plan_id: detail(),
        state: "abandoned".to_owned(),
    },
    OpenDraftExists {
        plan_id: detail(),
        revision: 3,
    },
    OverlayOpenDraftExists {
        price_overlay_id: detail(),
        revision: 3,
    },
    NotFound {
        subject: detail(),
        id: detail(),
    },
    StaleRowVersion {
        subject: detail(),
        id: detail(),
        current: 7,
        submitted: 4,
    },
    DuplicateScopeKey(detail()),
    PhaseIdInUse(detail()),
    DuplicateBundleOnPlan { plan_id: detail() },
    OverlayPrecedenceHeld {},
    RoundingPolicyUnresolved { price_id: detail() },
    TaxCategoryUnresolved { price_id: detail() },
    IdempotencyPayloadMismatch {
        operation: detail(),
        client_key: detail(),
    },
    IdempotencyKeyInFlight {
        operation: detail(),
        client_key: detail(),
    },
    BulkRowLocked {
        price_id: detail(),
        bulk_operation_id: detail(),
    },
    PriceWindowScheduled {
        price_id: detail(),
        window_id: detail(),
    },
    BulkLocksHeld {
        operation_id: detail(),
        cause: detail(),
    },
    ConcurrentMutation { aggregate: detail() },
    ValueOutOfRange {
        field: detail(),
        value: detail(),
    },
    WindowIntervalEmpty { requested: detail() },
    MembershipIntervalEmpty { requested: detail() },
    MembershipHistorical {
        membership_id: detail(),
        frozen: detail(),
    },
    GrandfatherHorizonOffClass { eligibility: detail() },
    UsageLineDisagrees {
        key_line: detail(),
        row_line: detail(),
    },
    TimestampPrecisionExceeded {
        field: detail(),
        value: detail(),
    },
    ApprovalNotPending {
        approval_id: detail(),
        state: "approved".to_owned(),
    },
    PendingKeyHeld { key: detail() },
    PendingPolicyUnitHeld { tenant_id: detail() },
    WindowOverlap {
        key: detail(),
        requested: detail(),
        conflicting: detail(),
    },
    WindowHistorical {
        window_id: detail(),
        frozen: detail(),
    },
    MembershipOverlap {
        payer_tenant_id: detail(),
        requested: detail(),
        conflicting: detail(),
    },
    MembershipConflict {
        payer_tenant_id: detail(),
        requested: detail(),
        conflicting: detail(),
    },
}

#[test]
fn every_repo_error_variant_lands_in_its_declared_class() {
    let roster = one_of_every_repo_error();

    // Completeness is the compiler's, not this assertion's: `variant_name`'s
    // generated `match` covers the same list the roster is built from, so a
    // variant missing from it is a build failure rather than a census silently
    // one arm short. What is left to check at runtime is that no variant was
    // written twice — `unreachable_patterns` catches a duplicated identifier, but
    // not two entries whose payloads differ and whose variant does not.
    let distinct: std::collections::HashSet<_> =
        roster.iter().map(std::mem::discriminant).collect();
    assert_eq!(
        distinct.len(),
        roster.len(),
        "the roster must carry one value of every variant and one value **each**: a duplicate \
         entry displaces the variant it was written in place of"
    );

    // Collected rather than asserted per iteration: a re-pointed arm usually
    // arrives with its neighbours — the ten variants this census was written for
    // were unobserved as a set — and a run that stops at the first names one of
    // them and hides the rest.
    let wrong: Vec<String> = roster
        .iter()
        .filter_map(|err| {
            let expected = declared_class(err);
            let got = repo_failure(err);
            (std::mem::discriminant(&got) != std::mem::discriminant(&expected)).then(|| {
                format!(
                    "{} must land in {expected:?} and landed in {got:?}",
                    variant_name(err)
                )
            })
        })
        .collect();

    assert!(
        wrong.is_empty(),
        "the ladder answers a class other than the declared one for {} of {} variants:\n  {}",
        wrong.len(),
        roster.len(),
        wrong.join("\n  ")
    );
}
