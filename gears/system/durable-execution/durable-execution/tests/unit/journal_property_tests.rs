use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases:128, failure_persistence:Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/unit/journal_property.proptest-regressions")))), ..ProptestConfig::default() })]
    #[test]
    fn transitions_preserve_checkpoints_and_reject_every_stale_claim(operations in prop::collection::vec(0u8..8, 1..96), parallel in any::<bool>()) {
        let (mut journal, mut definition, mut now) = setup();
        if parallel {
            definition.parallel_groups = vec![definition.activities.iter().map(|a| a.id.clone()).collect()];
            journal = Journal::new(journal.run.id, journal.run.owner.clone(), &definition, Value::Null, now).unwrap();
        }
        let mut claims = Vec::new();
        let mut checkpoints = std::collections::BTreeMap::new();
        for operation in operations {
            now += chrono::Duration::seconds(1);
            match operation {
                0 => {
                    if let Some(claim) = journal.claim(&definition, journal.delivery_generation, now, Duration::from_secs(4)).unwrap() {
                        prop_assert!(!checkpoints.contains_key(&claim.step));
                        claims.push(claim);
                    }
                }
                1 | 2 => if let Some(&claim) = claims.last() {
                    let owned = journal.owns(claim, now);
                    let result = if operation == 1 {
                        journal.complete(claim, serde_json::json!({"step":claim.step}), now)
                    } else {
                        journal.fail(claim, &ActivityError::retryable("transient"), &definition, now, 0)
                    };
                    if owned {
                        result.unwrap();
                        if operation == 1 && journal.run.activities[claim.step].status == ActivityStatus::Succeeded { checkpoints.insert(claim.step, serde_json::json!({"step":claim.step})); }
                    } else { prop_assert!(matches!(result, Err(DomainError::LeaseLost))); }
                },
                3 => { journal.request_cancel(now); }
                4 => {
                    now += chrono::Duration::seconds(5);
                    journal.recover(now).unwrap();
                }
                5 => {
                    let status = journal.run.status;
                    if matches!(status, RunStatus::Failed | RunStatus::Cancelled) {
                        journal.continue_execution(journal.run.execution_epoch, status, &definition, now).unwrap();
                    }
                }
                6 => if let Some(&claim) = claims.last() {
                    let owned = journal.owns(claim, now);
                    let renewed = journal.heartbeat(claim, now, Duration::from_secs(4)).unwrap();
                    prop_assert_eq!(renewed, owned);
                },
                _ => if let Some(&claim) = claims.last() && journal.owns(claim, now) {
                    journal.release(claim, now).unwrap();
                },
            }
            for (&step, value) in &checkpoints {
                prop_assert_eq!(journal.run.activities[step].status, ActivityStatus::Succeeded);
                prop_assert_eq!(journal.run.activities[step].result.as_ref(), Some(value));
            }
            // Exercise persisted state, then continue transitions on the restored journal.
            let encoded = serde_json::to_value(&journal).unwrap();
            journal = serde_json::from_value(encoded.clone()).unwrap();
            prop_assert_eq!(serde_json::to_value(&journal).unwrap(), encoded);
            for &old in &claims {
                if !journal.owns(old, now) {
                    let before = serde_json::to_value(&journal).unwrap();
                    prop_assert!(matches!(journal.complete(old, Value::Null, now), Err(DomainError::LeaseLost)));
                    prop_assert_eq!(serde_json::to_value(&journal).unwrap(), before);
                }
            }
        }
    }
}
