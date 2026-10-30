//! A whole run: every selected scenario's cells against one harness, then the
//! report.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::{Backend, BackendError};
use tokio_util::sync::CancellationToken;

use crate::args::{ScenarioId, Settings};
use crate::harness::{Harness, Sizing};
use crate::report::{CellReport, Report};
use crate::scenario::{catchup, ingest, pipeline, soak};
use crate::workload::wait_ready;

/// Runs every scenario `settings` selects against `harness`, prints the
/// markdown report and writes the JSON one to `settings.out`. Fails when any
/// cell's check failed or the report could not be written.
#[allow(unknown_lints, de1301_no_print_macros)] // the report goes to stdout, progress to stderr
pub async fn run_bench(harness: &dyn Harness, settings: &Settings) -> ExitCode {
    let mut bench = Bench {
        harness,
        settings,
        cells: Vec::new(),
        peaks: HashMap::new(),
    };
    for scenario in &settings.scenarios {
        eprintln!("== {}", scenario.title());
        match scenario {
            ScenarioId::B1 => ingest::run(&mut bench).await,
            ScenarioId::B2 => pipeline::run(&mut bench).await,
            ScenarioId::B3 => catchup::run(&mut bench).await,
            ScenarioId::B7 => soak::run(&mut bench).await,
        }
    }
    let report = Report {
        backend: harness.name().to_owned(),
        settings: settings.clone(),
        cells: bench.cells,
    };
    println!("{}", report.to_markdown());
    let written = settings.out.as_ref().is_none_or(|out| {
        let saved = report
            .to_json()
            .map_err(std::io::Error::other)
            .and_then(|json| {
                if let Some(dir) = out.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(out, json)
            });
        match saved {
            Ok(()) => {
                eprintln!("report written to {}", out.display());
                true
            }
            Err(err) => {
                eprintln!("could not write {}: {err}", out.display());
                false
            }
        }
    });
    if report.passed() && written {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// What scenarios share while a run goes on.
pub struct Bench<'a> {
    harness: &'a dyn Harness,
    /// The run's settings.
    pub settings: &'a Settings,
    cells: Vec<CellReport>,
    /// Sustained peak rate per qualified cell name, for later scenarios.
    peaks: HashMap<String, f64>,
}

#[allow(unknown_lints, de1301_no_print_macros)] // per-cell progress on the bench's stderr
impl Bench<'_> {
    /// Whether `cell` of `scenario` passes `--cells`.
    pub fn admits(&self, scenario: ScenarioId, cell: &str) -> bool {
        self.settings.cells.admits(scenario, cell)
    }

    /// A fresh backend instance for a phase of `callers` concurrent writers
    /// and readers, once it answers.
    pub async fn instance(&self, callers: usize) -> Result<Instance, BackendError> {
        let sizing = Sizing {
            connections: self.settings.pool_size.connections(callers),
        };
        let instance = Instance {
            backend: self.harness.build(sizing).await,
        };
        instance.backend.start().await?;
        if let Err(err) = wait_ready(instance.backend.as_ref()).await {
            instance.retire().await;
            return Err(err);
        }
        Ok(instance)
    }

    /// Adds a cell's report and prints its summary.
    pub fn record(&mut self, cell: CellReport) {
        eprintln!("{}", cell.summary());
        self.cells.push(cell);
    }

    /// Keeps a cell's sustained peak for later scenarios.
    pub fn remember_peak(&mut self, qualified: String, events_per_sec: f64) {
        self.peaks.insert(qualified, events_per_sec);
    }

    /// A peak an earlier scenario of this run measured.
    pub fn recall_peak(&self, qualified: &str) -> Option<f64> {
        self.peaks.get(qualified).copied()
    }
}

/// How long a retired instance's workers get to stop in good order.
const RETIRE_GRACE: Duration = Duration::from_secs(5);

/// A backend built and started for one phase; [`Instance::retire`] stops it.
pub struct Instance {
    /// The backend under load.
    pub backend: Arc<dyn Backend>,
}

impl Instance {
    /// Stops the instance's workers, waiting for them, then drops the backend
    /// here: its last handle goes with it, so an in-process backend frees its
    /// events now, on this task, rather than in a worker that wakes while the
    /// next phase is already allocating.
    pub async fn retire(self) {
        let deadline = CancellationToken::new();
        {
            let stopping = self.backend.stop(deadline.clone());
            tokio::pin!(stopping);
            tokio::select! {
                () = &mut stopping => {}
                () = tokio::time::sleep(RETIRE_GRACE) => {
                    deadline.cancel();
                    stopping.await;
                }
            }
        }
        drop(self);
    }
}
