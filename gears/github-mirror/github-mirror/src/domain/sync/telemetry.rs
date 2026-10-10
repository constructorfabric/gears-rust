use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::queue::TaskCounts;
use crate::domain::ports::telemetry_sink::TelemetrySink;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubApi {
    Rest,
    Graphql,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    Fresh,
    NotModified,
    RateLimited,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetryEntry {
    pub api: GithubApi,
    pub method: &'static str,
    pub url: String,
    pub status: u16,
    pub outcome: RequestOutcome,
    pub duration_ms: u64,
    pub rate_limit_remaining: Option<u32>,
    pub rate_limit_reset: Option<DateTime<Utc>>,
    pub cache_hit: bool,
    pub etag_used: bool,
    pub response_bytes: u64,
    pub graphql_points: Option<u64>,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct TelemetryLine<'a> {
    pub session_id: Uuid,
    pub repository: &'a str,
    pub tasks_pending: u64,
    #[serde(flatten)]
    pub entry: &'a TelemetryEntry,
}

#[derive(Debug)]
struct SessionLog {
    sink: Arc<dyn TelemetrySink>,
    file: PathBuf,
    session_id: Uuid,
    repository: String,
    failed: AtomicBool,
}

#[derive(Debug, Default)]
pub struct SessionTelemetry {
    rest_calls: AtomicU64,
    graphql_calls: AtomicU64,
    fresh: AtomicU64,
    not_modified: AtomicU64,
    rate_limited: AtomicU64,
    failed: AtomicU64,
    bytes_downloaded: AtomicU64,
    bytes_saved: AtomicU64,
    rate_limit_waits: AtomicU64,
    rate_limit_wait_ms: AtomicU64,
    graphql_points: AtomicU64,
    tasks_pending: AtomicU64,
    tasks_running: AtomicU64,
    tasks_done: AtomicU64,
    tasks_failed: AtomicU64,
    entities_indexed: AtomicU64,
    entities_refined: AtomicU64,
    entities_skipped: AtomicU64,
    log: Option<SessionLog>,
}

impl SessionTelemetry {
    #[must_use]
    pub fn logging_to(
        sink: Arc<dyn TelemetrySink>,
        file: PathBuf,
        session_id: Uuid,
        repository: String,
    ) -> Self {
        Self {
            log: Some(SessionLog {
                sink,
                file,
                session_id,
                repository,
                failed: AtomicBool::new(false),
            }),
            ..Self::default()
        }
    }

    pub fn record(&self, entry: &TelemetryEntry) {
        let Some(log) = &self.log else {
            return;
        };
        let line = TelemetryLine {
            session_id: log.session_id,
            repository: &log.repository,
            tasks_pending: self.tasks_pending.load(Ordering::Relaxed),
            entry,
        };
        if let Err(e) = log.sink.record(&log.file, &line)
            && !log.failed.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                session_id = %log.session_id,
                error = %e,
                "could not write the telemetry file; the sync goes on without it"
            );
        }
    }

    pub fn count_request(&self, api: GithubApi, outcome: RequestOutcome) {
        let calls = match api {
            GithubApi::Rest => &self.rest_calls,
            GithubApi::Graphql => &self.graphql_calls,
        };
        calls.fetch_add(1, Ordering::Relaxed);
        let answers = match outcome {
            RequestOutcome::Fresh => &self.fresh,
            RequestOutcome::NotModified => &self.not_modified,
            RequestOutcome::RateLimited => &self.rate_limited,
            RequestOutcome::Failed => &self.failed,
        };
        answers.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_downloaded(&self, bytes: usize) {
        self.bytes_downloaded
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn add_saved(&self, bytes: usize) {
        self.bytes_saved
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn add_rate_limit_wait(&self, waited: Duration) {
        self.rate_limit_waits.fetch_add(1, Ordering::Relaxed);
        self.rate_limit_wait_ms.fetch_add(
            u64::try_from(waited.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    pub fn add_graphql_points(&self, points: u64) {
        self.graphql_points.fetch_add(points, Ordering::Relaxed);
    }

    pub fn add_indexed(&self, entities: usize) {
        self.entities_indexed.fetch_add(
            u64::try_from(entities).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    pub fn add_refined(&self) {
        self.entities_refined.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_skipped(&self) {
        self.entities_skipped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_task_counts(&self, counts: TaskCounts) {
        self.tasks_pending.store(counts.pending, Ordering::Relaxed);
        self.tasks_running.store(counts.running, Ordering::Relaxed);
        self.tasks_done.store(counts.done, Ordering::Relaxed);
        self.tasks_failed.store(counts.failed, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> TelemetrySnapshot {
        TelemetrySnapshot {
            rest_calls: self.rest_calls.load(Ordering::Relaxed),
            graphql_calls: self.graphql_calls.load(Ordering::Relaxed),
            fresh: self.fresh.load(Ordering::Relaxed),
            not_modified: self.not_modified.load(Ordering::Relaxed),
            rate_limited: self.rate_limited.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            bytes_downloaded: self.bytes_downloaded.load(Ordering::Relaxed),
            bytes_saved: self.bytes_saved.load(Ordering::Relaxed),
            rate_limit_waits: self.rate_limit_waits.load(Ordering::Relaxed),
            rate_limit_wait_ms: self.rate_limit_wait_ms.load(Ordering::Relaxed),
            graphql_points: self.graphql_points.load(Ordering::Relaxed),
            tasks_pending: self.tasks_pending.load(Ordering::Relaxed),
            tasks_running: self.tasks_running.load(Ordering::Relaxed),
            tasks_done: self.tasks_done.load(Ordering::Relaxed),
            tasks_failed: self.tasks_failed.load(Ordering::Relaxed),
            entities_indexed: self.entities_indexed.load(Ordering::Relaxed),
            entities_refined: self.entities_refined.load(Ordering::Relaxed),
            entities_skipped: self.entities_skipped.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetrySnapshot {
    pub rest_calls: u64,
    pub graphql_calls: u64,
    pub fresh: u64,
    pub not_modified: u64,
    pub rate_limited: u64,
    pub failed: u64,
    pub bytes_downloaded: u64,
    pub bytes_saved: u64,
    pub rate_limit_waits: u64,
    pub rate_limit_wait_ms: u64,
    pub graphql_points: u64,
    pub tasks_pending: u64,
    pub tasks_running: u64,
    pub tasks_done: u64,
    pub tasks_failed: u64,
    pub entities_indexed: u64,
    pub entities_refined: u64,
    pub entities_skipped: u64,
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod telemetry_tests;
