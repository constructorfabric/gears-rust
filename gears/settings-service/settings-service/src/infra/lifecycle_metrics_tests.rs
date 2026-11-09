//! The lifecycle's OpenTelemetry binding: what it exports, under which names
//! and labels.

use super::OtelLifecycleMetrics;
use crate::domain::declaration::DeclarationStatus;
use crate::domain::metric_labels::{DeclarationSource, RetentionResult, ScopeClass, ScopeKind};
use crate::domain::ports::LifecycleMetrics as _;
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
    metrics.needs_review(DeclarationSource::ModuleContributed, 3);
    metrics.needs_review(DeclarationSource::ModuleContributed, 1);
    metrics.declarations(
        DeclarationSource::AdminAuthored,
        DeclarationStatus::Retired,
        7,
    );
    metrics.values(ScopeClass::Cascading, ScopeKind::Tenant, 4);
    metrics.retention_pass(RetentionResult::Ok, 5);
    metrics.retention_pass(RetentionResult::Failed, 0);
    metrics.retention_pass(RetentionResult::Ok, 2);

    assert_eq!(exported.points("settings_service_ready"), vec![(vec![], 1)]);
    assert_eq!(
        exported.points("settings_needs_review"),
        vec![(labels(&[("source", "module_contributed")]), 1)],
        "a gauge holds the last sample, not a sum"
    );
    assert_eq!(
        exported.points("settings_declarations"),
        vec![(
            labels(&[("source", "admin_authored"), ("status", "retired")]),
            7
        )]
    );
    assert_eq!(
        exported.points("settings_values"),
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
        exported.points("settings_service_ready"),
        vec![(vec![], 0)],
        "told to stop, the gauge reads zero"
    );
}
