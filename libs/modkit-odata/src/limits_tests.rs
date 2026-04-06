use super::*;

#[test]
fn test_default_limits() {
    let limits = ODataLimits::default();
    assert_eq!(limits.max_top, 1000);
    assert_eq!(limits.max_orderby_fields, 5);
    assert_eq!(limits.max_filter_length, 2000);
    assert!(!limits.require_signed_cursors);
}

#[test]
fn test_validate_top_ok() {
    let limits = ODataLimits::default();
    assert!(limits.validate_top(500).is_ok());
    assert!(limits.validate_top(1000).is_ok());
}

#[test]
fn test_validate_top_exceeds() {
    let limits = ODataLimits::default();
    assert!(limits.validate_top(1001).is_err());
}

#[test]
fn test_validate_filter_ok() {
    let limits = ODataLimits::default();
    assert!(limits.validate_filter("name eq 'John'").is_ok());
}

#[test]
fn test_validate_filter_too_long() {
    let limits = ODataLimits::default();
    let long_filter = "x".repeat(2001);
    assert!(limits.validate_filter(&long_filter).is_err());
}

#[test]
fn test_validate_orderby_count_ok() {
    let limits = ODataLimits::default();
    assert!(limits.validate_orderby_count(3).is_ok());
    assert!(limits.validate_orderby_count(5).is_ok());
}

#[test]
fn test_validate_orderby_count_exceeds() {
    let limits = ODataLimits::default();
    assert!(limits.validate_orderby_count(6).is_err());
}

#[test]
fn test_custom_limits() {
    let limits = ODataLimits::new()
        .with_max_top(100)
        .with_max_orderby_fields(3)
        .with_max_filter_length(500);

    assert_eq!(limits.max_top, 100);
    assert_eq!(limits.max_orderby_fields, 3);
    assert_eq!(limits.max_filter_length, 500);
}
