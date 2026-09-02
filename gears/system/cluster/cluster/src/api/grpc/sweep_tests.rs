//! Tests for the abandoned-subscription sweep (§5.4.1, item `S2`).
//!
//! The policy is exercised through [`sweep_once`], which is the same call the
//! background task makes — so what is asserted here is production's code path
//! with production's metrics attached, not a reimplementation of it.
//!
//! Time is the tokio clock throughout ([`Subscription::last_seen`] is a
//! `tokio::time::Instant`), so `#[tokio::test(start_paused = true)]` advances the
//! grace window instantly and none of these tests sleep.

use std::time::Duration;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

use super::{
    SUBSCRIPTIONS_ACTIVE, SUBSCRIPTIONS_REAPED, SWEEP_GRACE_MULTIPLIER, SWEEP_INTERVAL,
    SubscriptionMetrics, sweep_grace, sweep_once,
};
use crate::api::grpc::subscriptions::ElectionSubscriptions;

/// The grace window every test here ages entries past.
const GRACE: Duration = Duration::from_secs(15);

/// A metrics sink that discards, for the tests asserting the policy rather than
/// the signals.
fn discarding() -> SubscriptionMetrics {
    SubscriptionMetrics::new(&SdkMeterProvider::builder().build().meter("discarding"))
}

/// A sink plus the exporter its instruments land in.
fn recording() -> (
    SubscriptionMetrics,
    InMemoryMetricExporter,
    SdkMeterProvider,
) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = SubscriptionMetrics::new(&provider.meter("sweep-tests"));
    (metrics, exporter, provider)
}

/// One recorded data point: its sorted `(key, value)` attributes, and its value.
type Point = (Vec<(String, String)>, u64);

/// Every data point recorded for `name`, with the attributes sorted so a
/// comparison does not depend on emission order.
fn points(exporter: &InMemoryMetricExporter, name: &str) -> Vec<Point> {
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};

    let mut found = Vec::new();
    let Ok(metrics) = exporter.get_finished_metrics() else {
        return found;
    };
    for resource in &metrics {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics() {
                if metric.name() != name {
                    continue;
                }
                let AggregatedMetrics::U64(data) = metric.data() else {
                    continue;
                };
                let sampled: Vec<_> = match data {
                    MetricData::Sum(sum) => sum
                        .data_points()
                        .map(|dp| (attributes(dp.attributes()), dp.value()))
                        .collect(),
                    MetricData::Gauge(gauge) => gauge
                        .data_points()
                        .map(|dp| (attributes(dp.attributes()), dp.value()))
                        .collect(),
                    _ => Vec::new(),
                };
                found.extend(sampled);
            }
        }
    }
    found
}

fn attributes<'a>(
    keys: impl Iterator<Item = &'a opentelemetry::KeyValue>,
) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = keys
        .map(|kv| (kv.key.to_string(), kv.value.as_str().into_owned()))
        .collect();
    pairs.sort();
    pairs
}

// The policy

