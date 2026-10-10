use super::*;
fn plan(now: DateTime<Utc>) -> StageJournal {
    StageJournal::new(
        vec![
            vec!["parallel-left".into(), "parallel-right".into()],
            vec!["next-stage".into()],
        ],
        now,
    )
    .unwrap()
}
const LEASE: Duration = Duration::from_mins(2);

#[test]
fn independent_claims_and_barrier() {
    let now = Utc::now();
    let mut j = plan(now);
    let a = j.claim(0, now, LEASE).unwrap();
    let b = j.claim(1, now, LEASE).unwrap();
    assert!(j.claim(0, now, LEASE).is_err());
    assert!(j.claim(2, now, LEASE).is_err());
    j.complete(a, Value::from("a"), now).unwrap();
    assert!(j.ready(now).is_empty());
    assert!(j.owns(b, now));
    j.complete(b, Value::from("b"), now).unwrap();
    assert_eq!(j.ready(now), vec![2]);
}

#[test]
fn failed_activity_does_not_stop_sibling_and_retry_preserves_checkpoint() {
    let now = Utc::now();
    let mut j = plan(now);
    let a = j.claim(0, now, LEASE).unwrap();
    j.fail(a, "invalid_output", None, now).unwrap();
    assert_eq!(j.ready(now), vec![1]);
    assert!(j.continue_execution(0, false, now).is_err());
    let b = j.claim(1, now, LEASE).unwrap();
    j.complete(b, Value::from("saved"), now).unwrap();
    assert!(j.ready(now).is_empty());
    assert!(j.continue_execution(0, false, now).unwrap());
    assert!(!j.continue_execution(0, false, now).unwrap());
    assert_eq!(j.ready(now), vec![0]);
    assert_eq!(j.steps[1].result, Some(Value::from("saved")));
    assert_eq!(j.steps[0].attempts, 1);
    assert_eq!(j.steps[0].budget_attempts, 0);
    assert_eq!(j.steps[0].history.len(), 1);
}

#[test]
fn expired_worker_cannot_overwrite_replacement_or_healthy_sibling() {
    let now = Utc::now();
    let mut j = plan(now);
    let old = j.claim(0, now, LEASE).unwrap();
    let sibling = j.claim(1, now, LEASE).unwrap();
    j.heartbeat(sibling, now, LEASE * 2).unwrap();
    let later = now + chrono::Duration::seconds(121);
    j.recover(later);
    assert!(j.owns(sibling, later));
    let new = j.claim(0, later, LEASE).unwrap();
    assert_ne!(new.fence, old.fence);
    assert!(j.complete(old, Value::Null, later).is_err());
    j.complete(new, Value::from("new"), later).unwrap();
    assert_eq!(j.steps[0].history.len(), 2);
}

#[test]
fn stop_waits_for_active_activity_then_resume_reuses_completed_checkpoint() {
    let now = Utc::now();
    let mut j = plan(now);
    let a = j.claim(0, now, LEASE).unwrap();
    let b = j.claim(1, now, LEASE).unwrap();
    j.stop();
    assert!(j.ready(now).is_empty());
    assert!(j.continue_execution(0, true, now).is_err());
    j.complete(a, Value::from("confirmed"), now).unwrap();
    j.fail(b, "cancelled", None, now).unwrap();
    assert!(j.settled());
    let saved = serde_json::to_vec(&j).unwrap();
    let mut j: StageJournal = serde_json::from_slice(&saved).unwrap();
    assert!(j.continue_execution(0, true, now).unwrap());
    assert_eq!(j.ready(now), vec![1]);
    assert_eq!(j.steps[0].result, Some(Value::from("confirmed")));
}
