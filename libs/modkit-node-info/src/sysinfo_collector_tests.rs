use super::calculate_percent;

#[test]
fn test_calculate_percent_zero_total() {
    assert_eq!(calculate_percent(0, 0), 0);
    assert_eq!(calculate_percent(100, 0), 0);
}

#[test]
fn test_calculate_percent_normal() {
    assert_eq!(calculate_percent(50, 100), 50);
    assert_eq!(calculate_percent(1, 4), 25);
    assert_eq!(calculate_percent(3, 4), 75);
    assert_eq!(calculate_percent(0, 100), 0);
}

#[test]
fn test_calculate_percent_full() {
    assert_eq!(calculate_percent(100, 100), 100);
    assert_eq!(calculate_percent(200, 100), 100);
}

#[test]
fn test_calculate_percent_overflow_repro() {
    // used = u64::MAX would overflow u64 * 100 without u128 widening
    assert_eq!(calculate_percent(u64::MAX, 1), 100);
    assert_eq!(calculate_percent(u64::MAX, u64::MAX), 100);
    // Very large values typical of PB-scale storage
    let petabyte: u64 = 1_000_000_000_000_000;
    assert_eq!(calculate_percent(petabyte, 2 * petabyte), 50);
}
