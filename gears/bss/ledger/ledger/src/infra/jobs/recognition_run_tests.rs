//! Tests for `RecognitionRunJob` — the periodic S6 release ticker (Group F2).
//!
//! The load-bearing coverage (cross-tenant due-work enumeration → per-pair run →
//! at-most-once release, and the drain of a QUEUED out-of-order segment by a
//! later tick) is in the Docker-gated integration test
//! `tests/postgres_recognition_run.rs`, which boots Postgres and drives the real
//! `RecognitionRunService`. These plain unit tests pin the `RecognitionRunReport`
//! accounting shape (the per-tick tally the ticker logs) without a database.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::RecognitionRunReport;

#[test]
fn report_default_is_all_zero() {
    let report = RecognitionRunReport::default();
    assert_eq!(report.pairs, 0);
    assert_eq!(report.triggered, 0);
    assert_eq!(report.failed, 0);
}

#[test]
fn report_counts_are_independent() {
    // triggered + failed partition the pairs that had work; a tick with one
    // success and one failure tallies both legs against two pairs.
    let report = RecognitionRunReport {
        pairs: 2,
        triggered: 1,
        failed: 1,
    };
    assert_eq!(report.pairs, report.triggered + report.failed);
}
