use super::*;
use crate::contracts::{ActivitySpec, ExecutionContract};
use proptest::prelude::*;

fn contract(count: usize, version: u64, seconds: u64) -> ExecutionContract {
    ExecutionContract {
        name: format!("property.task.v{version}"),
        activities: (0..count)
            .map(|i| ActivitySpec {
                id: ActivityId(format!("step-{i}")),
                timeout: Duration::from_secs(seconds),
                retry: RetryPolicy::default(),
            })
            .collect(),
        parallel_groups: vec![],

        flow: None,
    }
}
proptest! {
    #![proptest_config(ProptestConfig { cases:128, failure_persistence:Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/unit/contract_property.proptest-regressions")))), ..ProptestConfig::default() })]
    #[test]
    fn valid_contract_roundtrip_preserves_fingerprint_and_stages(count in 1usize..=128, version in 1u64..u64::MAX, seconds in 1u64..=86_400, parallel in any::<bool>()) {
        let mut value = contract(count, version, seconds);
        if parallel && count >= 2 { value.parallel_groups.push(value.activities.iter().map(|a| a.id.clone()).collect()); }
        prop_assert!(value.validate().is_ok());
        let encoded = serde_json::to_vec(&value).unwrap();
        let decoded: ExecutionContract = serde_json::from_slice(&encoded).unwrap();
        prop_assert_eq!(decoded.fingerprint().unwrap(), value.fingerprint().unwrap());
        prop_assert_eq!(decoded.stages().unwrap(), value.stages().unwrap());
        prop_assert_eq!(decoded, value);
    }
    #[test]
    fn submillisecond_timeouts_have_distinct_fingerprints(millis in 0u64..86_400_000, nanos in 1u64..999_999) {
        let mut value = contract(1, 1, 1);
        value.activities[0].timeout = Duration::from_nanos(millis * 1_000_000 + nanos);
        let first = value.fingerprint().unwrap();
        value.activities[0].timeout += Duration::from_nanos(1);
        prop_assert_ne!(first, value.fingerprint().unwrap());
    }
    #[test]
    fn duplicate_ids_and_zero_versions_cannot_be_registered(count in 2usize..=128) {
        let mut value = contract(count, 1, 1);
        value.activities[1].id = value.activities[0].id.clone();
        prop_assert!(value.validate().is_err());
        prop_assert!(value.fingerprint().is_err());
        let value = contract(count, 0, 1);
        prop_assert!(value.validate().is_err());
    }
    #[test]
    fn arbitrary_error_details_cannot_escape_as_public_codes(detail in ".*") {
        let actual = ActivityError::retryable(&detail).code();
        prop_assert!(!actual.is_empty() && actual.len() <= 64);
        prop_assert!(actual.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        let valid = !detail.is_empty() && detail.len() <= 64 && detail.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if valid { prop_assert_eq!(actual, detail); }
        else { prop_assert_eq!(actual, "activity_failed"); }
    }
    #[test]
    fn retry_delay_is_monotone_bounded_and_never_shortens_provider(attempt in any::<u32>(), initial in 1u64..=10_000, extra in 0u64..=10_000, jitter in any::<u16>(), provider in 0u64..=100_000) {
        let policy = RetryPolicy { max_attempts: 10, initial_delay_secs: initial, max_delay_secs: initial + extra };
        let delay = policy.delay(attempt, Some(Duration::from_secs(provider)), jitter);
        prop_assert!(delay >= Duration::from_secs(provider));
        prop_assert!(delay <= Duration::from_millis(policy.max_delay_secs * 1200).max(Duration::from_secs(provider)));
        prop_assert!(policy.delay(attempt.saturating_add(1), None, jitter) >= policy.delay(attempt, None, jitter));
    }
}
