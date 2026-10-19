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
