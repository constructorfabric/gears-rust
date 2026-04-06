use super::*;

#[test]
fn first_call_returns_true() {
    let throttle = ThrottledLog::new(Duration::from_secs(10));
    assert!(throttle.should_log());
}

#[test]
fn second_call_within_interval_returns_false() {
    let throttle = ThrottledLog::new(Duration::from_secs(10));
    assert!(throttle.should_log());
    assert!(!throttle.should_log());
}
