use std::path::Path;

use chrono::Utc;
use uuid::Uuid;

use super::JsonlTelemetrySink;
use crate::domain::error::DomainError;
use crate::domain::ports::telemetry_sink::TelemetrySink;
use crate::domain::sync::{GithubApi, RequestOutcome, TelemetryEntry, TelemetryLine};

fn entry(status: u16) -> TelemetryEntry {
    TelemetryEntry {
        api: GithubApi::Rest,
        method: "GET",
        url: "https://api.github.com/repos/rust-lang/rust".to_owned(),
        status,
        outcome: RequestOutcome::Fresh,
        duration_ms: 212,
        rate_limit_remaining: Some(4870),
        rate_limit_reset: None,
        cache_hit: status == 304,
        etag_used: status == 304,
        response_bytes: if status == 304 { 0 } else { 5082 },
        graphql_points: None,
        requested_at: Utc::now(),
    }
}

fn record(file: &Path, session_id: Uuid, status: u16) -> Result<(), DomainError> {
    let entry = entry(status);
    JsonlTelemetrySink.record(
        file,
        &TelemetryLine {
            session_id,
            repository: "rust-lang/rust",
            tasks_pending: 3,
            entry: &entry,
        },
    )
}

#[test]
fn each_record_appends_one_json_line_and_creates_the_folders() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir
        .path()
        .join("00000000-df51-5b42-9538-d2b56b7ee953")
        .join("rust-lang")
        .join("rust.jsonl");
    let session_id = Uuid::new_v4();

    record(&file, session_id, 200).unwrap();
    record(&file, session_id, 304).unwrap();

    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.ends_with('\n'));
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["status"], 200);
    assert_eq!(lines[0]["response_bytes"], 5082);
    assert_eq!(lines[1]["status"], 304);
    assert_eq!(lines[1]["cache_hit"], true);
    assert_eq!(lines[1]["session_id"], session_id.to_string());
    assert_eq!(lines[1]["repository"], "rust-lang/rust");
    assert_eq!(lines[1]["tasks_pending"], 3);
}

#[test]
fn an_existing_file_is_appended_to_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rust.jsonl");
    std::fs::write(&file, "{\"earlier\":true}\n").unwrap();

    record(&file, Uuid::new_v4(), 200).unwrap();

    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("{\"earlier\":true}\n"));
    assert_eq!(text.lines().count(), 2);
}

#[test]
fn a_file_that_cannot_be_created_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("taken");
    std::fs::write(&blocker, "").unwrap();

    let result = record(&blocker.join("rust.jsonl"), Uuid::new_v4(), 200);

    assert!(result.is_err());
}
