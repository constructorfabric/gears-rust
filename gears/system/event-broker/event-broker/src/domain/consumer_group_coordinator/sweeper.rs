//! The loop that enforces member lifetimes (`ConsumerGroupCoordinator::sweep`)
//! and clears the routing markers of whatever it reaps.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{ConsumerGroupCoordinator, Removal};
use crate::domain::repo::RoutingMarkers;

/// How often lifetimes are checked. A constant rather than a knob: it bounds
/// how late a reap can be, not whether one happens, and one second is well
/// under the shortest lifetime a member can have (`min_session_timeout`).
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// Runs until `cancel` fires.
///
/// Every delivery instance runs its own: the members it sweeps are the ones
/// in its own memory, so there is nothing to elect a leader over.
pub async fn run(
    groups: Arc<ConsumerGroupCoordinator>,
    markers: Arc<dyn RoutingMarkers>,
    cancel: CancellationToken,
) {
    let mut ticks = tokio::time::interval(SWEEP_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            now = ticks.tick() => {
                for removal in groups.sweep(now) {
                    tracing::info!(
                        subscription_id = %removal.subscription_id,
                        group = %removal.group,
                        "subscription reaped"
                    );
                    clear_markers(&*markers, &removal).await;
                }
            }
        }
    }
}

/// Clears what a removed member leaves in the cluster cache. Best effort: the
/// member is already gone from memory, and a stale marker only routes a
/// request to an instance that truthfully answers it does not know the
/// subscription.
pub async fn clear_markers(markers: &dyn RoutingMarkers, removal: &Removal) {
    if let Err(err) = markers.unmark_subscription(removal.subscription_id).await {
        tracing::warn!(%err, subscription_id = %removal.subscription_id, "failed to clear subscription marker");
    }
    if removal.group_emptied
        && let Err(err) = markers.unmark_group(&removal.group).await
    {
        tracing::warn!(%err, group = %removal.group, "failed to clear group marker");
    }
}
