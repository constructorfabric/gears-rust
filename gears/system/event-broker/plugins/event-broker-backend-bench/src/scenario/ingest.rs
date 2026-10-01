//! B1 ingest append: writers only, every size, writer count and push mode.

use crate::args::ScenarioId;
use crate::matrix::IngestCell;
use crate::phase::{PipelinePlan, Readers};
use crate::run::Bench;
use crate::scenario::phases::run_peak_then_latency;

/// Runs every admitted B1 cell.
pub async fn run(bench: &mut Bench<'_>) {
    for cell in IngestCell::all() {
        let name = cell.name();
        if !bench.admits(ScenarioId::B1, &name) {
            continue;
        }
        let (warmup, duration) = (bench.settings.warmup, bench.settings.duration);
        run_peak_then_latency(bench, ScenarioId::B1, &name, |pacing| PipelinePlan {
            label: "ingest",
            writers: cell.writers.count(),
            events_per_append: cell.size.events_per_append(),
            flush: cell.push.flush(),
            pacing,
            readers: Readers::AfterwardsOnly,
            warmup,
            duration,
        })
        .await;
    }
}
