//! Every scenario against one harness, concurrently: one medium serves the
//! whole suite, so a medium slow to start is started once.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;

use futures_util::FutureExt;
use futures_util::future::join_all;

use crate::harness::Harness;

type Scenario = for<'a> fn(&'a dyn Harness) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

macro_rules! scenarios {
    ($($name:ident),+ $(,)?) => {
        [$((stringify!($name), (|h| Box::pin(crate::$name(h))) as Scenario)),+]
    };
}

pub const SCENARIOS: [(&str, Scenario); 27] = scenarios!(
    provision_compare_and_set,
    provision_reports_the_held_partition_count,
    not_provisioned_everywhere,
    append_reports_heads,
    append_assigns_rising_sequences,
    append_keeps_key_order,
    append_respects_its_limit,
    concurrent_appenders_assign_distinct_sequences,
    dedup_turns_a_retry_into_a_duplicate,
    dedup_refuses_a_late_copy,
    dedup_accepts_gaps,
    dedup_keeps_keys_apart,
    dedup_survives_a_rebuild,
    read_is_exclusive_and_bounded,
    read_wait_returns_on_append,
    read_wait_times_out_empty,
    resolve_positions,
    watch_starts_with_the_current_head,
    watch_yields_named_partitions_only,
    retention_keeps_events_within_bounds,
    retention_ignores_occurred_at,
    retention_removes_the_oldest_first,
    appended_events_survive_a_rebuild,
    tail_read_sees_another_instance,
    start_succeeds_with_the_medium_unreachable,
    stop_ends_the_workers,
    stop_is_harmless_unstarted_and_repeated,
);

/// Runs every scenario against `harness` at once and panics naming each one
/// that failed.
pub async fn run_all(harness: &dyn Harness) {
    let outcomes = join_all(SCENARIOS.iter().map(|(name, scenario)| async move {
        let outcome = AssertUnwindSafe(scenario(harness)).catch_unwind().await;
        (*name, outcome.is_ok())
    }))
    .await;
    for (name, passed) in &outcomes {
        if *passed {
            tracing::info!(scenario = *name, "conformance scenario ok");
        } else {
            tracing::warn!(scenario = *name, "conformance scenario FAILED");
        }
    }
    let failed: Vec<&str> = outcomes
        .iter()
        .filter(|(_, passed)| !passed)
        .map(|(name, _)| *name)
        .collect();
    assert!(
        failed.is_empty(),
        "conformance scenarios failed: {failed:?}"
    );
}
