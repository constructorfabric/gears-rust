//! The write path's OpenTelemetry binding: what it exports, under which
//! names and labels.

use super::OtelWriteMetrics;
use crate::domain::ports::labels::{COMMITTED, REJECTED, REVERT, SERVICE_PRINCIPAL, SET, VERIFIED};
use crate::domain::ports::{StepUpOperation, WriteMetrics as _};
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
    let metrics = OtelWriteMetrics::on(&exported.meter());

    metrics.value_write(COMMITTED);
    metrics.value_write(COMMITTED);
    metrics.value_write(REJECTED);
    metrics.step_up(StepUpOperation::Set, VERIFIED);
    metrics.step_up(StepUpOperation::Purge, SERVICE_PRINCIPAL);
    metrics.purge_released();
    metrics.purge_release_failed();
    metrics.purge_released();
    metrics.change(SET, COMMITTED);
    metrics.change(REVERT, REJECTED);

    assert_eq!(
        exported.points("settings_value_writes_total"),
        vec![
            (labels(&[("result", "committed")]), 2),
            (labels(&[("result", "rejected")]), 1),
        ]
    );
    assert_eq!(
        exported.points("settings_step_up_total"),
        vec![
            (
                labels(&[("operation", "purge"), ("result", "service_principal")]),
                1
            ),
            (labels(&[("operation", "set"), ("result", "verified")]), 1),
        ],
        "the operation label is the typed operation's own"
    );
    assert_eq!(
        exported.points("settings_declaration_purge_secret_releases_total"),
        vec![
            (labels(&[("result", "failed")]), 1),
            (labels(&[("result", "ok")]), 2),
        ]
    );
    assert_eq!(
        exported.points("settings_changes_committed_total"),
        vec![
            (
                labels(&[("change_type", "revert"), ("outcome", "rejected")]),
                1
            ),
            (
                labels(&[("change_type", "set"), ("outcome", "committed")]),
                1
            ),
        ]
    );
}
