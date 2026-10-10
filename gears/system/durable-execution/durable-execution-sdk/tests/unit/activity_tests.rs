use super::*;
use crate::contracts::{
    ActivityDefinition, ActivityInput, ErasedActivity, ExecutionContract, ExecutionDefinition,
};
struct Noop;
#[async_trait]
impl ErasedActivity for Noop {
    async fn execute(&self, _: ActivityContext, _: ActivityInput) -> Result<Value, ActivityError> {
        Ok(Value::Null)
    }
}
fn definition() -> ExecutionDefinition {
    ExecutionDefinition {
        name: "test.sequence.v1".into(),
        activities: vec![ActivityDefinition {
            id: ActivityId("one".into()),
            timeout: Duration::from_secs(90),
            retry: RetryPolicy::default(),
            handler: Arc::new(Noop),
        }],
        parallel_groups: Vec::new(),

        flow: None,
    }
}
#[test]
fn duplicate_and_unversioned_definitions_are_rejected() {
    let mut d = definition();
    d.activities.push(d.activities[0].clone());
    assert!(d.validate().is_err());
    d.activities.pop();
    d.name = "test.sequence".into();
    assert!(d.validate().is_err());
}
#[test]
fn fingerprint_detects_step_policy_and_order_changes() {
    let mut d = definition();
    let first = d.fingerprint().unwrap();
    d.activities[0].retry.max_attempts += 1;
    assert_ne!(first, d.fingerprint().unwrap());
    let mut second = d.activities[0].clone();
    second.id = ActivityId("two".into());
    d.activities.push(second);
    let before = d.fingerprint().unwrap();
    d.activities.reverse();
    assert_ne!(before, d.fingerprint().unwrap());
}
#[test]
fn groups_are_contiguous_disjoint_and_part_of_fingerprint() {
    let mut d = definition();
    for id in ["two", "three"] {
        let mut step = d.activities[0].clone();
        step.id = ActivityId(id.into());
        d.activities.push(step);
    }
    let sequential = d.fingerprint().unwrap();
    d.parallel_groups = vec![vec![ActivityId("one".into()), ActivityId("two".into())]];
    assert_ne!(sequential, d.fingerprint().unwrap());
    assert_eq!(d.stages().unwrap(), vec![vec!["one", "two"], vec!["three"]]);
    d.parallel_groups
        .push(vec![ActivityId("two".into()), ActivityId("three".into())]);
    assert!(d.validate().is_err());
    d.parallel_groups = vec![vec![ActivityId("one".into()), ActivityId("three".into())]];
    assert!(d.validate().is_err());
}
#[test]
fn retry_after_is_never_shortened_and_backoff_is_capped() {
    let p = RetryPolicy::default();
    assert_eq!(p.delay(1, None, 0), Duration::from_secs(15));
    assert_eq!(p.delay(3, None, 0), Duration::from_mins(1));
    assert_eq!(p.delay(u32::MAX, None, 0), Duration::from_mins(5));
    assert_eq!(
        p.delay(1, Some(Duration::from_mins(15)), 1000),
        Duration::from_mins(15)
    );
}
#[test]
fn error_messages_cannot_accidentally_become_public_codes() {
    assert_eq!(
        ActivityError::permanent("request failed: secret=abc").code(),
        "activity_failed"
    );
    assert_eq!(
        ActivityError::retryable("rate_limited").code(),
        "rate_limited"
    );
}

#[path = "contract_property_tests.rs"]
mod properties;

#[test]
fn fingerprints_preserve_submillisecond_timeout_precision() {
    let mut contract = definition().contract();
    contract.activities[0].timeout = Duration::from_nanos(1);
    let first = contract.fingerprint().unwrap();
    contract.activities[0].timeout = Duration::from_nanos(999_999);
    assert_ne!(first, contract.fingerprint().unwrap());
    let decoded: ExecutionContract =
        serde_json::from_slice(&serde_json::to_vec(&contract).unwrap()).unwrap();
    assert_eq!(
        decoded.fingerprint().unwrap(),
        contract.fingerprint().unwrap()
    );
}

#[test]
fn millisecond_aligned_fingerprints_keep_the_persisted_format() {
    let mut contract = definition().contract();
    contract.name = "test.catalog.v1".into();
    contract.activities[0].id = ActivityId("step".into());
    assert_eq!(
        contract.fingerprint().unwrap(),
        "3ffd53645c19231c097b1cd02f3ac546abbc2aa39fdf23c4d61a5443e96b88f3"
    );
}

#[test]
fn retry_delay_bound_is_schedulable_including_jitter() {
    let mut contract = definition().contract();
    contract.activities[0].retry.initial_delay_secs = u64::MAX;
    contract.activities[0].retry.max_delay_secs = u64::MAX;
    assert!(contract.validate().is_err());
    contract.activities[0].retry.initial_delay_secs = 86_400;
    contract.activities[0].retry.max_delay_secs = 86_400;
    assert!(contract.validate().is_ok());
    let delay = contract.activities[0].retry.delay(u32::MAX, None, 1000);
    assert_eq!(delay, Duration::from_mins(1728));
    assert!(
        chrono::Utc::now()
            .checked_add_signed(chrono::Duration::from_std(delay).unwrap())
            .is_some()
    );
    contract.activities[0].retry.max_delay_secs += 1;
    assert!(contract.validate().is_err());
}
