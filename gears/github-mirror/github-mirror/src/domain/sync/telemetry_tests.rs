use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use super::{
    GithubApi, RequestOutcome, SessionTelemetry, TelemetryEntry, TelemetryLine, TelemetrySnapshot,
};
use crate::domain::error::DomainError;
use crate::domain::ports::telemetry_sink::TelemetrySink;
use crate::domain::sync::TaskCounts;

#[derive(Debug, Default)]
struct RecordingSink {
    lines: Mutex<Vec<(PathBuf, serde_json::Value)>>,
    fail: bool,
}

impl TelemetrySink for RecordingSink {
    fn record(&self, file: &Path, line: &TelemetryLine<'_>) -> Result<(), DomainError> {
        self.lines
            .lock()
            .unwrap()
            .push((file.to_path_buf(), serde_json::to_value(line).unwrap()));
        if self.fail {
            Err(DomainError::internal("disk full"))
        } else {
            Ok(())
        }
    }
}

fn entry(status: u16) -> TelemetryEntry {
    TelemetryEntry {
        api: GithubApi::Rest,
        method: "GET",
        url: "https://api.github.com/repos/rust-lang/rust/issues?[REDACTED]".to_owned(),
        status,
        outcome: RequestOutcome::Fresh,
        duration_ms: 212,
        rate_limit_remaining: Some(4870),
        rate_limit_reset: None,
        cache_hit: false,
        etag_used: true,
        response_bytes: 5082,
        graphql_points: None,
        requested_at: Utc::now(),
    }
}

#[test]
fn a_recorded_request_reaches_the_sink_with_its_session_and_queue_depth() {
    let sink = Arc::new(RecordingSink::default());
    let session_id = Uuid::new_v4();
    let telemetry = SessionTelemetry::logging_to(
        Arc::clone(&sink) as Arc<dyn TelemetrySink>,
        PathBuf::from("/tmp/gm/tenant/rust-lang/rust.jsonl"),
        session_id,
        "rust-lang/rust".to_owned(),
    );
    telemetry.set_task_counts(TaskCounts {
        pending: 7,
        ..TaskCounts::default()
    });

    telemetry.record(&entry(200));

    let lines = sink.lines.lock().unwrap();
    assert_eq!(lines.len(), 1);
    let (file, line) = &lines[0];
    assert_eq!(file, &PathBuf::from("/tmp/gm/tenant/rust-lang/rust.jsonl"));
    assert_eq!(line["session_id"], session_id.to_string());
    assert_eq!(line["repository"], "rust-lang/rust");
    assert_eq!(line["tasks_pending"], 7);
    assert_eq!(line["api"], "rest");
    assert_eq!(line["outcome"], "fresh");
    assert_eq!(line["status"], 200);
    assert_eq!(line["response_bytes"], 5082);
    assert_eq!(line["etag_used"], true);
}

#[test]
fn a_telemetry_without_a_sink_records_nothing() {
    let telemetry = SessionTelemetry::default();

    telemetry.record(&entry(200));

    assert_eq!(telemetry.snapshot(), TelemetrySnapshot::default());
}

#[test]
fn a_failing_sink_is_tried_for_every_request_and_never_panics() {
    let sink = Arc::new(RecordingSink {
        fail: true,
        ..RecordingSink::default()
    });
    let telemetry = SessionTelemetry::logging_to(
        Arc::clone(&sink) as Arc<dyn TelemetrySink>,
        PathBuf::from("/nowhere/rust.jsonl"),
        Uuid::new_v4(),
        "rust-lang/rust".to_owned(),
    );

    telemetry.record(&entry(200));
    telemetry.record(&entry(304));

    assert_eq!(sink.lines.lock().unwrap().len(), 2);
}

#[test]
fn task_counts_replace_the_previous_ones() {
    let telemetry = SessionTelemetry::default();

    telemetry.set_task_counts(TaskCounts {
        pending: 104,
        running: 4,
        done: 275,
        failed: 0,
    });
    telemetry.set_task_counts(TaskCounts {
        pending: 0,
        running: 0,
        done: 383,
        failed: 1,
    });

    assert_eq!(
        telemetry.snapshot(),
        TelemetrySnapshot {
            tasks_done: 383,
            tasks_failed: 1,
            ..TelemetrySnapshot::default()
        }
    );
}

#[test]
fn entity_counts_add_up() {
    let telemetry = SessionTelemetry::default();

    telemetry.add_indexed(100);
    telemetry.add_indexed(27);
    telemetry.add_refined();
    telemetry.add_refined();
    telemetry.add_skipped();

    assert_eq!(
        telemetry.snapshot(),
        TelemetrySnapshot {
            entities_indexed: 127,
            entities_refined: 2,
            entities_skipped: 1,
            ..TelemetrySnapshot::default()
        }
    );
}

#[test]
fn each_request_is_counted_by_api_and_by_outcome() {
    let telemetry = SessionTelemetry::default();

    telemetry.count_request(GithubApi::Rest, RequestOutcome::Fresh);
    telemetry.count_request(GithubApi::Rest, RequestOutcome::NotModified);
    telemetry.count_request(GithubApi::Rest, RequestOutcome::NotModified);
    telemetry.count_request(GithubApi::Rest, RequestOutcome::RateLimited);
    telemetry.count_request(GithubApi::Graphql, RequestOutcome::Fresh);
    telemetry.count_request(GithubApi::Graphql, RequestOutcome::Failed);

    assert_eq!(
        telemetry.snapshot(),
        TelemetrySnapshot {
            rest_calls: 4,
            graphql_calls: 2,
            fresh: 2,
            not_modified: 2,
            rate_limited: 1,
            failed: 1,
            ..TelemetrySnapshot::default()
        }
    );
}

#[test]
fn bytes_waits_and_points_add_up() {
    let telemetry = SessionTelemetry::default();

    telemetry.add_downloaded(1_000);
    telemetry.add_downloaded(500);
    telemetry.add_saved(2_048);
    telemetry.add_rate_limit_wait(Duration::from_millis(1_500));
    telemetry.add_rate_limit_wait(Duration::from_millis(250));
    telemetry.add_graphql_points(1);
    telemetry.add_graphql_points(3);

    assert_eq!(
        telemetry.snapshot(),
        TelemetrySnapshot {
            bytes_downloaded: 1_500,
            bytes_saved: 2_048,
            rate_limit_waits: 2,
            rate_limit_wait_ms: 1_750,
            graphql_points: 4,
            ..TelemetrySnapshot::default()
        }
    );
}

#[test]
fn requests_counted_from_many_threads_are_all_kept() {
    let telemetry = SessionTelemetry::default();

    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..1_000 {
                    telemetry.count_request(GithubApi::Rest, RequestOutcome::NotModified);
                    telemetry.add_saved(10);
                }
            });
        }
    });

    let snapshot = telemetry.snapshot();
    assert_eq!(snapshot.rest_calls, 8_000);
    assert_eq!(snapshot.not_modified, 8_000);
    assert_eq!(snapshot.bytes_saved, 80_000);
}
