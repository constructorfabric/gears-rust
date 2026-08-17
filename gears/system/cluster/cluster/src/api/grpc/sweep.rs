// Created: 2026-08-13 by Constructor Tech
//! The abandoned-subscription sweep — DESIGN-DEPLOYABLE-GEAR §5.4.1 (ask `A6`,
//! decision 18), item `S2`.
//!
//! [`ElectionSubscriptions`](super::ElectionSubscriptions) holds the state and
//! decides which entries are abandoned; this module holds the *schedule* — the
//! cadence, the grace window, the background task, and the two metrics §5.4.1
//! contracts.
//!
//! # Why this exists, and why it is not hygiene
//!
//! `join` opens a subscription unconditionally, and a follower re-`join`s on the
//! renewal cadence because the server announces no leadership (§6.6). The client
//! pump keeps its original `election_id`, so every re-claim attempt leaves an
//! unattached entry behind — **one per renewal interval**, one every 10 s on the
//! default `ElectionConfig`. Measured before this was written: a follower took the
//! table from 2 entries to 6 across five 100 ms intervals
//! (`tests/remote_backends.rs`). Without a sweep that is unbounded growth in a
//! steady state, not merely in a failure mode.
//!
//! The same pass covers the case decision 18 was actually written about — a client
//! that vanished without closing its stream — because both look identical from
//! here: an entry no reader is holding.
//!
//! # The cheaper fix, and why it is not available
//!
//! Having `join` reuse the caller's existing subscription for the same election
//! would fix the *rate* at its source. It cannot be keyed safely: under v1's
//! `TrustedNetwork` mode every caller resolves to the single name
//! `unauthenticated` (`identity::UNAUTHENTICATED_CALLER`), so `(caller, election)`
//! names the whole fleet rather than a participant, and even under a real
//! authenticator two replicas of one Deployment share a `ServiceAccount` name.
//! Telling those apart is the entire reason the id is a fresh UUID. The sweep
//! bounds the population either way, so it is both the necessary fix and the
//! sufficient one (§5.4.1).

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use cluster_sdk::observability::fields::label;
use cluster_sdk::observability::primitive;
use opentelemetry::metrics::{Counter, Gauge, Meter};
use opentelemetry::{InstrumentationScope, KeyValue, global};
use tokio_util::sync::CancellationToken;

use super::subscriptions::{ElectionSubscriptions, SharedSubscriptions, SweepReport};

/// How often the sweep runs (§5.4.1).
///
/// The plugins' lock-reaper default (`default_lock_reaper_interval()`), which is
/// what "sharing the plugins' existing reaper cadence" can mean from here: a
/// plugin's interval is per-provider operator config the gear cannot read, and two
/// bound providers may disagree, so the gear carries the same *value* rather than
/// the same knob.
///
/// A constant and not a config key. No operator has a reason to tune the lifetime
/// of a server-side bookkeeping entry, and a key would be a schema addition
/// needing its own decision.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

/// How many sweep intervals an unread subscription survives before it is reaped.
///
/// Must be greater than one, or an entry could be reaped on the very pass that
/// first observes it unread; three gives two full passes of slack. On
/// [`SWEEP_INTERVAL`] that is a 15 s window, which bounds a steady-state
/// follower's leaked subscriptions at `grace / renewal_interval` — about two per
/// participant on the default `ElectionConfig`, a constant rather than a leak.
pub const SWEEP_GRACE_MULTIPLIER: u32 = 3;

/// The grace window for a given cadence — see [`SWEEP_GRACE_MULTIPLIER`].
#[must_use]
pub fn sweep_grace(interval: Duration) -> Duration {
    interval * SWEEP_GRACE_MULTIPLIER
}

/// Instrumentation scope for the gear's own, non-plugin metrics.
///
/// The same scope name the SDK's `OtelClusterMetrics` uses for the ADR-004
/// contract signals, because this *is* the cluster gear. The instruments below
/// are emitted through a meter this module owns directly rather than through the
/// `ClusterMetrics` port, on the precedent the Postgres plugin set for its own
/// gauges (`plugins/postgres-cluster-plugin/src/lock/reaper.rs`): that port is the
/// sink a **backend** reports through, it exposes neither a gauge nor a
/// free-form counter, and a session index is not a provider signal — no plugin
/// will ever emit it. Adding two methods there would move every implementor and
/// the `NoopMetrics` double for signals none of them produce.
const SWEEP_SCOPE: &str = "cf-gears-cluster";

/// The process-global meter under [`SWEEP_SCOPE`], used when no meter is injected
/// (production). Tests inject their own over an in-memory reader to read the
/// instruments back.
#[must_use]
pub fn sweep_meter() -> Meter {
    global::meter_with_scope(InstrumentationScope::builder(SWEEP_SCOPE).build())
}

