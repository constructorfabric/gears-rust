//! The lifecycle's OpenTelemetry binding: what it exports, under which names
//! and labels.

use super::OtelLifecycleMetrics;
use crate::domain::ports::LifecycleMetrics as _;
use crate::domain::ports::labels::{RETENTION_FAILED, RETENTION_OK};
use crate::test_support::ExportedMetrics;

fn labels(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn the_binding_exports_the_designs_names_and_labels() {
    let exported = ExportedMetrics::new();
    let metrics = OtelLifecycleMetrics::on(&exported.meter());

    metrics.service_up(true);
    metrics.needs_review("module_contributed", 3);
    metrics.needs_review("module_contributed", 1);
    metrics.declarations("admin_authored", "retired", 7);
    metrics.values("cascading", "tenant", 4);
    metrics.retention_pass(RETENTION_OK, 5);
    metrics.retention_pass(RETENTION_FAILED, 0);
    metrics.retention_pass(RETENTION_OK, 2);

    assert_eq!(exported.points("settings_service_up"), vec![(vec![], 1)]);
    assert_eq!(
        exported.points("settings_needs_review_total"),
        vec![(labels(&[("source", "module_contributed")]), 1)],
        "a gauge holds the last sample, not a sum"
    );
    assert_eq!(
        exported.points("settings_declarations_total"),
        vec![(
            labels(&[("source", "admin_authored"), ("status", "retired")]),
            7
        )]
    );
    assert_eq!(
        exported.points("settings_values_total"),
        vec![(
            labels(&[("scope_class", "cascading"), ("scope_kind", "tenant")]),
            4
        )]
    );
    assert_eq!(
        exported.points("settings_audit_retention_passes_total"),
        vec![
            (labels(&[("result", "failed")]), 1),
            (labels(&[("result", "ok")]), 2),
        ]
    );
    assert_eq!(
        exported.points("settings_audit_records_pruned_total"),
        vec![(vec![], 7)]
    );

    metrics.service_up(false);
    assert_eq!(
        exported.points("settings_service_up"),
        vec![(vec![], 0)],
        "told to stop, the gauge reads zero"
    );
}
