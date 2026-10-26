use super::*;
use crate::{ActivityContext, ActivityResultExt, ErrorCode};
use async_trait::async_trait;
struct Noop;
#[async_trait]
impl ErasedActivity for Noop {
    async fn execute(&self, _: ActivityContext, _: ActivityInput) -> Result<Value, ActivityError> {
        Ok(Value::Null)
    }
}
fn definition() -> DefinitionParts {
    DefinitionParts {
        saga: false,
        name: "test.sequence.v1".into(),
        activities: vec![ActivityDefinition {
            compensation: None,
            fanout: None,
            id: ActivityId("one".into()),
            timeout: Duration::from_secs(90),
            retry: RetryPolicy::default(),
            handler: Arc::new(Noop),
        }],
        parallel_groups: Vec::new(),
        flow: DataFlowSpec::independent(["one"]),
    }
}
/// Re-derive the independent flow after a test changes the activity list.
fn rewire(d: &mut DefinitionParts) {
    d.flow = DataFlowSpec::independent(d.activities.iter().map(|a| a.id.0.as_str()));
}
#[test]
fn duplicate_and_unversioned_definitions_are_rejected() {
    let mut d = definition();
    d.activities.push(d.activities[0].clone());
    assert_eq!(
        d.clone().build().unwrap_err().kind(),
        DefinitionErrorKind::DuplicateActivityId
    );
    d.activities.pop();
    d.name = "test.sequence".into();
    assert_eq!(
        d.clone().build().unwrap_err().kind(),
        DefinitionErrorKind::InvalidName
    );
}
#[test]
fn fingerprint_detects_step_policy_and_order_changes() {
    let mut d = definition();
    let first = d.clone().build().unwrap().fingerprint().to_owned();
    d.activities[0].retry.max_attempts += 1;
    assert_ne!(first, d.clone().build().unwrap().fingerprint().to_owned());
    let mut second = d.activities[0].clone();
    second.id = ActivityId("two".into());
    d.activities.push(second);
    rewire(&mut d);
    let before = d.clone().build().unwrap().fingerprint().to_owned();
    d.activities.reverse();
    rewire(&mut d);
    assert_ne!(before, d.clone().build().unwrap().fingerprint().to_owned());
}
#[test]
fn groups_are_contiguous_disjoint_and_part_of_fingerprint() {
    let mut d = definition();
    for id in ["two", "three"] {
        let mut step = d.activities[0].clone();
        step.id = ActivityId(id.into());
        d.activities.push(step);
    }
    rewire(&mut d);
    let sequential = d.clone().build().unwrap().fingerprint().to_owned();
    d.parallel_groups = vec![vec![ActivityId("one".into()), ActivityId("two".into())]];
    assert_ne!(
        sequential,
        d.clone().build().unwrap().fingerprint().to_owned()
    );
    assert_eq!(
        d.clone().build().unwrap().stages(),
        vec![vec!["one", "two"], vec!["three"]]
    );
    d.parallel_groups
        .push(vec![ActivityId("two".into()), ActivityId("three".into())]);
    let error = d.clone().build().unwrap_err();
    assert_eq!(
        (error.kind(), error.step()),
        (DefinitionErrorKind::InvalidStages, Some("two"))
    );
    d.parallel_groups = vec![vec![ActivityId("one".into()), ActivityId("three".into())]];
    assert!(d.clone().build().is_err());
    d.parallel_groups = vec![vec![ActivityId("one".into())]];
    assert_eq!(
        d.clone().build().unwrap_err().kind(),
        DefinitionErrorKind::InvalidParallelStage
    );
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
    let fast = RetryPolicy::exponential(3, Duration::from_millis(200), Duration::from_secs(1));
    assert!(fast.validate().is_ok());
    let precise = RetryPolicy::exponential(2, Duration::from_micros(500), Duration::from_millis(1));
    assert!(precise.validate().is_ok());
    assert_eq!(precise.delay(1, None, 0), Duration::from_micros(500));
    assert_eq!(precise.delay(1, None, 1000), Duration::from_micros(600));
    assert_eq!(fast.delay(2, None, 0), Duration::from_millis(400));
    assert_eq!(RetryPolicy::no_retry().max_attempts, 1);
}
#[test]
fn error_messages_cannot_accidentally_become_public_codes() {
    const STATIC: ErrorCode = ErrorCode::from_static("stock_unavailable");
    assert_eq!(
        ActivityError::permanent("request failed: secret=abc").code(),
        "invalid_error_code"
    );
    assert_eq!(
        ActivityError::retryable("rate_limited").code(),
        "rate_limited"
    );
    assert_eq!(ActivityError::Cancelled.code(), "cancelled");
    assert_eq!(ErrorCode::new("Upper"), None);
    assert_eq!(ErrorCode::new("ok_1").unwrap(), "ok_1");
    assert_eq!(
        ActivityError::permanent(STATIC).to_string(),
        "stock_unavailable"
    );
    let error = ActivityError::retryable_after("throttled", Duration::from_secs(12));
    assert!(error.is_retryable());
    assert!(
        matches!(error, ActivityError::Retryable { retry_after: Some(d), .. } if d == Duration::from_secs(12))
    );
    assert_eq!(
        "x".parse::<u32>()
            .or_permanent("quantity_invalid")
            .unwrap_err()
            .code(),
        "quantity_invalid"
    );
    assert!("1".parse::<u32>().or_retryable("unused").is_ok());
}

#[path = "contract_property_tests.rs"]
mod properties;

#[test]
fn fingerprints_preserve_submillisecond_timeout_precision() {
    let mut contract = definition().build().unwrap().contract().clone();
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
fn new_format_fingerprint_is_stable() {
    let mut contract = definition().build().unwrap().contract().clone();
    contract.name = "test.catalog.v1".into();
    contract.activities[0].id = ActivityId("step".into());
    contract.flow = DataFlowSpec::independent(["step"]);
    contract.stages = vec![StageSpec::Sequential {
        step: ActivityId("step".into()),
    }];
    assert_eq!(
        contract.fingerprint().unwrap(),
        "fd7c995c44d47da8449aea0e1260c0fcb06050010cf4dcf4668af16e3c7eb05b"
    );
}

#[test]
fn retry_delay_bound_is_schedulable_including_jitter() {
    let mut contract = definition().build().unwrap().contract().clone();
    contract.activities[0].retry = RetryPolicy::exponential(10, Duration::MAX, Duration::MAX);
    assert_eq!(
        contract.validate().unwrap_err().kind(),
        DefinitionErrorKind::InvalidRetryPolicy
    );
    contract.activities[0].retry =
        RetryPolicy::exponential(10, RetryPolicy::MAX_DELAY, RetryPolicy::MAX_DELAY);
    assert!(contract.validate().is_ok());
    let delay = contract.activities[0].retry.delay(u32::MAX, None, 1000);
    assert_eq!(delay, Duration::from_mins(1728));
    assert!(
        chrono::Utc::now()
            .checked_add_signed(chrono::Duration::from_std(delay).unwrap())
            .is_some()
    );
    contract.activities[0].retry.max_delay += Duration::from_secs(1);
    assert!(contract.validate().is_err());
}