/// The reap counter's **instrument** name (§5.4.1).
///
/// No `_total`, per the observability catalog's §5.1 rule: the Prometheus
/// exporter appends it, so the scraped series is `cluster_subscriptions_reaped_total`
/// and including it here would double it.
pub const SUBSCRIPTIONS_REAPED: &str = "cluster_subscriptions_reaped";

/// The live-population gauge's name (§5.4.1). A gauge takes no `_total`.
pub const SUBSCRIPTIONS_ACTIVE: &str = "cluster_subscriptions_active";

/// The sweep's two instruments, and the labels they carry.
///
/// Exactly `(profile, primitive)` on both, and nothing else. An election name is
/// unbounded-cardinality and stays in the reap log (invariant I15, ADR-004).
pub struct SubscriptionMetrics {
    /// `cluster_subscriptions_reaped{profile,primitive}`.
    reaped: Counter<u64>,
    /// `cluster_subscriptions_active{profile,primitive}`.
    active: Gauge<u64>,
    /// Profiles the gauge has reported a non-zero population for.
    ///
    /// Without this, a profile whose last subscription is reaped simply stops
    /// being reported and its series strands at its final non-zero value — which
    /// reads as "still occupied" forever. Kept here rather than in the task loop
    /// so one pass is a self-contained call and the tests can drive it directly.
    reported: Mutex<BTreeSet<&'static str>>,
}

impl SubscriptionMetrics {
    /// Builds the instruments on `meter`.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            reaped: meter
                .u64_counter(SUBSCRIPTIONS_REAPED)
                .with_description("Abandoned election watch subscriptions reaped by the sweep")
                .build(),
            active: meter
                .u64_gauge(SUBSCRIPTIONS_ACTIVE)
                .with_description("Election watch subscriptions this replica is serving")
                .build(),
            reported: Mutex::new(BTreeSet::new()),
        }
    }

    /// The instruments on the process-global meter — the production sink.
    #[must_use]
    pub fn global() -> Self {
        Self::new(&sweep_meter())
    }

    /// Records one pass.
    fn record(&self, report: &SweepReport) {
        for (profile, count) in &report.reaped {
            self.reaped.add(*count, &labels(profile));
        }

        let mut reported = self.reported.lock().unwrap_or_else(PoisonError::into_inner);
        for (profile, count) in &report.live {
            self.active.record(*count, &labels(profile));
            reported.insert(profile);
        }
        // Whatever was reported last time and is absent now has fallen to zero,
        // so say zero rather than going quiet.
        let emptied: Vec<&'static str> = reported
            .iter()
            .filter(|profile| !report.live.contains_key(*profile))
            .copied()
            .collect();
        for profile in emptied {
            self.active.record(0, &labels(profile));
            reported.remove(profile);
        }
    }
}

/// The label set every instrument here carries.
fn labels(profile: &'static str) -> [KeyValue; 2] {
    [
        KeyValue::new(label::PROFILE, profile),
        // Constant for now: this table holds election subscriptions only. The
        // label is still emitted, so a second watch table slots in beside it
        // without the series changing shape.
        KeyValue::new(label::PRIMITIVE, primitive::LEADER),
    ]
}

/// One pass: reap, then report.
///
/// The unit the task loops over, and the unit the tests drive — so a test asserts
/// the same code production runs, with no sleeping.
pub fn sweep_once(
    subscriptions: &ElectionSubscriptions,
    grace: Duration,
    metrics: &SubscriptionMetrics,
) -> SweepReport {
    let report = subscriptions.sweep(grace);
    metrics.record(&report);
    report
}

/// Spawns the sweep, returning its task handle.
///
/// Sleeps first. There is nothing to reap at `t = 0`, and a pass then would only
/// walk an empty table before any entry could have aged into its window.
///
/// Cancellation ends it from inside the sleep, so a `stop()` is not held up for
/// most of an interval.
pub fn spawn_subscription_sweep(
    subscriptions: SharedSubscriptions,
    interval: Duration,
    metrics: SubscriptionMetrics,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let grace = sweep_grace(interval);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(interval) => {}
            }
            let report = sweep_once(&subscriptions, grace, &metrics);
            let reaped = report.reaped_total();
            if reaped > 0 {
                tracing::debug!(
                    reaped,
                    live = subscriptions.len(),
                    "cluster: swept abandoned election subscriptions"
                );
            }
        }
    })
}

#[cfg(test)]
#[path = "sweep_tests.rs"]
mod sweep_tests;
