//! Tests for the recognition input value type — the timing predicate and the
//! SSP-missing decision (the input-level half of the §4.4 gate).

use super::*;

/// A point-in-time spec (no deferral), single-PO.
fn point_in_time() -> RecognitionInput {
    RecognitionInput {
        policy_ref: "policy.v1".to_owned(),
        timing: RecognitionTiming::PointInTime,
        po_allocation_group: Some("default".to_owned()),
        multi_po: false,
        ssp_snapshot_ref: None,
        subscription_ref: None,
        vc_estimate_ref: None,
        vc_method_ref: None,
        immaterial_one_shot_sku: false,
    }
}

#[test]
fn point_in_time_is_not_deferred() {
    assert!(!RecognitionTiming::PointInTime.is_deferred());
}

#[test]
fn straight_line_is_deferred() {
    let t = RecognitionTiming::StraightLine {
        periods: 12,
        first_period_id: None,
    };
    assert!(t.is_deferred());
}

#[test]
fn single_po_never_requires_ssp() {
    let input = point_in_time();
    assert!(!input.ssp_snapshot_missing());
}

#[test]
fn multi_po_without_ref_is_ssp_missing() {
    let input = RecognitionInput {
        multi_po: true,
        ssp_snapshot_ref: None,
        ..point_in_time()
    };
    assert!(input.ssp_snapshot_missing());
}

#[test]
fn multi_po_with_blank_ref_is_ssp_missing() {
    let input = RecognitionInput {
        multi_po: true,
        ssp_snapshot_ref: Some(String::new()),
        ..point_in_time()
    };
    assert!(input.ssp_snapshot_missing());
}

#[test]
fn multi_po_with_ref_is_present() {
    let input = RecognitionInput {
        multi_po: true,
        ssp_snapshot_ref: Some("ssp.snap.v3".to_owned()),
        ..point_in_time()
    };
    assert!(!input.ssp_snapshot_missing());
}
