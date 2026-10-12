#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::switch_actor;
use bss_approval::{Decision, Unit, UnitState, Verdict};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

fn unit(generation: i32) -> Unit {
    Unit {
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        kind: "plan_revision".into(),
        ref_type: "plan_revision".into(),
        ref_id: Uuid::new_v4(),
        state: UnitState::Approved,
        common_effective_date: None,
        quorum_required: 2,
        generation,
        submitted_by: Uuid::new_v4(),
        submitted_at: OffsetDateTime::UNIX_EPOCH,
        submit_note: None,
        decided_at: None,
        decided_note: None,
        snapshot: json!({}),
        snapshot_hash: "hash".into(),
        version: 1,
    }
}
fn decision(u: &Unit, generation: i32, verdict: Verdict, minutes: i64, stale: bool) -> Decision {
    Decision {
        unit_id: u.id,
        actor: Uuid::new_v4(),
        generation,
        verdict,
        note: None,
        at: OffsetDateTime::UNIX_EPOCH + Duration::minutes(minutes),
        stale,
    }
}

#[test]
fn the_switch_names_the_latest_current_approver() {
    let u = unit(2);
    let first = decision(&u, 2, Verdict::Approve, 1, false);
    let last = decision(&u, 2, Verdict::Approve, 5, false);
    // Later than both, but not a current approval: a reject, a stale vote, an older generation.
    let rejected = decision(&u, 2, Verdict::Reject, 9, false);
    let stale = decision(&u, 2, Verdict::Approve, 9, true);
    let older = decision(&u, 1, Verdict::Approve, 9, false);
    assert_eq!(switch_actor(&u, std::slice::from_ref(&first)), first.actor);
    let decisions = [stale, last.clone(), older, first, rejected];
    assert_eq!(switch_actor(&u, &decisions), last.actor);
}

#[test]
fn the_switch_names_the_submitter_when_no_current_approval_was_recorded() {
    let u = unit(2);
    assert_eq!(switch_actor(&u, &[]), u.submitted_by, "quorum 0");
    let not_current = [
        decision(&u, 2, Verdict::Reject, 1, false),
        decision(&u, 2, Verdict::Approve, 2, true),
        decision(&u, 1, Verdict::Approve, 3, false),
    ];
    assert_eq!(switch_actor(&u, &not_current), u.submitted_by);
}

/// A lock poisoned while it held the apply's outcome still answers it: the door must not commit a
/// publication without its `PlanRevisionPublished` (PS-34). The same holds for the refusal slot.
#[test]
fn a_poisoned_lock_keeps_what_the_apply_and_the_refusal_recorded() {
    let ctx = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .subject_type("user")
        .build()
        .unwrap();
    let subject = super::PlanRevisionSubject::new(
        ctx,
        std::sync::Arc::new(toolkit::ClientHub::default()),
        Uuid::new_v4(),
        OffsetDateTime::UNIX_EPOCH,
    );
    let superseded = Uuid::new_v4();
    let review = subject.review.clone();
    let poisoned = std::thread::spawn(move || {
        let mut held = review.lock().unwrap();
        held.published = true;
        held.superseded = Some(superseded);
        panic!("the lock is poisoned while it holds the apply's outcome");
    })
    .join();
    assert!(poisoned.is_err());
    assert!(subject.review.is_poisoned());
    assert!(subject.published_now(), "the apply published the revision");
    assert_eq!(subject.superseded(), Some(superseded));
    let refused = subject.refused.clone();
    let poisoned = std::thread::spawn(move || {
        let _held = refused.lock().unwrap();
        panic!("the refusal slot is poisoned");
    })
    .join();
    assert!(poisoned.is_err());
    subject.refuse(toolkit_canonical_errors::CanonicalError::internal("kept").create());
    assert!(subject.take_refusal().is_some(), "the refusal is kept");
}
