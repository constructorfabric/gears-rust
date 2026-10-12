use super::ValidationReport;

#[test]
fn a_report_keeps_every_violation_in_the_order_collected() {
    let mut report = ValidationReport::new();
    assert!(report.is_empty());
    report.violate("VALIDATION", "name", "blank");
    report.violate("USAGE_TYPE_UNRESOLVED", "usage_type_ref", "unknown");
    assert!(!report.is_empty());
    let codes: Vec<_> = report.violations().iter().map(|v| v.code).collect();
    assert_eq!(codes, vec!["VALIDATION", "USAGE_TYPE_UNRESOLVED"]);
    assert_eq!(report.to_string(), "2 violation(s)");
}
