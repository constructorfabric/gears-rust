//! What every task of the run counts, shared: totals, the outage clock and
//! the violations.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use event_broker_backend_bench::Histogram;
use event_broker_sdk::models::Event;
use serde::Serialize;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::topics::Kind;

/// Violations kept for the report; the count goes on past it.
const KEPT_VIOLATIONS: usize = 200;

#[derive(Debug, Clone, Serialize)]
pub struct Outage {
    /// Seconds from the start of the run.
    pub started_at_s: f64,
    pub lasted_s: f64,
}

#[derive(Default)]
struct OutageClock {
    down_since: Option<Instant>,
    outages: Vec<Outage>,
}

pub struct Tally {
    pub started: Instant,
    /// Events whose append was answered, per topic.
    pub acked: [AtomicU64; 3],
    /// Events of appends still unanswered when writers stopped, per topic.
    pub unresolved: [AtomicU64; 3],
    /// Events the tail readers read.
    pub read: AtomicU64,
    /// Events the lagging readers lost below the floor.
    pub skipped: AtomicU64,
    /// Events the tail readers lost below the floor.
    pub tail_skipped: AtomicU64,
    pub append_errors: AtomicU64,
    pub read_errors: AtomicU64,
    pub watch_errors: AtomicU64,
    pub check_errors: AtomicU64,
    /// Live findings that did not repeat in the next check.
    pub transient: AtomicU64,
    pub violations: AtomicU64,
    /// Append latencies since the last status line.
    latency: Mutex<Latency>,
    /// Tail position per `(topic, partition)`.
    tail: Mutex<HashMap<(Kind, u32), i64>>,
    outage: Mutex<OutageClock>,
    kept: Mutex<Vec<String>>,
    /// Cancelled at the first violation under `--stop-on-violation`.
    stop: Option<CancellationToken>,
}

impl Tally {
    pub fn new(stop: Option<CancellationToken>) -> Self {
        Self {
            started: Instant::now(),
            acked: Default::default(),
            unresolved: Default::default(),
            read: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            tail_skipped: AtomicU64::new(0),
            append_errors: AtomicU64::new(0),
            read_errors: AtomicU64::new(0),
            watch_errors: AtomicU64::new(0),
            check_errors: AtomicU64::new(0),
            transient: AtomicU64::new(0),
            violations: AtomicU64::new(0),
            latency: Mutex::new(Latency::default()),
            tail: Mutex::new(HashMap::new()),
            outage: Mutex::new(OutageClock::default()),
            kept: Mutex::new(Vec::new()),
            stop,
        }
    }

    /// `HH:MM:SS` since the start.
    pub fn clock(&self) -> String {
        let s = self.started.elapsed().as_secs();
        format!(
            "{:02}:{:02}:{:02}",
            s.div_euclid(3600),
            s.div_euclid(60).rem_euclid(60),
            s.rem_euclid(60)
        )
    }

    pub fn log(&self, line: &str) {
        eprintln!("{} {line}", self.clock());
    }

    pub fn violation(&self, line: String) {
        self.violations.fetch_add(1, Ordering::Relaxed);
        self.log(&format!("VIOLATION {line}"));
        let mut entry = line;
        entry.insert_str(0, &format!("{} ", self.clock()));
        let mut kept = self
            .kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if kept.len() < KEPT_VIOLATIONS {
            kept.push(entry);
        }
        drop(kept);
        if let Some(stop) = &self.stop {
            stop.cancel();
        }
    }

    pub fn kept_violations(&self) -> Vec<String> {
        self.kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// `events` acknowledged: sent `behind` after they were due, answered
    /// `append` after that.
    pub fn acked(&self, kind: Kind, events: u64, behind: Duration, append: Duration) {
        self.acked[kind.index()].fetch_add(events, Ordering::Relaxed);
        let mut latency = self
            .latency
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        latency.behind.record_n(nanos(behind), events);
        latency.append.record_n(nanos(append), events);
    }

    /// A tail read that took `took` and returned `events`.
    pub fn tail_read(&self, took: Duration, events: &[Event]) {
        let now = Utc::now();
        let mut latency = self
            .latency
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        latency.read.record(nanos(took));
        for at in events.iter().filter_map(|e| e.sequence_time) {
            latency
                .delivery
                .record(nanos((now - at).to_std().unwrap_or_default()));
        }
    }

    /// The latencies since the last call.
    pub fn take_latency(&self) -> Latency {
        std::mem::take(
            &mut *self
                .latency
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    pub fn total_acked(&self) -> u64 {
        self.acked.iter().map(|a| a.load(Ordering::Relaxed)).sum()
    }

    pub fn tail_at(&self, kind: Kind, partition: u32, sequence: i64) {
        self.tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((kind, partition), sequence);
    }

    pub fn tail_positions(&self) -> HashMap<(Kind, u32), i64> {
        self.tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// A call reached the server.
    pub fn up(&self) {
        let mut clock = self
            .outage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(since) = clock.down_since.take() {
            let lasted = since.elapsed();
            clock.outages.push(Outage {
                started_at_s: since.duration_since(self.started).as_secs_f64(),
                lasted_s: lasted.as_secs_f64(),
            });
            drop(clock);
            self.log(&format!("outage ended after {:.1}s", lasted.as_secs_f64()));
        }
    }

    /// A call failed because the server could not be reached.
    pub fn down(&self, reason: &str) {
        let mut clock = self
            .outage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if clock.down_since.is_none() {
            clock.down_since = Some(Instant::now());
            drop(clock);
            self.log(&format!("outage started: {reason}"));
        }
    }

    pub fn outages(&self) -> Vec<Outage> {
        self.outage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .outages
            .clone()
    }
}

/// Where the time went, since the last status line.
#[derive(Default)]
pub struct Latency {
    /// Per event written: from when its batch was due on the writer's
    /// schedule to its first send. Near 0 while writers hold the target rate;
    /// it grows without bound when the backend cannot.
    pub behind: Histogram,
    /// Per event written: from the first send to the answer, resends
    /// included.
    pub append: Histogram,
    /// Per event the tail readers read: from its `sequence_time`, the
    /// server's clock at append, to the read returning it.
    pub delivery: Histogram,
    /// Per tail read call, up to `READ_MAX` events of one partition.
    pub read: Histogram,
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}
