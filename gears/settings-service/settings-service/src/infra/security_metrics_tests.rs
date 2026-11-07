//! The security metrics' OpenTelemetry binding: what it exports, under which
//! names and labels.

use super::OtelSecurityMetrics;
use crate::domain::ports::SecurityMetrics as _;
use crate::domain::ports::labels::{ALLOWED, AUTHZ_UNAVAILABLE, DENIED, MUTATION, SECRET_USE};
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

    metrics.authz_denial("read", DENIED);
    metrics.authz_denial("read", DENIED);
    metrics.authz_denial("write", AUTHZ_UNAVAILABLE);
    metrics.secret_use(ALLOWED);
    metrics.secret_use(DENIED);
    metrics.audit_event(MUTATION);
    metrics.audit_event(SECRET_USE);
    metrics.audit_event(MUTATION);

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
