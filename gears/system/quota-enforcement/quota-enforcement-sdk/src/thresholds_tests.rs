use uuid::Uuid;

use super::threshold_crossings;
use crate::models::QuotaId;

fn quota() -> QuotaId {
    QuotaId::new(Uuid::from_u128(0x2026_0317))
}

/// Every crossing of `pre -> post` against cap 100 with the usual thresholds.
fn against_100(pre: u64, post: u64, marker: Option<u8>) -> Vec<u8> {
    threshold_crossings(quota(), pre, post, Some(100), &[50, 80, 100], marker)
        .map(|crossing| crossing.crossed_thresholds)
        .unwrap_or_default()
}

// --- the specified vector --------------------------------------------------

#[test]
fn a_jump_past_two_thresholds_reports_both_and_the_highest() {
    let crossing = threshold_crossings(quota(), 30, 85, Some(100), &[50, 80, 100], None)
        .expect("an upward move past 50 and 80 crosses them");

    assert_eq!(crossing.crossed_thresholds, vec![50, 80]);
    assert_eq!(crossing.highest_crossed_threshold, 80);
    assert_eq!(crossing.quota_id, quota());
}

#[test]
fn a_threshold_at_or_below_the_marker_does_not_fire_again() {
    assert!(
        threshold_crossings(quota(), 85, 90, Some(100), &[50, 80, 100], Some(80)).is_none(),
        "80 was already emitted this period and 100 is not reached"
    );
}

// --- direction and edges ---------------------------------------------------

#[test]
fn a_downward_or_flat_move_never_crosses_anything() {
    assert!(against_100(90, 40, None).is_empty());
    assert!(against_100(50, 50, None).is_empty());
}

#[test]
fn landing_exactly_on_a_threshold_crosses_it() {
    assert_eq!(against_100(49, 50, None), vec![50]);
}

#[test]
fn leaving_from_exactly_a_threshold_does_not_cross_it_again() {
    assert_eq!(against_100(50, 79, None), Vec::<u8>::new());
    assert_eq!(against_100(50, 80, None), vec![80]);
}

#[test]
fn reaching_the_cap_crosses_the_hundred_percent_threshold() {
    assert_eq!(against_100(0, 100, None), vec![50, 80, 100]);
}

// --- caps and configuration ------------------------------------------------

#[test]
fn an_unbounded_or_zero_cap_has_no_percentage_to_cross() {
    assert!(threshold_crossings(quota(), 0, 500, None, &[50], None).is_none());
    assert!(threshold_crossings(quota(), 0, 500, Some(0), &[50], None).is_none());
}

#[test]
fn no_configured_thresholds_means_no_crossing() {
    assert!(threshold_crossings(quota(), 0, 100, Some(100), &[], None).is_none());
}

#[test]
fn duplicate_and_unordered_thresholds_are_normalized() {
    let crossing = threshold_crossings(quota(), 0, 90, Some(100), &[80, 50, 80], None)
        .expect("both thresholds are crossed");

    assert_eq!(crossing.crossed_thresholds, vec![50, 80]);
}

// --- exact arithmetic ------------------------------------------------------

#[test]
fn percentages_are_exact_rather_than_rounded_through_division() {
    // Cap 3: 33% is 0.99 units, 34% is 1.02, 50% is 1.5. A counter at 1 has
    // passed 33% only; integer division by 100 would have reported none.
    let crossing = threshold_crossings(quota(), 0, 1, Some(3), &[33, 34, 50], None)
        .expect("one unit of three passes 33 percent");

    assert_eq!(crossing.crossed_thresholds, vec![33]);
    assert_eq!(
        threshold_crossings(quota(), 1, 2, Some(3), &[33, 34, 50], Some(33))
            .expect("two of three passes 34 and 50 percent")
            .crossed_thresholds,
        vec![34, 50]
    );
}

#[test]
fn a_counter_near_the_integer_limit_does_not_overflow() {
    let cap = u64::MAX;
    let crossing = threshold_crossings(quota(), 0, cap, Some(cap), &[50, 100], None)
        .expect("filling an enormous cap crosses every threshold");

    assert_eq!(crossing.crossed_thresholds, vec![50, 100]);
}
