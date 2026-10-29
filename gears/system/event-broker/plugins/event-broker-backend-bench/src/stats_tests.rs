use std::time::Duration;

use crate::stats::{Histogram, Percentiles, per_second, period_nanos};

fn of(values: &[u64]) -> Percentiles {
    let mut histogram = Histogram::default();
    for v in values {
        histogram.record(*v);
    }
    histogram.percentiles()
}

#[test]
fn empty_input_is_all_zero() {
    assert_eq!(Histogram::default().percentiles(), Percentiles::default());
}

#[test]
fn one_sample_is_every_percentile() {
    let p = of(&[7]);
    assert_eq!((p.samples, p.p50_ns, p.p99_9_ns, p.max_ns), (1, 7, 7, 7));
}

#[test]
fn small_values_are_exact_by_nearest_rank() {
    let values: Vec<u64> = (1..=100).rev().collect();
    let p = of(&values);
    assert_eq!(
        (p.p50_ns, p.p90_ns, p.p99_ns, p.p99_9_ns, p.max_ns),
        (50, 90, 99, 100, 100)
    );
}

#[test]
fn nearest_rank_rounds_up() {
    // ceil(0.5 * 3) = 2nd, ceil(0.99 * 3) = 3rd.
    let p = of(&[30, 10, 20]);
    assert_eq!((p.p50_ns, p.p99_ns), (20, 30));
}

#[test]
fn large_values_are_within_the_bucket_error() {
    let values: Vec<u64> = (1..=10_000).map(|i| i * 1_000).collect();
    let p = of(&values);
    for (got, want) in [
        (p.p50_ns, 5_000_000),
        (p.p90_ns, 9_000_000),
        (p.p99_ns, 9_900_000),
        (p.p99_9_ns, 9_990_000),
    ] {
        assert!(got >= want && (got - want) * 64 <= want, "{got} vs {want}");
    }
    assert_eq!(p.max_ns, 10_000_000);
}

#[test]
fn the_largest_value_has_a_bucket() {
    let p = of(&[u64::MAX, 1]);
    assert_eq!(p.max_ns, u64::MAX);
    assert_eq!(p.p99_ns, u64::MAX);
}

#[test]
fn merge_adds_counts() {
    let mut a = Histogram::default();
    a.record_n(10, 3);
    let mut b = Histogram::default();
    b.record(1_000_000);
    a.merge(&b);
    let p = a.percentiles();
    assert_eq!((p.samples, p.p50_ns, p.max_ns), (4, 10, 1_000_000));
}

#[test]
fn rates_and_periods() {
    assert!((per_second(500, Duration::from_millis(250)) - 2000.0).abs() < 1e-9);
    assert!(per_second(5, Duration::ZERO).abs() < 1e-9);
    // 16 writers sharing 1600 events/s in appends of 100: one append a second each.
    assert_eq!(period_nanos(100, 16, 1600.0), 1_000_000_000);
}
