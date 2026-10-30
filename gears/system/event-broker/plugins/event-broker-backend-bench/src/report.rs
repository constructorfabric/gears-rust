//! What a run measured: one row per cell phase, printed as markdown tables
//! and written as JSON.

use serde::Serialize;

use crate::args::{ScenarioId, Settings};
use crate::check::Verdict;
use crate::phase::PhaseOutcome;
use crate::produce::Pacing;
use crate::stats::{Percentiles, mib, ms, per_second};
use crate::window::WindowStat;

/// Which kind of phase a report row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PhaseKind {
    /// Closed loop: as fast as the writers go.
    Peak,
    /// Open loop at a fraction of the cell's peak.
    Latency,
    /// Draining a prefilled topic.
    CatchUp,
    /// Open loop for the soak duration.
    Soak,
}

impl PhaseKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Peak => "peak",
            Self::Latency => "latency",
            Self::CatchUp => "catch-up",
            Self::Soak => "soak",
        }
    }
}

/// One phase of one cell.
#[derive(Debug, Clone, Serialize)]
pub struct CellReport {
    pub scenario: ScenarioId,
    pub cell: String,
    pub phase: PhaseKind,
    /// Whether `--max-events-per-phase` ended the phase before its duration;
    /// rates are then taken over the shorter window.
    pub capped: bool,
    /// The open-loop rate asked for; `None` closed loop.
    pub target_events_per_sec: Option<f64>,
    /// Acked events whose append returned inside the measured window, per
    /// second.
    pub written_events_per_sec: f64,
    /// Events read inside the measured window per second (B3: all events read,
    /// over the time to drain or the cap).
    pub read_events_per_sec: f64,
    /// Padding bytes of the headline rate: written, or read for B3.
    pub payload_mib_per_sec: f64,
    pub acked: u64,
    pub read: u64,
    pub appends: u64,
    pub resends: u64,
    /// Append, read and watch errors; resent or retried, so not failures by
    /// themselves.
    pub errors: u64,
    /// Scheduled send to the append's answer.
    pub ack_latency: Percentiles,
    /// Scheduled send to the consumer seeing a head that covers the event.
    pub head_latency: Percentiles,
    /// Scheduled send to the consumer's read returning the event.
    pub read_latency: Percentiles,
    /// Duration of each read call that returned events (B3).
    pub read_call_latency: Percentiles,
    /// Per-second windows (B7).
    pub windows: Vec<WindowStat>,
    pub verdict: Verdict,
}

/// Which side of a phase its MiB/s is taken from.
#[derive(Debug, Clone, Copy)]
pub enum Headline {
    /// The written rate.
    Written,
    /// The read rate.
    Read,
}

impl CellReport {
    #[must_use]
    pub fn of(
        scenario: ScenarioId,
        cell: String,
        phase: PhaseKind,
        target: Pacing,
        outcome: PhaseOutcome,
        headline: Headline,
        payload_bytes: usize,
    ) -> Self {
        let measured = outcome.measured;
        let written = per_second(outcome.write.acked_in_window, measured);
        let read = per_second(read_count(&outcome, headline), measured);
        let headline_events = match headline {
            Headline::Written => written,
            Headline::Read => read,
        };
        Self {
            scenario,
            cell,
            phase,
            capped: outcome.capped,
            target_events_per_sec: match target {
                Pacing::Closed => None,
                Pacing::Open { events_per_sec } => Some(events_per_sec),
            },
            written_events_per_sec: written,
            read_events_per_sec: read,
            payload_mib_per_sec: headline_events * mib(payload_bytes as u64),
            acked: outcome.write.acked,
            read: outcome.read.consumed,
            appends: outcome.write.appends,
            resends: outcome.write.resends,
            errors: outcome.write.errors + outcome.read.read_errors + outcome.read.watch_errors,
            ack_latency: outcome.write.ack.percentiles(),
            head_latency: outcome.read.head.percentiles(),
            read_latency: outcome.read.read.percentiles(),
            read_call_latency: outcome.read.read_calls.percentiles(),
            windows: Vec::new(),
            verdict: outcome.verdict,
        }
    }

    /// A phase that could not run at all.
    #[must_use]
    pub fn failed(scenario: ScenarioId, cell: String, phase: PhaseKind, reason: String) -> Self {
        let mut verdict = Verdict::default();
        verdict.fail(reason);
        Self {
            scenario,
            cell,
            phase,
            capped: false,
            target_events_per_sec: None,
            written_events_per_sec: 0.0,
            read_events_per_sec: 0.0,
            payload_mib_per_sec: 0.0,
            acked: 0,
            read: 0,
            appends: 0,
            resends: 0,
            errors: 0,
            ack_latency: Percentiles::default(),
            head_latency: Percentiles::default(),
            read_latency: Percentiles::default(),
            read_call_latency: Percentiles::default(),
            windows: Vec::new(),
            verdict,
        }
    }

    /// One line for progress output.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{}/{} {}: written {:.0} ev/s, read {:.0} ev/s, read p99 {} ms, {}",
            self.scenario,
            self.cell,
            self.phase.label(),
            self.written_events_per_sec,
            self.read_events_per_sec,
            ms(self.read_latency.p99_ns),
            check(&self.verdict),
        )
    }
}

fn read_count(outcome: &PhaseOutcome, headline: Headline) -> u64 {
    match headline {
        Headline::Written => outcome.read.consumed_in_window,
        Headline::Read => outcome.read.consumed,
    }
}

/// Everything one run measured.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub backend: String,
    pub settings: Settings,
    pub cells: Vec<CellReport>,
}

