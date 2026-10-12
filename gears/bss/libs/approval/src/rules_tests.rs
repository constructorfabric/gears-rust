#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::{
    ApproveEligibility, ApproveRefusal, ApproveStep, approve_eligibility, counted_approvals,
    evaluate_approve,
};
use crate::model::{ApprovalError, Decision, ItemRef, Policy, Unit, UnitState, Verdict};
use time::macros::datetime;
use uuid::Uuid;

fn unit(quorum: u32, submitted_by: Uuid) -> Unit {
    Unit {
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        kind: "prices".into(),
        ref_type: "book".into(),
        ref_id: Uuid::new_v4(),
        state: UnitState::Pending,
        common_effective_date: None,
        quorum_required: quorum,
        generation: 1,
        submitted_by,
        submitted_at: datetime!(2026-09-24 10:00 UTC),
        submit_note: None,
        decided_at: None,
        decided_note: None,
        snapshot: serde_json::json!({}),
        snapshot_hash: "h".into(),
        version: 1,
    }
}
fn vote(unit: &Unit, actor: Uuid, generation: i32) -> Decision {
    Decision {
        unit_id: unit.id,
        actor,
        generation,
        verdict: Verdict::Approve,
        note: None,
        at: datetime!(2026-09-24 11:00 UTC),
        stale: generation < unit.generation,
    }
}

/// One stored item per author, as a unit's items name them (only `created_by` is judged).
fn authored(authors: &[Uuid]) -> Vec<ItemRef> {
    authors
        .iter()
        .map(|a| ItemRef {
            item_type: "price".into(),
            item_id: Uuid::new_v4(),
            created_by: *a,
            before: None,
            after: serde_json::json!({}),
        })
        .collect()
}

