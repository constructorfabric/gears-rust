use super::*;

#[test]
fn map_scope_error_empty_scope_maps_to_permission_denied() {
    let err = map_scope_error(&ScopeTranslationError::EmptyScope);
    assert!(
        matches!(err, UsageCollectorError::PermissionDenied { .. }),
        "expected PermissionDenied, got: {err:?}"
    );
}

#[test]
fn map_scope_error_unsupported_predicate_maps_to_permission_denied() {
    let err = map_scope_error(&ScopeTranslationError::UnsupportedPredicate {
        kind: "InGroup/InGroupSubtree".to_owned(),
    });
    assert!(
        matches!(err, UsageCollectorError::PermissionDenied { .. }),
        "expected PermissionDenied, got: {err:?}"
    );
}

fn at(h: u32, m: u32, s: u32, ns: u32) -> DateTime<Utc> {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_nano_opt(h, m, s, ns)
        .unwrap()
        .and_utc()
}

/// Pinned "now" anchor far enough past the test times that the
/// materialized-horizon check is satisfied unconditionally, so the
/// alignment-only tests below isolate the alignment predicate.
fn pinned_now() -> DateTime<Utc> {
    // Day after the `at(...)` anchor — materialized horizon is `now - 1h`
    // which is still well after any 2026-01-01 fixture time.
    chrono::NaiveDate::from_ymd_opt(2026, 1, 2)
        .unwrap()
        .and_hms_nano_opt(0, 0, 0, 0)
        .unwrap()
        .and_utc()
}

#[test]
fn cagg_safe_range_rejects_non_hour_aligned_start() {
    let start = at(10, 30, 0, 0);
    let end = at(11, 0, 0, 0);
    assert!(!cagg_safe_range(start, end, pinned_now()));
}

#[test]
fn cagg_safe_range_rejects_non_hour_aligned_end() {
    let start = at(10, 0, 0, 0);
    let end = at(11, 0, 0, 1);
    assert!(!cagg_safe_range(start, end, pinned_now()));
}

#[test]
fn cagg_safe_range_rejects_end_inside_unmaterialized_window() {
    // `now` is fixed; `end` is one nanosecond past the materialized
    // horizon (`now - 1h`). Deterministic; no clock racing.
    let now = at(12, 0, 0, 0);
    let start = at(9, 0, 0, 0);
    let end = at(11, 0, 0, 1);
    assert!(!cagg_safe_range(start, end, now));
}

#[test]
fn cagg_safe_range_rejects_end_exactly_one_ns_past_horizon() {
    // Pins the horizon boundary: `end == now - 1h` is accepted (see the
    // _accepts_ test below), `end == now - 1h + 1ns` is rejected.
    let now = at(12, 0, 0, 0);
    let start = at(9, 0, 0, 0);
    let end = at(11, 0, 0, 0) + chrono::Duration::nanoseconds(1);
    assert!(!cagg_safe_range(start, end, now));
}

#[test]
fn validate_bucket_size_accepts_consistent_pair() {
    let group_by = vec![GroupByDimension::TimeBucket(BucketSize::Hour)];
    assert!(validate_bucket_size_consistency(Some(BucketSize::Hour), &group_by).is_ok());
}

#[test]
fn validate_bucket_size_accepts_missing_bucket_size() {
    let group_by = vec![GroupByDimension::TimeBucket(BucketSize::Hour)];
    // Gateway already rejects this case; the plugin-side check just falls
    // through because there is no `bucket_size` to compare.
    assert!(validate_bucket_size_consistency(None, &group_by).is_ok());
}

#[test]
fn validate_bucket_size_accepts_absent_time_bucket_dim() {
    // `bucket_size` without a `TimeBucket` dim is a no-op for routing; the
    // current `cagg_too_coarse` check still consumes it. Don't reject here.
    let group_by = vec![GroupByDimension::UsageType];
    assert!(validate_bucket_size_consistency(Some(BucketSize::Hour), &group_by).is_ok());
}

#[test]
fn validate_bucket_size_rejects_mismatch() {
    let group_by = vec![GroupByDimension::TimeBucket(BucketSize::Day)];
    let err = validate_bucket_size_consistency(Some(BucketSize::Hour), &group_by)
        .expect_err("mismatch must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { .. }),
        "expected InvalidArgument, got: {err:?}"
    );
}

#[test]
fn cagg_safe_range_accepts_hour_aligned_materialized_range() {
    // `end` is hour-aligned and exactly at the materialized horizon
    // (`now - 1h`); the upper-bound check is `end <= horizon` so this is
    // the inclusive edge case.
    let now = at(12, 0, 0, 0);
    let end = at(11, 0, 0, 0);
    let start = at(9, 0, 0, 0);
    assert!(cagg_safe_range(start, end, now));
}