impl Report {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.cells.iter().all(|cell| cell.verdict.passed())
    }

    /// One table per scenario that ran.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut sections = vec![format!(
            "# Backend bench: {}\n\nduration {}, warmup {}, payload {} B, keys {}, at most {} events per phase, pool {}",
            self.backend,
            humantime::format_duration(self.settings.duration),
            humantime::format_duration(self.settings.warmup),
            self.settings.payload_bytes,
            self.settings.keys,
            self.settings.max_events_per_phase,
            self.settings.pool_size,
        )];
        for scenario in &self.settings.scenarios {
            let cells: Vec<&CellReport> = self
                .cells
                .iter()
                .filter(|cell| cell.scenario == *scenario)
                .collect();
            if cells.is_empty() {
                continue;
            }
            let table = match scenario {
                ScenarioId::B1 => ingest_table(&cells),
                ScenarioId::B2 => pipeline_table(&cells),
                ScenarioId::B3 => catchup_table(&cells),
                ScenarioId::B7 => soak_table(&cells),
            };
            sections.push(format!("## {}\n\n{table}", scenario.title()));
        }
        sections.join("\n\n") + "\n"
    }

    /// # Errors
    /// None in practice: every field serializes.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

fn check(verdict: &Verdict) -> String {
    if verdict.passed() {
        "ok".to_owned()
    } else {
        format!("FAIL: {}", verdict.failures.join("; "))
    }
}

fn spread(p: &Percentiles) -> String {
    if p.samples == 0 {
        return "-".to_owned();
    }
    format!(
        "{} / {} / {} / {}",
        ms(p.p50_ns),
        ms(p.p99_ns),
        ms(p.p99_9_ns),
        ms(p.max_ns)
    )
}

/// The phase, marked when the event cap cut it short.
fn phase(cell: &CellReport) -> String {
    if cell.capped {
        format!("{} (capped)", cell.phase.label())
    } else {
        cell.phase.label().to_owned()
    }
}

fn rate(value: f64) -> String {
    format!("{value:.0}")
}

fn target(cell: &CellReport) -> String {
    cell.target_events_per_sec
        .map_or_else(|| "-".to_owned(), rate)
}

fn table(header: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut lines = vec![
        format!("| {} |", header.join(" | ")),
        format!("|{}", "---|".repeat(header.len())),
    ];
    lines.extend(
        rows.into_iter()
            .map(|row| format!("| {} |", row.join(" | "))),
    );
    lines.join("\n")
}

fn ingest_table(cells: &[&CellReport]) -> String {
    table(
        &[
            "cell",
            "phase",
            "target ev/s",
            "written ev/s",
            "MiB/s",
            "ack ms p50 / p99 / p99.9 / max",
            "errors",
            "check",
        ],
        cells
            .iter()
            .map(|c| {
                vec![
                    c.cell.clone(),
                    phase(c),
                    target(c),
                    rate(c.written_events_per_sec),
                    format!("{:.2}", c.payload_mib_per_sec),
                    spread(&c.ack_latency),
                    c.errors.to_string(),
                    check(&c.verdict),
                ]
            })
            .collect(),
    )
}

fn pipeline_row(c: &CellReport) -> Vec<String> {
    vec![
        c.cell.clone(),
        phase(c),
        target(c),
        rate(c.written_events_per_sec),
        rate(c.read_events_per_sec),
        format!("{:.2}", c.payload_mib_per_sec),
        spread(&c.head_latency),
        spread(&c.read_latency),
        c.errors.to_string(),
        check(&c.verdict),
    ]
}

const PIPELINE_HEADER: [&str; 10] = [
    "cell",
    "phase",
    "target ev/s",
    "written ev/s",
    "read ev/s",
    "MiB/s",
    "head ms p50 / p99 / p99.9 / max",
    "read ms p50 / p99 / p99.9 / max",
    "errors",
    "check",
];

fn pipeline_table(cells: &[&CellReport]) -> String {
    table(
        &PIPELINE_HEADER,
        cells.iter().map(|c| pipeline_row(c)).collect(),
    )
}

fn catchup_table(cells: &[&CellReport]) -> String {
    table(
        &[
            "cell",
            "prefilled",
            "read",
            "read ev/s",
            "MiB/s",
            "read call ms p50 / p99 / p99.9 / max",
            "errors",
            "check",
        ],
        cells
            .iter()
            .map(|c| {
                vec![
                    c.cell.clone(),
                    c.acked.to_string(),
                    c.read.to_string(),
                    rate(c.read_events_per_sec),
                    format!("{:.2}", c.payload_mib_per_sec),
                    spread(&c.read_call_latency),
                    c.errors.to_string(),
                    check(&c.verdict),
                ]
            })
            .collect(),
    )
}

fn soak_table(cells: &[&CellReport]) -> String {
    let mut parts = vec![table(
        &PIPELINE_HEADER,
        cells.iter().map(|c| pipeline_row(c)).collect(),
    )];
    for cell in cells.iter().filter(|c| !c.windows.is_empty()) {
        parts.push(table(
            &[
                "second",
                "events",
                "head ms p99",
                "head ms max",
                "read ms p99",
                "read ms max",
            ],
            cell.windows
                .iter()
                .map(|w| {
                    vec![
                        w.second.to_string(),
                        w.read.samples.to_string(),
                        ms(w.head.p99_ns),
                        ms(w.head.max_ns),
                        ms(w.read.p99_ns),
                        ms(w.read.max_ns),
                    ]
                })
                .collect(),
        ));
    }
    parts.join("\n\n")
}
