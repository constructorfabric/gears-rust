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
    pub settings: &'a Settings,
    cells: Vec<CellReport>,
    /// Sustained peak rate per qualified cell name, for later scenarios.
    peaks: HashMap<String, f64>,
}

impl Bench<'_> {
    pub fn admits(&self, scenario: ScenarioId, cell: &str) -> bool {
        self.settings.cells.admits(scenario, cell)
    }

    /// A fresh backend instance for a phase of `callers` concurrent writers
    /// and readers, once it answers.
    pub async fn instance(&self, callers: usize) -> Result<Instance, BackendError> {
        let stop = CancellationToken::new();
        let sizing = Sizing {
            connections: self.settings.pool_size.connections(callers),
        };
        let backend = self.harness.build(sizing, stop.clone()).await;
        let instance = Instance { backend, stop };
        wait_ready(instance.backend.as_ref()).await?;
        Ok(instance)
    }

    pub fn record(&mut self, cell: CellReport) {
        eprintln!("{}", cell.summary());
        self.cells.push(cell);
    }

    pub fn remember_peak(&mut self, qualified: String, events_per_sec: f64) {
        self.peaks.insert(qualified, events_per_sec);
    }

    pub fn recall_peak(&self, qualified: &str) -> Option<f64> {
        self.peaks.get(qualified).copied()
    }
}

/// How long a retired instance's workers get to see the stop.
const RETIRE_GRACE: Duration = Duration::from_millis(200);

/// A backend built for one phase; its workers stop when this is dropped.
pub struct Instance {
    pub backend: Arc<dyn Backend>,
    stop: CancellationToken,
}

impl Instance {
    /// Stops the instance's workers, lets them drop their handles, then drops
    /// the backend here: its last handle goes with it, so an in-process
    /// backend frees its events now, on this task, rather than in a worker
    /// that wakes while the next phase is already allocating.
    pub async fn retire(self) {
        self.stop.cancel();
        tokio::time::sleep(RETIRE_GRACE).await;
        drop(self);
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
