//! A cell's peak phase, then its latency phase at a fraction of that peak.
//! Every phase gets a fresh backend instance, so an in-process backend frees
//! one phase's events before the next phase writes its own.

use crate::args::ScenarioId;
use crate::matrix::qualified;
use crate::phase::{PipelinePlan, Readers, run_pipeline};
use crate::produce::Pacing;
use crate::report::{CellReport, Headline, PhaseKind};
use crate::run::Bench;

/// The latency phase runs at this fraction of the measured peak.
pub const LATENCY_LOAD: f64 = 0.5;

/// Runs `plan` closed loop, records it, and returns the rate the cell
/// sustained: written, or the lower of written and read when consumers
/// follow. `None` when the phase failed to run or moved nothing.
pub async fn run_peak(
    bench: &mut Bench<'_>,
    scenario: ScenarioId,
    cell: &str,
    plan: PipelinePlan,
) -> Option<f64> {
    let follows = matches!(plan.readers, Readers::Follow(_));
    let report = run_phase(bench, scenario, cell, PhaseKind::Peak, plan).await;
    let sustained = if follows {
        report
            .written_events_per_sec
            .min(report.read_events_per_sec)
    } else {
        report.written_events_per_sec
    };
    bench.record(report);
    (sustained > 0.0).then(|| {
        bench.remember_peak(qualified(scenario, cell), sustained);
        sustained
    })
}

/// Runs one phase of `plan` on a fresh instance and returns its report,
/// unrecorded.
pub async fn run_phase(
    bench: &Bench<'_>,
    scenario: ScenarioId,
    cell: &str,
    phase: PhaseKind,
    plan: PipelinePlan,
) -> CellReport {
    let instance = match bench.instance(plan.callers()).await {
        Ok(instance) => instance,
        Err(err) => {
            let reason = format!("backend did not start: {err}");
            return CellReport::failed(scenario, cell.to_owned(), phase, reason);
        }
    };
    let pacing = plan.pacing;
    let outcome = run_pipeline(&instance.backend, bench.settings, plan).await;
    instance.retire().await;
    match outcome {
        Ok(outcome) => CellReport::of(
            scenario,
            cell.to_owned(),
            phase,
            pacing,
            outcome,
            Headline::Written,
            bench.settings.payload_bytes,
        ),
        Err(err) => CellReport::failed(scenario, cell.to_owned(), phase, err.to_string()),
    }
}

/// Builds the peak phase's plan with `plan(Pacing::Closed)`, then the latency
/// phase's at [`LATENCY_LOAD`] of the sustained peak.
pub async fn run_peak_then_latency(
    bench: &mut Bench<'_>,
    scenario: ScenarioId,
    cell: &str,
    plan: impl Fn(Pacing) -> PipelinePlan,
) {
    let Some(peak) = run_peak(bench, scenario, cell, plan(Pacing::Closed)).await else {
        bench.record(CellReport::failed(
            scenario,
            cell.to_owned(),
            PhaseKind::Latency,
            "no peak rate to pace at".to_owned(),
        ));
        return;
    };
    let pacing = Pacing::Open {
        events_per_sec: peak * LATENCY_LOAD,
    };
    let report = run_phase(bench, scenario, cell, PhaseKind::Latency, plan(pacing)).await;
    bench.record(report);
}
