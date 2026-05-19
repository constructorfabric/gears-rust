use super::{HealthLog, classify_health_transition};

#[test]
fn first_failure_after_healthy_is_initial_failure() {
    assert_eq!(
        classify_health_transition(Some("healthy"), "probe_failed"),
        HealthLog::InitialFailure
    );
}

#[test]
fn first_failure_at_startup_is_initial_failure() {
    assert_eq!(
        classify_health_transition(None, "probe_failed"),
        HealthLog::InitialFailure
    );
}

#[test]
fn repeated_failure_is_persistent_failure() {
    assert_eq!(
        classify_health_transition(Some("probe_failed"), "probe_failed"),
        HealthLog::PersistentFailure
    );
}

#[test]
fn recovery_from_failure_is_recovery() {
    assert_eq!(
        classify_health_transition(Some("probe_failed"), "healthy"),
        HealthLog::Recovery
    );
}

#[test]
fn healthy_to_healthy_is_healthy() {
    assert_eq!(
        classify_health_transition(Some("healthy"), "healthy"),
        HealthLog::Healthy
    );
}

#[test]
fn first_healthy_is_healthy() {
    assert_eq!(
        classify_health_transition(None, "healthy"),
        HealthLog::Healthy
    );
}