#[tokio::test(start_paused = true)]
async fn an_attached_subscription_survives_however_long_it_is_held() {
    // The property that keeps the sweep from severing healthy elections: a
    // client holding its stream is present, and every pass says so by refreshing
    // `last_seen`. Held here for a hundred grace windows.
    let table = ElectionSubscriptions::new();
    let id = table.open("event-broker", "ledger", "orders");
    let _reader = table.attach(&id, "event-broker").expect("attaches");

    for _ in 0..100 {
        tokio::time::advance(GRACE).await;
        let report = sweep_once(&table, GRACE, &discarding());
        assert_eq!(report.reaped_total(), 0);
    }
    assert_eq!(table.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_never_attached_subscription_is_reaped_after_the_grace_window() {
    // The follower pump's leak: `join` opened it, `await_change` never came.
    let table = ElectionSubscriptions::new();
    let _leaked = table.open("event-broker", "ledger", "orders");

    // Inside the window it stays, so a client whose `await_change` is merely slow
    // still finds its id.
    tokio::time::advance(GRACE / 2).await;
    assert_eq!(sweep_once(&table, GRACE, &discarding()).reaped_total(), 0);
    assert_eq!(table.len(), 1);

    tokio::time::advance(GRACE).await;
    assert_eq!(sweep_once(&table, GRACE, &discarding()).reaped_total(), 1);
    assert!(table.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_subscription_whose_reader_went_away_is_reaped_after_the_grace_window() {
    // The case decision 18 was written about - a client that vanished without
    // closing its stream. Dropping the receiver is what the server's stream task
    // does when it notices the peer is gone.
    let table = ElectionSubscriptions::new();
    let id = table.open("event-broker", "ledger", "orders");
    let reader = table.attach(&id, "event-broker").expect("attaches");

    tokio::time::advance(GRACE * 10).await;
    assert_eq!(
        sweep_once(&table, GRACE, &discarding()).reaped_total(),
        0,
        "held for ten windows and still read, so still live"
    );

    drop(reader);

    // The window runs from the last pass that saw a live reader, not from the
    // `attach` ten windows ago - so it is not reaped immediately.
    assert_eq!(sweep_once(&table, GRACE, &discarding()).reaped_total(), 0);
    tokio::time::advance(GRACE).await;
    assert_eq!(sweep_once(&table, GRACE, &discarding()).reaped_total(), 1);
    assert!(table.is_empty());
}

#[tokio::test(start_paused = true)]
async fn attaching_inside_the_window_saves_an_unread_subscription() {
    // Section 6.6's reconnect affordance, so the sweep waits out a
    // grace window instead of removing an unread entry on sight.
    let table = ElectionSubscriptions::new();
    let id = table.open("event-broker", "ledger", "orders");

    tokio::time::advance(GRACE / 2).await;
    let _reader = table
        .attach(&id, "event-broker")
        .expect("the id is still valid");

    tokio::time::advance(GRACE).await;
    assert_eq!(sweep_once(&table, GRACE, &discarding()).reaped_total(), 0);
    assert_eq!(table.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_report_counts_per_profile() {
    // The gauge and the counter are both per `(profile, primitive)`, so the pass
    // has to separate them - one profile's abandoned subscriptions must not be
    // reported against another's.
    let table = ElectionSubscriptions::new();
    let _orders_leak = table.open("event-broker", "ledger", "orders");
    let _billing_leak = table.open("event-broker", "invoices", "billing");
    let held = table.open("api-gateway", "routes", "orders");
    let _reader = table.attach(&held, "api-gateway").expect("attaches");

    tokio::time::advance(GRACE * 2).await;
    let report = sweep_once(&table, GRACE, &discarding());

    assert_eq!(report.reaped.get("orders"), Some(&1));
    assert_eq!(report.reaped.get("billing"), Some(&1));
    assert_eq!(report.reaped_total(), 2);
    assert_eq!(report.live.get("orders"), Some(&1));
    assert_eq!(
        report.live.get("billing"),
        None,
        "billing has nothing left, so it is absent from `live` and the gauge \
         reports it as zero"
    );
}

// The signals

#[tokio::test(start_paused = true)]
async fn the_sweep_emits_the_reap_counter_and_the_population_gauge() {
    let (metrics, exporter, provider) = recording();
    let table = ElectionSubscriptions::new();
    let _leaked = table.open("event-broker", "ledger", "orders");
    let held = table.open("api-gateway", "ledger", "orders");
    let _reader = table.attach(&held, "api-gateway").expect("attaches");

    tokio::time::advance(GRACE * 2).await;
    assert_eq!(sweep_once(&table, GRACE, &metrics).reaped_total(), 1);
    provider.force_flush().expect("the reader flushes");

    let expected = vec![
        ("primitive".to_owned(), "leader".to_owned()),
        ("profile".to_owned(), "orders".to_owned()),
    ];
    assert_eq!(
        points(&exporter, SUBSCRIPTIONS_REAPED),
        vec![(expected.clone(), 1)],
        "one reap on `orders`, and the counter carries no `_total` on the \
         instrument - the exporter appends it (OBSERVABILITY.md section 5.1)"
    );
    assert_eq!(
        points(&exporter, SUBSCRIPTIONS_ACTIVE),
        vec![(expected, 1)],
        "one survivor on `orders`"
    );
}

#[tokio::test(start_paused = true)]
async fn neither_signal_carries_an_unbounded_label() {
    // Invariant I15, and item `S2`'s exit criterion in one assertion: the
    // election name and the caller are both unbounded-cardinality, and neither
    // may reach a metric. They are named distinctively here so a leak would show
    // up as a value, not just a key.
    let (metrics, exporter, provider) = recording();
    let table = ElectionSubscriptions::new();
    let _leaked = table.open(
        "a-very-distinctive-caller",
        "a-very-distinctive-election",
        "orders",
    );

    // One pass inside the window, so the gauge records a population, then one
    // past it, so the counter records a reap. Both instruments then have points
    // to inspect.
    let _live = sweep_once(&table, GRACE, &metrics);
    tokio::time::advance(GRACE * 2).await;
    let swept = sweep_once(&table, GRACE, &metrics);
    assert_eq!(swept.reaped_total(), 1);
    provider.force_flush().expect("the reader flushes");

    for name in [SUBSCRIPTIONS_REAPED, SUBSCRIPTIONS_ACTIVE] {
        let recorded = points(&exporter, name);
        // Without this the loop below passes by recording nothing at all, which
        // is how a label assertion goes green against a signal that stopped
        // being emitted.
        assert!(!recorded.is_empty(), "`{name}` recorded no data point");
        for (attributes, _value) in recorded {
            let keys: Vec<&str> = attributes.iter().map(|(key, _)| key.as_str()).collect();
            assert_eq!(
                keys,
                vec!["primitive", "profile"],
                "`{name}` may carry exactly the two bounded labels"
            );
            for (key, value) in &attributes {
                assert!(
                    !value.contains("distinctive"),
                    "`{name}` leaked an unbounded value into `{key}`: {value}"
                );
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_profile_that_empties_is_reported_as_zero_rather_than_going_quiet() {
    // A gauge that simply stops being recorded strands at its last non-zero
    // value, which reads as "still occupied" for as long as the series lives.
    let (metrics, exporter, provider) = recording();
    let table = ElectionSubscriptions::new();
    let _leaked = table.open("event-broker", "ledger", "orders");

    let first = sweep_once(&table, GRACE, &metrics);
    assert_eq!(first.live.get("orders"), Some(&1));

    tokio::time::advance(GRACE * 2).await;
    let second = sweep_once(&table, GRACE, &metrics);
    assert_eq!(second.reaped_total(), 1);
    assert!(second.live.is_empty());
    provider.force_flush().expect("the reader flushes");

    let values: Vec<u64> = points(&exporter, SUBSCRIPTIONS_ACTIVE)
        .into_iter()
        .map(|(_attributes, value)| value)
        .collect();
    assert_eq!(
        values,
        vec![0],
        "the last recorded population for `orders` must be zero"
    );
}

// The cadence

#[test]
fn the_grace_window_outlasts_more_than_one_pass() {
    // The multiplier's whole job: an entry must never be reaped on the pass that
    // first observes it unread, or the reconnect affordance and the
    // `join`-to-`await_change` window both close.
    const { assert!(SWEEP_GRACE_MULTIPLIER > 1) };
    assert_eq!(sweep_grace(SWEEP_INTERVAL), Duration::from_secs(15));
    assert!(sweep_grace(SWEEP_INTERVAL) > SWEEP_INTERVAL);
}

#[test]
fn the_cadence_is_the_plugins_lock_reaper_default() {
    // Section 5.4.1 carries the plugins' *value* rather than their knob, and this
    // pins the value. It cannot pin the coupling: the plugin's
    // `default_lock_reaper_interval` is private, and widening a plugin's public
    // API so a gear test can read a default back would be a worse trade than
    // this comment - `plugins/postgres-cluster-plugin/src/config.rs` is where the
    // 5_000 comes from.
    assert_eq!(SWEEP_INTERVAL, Duration::from_secs(5));
}
