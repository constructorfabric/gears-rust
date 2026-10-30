//! B3 catch-up: a prefilled topic drained from the start.

use crate::args::ScenarioId;
use crate::consume::{ReadPlan, Samples};
use crate::matrix::{CatchupCell, ReadMode};
use crate::phase::run_catchup;
use crate::produce::Pacing;
use crate::report::{CellReport, Headline, PhaseKind};
use crate::run::Bench;
use crate::workload::PARTITIONS;

/// Runs every admitted B3 cell.
pub async fn run(bench: &mut Bench<'_>) {
    for cell in CatchupCell::all() {
        let name = cell.name();
        if !bench.admits(ScenarioId::B3, &name) {
            continue;
        }
        // The 16 prefill writers, then the consumers alone.
        let callers = cell.consumers.count().max(PARTITIONS as usize);
        let report = match bench.instance(callers).await {
            Ok(instance) => {
                let read = ReadPlan {
                    consumers: cell.consumers.count(),
                    max_events: cell.size.max_events(),
                    mode: ReadMode::Sync,
                    samples: Samples::ReadCalls,
                };
                let outcome = run_catchup(
                    &instance.backend,
                    bench.settings,
                    bench.settings.catchup_events,
                    read,
                )
                .await;
                instance.retire().await;
                match outcome {
                    Ok(outcome) => CellReport::of(
                        ScenarioId::B3,
                        name,
                        PhaseKind::CatchUp,
                        Pacing::Closed,
                        outcome,
                        Headline::Read,
                        bench.settings.payload_bytes,
                    ),
                    Err(err) => CellReport::failed(
                        ScenarioId::B3,
                        name,
                        PhaseKind::CatchUp,
                        err.to_string(),
                    ),
                }
            }
            Err(err) => CellReport::failed(
                ScenarioId::B3,
                name,
                PhaseKind::CatchUp,
                format!("backend did not start: {err}"),
            ),
        };
        bench.record(report);
    }
}
