// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-delivery:p1
//! The delivery loop: one `stateful` background task per replica that runs a
//! pass at start and then every `redeliver_interval_seconds`, re-publishing
//! what stays `awaiting`, calling silent what had its report timeout, and
//! forgetting what stayed silent for a day.
//!
//! The task owns the tick; its retries — the broker lookup, the activation
//! types' registration — ride on it, so no separate backoff is kept. Each tick
//! is the domain's [`ActivationNotifier::tick`]: the gauges are refreshed and
//! one pass runs under the scan lock whether or not a broker resolves — its
//! forgetting and orphan sweep always, its call of silence and its publishing
//! only with a broker, a known root tenant and the activation types
//! registered. The stop keeps every `awaiting` state: a pass in flight is
//! abandoned and the lock released.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::domain::activation::{ActivationNotifier, Tick};

/// The ceiling of the start jitter, so replicas started together do not align.
const START_JITTER_CEILING: Duration = Duration::from_secs(1);

/// The replica's delivery loop.
pub struct ActivationDelivery {
    notifier: Arc<ActivationNotifier>,
}

impl ActivationDelivery {
    /// A loop over `notifier`, ticking at its `redeliver_interval_seconds`.
    #[must_use]
    pub const fn new(notifier: Arc<ActivationNotifier>) -> Self {
        Self { notifier }
    }

    /// Run until `cancel` fires. The first pass runs at the start (after a
    /// random jitter of at most one second, capped by the interval) — it idles
    /// harmlessly while there is no broker or root tenant — then one comes every
    /// interval; a tick that overran delays the next rather than bursting.
    pub async fn run(&self, cancel: CancellationToken) {
        self.started();
        let period = self.notifier.interval();
        let first = Instant::now()
            .checked_add(start_jitter(period))
            .unwrap_or_else(Instant::now);
        let mut interval = tokio::time::interval_at(first, period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-7
        while ticked(&mut interval, &cancel).await {
            let tick = self.notifier.tick(&cancel).await;
            log(tick);
            if matches!(tick, Tick::Cancelled) {
                break;
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-7
        stopped();
    }

    fn started(&self) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-6
        info!(
            interval_secs = self.notifier.interval().as_secs_f64(),
            "activation delivery loop started"
        );
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-6
    }
}

/// A random delay of `0..=1 s`, never above `period`.
fn start_jitter(period: Duration) -> Duration {
    let ceiling = START_JITTER_CEILING.min(period);
    let millis = u64::try_from(ceiling.as_millis()).unwrap_or(0).max(1);
    let random = u64::try_from(uuid::Uuid::new_v4().as_u128() % u128::from(millis)).unwrap_or(0);
    Duration::from_millis(random)
}

/// Wait for the next tick: `false` once the token fired.
async fn ticked(interval: &mut tokio::time::Interval, cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        _ = interval.tick() => true,
    }
}

fn stopped() {
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-15
    // The token fired: a pass in flight was finished or abandoned by the tick
    // itself, the scan lock released, and every awaiting state kept.
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-15
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-18
    info!("activation delivery loop stopped");
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-18
}

/// What a tick says in the log: every refusal was logged at `warn` where it
/// happened; this line is the pass's summary.
fn log(tick: Tick) {
    match tick {
        Tick::Complete { published } if published > 0 => published_line(published),
        Tick::NotRegistered | Tick::Failed | Tick::BrokerLost | Tick::Stalled => ended_early(tick),
        Tick::Complete { .. } | Tick::Idle | Tick::Skipped | Tick::Cancelled => quiet(tick),
    }
}

fn published_line(published: usize) {
    info!(published, "activation delivery pass published");
}

fn ended_early(tick: Tick) {
    warn!(?tick, "activation delivery tick ended early");
}

fn quiet(tick: Tick) {
    debug!(?tick, "activation delivery tick ended");
}

#[cfg(test)]
#[path = "activation_delivery_tests.rs"]
mod activation_delivery_tests;
