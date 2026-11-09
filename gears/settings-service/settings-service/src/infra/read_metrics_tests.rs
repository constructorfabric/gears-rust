//! The read histogram's boundaries.

use super::READ_DURATION_BUCKETS;

#[test]
fn the_read_histogram_boundaries_are_the_designs_so_the_p95_targets_are_boundaries() {
    let designed = [
        0.001, 0.002, 0.005, 0.01, 0.015, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
    ];
    assert_eq!(READ_DURATION_BUCKETS.len(), designed.len());
    assert!(
        READ_DURATION_BUCKETS
            .iter()
            .zip(designed)
            .all(|(have, want)| (have - want).abs() < f64::EPSILON),
        "seconds, as the DESIGN names them"
    );
    assert!(
        READ_DURATION_BUCKETS.contains(&0.002) && READ_DURATION_BUCKETS.contains(&0.015),
        "the cache-hit and resolve p95 targets are boundaries"
    );
    assert!(
        READ_DURATION_BUCKETS.windows(2).all(|w| w[0] < w[1]),
        "strictly increasing"
    );
}

fn labels(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn the_binding_exports_the_designs_names_labels_and_boundaries() {
    use crate::domain::metric_labels::{LookupResult, ReadSource, ResolveFailure, ScopeClass};
    use crate::domain::ports::ReadMetrics as _;

    let exported = crate::test_support::ExportedMetrics::new();
    let metrics = super::OtelReadMetrics::on(&exported.meter());

    metrics.lookup(LookupResult::Hit, ScopeClass::Global);
    metrics.lookup(LookupResult::Miss, ScopeClass::Cascading);
    metrics.lookup(LookupResult::Miss, ScopeClass::Cascading);
    metrics.read(ReadSource::Cache, ScopeClass::Global, 0.0005);
    metrics.read(ReadSource::Resolve, ScopeClass::Cascading, 0.012);
    metrics.resolve_failure(ResolveFailure::Unavailable);
    metrics.resolve_failure(ResolveFailure::Internal);
    metrics.resolve_failure(ResolveFailure::Unavailable);

    assert_eq!(
        exported.points("settings_cache_lookups_total"),
        vec![
            (labels(&[("result", "hit"), ("scope_class", "global")]), 1),
            (
                labels(&[("result", "miss"), ("scope_class", "cascading")]),
                2
            ),
        ]
    );
    assert_eq!(
        exported.points("settings_effective_read_duration_seconds"),
        vec![
            (
                labels(&[("scope_class", "cascading"), ("source", "resolve")]),
                1
            ),
            (labels(&[("scope_class", "global"), ("source", "cache")]), 1),
        ]
    );
    assert_eq!(
        exported.boundaries("settings_effective_read_duration_seconds"),
        READ_DURATION_BUCKETS.to_vec(),
        "the histogram carries the DESIGN's boundaries, not the SDK's defaults"
    );
    assert_eq!(
        exported.points("settings_resolve_failures_total"),
        vec![
            (labels(&[("reason", "internal")]), 1),
            (labels(&[("reason", "unavailable")]), 2),
        ]
    );
}
