//! The security metrics' OpenTelemetry binding: what it exports, under which
//! names and labels.

use super::OtelSecurityMetrics;
use crate::domain::metric_labels::{AuditKind, AuthzReason, Operation, SecretUseResult};
use crate::domain::ports::SecurityMetrics as _;
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
    let metrics = OtelSecurityMetrics::on(&exported.meter());

    metrics.authz_denial(Operation::Read, AuthzReason::Denied);
    metrics.authz_denial(Operation::Read, AuthzReason::Denied);
    metrics.authz_denial(Operation::Write, AuthzReason::Unavailable);
    metrics.secret_use(SecretUseResult::Allowed);
    metrics.secret_use(SecretUseResult::Denied);
    metrics.audit_event(AuditKind::Mutation);
    metrics.audit_event(AuditKind::SecretUse);
    metrics.audit_event(AuditKind::Mutation);

    assert_eq!(
        exported.points("settings_authz_denial_total"),
        vec![
            (labels(&[("operation", "read"), ("reason", "denied")]), 2),
            (
                labels(&[("operation", "write"), ("reason", "unavailable")]),
                1
            ),
        ]
    );
    assert_eq!(
        exported.points("settings_secret_use_total"),
        vec![
            (labels(&[("result", "allowed")]), 1),
            (labels(&[("result", "denied")]), 1),
        ]
    );
    assert_eq!(
        exported.points("settings_audit_events_total"),
        vec![
            (labels(&[("kind", "mutation")]), 2),
            (labels(&[("kind", "secret_use")]), 1),
        ]
    );
}
