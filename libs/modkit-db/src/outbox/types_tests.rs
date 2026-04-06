use super::*;

// -- Bug 3: validate() doesn't catch zero retry_base or zero degradation_threshold --

#[test]
#[should_panic(expected = "retry_base must be > 0")]
fn validate_rejects_zero_retry_base() {
    WorkerTuning::processor_default()
        .retry_base(Duration::ZERO)
        .validate();
}

#[test]
#[should_panic(expected = "degradation_threshold must be >= 1")]
fn validate_rejects_zero_degradation_threshold() {
    WorkerTuning::processor_default()
        .degradation_threshold(0)
        .validate();
}

#[test]
fn lease_config_default() {
    let cfg = LeaseConfig::default();
    assert_eq!(cfg.duration, Duration::from_secs(30));
    assert_eq!(cfg.headroom, Duration::from_secs(2));
    assert_eq!(cfg.handler_budget(), Duration::from_secs(28));
}

#[test]
#[should_panic(expected = "headroom")]
fn lease_config_rejects_headroom_equal_to_duration() {
    LeaseConfig {
        duration: Duration::from_secs(5),
        headroom: Duration::from_secs(5),
    }
    .validate();
}

#[test]
#[should_panic(expected = "headroom")]
fn lease_config_rejects_headroom_greater_than_duration() {
    LeaseConfig {
        duration: Duration::from_secs(5),
        headroom: Duration::from_secs(10),
    }
    .validate();
}
