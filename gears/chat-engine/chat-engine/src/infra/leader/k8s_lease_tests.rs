use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Instant;

#[test]
fn validate_rejects_subsecond_lease_duration() {
    let err = test_config()
        .with_timing(Duration::from_millis(500), Duration::from_millis(100))
        .validate()
        .expect_err("should reject sub-second lease_duration");

    assert!(err.to_string().contains("at least 1 second"));
}

#[test]
fn validate_rejects_lease_duration_outside_k8s_range() {
    let err = test_config()
        .with_timing(
            Duration::from_secs(i32::MAX as u64 + 1),
            Duration::from_secs(1),
        )
        .validate()
        .expect_err("should reject lease_duration outside i32 range");

    assert!(err.to_string().contains("i32 seconds range"));
}

#[test]
fn cannot_acquire_when_other_holder_is_still_fresh() {
    let now = Timestamp::now();
    let renew_time = now
        .checked_add(SignedDuration::from_secs(-3))
        .expect("trivial timestamp arithmetic");

    let acquired = can_acquire_leadership(Some("pod-b"), Some(renew_time), 15, "pod-a", now);

    assert!(!acquired);
}

#[test]
fn can_acquire_when_other_holder_is_expired() {
    let now = Timestamp::now();
    let renew_time = now
        .checked_add(SignedDuration::from_secs(-30))
        .expect("trivial timestamp arithmetic");

    let acquired = can_acquire_leadership(Some("pod-b"), Some(renew_time), 15, "pod-a", now);

    assert!(acquired);
}

#[test]
fn can_acquire_when_already_holder() {
    let now = Timestamp::now();
    let renew_time = now
        .checked_add(SignedDuration::from_secs(-3))
        .expect("trivial timestamp arithmetic");

    let acquired = can_acquire_leadership(Some("pod-a"), Some(renew_time), 15, "pod-a", now);

    assert!(acquired);
}

#[tokio::test]
async fn stop_times_out_and_aborts_unresponsive_work() {
    let started = Arc::new(AtomicBool::new(false));
    let started_flag = Arc::clone(&started);
    let child = CancellationToken::new();
    let handle = tokio::spawn(async move {
        started_flag.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_mins(1)).await;
        Ok::<(), anyhow::Error>(())
    });
    let role = ActiveRole { child, handle };

    let begun = Instant::now();
    role.stop_with_timeout("test", Duration::from_millis(20))
        .await;

    assert!(started.load(Ordering::SeqCst));
    assert!(begun.elapsed() < Duration::from_secs(1));
}

fn test_config() -> K8sLeaseConfig {
    K8sLeaseConfig {
        namespace: "default".to_owned(),
        identity: "pod-a".to_owned(),
        lease_prefix: "chat-engine".to_owned(),
        lease_duration: Duration::from_secs(15),
        renew_period: Duration::from_secs(2),
    }
}
