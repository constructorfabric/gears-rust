//! B7 soak: the batching 16p16c async/async pipeline cell, open loop at
//! [`SOAK_LOAD`] of its peak, reported per second.

use crate::args::ScenarioId;
use crate::consume::Samples;
use crate::matrix::{PipelineCell, qualified};
use crate::produce::Pacing;
use crate::report::{CellReport, PhaseKind};
use crate::run::Bench;
use crate::scenario::phases::{run_peak, run_phase};
use crate::scenario::pipeline::plan;
use crate::window::Windows;
use std::sync::Arc;

/// The soak runs at this fraction of the measured peak.
pub const SOAK_LOAD: f64 = 0.7;

/// Runs the B7 soak, measuring its peak first unless B2 already did.
pub async fn run(bench: &mut Bench<'_>) {
    let cell = PipelineCell::SOAK;
    let name = cell.name();
    if !bench.admits(ScenarioId::B7, &name) {
        return;
    }
    let window = (bench.settings.warmup, bench.settings.duration);
    // B2's peak of the same cell when this run measured it; otherwise measure it here.
    let mut peak = bench.recall_peak(&qualified(ScenarioId::B2, &name));
    if peak.is_none() {
        let peak_plan = plan(cell, Pacing::Closed, Samples::Publish, window);
        peak = run_peak(bench, ScenarioId::B7, &name, peak_plan).await;
    }
    let Some(peak) = peak else {
        let reason = "no peak rate to pace at".to_owned();
        bench.record(CellReport::failed(
            ScenarioId::B7,
            name,
            PhaseKind::Soak,
            reason,
        ));
        return;
    };
    let windows = Arc::new(Windows::new());
    let soak_plan = plan(
        cell,
        Pacing::Open {
            events_per_sec: peak * SOAK_LOAD,
        },
        Samples::PerSecond(Arc::clone(&windows)),
        (bench.settings.warmup, bench.settings.soak_duration),
    );
    let mut report = run_phase(bench, ScenarioId::B7, &name, PhaseKind::Soak, soak_plan).await;
    let (read, head) = windows.merged();
    report.read_latency = read.percentiles();
    report.head_latency = head.percentiles();
    report.windows = windows.collect();
    bench.record(report);
}
