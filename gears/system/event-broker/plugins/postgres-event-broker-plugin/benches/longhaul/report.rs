//! The pass criteria, judged from the samples and the tally, and the report.

use std::time::Duration;

use serde::Serialize;

use crate::probe::Sample;
use crate::tally::Outage;
use crate::topics::Kind;

/// One figure of a sample, as a number.
type Metric = fn(&Sample) -> f64;

/// How far a peak or a count may move before it fails.
const BAND: f64 = 0.2;

#[derive(Debug, Clone, Serialize)]
pub struct Criterion {
    pub name: &'static str,
    /// `None`: not judged, e.g. the run ended before its warmup.
    pub passed: Option<bool>,
    pub detail: String,
}

impl Criterion {
    fn judged(name: &'static str, passed: bool, detail: String) -> Self {
        Self {
            name,
            passed: Some(passed),
            detail,
        }
    }

    fn not_judged(name: &'static str, detail: &str) -> Self {
        Self {
            name,
            passed: None,
            detail: detail.to_owned(),
        }
    }
}

/// How the settled invariants came out at the end.
pub enum Settled {
    /// None broken, after this long.
    Clean(Duration),
    /// Still broken when the time ran out.
    Broken(Vec<String>),
    /// Not checked, and why.
    NotChecked(&'static str),
}

/// What the criteria are judged against.
pub struct Bounds {
    pub warmup: Duration,
    pub window: Duration,
    pub max_age: Duration,
    /// One segment's fill time at the base rate: the oldest event waits that
    /// long for its segment to seal before it can expire.
    pub fill: Duration,
    pub grace: Duration,
}

pub fn criteria(
    samples: &[Sample],
    bounds: &Bounds,
    violations: u64,
    settled: &Settled,
) -> Vec<Criterion> {
    let mut out = vec![
        Criterion::judged(
            "no violation",
            violations == 0,
            format!("{violations} counted"),
        ),
        match settled {
            Settled::Clean(took) => Criterion::judged(
                "settled invariants",
                true,
                format!("none broken after {:.0}s", took.as_secs_f64()),
            ),
            Settled::Broken(broken) => {
                Criterion::judged("settled invariants", false, broken.join("; "))
            }
            Settled::NotChecked(why) => Criterion::not_judged("settled invariants", why),
        },
    ];
    let peaks: [(&'static str, Metric); 3] = [
        ("segment rows plateau (age)", |s| rows(s, Kind::Age)),
        ("segment rows plateau (bytes)", |s| rows(s, Kind::Bytes)),
        ("resident memory plateau", |s| s.rss_bytes as f64),
    ];
    for (name, metric) in peaks {
        out.push(plateau(name, samples, bounds, metric));
    }

    let excess = samples
        .windows(2)
        .filter(|w| w[0].bytes_excess > 0 && w[1].bytes_excess > 0)
        .map(|w| w[0].bytes_excess.min(w[1].bytes_excess))
        .max();
    out.push(Criterion::judged(
        "bytes bound",
        excess.is_none(),
        excess.map_or_else(
            || "never over max_bytes plus two segments twice in a row".to_owned(),
            |e| format!("{e} bytes over, in two samples in a row"),
        ),
    ));

    let worst = samples.iter().map(|s| s.oldest_age_s).fold(0.0, f64::max);
    let limit = (bounds.max_age + bounds.fill + bounds.grace).as_secs_f64();
    out.push(Criterion::judged(
        "age bound",
        worst <= limit,
        format!(
            "oldest event at most {worst:.0}s old, limit {limit:.0}s \
             (max_age {:.0}s + segment fill {:.0}s + grace {:.0}s)",
            bounds.max_age.as_secs_f64(),
            bounds.fill.as_secs_f64(),
            bounds.grace.as_secs_f64()
        ),
    ));

    out.push(
        match (
            samples
                .iter()
                .find(|s| s.at_s >= bounds.warmup.as_secs_f64()),
            samples.last(),
        ) {
            (Some(start), Some(end)) if end.at_s > start.at_s => {
                let (a, b) = (start.sessions as f64, end.sessions as f64);
                Criterion::judged(
                    "plugin sessions steady",
                    (b - a).abs() <= a * BAND,
                    format!(
                        "{} at warmup end, {} at the end",
                        start.sessions, end.sessions
                    ),
                )
            }
            _ => Criterion::not_judged("plugin sessions steady", "the run ended before its warmup"),
        },
    );
    out
}

fn rows(sample: &Sample, kind: Kind) -> f64 {
    sample.rows.get(&kind).copied().unwrap_or(0) as f64
}

/// The peak of `metric` in the last whole window after warmup against the
/// first: each window holds one burst.
fn plateau(name: &'static str, samples: &[Sample], bounds: &Bounds, metric: Metric) -> Criterion {
    let from = bounds.warmup.as_secs_f64();
    let window = bounds.window.as_secs_f64().max(1.0);
    let end = samples.last().map_or(0.0, |s| s.at_s);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small whole count"
    )]
    let whole = ((end - from) / window).floor().max(0.0) as u64;
    if whole < 2 {
        return Criterion::not_judged(name, "fewer than two whole windows after warmup");
    }
    let peak = |i: u64| {
        let (a, b) = (from + window * i as f64, from + window * (i + 1) as f64);
        samples
            .iter()
            .filter(|s| s.at_s >= a && s.at_s < b)
            .map(metric)
            .fold(0.0, f64::max)
    };
    let (first, last) = (peak(0), peak(whole - 1));
    Criterion::judged(
        name,
        last <= first * (1.0 + BAND),
        format!("peak {first:.0} in the first of {whole} windows, {last:.0} in the last"),
    )
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub args: serde_json::Value,
    pub settings: serde_json::Value,
    pub elapsed_s: f64,
    pub warmup_s: f64,
    pub acked: Vec<(Kind, u64)>,
    pub unresolved: Vec<(Kind, u64)>,
    pub read: u64,
    pub skipped: u64,
    pub tail_skipped: u64,
    pub append_errors: u64,
    pub read_errors: u64,
    pub watch_errors: u64,
    pub check_errors: u64,
    pub transient: u64,
    pub violations: u64,
    pub violation_log: Vec<String>,
    pub outages: Vec<Outage>,
    pub settle_s: Option<f64>,
    pub criteria: Vec<Criterion>,
    pub samples: Vec<Sample>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.criteria.iter().all(|c| c.passed != Some(false))
    }

    pub fn markdown(&self) -> String {
        let mut lines = vec![
            "# Long-haul run".to_owned(),
            String::new(),
            format!(
                "{:.0}s, warmup {:.0}s; acked {}; read {}; skipped below the floor {} (tail {}); \
                 errors append {} read {} watch {} check {}; transient {}; violations {}; settled in {}",
                self.elapsed_s,
                self.warmup_s,
                self.acked
                    .iter()
                    .map(|(k, n)| format!("{}={n}", k.name()))
                    .collect::<Vec<_>>()
                    .join(" "),
                self.read,
                self.skipped,
                self.tail_skipped,
                self.append_errors,
                self.read_errors,
                self.watch_errors,
                self.check_errors,
                self.transient,
                self.violations,
                self.settle_s
                    .map_or_else(|| "-".to_owned(), |s| format!("{s:.0}s")),
            ),
            String::new(),
        ];
        if !self.outages.is_empty() {
            lines.push("| outage at | lasted |".to_owned());
            lines.push("|---|---|".to_owned());
            lines.extend(
                self.outages
                    .iter()
                    .map(|o| format!("| {:.0}s | {:.1}s |", o.started_at_s, o.lasted_s)),
            );
            lines.push(String::new());
        }
        lines.push("| criterion | result | detail |".to_owned());
        lines.push("|---|---|---|".to_owned());
        for c in &self.criteria {
            let result = match c.passed {
                Some(true) => "pass",
                Some(false) => "FAIL",
                None => "-",
            };
            lines.push(format!("| {} | {result} | {} |", c.name, c.detail));
        }
        if !self.violation_log.is_empty() {
            lines.push(String::new());
            lines.extend(self.violation_log.iter().map(|v| format!("- {v}")));
        }
        lines.join("\n")
    }
}