#[test]
fn quorum_one_applies_on_the_first_independent_approve() {
    let author = Uuid::new_v4();
    let u = unit(1, author);
    assert_eq!(
        evaluate_approve(&u, &[], Uuid::new_v4(), &authored(&[author])).unwrap(),
        ApproveStep::Apply
    );
}
#[test]
fn quorum_two_needs_a_second_distinct_approver() {
    let author = Uuid::new_v4();
    let u = unit(2, author);
    let first = Uuid::new_v4();
    assert_eq!(
        evaluate_approve(&u, &[], first, &authored(&[author])).unwrap(),
        ApproveStep::NeedMore { have: 1, need: 2 }
    );
    assert_eq!(
        evaluate_approve(
            &u,
            &[vote(&u, first, 1)],
            Uuid::new_v4(),
            &authored(&[author])
        )
        .unwrap(),
        ApproveStep::Apply
    );
}
#[test]
fn the_submitter_and_every_item_author_are_excluded() {
    let author = Uuid::new_v4();
    let submitter = Uuid::new_v4();
    let u = unit(1, submitter);
    assert!(matches!(
        evaluate_approve(&u, &[], submitter, &authored(&[author])),
        Err(ApprovalError::SodViolation)
    ));
    assert!(matches!(
        evaluate_approve(&u, &[], author, &authored(&[author])),
        Err(ApprovalError::SodViolation)
    ));
}
#[test]
fn a_second_vote_by_the_same_actor_in_the_same_generation_is_refused() {
    let u = unit(2, Uuid::new_v4());
    let a = Uuid::new_v4();
    assert!(matches!(
        evaluate_approve(&u, &[vote(&u, a, 1)], a, &authored(&[])),
        Err(ApprovalError::DuplicateVote)
    ));
}
#[test]
fn a_vote_from_an_earlier_generation_neither_counts_nor_blocks_the_actor() {
    let mut u = unit(2, Uuid::new_v4());
    u.generation = 2;
    let a = Uuid::new_v4();
    let old = vote(&u, a, 1);
    assert!(old.stale);
    assert_eq!(
        evaluate_approve(&u, &[old], a, &authored(&[])).unwrap(),
        ApproveStep::NeedMore { have: 1, need: 2 }
    );
}
#[test]
fn a_decided_unit_takes_no_vote() {
    let mut u = unit(1, Uuid::new_v4());
    u.state = UnitState::Approved;
    assert!(matches!(
        evaluate_approve(&u, &[], Uuid::new_v4(), &authored(&[])),
        Err(ApprovalError::AlreadyDecided)
    ));
}
/// W2: the counts the readers show are the approve votes the quorum counts: of the unit's
/// current generation and not stale. A reject, a stale vote and a vote of an earlier generation
/// do not count.
#[test]
fn the_predicate_counts_the_current_generations_live_approves() {
    let mut refreshed = unit(3, Uuid::new_v4());
    refreshed.generation = 2;
    let (live, earlier, refuser, marked_voter) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let mut rejected = vote(&refreshed, refuser, 2);
    rejected.verdict = Verdict::Reject;
    let mut marked = vote(&refreshed, marked_voter, 2);
    marked.stale = true;
    let decisions = [
        vote(&refreshed, live, 2),
        vote(&refreshed, earlier, 1),
        rejected,
        marked,
    ];
    let judged = approve_eligibility(&refreshed, [], &decisions, Uuid::new_v4());
    assert_eq!(judged.approvals, 1, "{judged:?}");
    assert!(judged.refusal.is_none(), "{judged:?}");
    // The count alone needs no item and no actor (the phase 9 review's R4).
    assert_eq!(counted_approvals(&refreshed, &decisions), 1);
}
/// W2 and the phase 9 review's R40: the predicate and the engine against a table written out,
/// never against each other (the engine judges through the predicate, so comparing the two cannot
/// fail on a defect in it). For every fixture (quorum 0, 1 and 2 with no vote and with the
/// reviewer's, a stale vote beside a live one, a current vote marked stale, a decided unit) and
/// every actor (the submitter, an item author, the reviewer, a voter of an earlier generation, a
/// fresh reviewer), the table says the outcome: a refusal, or the step the vote takes. The
/// predicate counts the fixture's live approves and refuses as the table does; the engine answers
/// the table's step, or the refusal's own error.
#[test]
fn the_predicate_and_the_engine_answer_the_written_table() {
    use ApproveRefusal::{
        AlreadyDecided as Decided, DuplicateVote as Duplicate, SodViolation as Sod,
    };
    use ApproveStep::{Apply, NeedMore};
    type Outcome = Result<ApproveStep, ApproveRefusal>;
    /// A fixture, its unit and decisions, its counted approves, and each actor's outcome.
    type Row<'a> = (&'a str, &'a Unit, Vec<Decision>, u32, [Outcome; 5]);
    let (submitter, author, reviewer, earlier_reviewer, fresh) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let items = authored(&[author]);
    let voted = |u: &Unit| vec![vote(u, reviewer, 1)];
    let (q0, q1, q2) = (unit(0, submitter), unit(1, submitter), unit(2, submitter));
    let (q0_voted, q1_voted, q2_voted) =
        (unit(0, submitter), unit(1, submitter), unit(2, submitter));
    let mut refreshed = unit(2, submitter);
    refreshed.generation = 2;
    let beside = vec![
        vote(&refreshed, earlier_reviewer, 1),
        vote(&refreshed, reviewer, 2),
    ];
    // A vote of the current generation is the voter's vote, stale flag or not: a refresh moves the
    // generation, so the votes it makes stale are of an earlier one.
    let mut marked = vote(&refreshed, earlier_reviewer, 2);
    marked.stale = true;
    let mut decided = unit(1, submitter);
    decided.state = UnitState::Approved;
    let need: Outcome = Ok(NeedMore { have: 1, need: 2 });
    // (fixture, unit, decisions, counted approves, the outcome for the submitter, the author, the
    // reviewer, the voter of an earlier generation and a fresh reviewer)
    let table: Vec<Row<'_>> = vec![
        (
            "quorum 0, no vote",
            &q0,
            vec![],
            0,
            [Err(Sod), Err(Sod), Ok(Apply), Ok(Apply), Ok(Apply)],
        ),
        (
            "quorum 0, the reviewer's vote",
            &q0_voted,
            voted(&q0_voted),
            1,
            [Err(Sod), Err(Sod), Err(Duplicate), Ok(Apply), Ok(Apply)],
        ),
        (
            "quorum 1, no vote",
            &q1,
            vec![],
            0,
            [Err(Sod), Err(Sod), Ok(Apply), Ok(Apply), Ok(Apply)],
        ),
        (
            "quorum 1, the reviewer's vote",
            &q1_voted,
            voted(&q1_voted),
            1,
            [Err(Sod), Err(Sod), Err(Duplicate), Ok(Apply), Ok(Apply)],
        ),
        (
            "quorum 2, no vote",
            &q2,
            vec![],
            0,
            [Err(Sod), Err(Sod), need, need, need],
        ),
        (
            "quorum 2, the reviewer's vote",
            &q2_voted,
            voted(&q2_voted),
            1,
            [Err(Sod), Err(Sod), Err(Duplicate), Ok(Apply), Ok(Apply)],
        ),
        (
            "a stale vote beside a live one",
            &refreshed,
            beside,
            1,
            [Err(Sod), Err(Sod), Err(Duplicate), Ok(Apply), Ok(Apply)],
        ),
        (
            "a current vote marked stale",
            &refreshed,
            vec![marked],
            0,
            [Err(Sod), Err(Sod), need, Err(Duplicate), need],
        ),
        ("a decided unit", &decided, vec![], 0, [Err(Decided); 5]),
    ];
    let actors = [submitter, author, reviewer, earlier_reviewer, fresh];
    for (name, u, decisions, counted, outcomes) in &table {
        for (actor, expected) in actors.iter().zip(outcomes) {
            let judged =
                approve_eligibility(u, items.iter().map(|i| i.created_by), decisions, *actor);
            assert_eq!(judged.approvals, *counted, "{name}");
            assert_eq!(judged.refusal, expected.err(), "{name}, {actor}");
            assert_eq!(
                evaluate_approve(u, decisions, *actor, &items).map_err(|e| e.code()),
                expected.map_err(|r| ApprovalError::from(r).code()),
                "{name}, {actor}"
            );
        }
    }
}
/// The phase 9 review's R3: the predicate's refusal is one of three, each the engine's own error,
/// with its code and its text unchanged.
#[test]
fn each_refusal_is_the_engines_error_byte_for_byte() {
    for (refusal, code, text) in [
        (
            ApproveRefusal::AlreadyDecided,
            "UNIT_ALREADY_DECIDED",
            "the unit is already decided",
        ),
        (
            ApproveRefusal::SodViolation,
            "SOD_VIOLATION",
            "the actor authored or submitted this unit",
        ),
        (
            ApproveRefusal::DuplicateVote,
            "DUPLICATE_VOTE",
            "the actor already voted in this generation",
        ),
    ] {
        let error = ApprovalError::from(refusal);
        assert_eq!((error.code(), error.to_string().as_str()), (code, text));
    }
    let author = Uuid::new_v4();
    let u = unit(1, Uuid::new_v4());
    // The authors alone judge the separation of duties, from any iterator of them.
    assert_eq!(
        approve_eligibility(&u, vec![Uuid::new_v4(), author], &[], author),
        ApproveEligibility {
            approvals: 0,
            refusal: Some(ApproveRefusal::SodViolation),
        }
    );
    assert_eq!(
        approve_eligibility(&u, std::iter::empty(), &[], author).refusal,
        None
    );
}
#[test]
fn the_policy_overrides_per_kind_and_falls_back_to_star() {
    let p = Policy {
        default_quorum: 1,
        overrides: [("promotion".to_owned(), 0u32)].into_iter().collect(),
    };
    assert_eq!(p.quorum_for("promotion"), 0);
    assert_eq!(p.quorum_for("prices"), 1);
}
/// Every variant's `code()`, which the gears map to their wire codes. The expectation is an
/// exhaustive match, so a new variant does not compile until its code is pinned here.
#[test]
fn every_error_has_a_stable_code() {
    fn expected(error: &ApprovalError) -> &'static str {
        match error {
            ApprovalError::SodViolation => "SOD_VIOLATION",
            ApprovalError::AlreadyDecided => "UNIT_ALREADY_DECIDED",
            ApprovalError::DuplicateVote => "DUPLICATE_VOTE",
            ApprovalError::Contended => "UNIT_CONTENDED",
            ApprovalError::Locked { .. } => "ROW_LOCKED_PENDING",
            ApprovalError::NotSubmitter => "NOT_SUBMITTER",
            ApprovalError::NoteRequired => "NOTE_REQUIRED",
            ApprovalError::NoteTooLong => "NOTE_TOO_LONG",
            ApprovalError::UnitNotFound { .. } => "UNIT_NOT_FOUND",
            ApprovalError::Empty | ApprovalError::InvalidSubmit { .. } => "VALIDATION",
            ApprovalError::ApplyRefused { .. } => "APPLY_REFUSED",
            ApprovalError::GenerationMismatch { .. } => "GENERATION_MISMATCH",
            ApprovalError::Db(_) => "DB",
            ApprovalError::Store(_) => "STORE",
        }
    }
    let every = [
        ApprovalError::SodViolation,
        ApprovalError::AlreadyDecided,
        ApprovalError::DuplicateVote,
        ApprovalError::Contended,
        ApprovalError::Locked {
            item_type: "sku".into(),
            item_id: Uuid::nil(),
        },
        ApprovalError::NotSubmitter,
        ApprovalError::NoteRequired,
        ApprovalError::NoteTooLong,
        ApprovalError::UnitNotFound {
            unit_id: Uuid::nil(),
        },
        ApprovalError::Empty,
        ApprovalError::InvalidSubmit {
            code: "USAGE_NEEDS_METER",
            field: "unit".into(),
            detail: String::new(),
        },
        ApprovalError::ApplyRefused {
            code: "SKU_NAME_TAKEN",
            detail: String::new(),
        },
        ApprovalError::GenerationMismatch {
            seen: 1,
            current: 2,
        },
        ApprovalError::Db(sea_orm::DbErr::Custom("driver".into())),
        ApprovalError::Store("store".into()),
    ];
    let pinned: Vec<&str> = every.iter().map(expected).collect();
    assert_eq!(
        pinned,
        [
            "SOD_VIOLATION",
            "UNIT_ALREADY_DECIDED",
            "DUPLICATE_VOTE",
            "UNIT_CONTENDED",
            "ROW_LOCKED_PENDING",
            "NOT_SUBMITTER",
            "NOTE_REQUIRED",
            "NOTE_TOO_LONG",
            "UNIT_NOT_FOUND",
            "VALIDATION",
            "VALIDATION",
            "APPLY_REFUSED",
            "GENERATION_MISMATCH",
            "DB",
            "STORE",
        ],
        "one of each variant, in declaration order"
    );
    for error in &every {
        assert_eq!(error.code(), expected(error), "{error:?}");
    }
    let db_error = sea_orm::DbErr::Custom("retry classification".into());
    let error = ApprovalError::from(db_error.clone());
    assert_eq!(error.db_err(), Some(&db_error));
    assert!(ApprovalError::Contended.db_err().is_none());
}
