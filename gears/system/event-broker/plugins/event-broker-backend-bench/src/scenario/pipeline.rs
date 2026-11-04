//! B2 pipeline: writers and consumers together over the full matrix.

use std::time::Duration;

use crate::args::ScenarioId;
use crate::consume::{ReadPlan, Samples};
use crate::matrix::PipelineCell;
use crate::phase::{PipelinePlan, Readers};
use crate::produce::Pacing;
use crate::run::Bench;
use crate::scenario::phases::run_peak_then_latency;

/// Runs every admitted B2 cell.
pub async fn run(bench: &mut Bench<'_>) {
    for cell in PipelineCell::all() {
        let name = cell.name();
        if !bench.admits(ScenarioId::B2, &name) {
            continue;
        }
        let (warmup, duration) = (bench.settings.warmup, bench.settings.duration);
        run_peak_then_latency(bench, ScenarioId::B2, &name, |pacing| {
            plan(cell, pacing, Samples::Publish, (warmup, duration))
        })
        .await;
    }
}

/// The plan of one phase of `cell`, `window` being warmup then measured time.
pub fn plan(
    cell: PipelineCell,
    pacing: Pacing,
    samples: Samples,
    window: (Duration, Duration),
) -> PipelinePlan {
    let (warmup, duration) = window;
    PipelinePlan {
        label: "pipeline",
        writers: cell.fanout.count(),
        events_per_append: cell.size.events_per_append(),
        flush: cell.push.flush(),
        pacing,
        readers: Readers::Follow(ReadPlan {
            consumers: cell.fanout.count(),
            max_events: cell.size.max_events(),
            mode: cell.read,
            samples,
        }),
        warmup,
        duration,
    }
}
