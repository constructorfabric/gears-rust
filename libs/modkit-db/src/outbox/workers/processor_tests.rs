use std::time::Duration;

use super::*;

// Helper: create a degraded PartitionMode.
fn degraded(effective_size: u32, consecutive_successes: u32) -> PartitionMode {
    PartitionMode {
        state: PartitionModeState::Degraded {
            effective_size,
            consecutive_successes,
        },
        consecutive_failures: 0,
    }
}

// ---- PartitionMode state machine tests ----

#[test]
fn partition_mode_normal_uses_configured_size() {
    let mode = PartitionMode::new();
    assert_eq!(mode.effective_batch_size(50), 50);
}

#[test]
fn partition_mode_degraded_uses_effective_size() {
    let mode = degraded(4, 2);
    assert_eq!(mode.effective_batch_size(50), 4);
}

#[test]
fn partition_mode_retry_degrades_to_one() {
    let mut mode = PartitionMode::new();
    mode.transition(
        &HandlerResult::Retry {
            reason: "fail".into(),
        },
        50,
        None, // batch handler
        1,    // degrade immediately
    );
    assert!(matches!(
        mode.state,
        PartitionModeState::Degraded {
            effective_size: 1,
            consecutive_successes: 0,
        }
    ));
}

#[test]
fn partition_mode_success_ramps_up() {
    let mut mode = degraded(1, 0);
    // 1 → 2
    mode.transition(&HandlerResult::Success, 50, None, 1);
    assert!(matches!(
        mode.state,
        PartitionModeState::Degraded {
            effective_size: 2,
            consecutive_successes: 1,
        }
    ));
    // 2 → 4
    mode.transition(&HandlerResult::Success, 50, None, 1);
    assert!(matches!(
        mode.state,
        PartitionModeState::Degraded {
            effective_size: 4,
            ..
        }
    ));
    // 4 → 8
    mode.transition(&HandlerResult::Success, 50, None, 1);
    assert!(matches!(
        mode.state,
        PartitionModeState::Degraded {
            effective_size: 8,
            ..
        }
    ));
}

#[test]
fn partition_mode_ramps_up_to_normal() {
    let mut mode = degraded(16, 4);
    // 16 → 32
    mode.transition(&HandlerResult::Success, 32, None, 1);
    // Should transition back to Normal since 32 >= configured(32)
    assert!(matches!(mode.state, PartitionModeState::Normal));
}

#[test]
fn partition_mode_reject_in_normal_degrades() {
    let mut mode = PartitionMode::new();
    mode.transition(
        &HandlerResult::Reject {
            reason: "bad".into(),
        },
        50,
        None, // batch handler — falls back to 1
        1,    // degrade immediately
    );
    assert!(matches!(
        mode.state,
        PartitionModeState::Degraded {
            effective_size: 1,
            consecutive_successes: 0,
        }
    ));
}

#[test]
fn partition_mode_reject_with_processed_count() {
    // PerMessageAdapter handler processed 3 msgs before poison at index 3
    let mut mode = PartitionMode::new();
    mode.transition(
        &HandlerResult::Reject {
            reason: "bad".into(),
        },
        50,
        Some(3), // PerMessageAdapter processed 3 successfully
        1,       // degrade immediately
    );
    assert!(matches!(
        mode.state,
        PartitionModeState::Degraded {
            effective_size: 3,
            consecutive_successes: 0,
        }
    ));
}

#[test]
fn partition_mode_retry_with_processed_count_zero() {
    // PerMessageAdapter failed at the very first message
    let mut mode = PartitionMode::new();
    mode.transition(
        &HandlerResult::Retry {
            reason: "fail".into(),
        },
        50,
        Some(0), // failed at first message
        1,       // degrade immediately
    );
    // max(0, 1) = 1
    assert!(matches!(
        mode.state,
        PartitionModeState::Degraded {
            effective_size: 1,
            consecutive_successes: 0,
        }
    ));
}

#[test]
fn partition_mode_success_in_normal_stays_normal() {
    let mut mode = PartitionMode::new();
    mode.transition(&HandlerResult::Success, 50, None, 1);
    assert!(matches!(mode.state, PartitionModeState::Normal));
}

#[test]
fn partition_mode_full_recovery_cycle() {
    let mut mode = PartitionMode::new();

    // Retry → Degraded(1)
    mode.transition(&HandlerResult::Retry { reason: "x".into() }, 8, None, 1);
    assert_eq!(mode.effective_batch_size(8), 1);

    // Success: 1→2→4→8→Normal
    mode.transition(&HandlerResult::Success, 8, None, 1);
    assert_eq!(mode.effective_batch_size(8), 2);
    mode.transition(&HandlerResult::Success, 8, None, 1);
    assert_eq!(mode.effective_batch_size(8), 4);
    mode.transition(&HandlerResult::Success, 8, None, 1);
    assert!(matches!(mode.state, PartitionModeState::Normal));
    assert_eq!(mode.effective_batch_size(8), 8);
}

// ---- Degradation threshold tests ----

#[test]
fn partition_mode_does_not_degrade_below_threshold() {
    let mut mode = PartitionMode::new();
    // threshold=3, so first two failures should NOT degrade
    mode.transition(&HandlerResult::Retry { reason: "x".into() }, 50, None, 3);
    assert!(matches!(mode.state, PartitionModeState::Normal));
    assert_eq!(mode.consecutive_failures, 1);

    mode.transition(&HandlerResult::Retry { reason: "x".into() }, 50, None, 3);
    assert!(matches!(mode.state, PartitionModeState::Normal));
    assert_eq!(mode.consecutive_failures, 2);

    // Third failure hits threshold → degrades
    mode.transition(&HandlerResult::Retry { reason: "x".into() }, 50, None, 3);
    assert!(matches!(
        mode.state,
        PartitionModeState::Degraded {
            effective_size: 1,
            ..
        }
    ));
    assert_eq!(mode.consecutive_failures, 3);
}

#[test]
fn partition_mode_success_resets_consecutive_failures() {
    let mut mode = PartitionMode::new();
    mode.transition(&HandlerResult::Retry { reason: "x".into() }, 50, None, 3);
    assert_eq!(mode.consecutive_failures, 1);
    mode.transition(&HandlerResult::Success, 50, None, 3);
    assert_eq!(mode.consecutive_failures, 0);
}

// ---- current_backoff tests ----

#[test]
fn current_backoff_no_failures_returns_base() {
    let mode = PartitionMode::new();
    let base = Duration::from_millis(100);
    let max = Duration::from_secs(30);
    assert_eq!(mode.current_backoff(base, max), base);
}

#[test]
fn current_backoff_escalates_exponentially() {
    let base = Duration::from_millis(100);
    let max = Duration::from_secs(30);

    let mut mode = PartitionMode {
        state: PartitionModeState::Normal,
        consecutive_failures: 1,
    };
    // 1 failure: base * 2^0 = 100ms
    assert_eq!(mode.current_backoff(base, max), Duration::from_millis(100));

    mode.consecutive_failures = 2;
    // 2 failures: base * 2^1 = 200ms
    assert_eq!(mode.current_backoff(base, max), Duration::from_millis(200));

    mode.consecutive_failures = 3;
    // 3 failures: base * 2^2 = 400ms
    assert_eq!(mode.current_backoff(base, max), Duration::from_millis(400));
}

#[test]
fn current_backoff_caps_at_max() {
    let base = Duration::from_millis(100);
    let max = Duration::from_millis(500);

    let mode = PartitionMode {
        state: PartitionModeState::Normal,
        consecutive_failures: 10,
    };
    assert_eq!(mode.current_backoff(base, max), max);
}
