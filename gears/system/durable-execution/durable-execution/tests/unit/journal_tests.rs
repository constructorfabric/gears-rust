use super::*;
use durable_execution_sdk::contracts::ActivitySpec;
use durable_execution_sdk::{ActivityId, RetryPolicy};
use uuid::Uuid;
pub(super) fn definition() -> ExecutionContract {
    ExecutionContract {
        name: "test.run.v1".into(),
        parallel_groups: Vec::new(),
        activities: ["first", "second"]
            .into_iter()
            .map(|id| ActivitySpec {
                id: ActivityId(id.into()),
                timeout: Duration::from_secs(90),
                retry: RetryPolicy::default(),
            })
            .collect(),

        flow: None,
    }
}
fn setup() -> (Journal, ExecutionContract, DateTime<Utc>) {
    let d = definition();
    let now = DateTime::from_timestamp(1_000_000, 0).unwrap();
    let j = Journal::new(
        RunId(Uuid::new_v4()),
        ExecutionOwner {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        },
        &d,
        Value::Null,
        now,
    )
    .unwrap();
    (j, d, now)
}

#[test]
fn late_delivery_keeps_the_activity_selected_at_its_due_time() {
    let (_, mut d, now) = setup();
    d.parallel_groups = vec![d.activities.iter().map(|a| a.id.clone()).collect()];
    let mut journal = Journal::new(
        RunId(Uuid::new_v4()),
        ExecutionOwner {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        },
        &d,
        Value::Null,
        now,
    )
    .unwrap();
    journal.parallel.as_mut().unwrap().steps[0].due_at = now + chrono::Duration::seconds(10);
    assert_eq!(journal.delivery_activity().as_deref(), Some("second"));
    assert!(
        journal
            .claim(
                &d,
                0,
                now - chrono::Duration::seconds(1),
                Duration::from_mins(2)
            )
            .unwrap()
            .is_none()
    );
    let claim = journal
        .claim(
            &d,
            0,
            now + chrono::Duration::seconds(20),
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    assert_eq!(claim.step, 1);
}
#[test]
fn manual_retry_preserves_successful_result_and_lifetime_attempts() {
    let (mut journal, mut definition, now) = setup();
    definition.activities[1].retry.max_attempts = 1;
    journal.fingerprint = definition.fingerprint().unwrap();
    let first = journal
        .claim(&definition, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    journal
        .complete(first, serde_json::json!({"saved": 7}), now)
        .unwrap();
    let second = journal
        .claim(
            &definition,
            journal.delivery_generation,
            now,
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    journal
        .fail(
            second,
            &ActivityError::retryable("network"),
            &definition,
            now,
            0,
        )
        .unwrap();
    assert_eq!(journal.run.status, RunStatus::Failed);
    assert!(
        journal
            .continue_execution(0, RunStatus::Failed, &definition, now)
            .unwrap()
    );
    assert!(
        !journal
            .continue_execution(0, RunStatus::Failed, &definition, now)
            .unwrap()
    );
    let retry = journal
        .claim(
            &definition,
            journal.delivery_generation,
            now,
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    assert_eq!(retry.step, 1);
    assert_eq!(
        journal.run.activities[0].result,
        Some(serde_json::json!({"saved": 7}))
    );
    assert_eq!(journal.run.activities[1].attempts, 2);
    assert_eq!(journal.run.activities[1].budget_attempts, 1);
    assert_eq!(
        journal.run.previous_executions[0].activities[1]
            .error_code
            .as_deref(),
        Some("network")
    );
    journal
        .fail(
            retry,
            &ActivityError::permanent("invalid_output"),
            &definition,
            now,
            0,
        )
        .unwrap();
    // A delayed duplicate of the old retry does not restart a new failure.
    assert!(
        !journal
            .continue_execution(0, RunStatus::Failed, &definition, now)
            .unwrap()
    );
    assert_eq!(journal.run.status, RunStatus::Failed);
    assert!(
        journal
            .continue_execution(1, RunStatus::Failed, &definition, now)
            .unwrap()
    );
}

#[test]
fn delayed_stop_cannot_cancel_resumed_execution() {
    let (mut journal, definition, now) = setup();
    assert!(journal.request_cancel_at(0, now, None).unwrap());
    let revision = journal.revision;
    assert!(!journal.request_cancel_at(0, now, None).unwrap());
    assert_eq!(journal.revision, revision);
    assert!(
        journal
            .continue_execution(0, RunStatus::Cancelled, &definition, now)
            .unwrap()
    );
    assert_eq!(
        journal.request_cancel_at(0, now, None),
        Err(DomainError::ConcurrentUpdate)
    );
    assert_eq!(journal.run.status, RunStatus::Queued);
    assert!(!journal.cancellation_requested);
    assert!(journal.request_cancel_at(1, now, None).unwrap());
}

#[test]
fn stop_reason_stays_with_its_epoch() {
    let (mut journal, definition, now) = setup();
    let claim = journal
        .claim(&definition, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    assert!(
        journal
            .request_cancel_at(0, now, Some("Stop requested by operator"))
            .unwrap()
    );
    assert!(!journal.request_cancel_at(0, now, Some("other")).unwrap());
    assert_eq!(
        journal.run.stop_reason.as_deref(),
        Some("Stop requested by operator")
    );
    journal.complete(claim, serde_json::json!({}), now).unwrap();
    assert_eq!(journal.run.status, RunStatus::Cancelled);
    assert!(
        journal
            .continue_execution(0, RunStatus::Cancelled, &definition, now)
            .unwrap()
    );
    assert_eq!(journal.run.stop_reason, None);
    assert_eq!(journal.run.execution_epoch, 1);
    assert_eq!(
        journal.run.previous_executions[0].stop_reason.as_deref(),
        Some("Stop requested by operator")
    );
    assert_eq!(
        journal.request_cancel_at(0, now, Some("Stop requested by operator")),
        Err(DomainError::ConcurrentUpdate)
    );
    assert_eq!(journal.run.stop_reason, None);
    journal.run.status = RunStatus::Succeeded;
    assert!(
        !journal
            .request_cancel_at(1, now, Some("Stop requested by operator"))
            .unwrap()
    );
    assert_eq!(journal.run.status, RunStatus::Succeeded);
    assert_eq!(journal.run.stop_reason, None);
}

#[test]
fn resume_requires_confirmed_stop_and_rejects_changed_definition() {
    let (mut journal, definition, now) = setup();
    let claim = journal
        .claim(&definition, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    journal.request_cancel(now);
    assert!(
        journal
            .continue_execution(0, RunStatus::Cancelled, &definition, now)
            .is_err()
    );
    journal
        .fail(claim, &ActivityError::Cancelled, &definition, now, 0)
        .unwrap();
    let mut changed = definition.clone();
    changed.activities.reverse();
    assert!(
        journal
            .continue_execution(0, RunStatus::Cancelled, &changed, now)
            .is_err()
    );
    assert!(
        journal
            .continue_execution(0, RunStatus::Failed, &definition, now)
            .is_err()
    );
    assert!(
        journal
            .continue_execution(0, RunStatus::Cancelled, &definition, now)
            .unwrap()
    );
    assert!(!journal.cancellation_requested);
    assert!(
        journal
            .claim(
                &definition,
                journal.delivery_generation,
                now,
                Duration::from_mins(2)
            )
            .unwrap()
            .is_some()
    );
}
#[test]
fn successful_checkpoint_survives_roundtrip_and_next_claim_skips_it() {
    let (mut j, d, now) = setup();
    let c = j
        .claim(&d, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    j.complete(c, serde_json::json!({"count": 2}), now).unwrap();
    let mut restored: Journal = serde_json::from_slice(&serde_json::to_vec(&j).unwrap()).unwrap();
    assert!(
        restored
            .claim(&d, 0, now, Duration::from_mins(2))
            .unwrap()
            .is_none()
    );
    let c = restored
        .claim(&d, 1, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    assert_eq!(c.step, 1);
    assert_eq!(restored.run.activities[0].attempts, 1);
    restored.complete(c, Value::Null, now).unwrap();
    assert_eq!(restored.run.status, RunStatus::Succeeded);
}
#[test]
fn expired_worker_cannot_checkpoint_after_recovery() {
    let (mut j, d, now) = setup();
    let old = j
        .claim(&d, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    let later = now + chrono::Duration::seconds(121);
    assert!(j.complete(old, Value::Null, later).is_err());
    assert!(j.recover(later).unwrap());
    let current = j
        .claim(&d, 1, later, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    assert_ne!(old.fence, current.fence);
    assert!(j.complete(old, Value::Null, later).is_err());
    j.complete(current, Value::Null, later).unwrap();
    assert_eq!(j.run.activities[0].attempts, 2);
}
#[test]
fn cancellation_after_effect_prevents_next_activity() {
    let (mut j, d, now) = setup();
    let c = j
        .claim(&d, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    j.request_cancel(now);
    assert_eq!(j.run.status, RunStatus::Cancelling);
    j.complete(c, serde_json::json!("confirmed-effect"), now)
        .unwrap();
    assert_eq!(j.run.status, RunStatus::Cancelled);
    assert_eq!(j.run.activities[0].status, ActivityStatus::Succeeded);
    assert_eq!(
        j.run.activities[0].result,
        Some(serde_json::json!("confirmed-effect"))
    );
    assert_eq!(
        j.run.activities[0].attempt_history[0].status,
        ActivityStatus::Succeeded
    );
    assert!(
        j.claim(&d, 0, now, Duration::from_mins(2))
            .unwrap()
            .is_none()
    );
    assert_eq!(j.run.activities[1].attempts, 0);
    j.continue_execution(0, RunStatus::Cancelled, &d, now)
        .unwrap();
    let next = j
        .claim(&d, j.delivery_generation, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    assert_eq!(next.step, 1);
    assert_eq!(j.run.activities[0].attempts, 1);
}
#[test]
fn cancelled_dead_worker_is_finalized_by_recovery() {
    let (mut j, d, now) = setup();
    j.claim(&d, 0, now, Duration::from_mins(2)).unwrap();
    j.request_cancel(now);
    j.recover(now + chrono::Duration::seconds(121)).unwrap();
    assert_eq!(j.run.status, RunStatus::Cancelled);
}
#[test]
fn retry_wait_honors_provider_delay_then_stops_at_limit() {
    let (mut j, mut d, now) = setup();
    d.activities[0].retry.max_attempts = 2;
    j.fingerprint = d.fingerprint().unwrap();
    let c = j
        .claim(&d, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    let error = ActivityError::Retryable {
        code: "rate_limited".into(),
        retry_after: Some(Duration::from_mins(10)),
    };
    j.fail(c, &error, &d, now, 0).unwrap();
    assert_eq!(j.run.status, RunStatus::RetryWait);
    assert!(
        j.claim(&d, 1, now, Duration::from_mins(2))
            .unwrap()
            .is_none()
    );
    let later = now + chrono::Duration::seconds(600);
    let c = j
        .claim(&d, 1, later, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    j.fail(c, &error, &d, later, 0).unwrap();
    assert_eq!(j.run.status, RunStatus::Failed);
}
#[test]
fn shutdown_releases_work_without_user_cancellation() {
    let (mut j, d, now) = setup();
    let c = j
        .claim(&d, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    j.release(c, now).unwrap();
    assert_eq!(j.run.status, RunStatus::Queued);
    assert!(!j.cancellation_requested);
}
#[test]
fn changed_definition_blocks_existing_run() {
    let (mut j, mut d, now) = setup();
    d.activities.reverse();
    assert!(
        j.claim(&d, 0, now, Duration::from_mins(2))
            .unwrap()
            .is_none()
    );
    assert_eq!(j.run.status, RunStatus::Blocked);
}

#[test]
fn parallel_stop_does_not_advance_an_exhausted_delivery_generation() {
    let (seed, mut definition, now) = setup();
    definition.parallel_groups = vec![definition.activities.iter().map(|s| s.id.clone()).collect()];
    let mut journal =
        Journal::new(seed.run.id, seed.run.owner, &definition, Value::Null, now).unwrap();
    let claim = journal
        .claim(&definition, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    journal.delivery_generation = i64::MAX;

    journal.request_cancel(now);

    assert_eq!(journal.delivery_generation, i64::MAX);
    assert_eq!(journal.run.status, RunStatus::Cancelling);
    assert!(journal.run.next_attempt_at.is_none());
    assert_eq!(
        journal.run.activities[claim.step].status,
        ActivityStatus::Running
    );
    assert_eq!(
        journal.run.activities[1 - claim.step].status,
        ActivityStatus::Cancelled
    );
    journal.recover(now + chrono::Duration::minutes(3)).unwrap();
    assert_eq!(journal.run.status, RunStatus::Cancelled);
    assert_eq!(journal.delivery_generation, i64::MAX);
    assert!(
        journal
            .run
            .activities
            .iter()
            .all(|activity| { activity.status == ActivityStatus::Cancelled })
    );
}

#[test]
fn parallel_checkpoint_retry_and_barrier_use_independent_fences() {
    let (seed, mut d, now) = setup();
    d.parallel_groups = vec![d.activities.iter().map(|s| s.id.clone()).collect()];
    let mut next_stage = d.activities[0].clone();
    next_stage.id = ActivityId("next-stage".into());
    d.activities.push(next_stage);
    let mut j = Journal::new(seed.run.id, seed.run.owner, &d, Value::Null, now).unwrap();
    let a = j
        .claim(&d, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    assert_eq!(j.run.status, RunStatus::Running);
    assert_eq!(j.run.next_attempt_at, Some(now));
    let b = j
        .claim(&d, j.delivery_generation, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    assert_ne!(a.step, b.step);
    assert!(j.owns(a, now));
    assert!(j.owns(b, now));
    assert!(j.run.next_attempt_at.is_none());
    j.fail(a, &ActivityError::permanent("invalid_output"), &d, now, 0)
        .unwrap();
    assert_eq!(j.run.status, RunStatus::Running);
    j.complete(b, Value::from("checkpoint"), now).unwrap();
    assert_eq!(j.run.status, RunStatus::Failed);
    assert!(j.continue_execution(0, RunStatus::Failed, &d, now).unwrap());
    let retry = j
        .claim(&d, j.delivery_generation, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    assert_eq!(retry.step, a.step);
    assert!(!j.owns(a, now));
    assert_eq!(
        j.run.activities[b.step].result,
        Some(Value::from("checkpoint"))
    );
    j.complete(retry, Value::Null, now).unwrap();
    let next_stage = j
        .claim(&d, j.delivery_generation, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    assert_eq!(next_stage.step, 2);
    j.complete(next_stage, Value::Null, now).unwrap();
    assert_eq!(j.run.status, RunStatus::Succeeded);
}

#[path = "journal_property_tests.rs"]
mod properties;

#[test]
fn cancellation_racing_with_final_success_keeps_the_run_successful() {
    let (seed, mut definition, now) = setup();
    definition.activities.truncate(1);
    let mut journal =
        Journal::new(seed.run.id, seed.run.owner, &definition, Value::Null, now).unwrap();
    let claim = journal
        .claim(&definition, 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    journal
        .request_cancel_at(0, now, Some("operator stop"))
        .unwrap();
    journal
        .complete(claim, serde_json::json!("confirmed-effect"), now)
        .unwrap();
    assert_eq!(journal.run.status, RunStatus::Succeeded);
    assert_eq!(
        journal.run.activities[0].result,
        Some(serde_json::json!("confirmed-effect"))
    );
    assert_eq!(
        journal.run.activities[0].attempt_history[0].status,
        ActivityStatus::Succeeded
    );
    assert_eq!(journal.run.next_attempt_at, None);
    assert_eq!(journal.lease_until, None);
    let before = serde_json::to_value(&journal).unwrap();
    assert_eq!(
        journal.complete(claim, Value::Null, now),
        Err(DomainError::LeaseLost)
    );
    assert_eq!(serde_json::to_value(&journal).unwrap(), before);
}

#[test]
fn rejected_continuations_do_not_change_journal_or_stop_history() {
    for parallel in [false, true] {
        let (seed, mut definition, now) = setup();
        if parallel {
            definition.parallel_groups = vec![
                definition
                    .activities
                    .iter()
                    .map(|step| step.id.clone())
                    .collect(),
            ];
        }
        let mut journal =
            Journal::new(seed.run.id, seed.run.owner, &definition, Value::Null, now).unwrap();
        journal
            .request_cancel_at(0, now, Some("keep this reason"))
            .unwrap();
        let before = serde_json::to_value(&journal).unwrap();
        assert_eq!(
            journal.continue_execution(1, RunStatus::Cancelled, &definition, now),
            Err(DomainError::ConcurrentUpdate)
        );
        assert_eq!(serde_json::to_value(&journal).unwrap(), before);
        assert!(matches!(
            journal.continue_execution(0, RunStatus::Failed, &definition, now),
            Err(DomainError::InvalidState(_))
        ));
        assert_eq!(serde_json::to_value(&journal).unwrap(), before);
        journal.delivery_generation = i64::MAX;
        let before_overflow = serde_json::to_value(&journal).unwrap();
        assert_eq!(
            journal.continue_execution(0, RunStatus::Cancelled, &definition, now),
            Err(DomainError::Internal("delivery generation overflow"))
        );
        assert_eq!(serde_json::to_value(&journal).unwrap(), before_overflow);
    }
}
