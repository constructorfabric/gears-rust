//! The command line a plugin's bench entry hands over, and the settings it
//! resolves to.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use serde::Serialize;

use crate::matrix::CellFilter;

/// One scenario of the bench (`README.md` lists what each runs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScenarioId {
    /// Ingest append: producers only.
    B1,
    /// Pipeline: producers and consumers over the full matrix.
    B2,
    /// Catch-up: consumers draining a prefilled topic.
    B3,
    /// Soak: one pipeline cell at a steady rate for a long time.
    B7,
}

impl ScenarioId {
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::B1 => "B1 ingest append",
            Self::B2 => "B2 pipeline",
            Self::B3 => "B3 catch-up",
            Self::B7 => "B7 soak",
        }
    }
}

impl fmt::Display for ScenarioId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::B1 => "b1",
            Self::B2 => "b2",
            Self::B3 => "b3",
            Self::B7 => "b7",
        })
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "event-broker-backend-bench",
    about = "Benchmarks an event-broker storage backend through the backend contract"
)]
struct Args {
    /// Scenarios to run, comma separated.
    #[arg(long, value_delimiter = ',', default_values = ["b1", "b2", "b3", "b7"])]
    scenarios: Vec<ScenarioId>,

    /// Cells to run: comma-separated patterns matched against `<scenario>/<cell>`,
    /// `*` matching any run of characters.
    #[arg(long, value_delimiter = ',', default_value = "*")]
    cells: Vec<String>,

    /// Measured time of each phase [default: 20s, 5s with --quick].
    #[arg(long, value_parser = humantime::parse_duration)]
    duration: Option<Duration>,

    /// Unmeasured time before each phase's measurement [default: 3s, 1s with --quick].
    #[arg(long, value_parser = humantime::parse_duration)]
    warmup: Option<Duration>,

    /// Short phases for a smoke run; explicit flags still win.
    #[arg(long)]
    quick: bool,

    /// Bytes of padding in each event's payload.
    #[arg(long, default_value_t = 512)]
    payload_bytes: usize,

    /// Distinct partition keys the producers draw from.
    #[arg(long, default_value_t = 10_000)]
    keys: u32,

    /// Where to write the JSON report.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Events after which a phase ends even if its duration has not passed;
    /// B3 prefills at most this many [default: 2000000, 500000 with --quick].
    #[arg(long)]
    max_events_per_phase: Option<u64>,

    /// Measured time of the B7 soak [default: 10m, 30s with --quick].
    #[arg(long, value_parser = humantime::parse_duration)]
    soak_duration: Option<Duration>,

    /// Events B3 prefills before draining [default: 1000000, 100000 with --quick].
    #[arg(long)]
    catchup_events: Option<u64>,

    /// Connections every backend instance may pool [default: per phase, its
    /// concurrent writers and consumers plus 2].
    #[arg(long)]
    pool_size: Option<u32>,
    /// Passed by `cargo bench`; ignored.
    #[arg(long, hide = true)]
    bench: bool,
}

/// What one run does, resolved from the command line.
#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub scenarios: Vec<ScenarioId>,
    #[serde(skip)]
    pub cells: CellFilter,
    #[serde(with = "humantime_serde_str")]
    pub duration: Duration,
    #[serde(with = "humantime_serde_str")]
    pub warmup: Duration,
    pub payload_bytes: usize,
    pub keys: u32,
    #[serde(skip)]
    pub out: Option<PathBuf>,
    #[serde(with = "humantime_serde_str")]
    pub soak_duration: Duration,
    pub catchup_events: u64,
    pub max_events_per_phase: u64,
    pub pool_size: PoolSize,
}

/// How many connections a phase's backend instance may pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolSize {
    /// The phase's concurrent callers plus [`PoolSize::HEADROOM`], so a
    /// caller never waits for a connection.
    PerPhase,
    /// This many in every phase, such as a deployment's setting.
    Fixed(u32),
}

impl PoolSize {
    /// Connections beyond the callers: maintenance and the provisioning probe.
    pub const HEADROOM: u32 = 2;

    /// Connections for a phase with `callers` concurrent writers and readers.
    #[must_use]
    pub fn connections(self, callers: usize) -> u32 {
        match self {
            Self::PerPhase => u32::try_from(callers)
                .unwrap_or(u32::MAX)
                .saturating_add(Self::HEADROOM),
            Self::Fixed(connections) => connections,
        }
    }
}

impl fmt::Display for PoolSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PerPhase => write!(f, "per phase (callers + {})", Self::HEADROOM),
            Self::Fixed(connections) => write!(f, "{connections}"),
        }
    }
}

impl Settings {
    /// Parses a command line such as `std::env::args()`; prints usage and
    /// exits on a malformed one, as a binary's own parser would.
    pub fn parse_from<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        Self::from(Args::parse_from(args))
    }
}

impl From<Args> for Settings {
    fn from(args: Args) -> Self {
        let pick = |given: Option<Duration>, full: Duration, quick: Duration| {
            given.unwrap_or(if args.quick { quick } else { full })
        };
        let mut scenarios = args.scenarios.clone();
        scenarios.sort_unstable();
        scenarios.dedup();
        Self {
            scenarios,
            cells: CellFilter::new(args.cells.clone()),
            duration: pick(
                args.duration,
                Duration::from_secs(20),
                Duration::from_secs(5),
            ),
            warmup: pick(args.warmup, Duration::from_secs(3), Duration::from_secs(1)),
            payload_bytes: args.payload_bytes,
            keys: args.keys.max(1),
            out: args.out,
            soak_duration: pick(
                args.soak_duration,
                Duration::from_mins(10),
                Duration::from_secs(30),
            ),
            catchup_events: args.catchup_events.unwrap_or(if args.quick {
                100_000
            } else {
                1_000_000
            }),
            max_events_per_phase: args
                .max_events_per_phase
                .unwrap_or(if args.quick { 500_000 } else { 2_000_000 })
                .max(1),
            pool_size: args
                .pool_size
                .map_or(PoolSize::PerPhase, |n| PoolSize::Fixed(n.max(1))),
        }
    }
}

/// Durations in the JSON report read as `20s`, not as a seconds/nanos pair.
mod humantime_serde_str {
    use std::time::Duration;

    use serde::Serializer;

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&humantime::format_duration(*value))
    }
}
