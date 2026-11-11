//! The notification half of the Activation Publisher: the delivery loop's
//! pass. (The end-of-request publish through the hook is tested beside the hook,
//! in the coherence half.)
//!
//! Level 1 rows of DESIGN-activation §7 (group B): *Delivery loop re-publish*,
//! *Split notification*, *Orphan row swept*, *Publish timeout and skip*; and
//! the lifecycle rows — registration refused and retried, delivery idle while
//! the root is unknown, a missing `produce` grant, the broker lost mid-pass,
//! `stop` during a pass.
//!
//! Log-capture assertions assume one process per test (nextest, as the gear's
//! `make` targets run) and may flake under plain `cargo test`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::domain::activation::testing::{
    INTERVAL, KEY_A, KEY_B, ONE_INSTANCE, PublishScript, T1, T2, body_changes, capture, change_ref,
    logged, one, pair, rig, rig_with_timeout,
};
use crate::domain::activation::{
    ActivationStatus, DueRow, InstanceKey, NotifiedChange, SILENT_FORGET, Tick,
};
use crate::infra::broadcast::{BROKER_EVENT_LIMIT_BYTES, serialised_len};

fn no_cancel() -> CancellationToken {
    CancellationToken::new()
}
// ---------------------------------------------------------------------------
// The delivery loop's tick
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_gauges_are_refreshed_on_every_tick_with_or_without_a_broker() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.add("gear-x", T2, KEY_A, change_ref(2));
    r.repo.add("gear-x", T2, KEY_B, change_ref(3));
    r.repo
        .set_status(&one("gear-x", KEY_B, T2), ActivationStatus::Silent);
    r.repo
        .subscribed
        .lock()
        .unwrap()
        .insert(one("gear-x", KEY_B, T2), Some(Duration::ZERO));
    r.gateway.broker.store(false, Ordering::SeqCst);
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Idle);
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Idle);
    assert_eq!(*r.metrics.awaiting.lock().unwrap(), [2, 2]);
    assert_eq!(*r.metrics.subscriptions.lock().unwrap(), [3, 3]);
    assert_eq!(*r.metrics.silent.lock().unwrap(), [1, 1]);
    assert_eq!(*r.metrics.silent_subscriptions.lock().unwrap(), [1, 1]);
    assert!(r.gateway.events().is_empty());
    assert_eq!(
        r.scan.attempts.load(Ordering::SeqCst),
        2,
        "a pass runs on every tick, broker or not, for the forgetting and the sweep"
    );
    assert_eq!(r.repo.sweeps.load(Ordering::SeqCst), 2);
    assert_eq!(
        r.repo.scans.load(Ordering::SeqCst),
        0,
        "no due scan without a broker"
    );
}

#[tokio::test]
async fn an_unknown_root_tenant_idles_the_tick_and_publishes_nothing() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    *r.gateway.root.lock().unwrap() = None;
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Idle);
    assert_eq!(*r.metrics.awaiting.lock().unwrap(), [1]);
    assert!(r.gateway.events().is_empty());
    assert_eq!(
        r.repo.sweeps.load(Ordering::SeqCst),
        1,
        "forgets and sweeps"
    );
    assert_eq!(r.repo.scans.load(Ordering::SeqCst), 0, "publishes nothing");
    assert_eq!(
        r.scan.held.load(Ordering::SeqCst),
        0,
        "the lock is released"
    );
    assert_eq!(r.gateway.registrations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_refused_registration_ends_the_tick_and_is_retried_on_the_next() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.gateway.registered.store(false, Ordering::SeqCst);
    r.gateway
        .registration_succeeds
        .store(false, Ordering::SeqCst);
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::NotRegistered);
    assert_eq!(r.gateway.registrations.load(Ordering::SeqCst), 1);
    assert!(
        r.gateway.events().is_empty(),
        "no publish, no due scan meanwhile"
    );
    assert_eq!(r.repo.scans.load(Ordering::SeqCst), 0);
    assert_eq!(
        r.repo.sweeps.load(Ordering::SeqCst),
        1,
        "the forgetting and the sweep still run"
    );
    // The registry answers on the next tick: the pass runs in the same tick.
    r.gateway
        .registration_succeeds
        .store(true, Ordering::SeqCst);
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 }
    );
    assert_eq!(r.gateway.registrations.load(Ordering::SeqCst), 2);
    assert_eq!(r.gateway.events().len(), 1);
}

#[tokio::test]
async fn the_loop_republishes_a_row_once_the_interval_has_passed_and_not_before() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    // A row with no stamp is due at the first tick.
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 }
    );
    assert_eq!(r.metrics.redeliveries.load(Ordering::SeqCst), 1);
    let stamped = r.repo.row("gear-x", T1, KEY_A).unwrap().last_notified;
    assert!(stamped.is_some(), "the accepted publish stamps the row");
    // Within the interval: not again.
    r.repo.advance(INTERVAL / 2);
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 0 }
    );
    assert_eq!(r.gateway.events().len(), 1);
    // Past it: the row is published again, and the stamp moves.
    r.repo.advance(INTERVAL);
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 }
    );
    assert_eq!(r.gateway.events().len(), 2);
    assert_eq!(r.metrics.redeliveries.load(Ordering::SeqCst), 2);
    assert_ne!(
        r.repo.row("gear-x", T1, KEY_A).unwrap().last_notified,
        stamped
    );
    assert_eq!(
        r.scan.held.load(Ordering::SeqCst),
        0,
        "the scan lock is released after every pass"
    );
}

#[tokio::test]
async fn a_terminal_row_is_never_republished() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.add("gear-x", T1, KEY_B, change_ref(2));
    r.repo.rows.lock().unwrap()[0].status = ActivationStatus::Failed;
    r.repo.advance(INTERVAL * 10);
    r.notifier.tick(&no_cancel()).await;
    let events = r.gateway.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        body_changes(&events[0]),
        [(KEY_B.to_owned(), change_ref(2).to_string())]
    );
}

#[tokio::test]
async fn one_notification_per_pair_carries_the_pairs_due_keys() {
    let r = rig();
    r.repo.add("gear-y", T1, KEY_A, change_ref(1));
    r.repo.add("gear-x", T2, KEY_A, change_ref(2));
    r.repo.add("gear-x", T1, KEY_B, change_ref(3));
    r.repo.add("gear-x", T1, KEY_A, change_ref(4));
    r.notifier.tick(&no_cancel()).await;
    let events = r.gateway.events();
    assert_eq!(events.len(), 3, "one per pair");
    let seen: Vec<_> = events
        .iter()
        .map(|e| {
            let data = e.data().cloned().unwrap();
            (
                data["subscriber"].as_str().unwrap().to_owned(),
                data["tenant"].as_str().unwrap().to_owned(),
                body_changes(e).len(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("gear-x".to_owned(), T1.to_owned(), 2),
            ("gear-x".to_owned(), T2.to_owned(), 1),
            ("gear-y".to_owned(), T1.to_owned(), 1),
        ]
    );
}

#[tokio::test]
async fn a_bundle_over_the_event_limit_is_split_with_every_entry_once() {
    let r = rig();
    let keys: Vec<String> = (0..220)
        .map(|i| {
            format!(
                "gts.cf.core.settings.setting_type.v1~acme.net.k{i:04}{}.v1~",
                "x".repeat(300)
            )
        })
        .collect();
    for (i, key) in keys.iter().enumerate() {
        r.repo.add("gear-x", T1, key, change_ref(i as u128 + 1));
    }
    r.notifier.tick(&no_cancel()).await;
    let events = r.gateway.events();
    assert!(events.len() > 1, "the bundle splits");
    let mut carried = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for event in &events {
        assert!(
            serialised_len(event) < BROKER_EVENT_LIMIT_BYTES,
            "each notification stays under the limit"
        );
        assert!(ids.insert(event.id()), "an id of its own");
        let data = event.data().cloned().unwrap();
        assert_eq!(data["tenant"], T1);
        assert_eq!(data["subscriber"], "gear-x");
        carried.extend(body_changes(event).into_iter().map(|(k, _)| k));
    }
    let mut expected = keys.clone();
    expected.sort();
    assert_eq!(carried, expected, "every entry once, in key order");
}

#[tokio::test]
async fn orphan_rows_are_swept_in_one_statement_whatever_their_status() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.add("gear-x", T1, KEY_B, change_ref(2));
    r.repo.add("gear-y", T2, KEY_A, change_ref(3));
    // Two subscriptions are gone: an awaiting row and a terminal one.
    r.repo.drop_subscriptions("gear-x", T1, KEY_B);
    r.repo.drop_subscriptions("gear-y", T2, KEY_A);
    r.repo.rows.lock().unwrap()[2].status = ActivationStatus::Succeeded;
    r.notifier.tick(&no_cancel()).await;
    assert_eq!(r.repo.sweeps.load(Ordering::SeqCst), 1, "one statement");
    assert!(
        r.repo.row("gear-x", T1, KEY_A).is_some(),
        "its subscription exists"
    );
    assert!(r.repo.row("gear-x", T1, KEY_B).is_none());
    assert!(r.repo.row("gear-y", T2, KEY_A).is_none());
    let events = r.gateway.events();
    assert_eq!(events.len(), 1, "orphans are swept, never published");
    assert_eq!(
        body_changes(&events[0]),
        [(KEY_A.to_owned(), change_ref(1).to_string())]
    );
}

#[tokio::test]
async fn a_replica_that_does_not_get_the_scan_lock_skips_the_pass_without_waiting() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.scan.held_elsewhere.store(true, Ordering::SeqCst);
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Skipped);
    assert_eq!(r.scan.attempts.load(Ordering::SeqCst), 1, "one attempt");
    assert_eq!(r.repo.sweeps.load(Ordering::SeqCst), 0);
    assert_eq!(r.repo.scans.load(Ordering::SeqCst), 0);
    assert!(r.gateway.events().is_empty());
    assert_eq!(
        *r.metrics.awaiting.lock().unwrap(),
        [1],
        "the gauges are refreshed on a skipped tick as well"
    );
}

#[tokio::test]
async fn a_lock_backend_failure_skips_the_tick_with_a_warn() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.scan.broken.store(true, Ordering::SeqCst);
    let (buf, _guard) = capture();
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Failed);
    assert!(logged(&buf).contains("WARN"));
    assert!(r.gateway.events().is_empty());
}

#[tokio::test]
async fn a_store_failure_abandons_the_pass_and_releases_the_lock() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.failing.store(true, Ordering::SeqCst);
    // The gauge refresh fails too: logged, the tick goes on to the pass.
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Failed);
    assert_eq!(r.scan.held.load(Ordering::SeqCst), 0);
    assert_eq!(r.scan.released.load(Ordering::SeqCst), 1);
    r.repo.failing.store(false, Ordering::SeqCst);
    assert!(
        r.repo.row("gear-x", T1, KEY_A).is_some(),
        "the state is kept"
    );
}

#[tokio::test]
async fn a_refused_publish_of_the_pass_leaves_the_rows_awaiting_and_due_again() {
    // A missing `produce` grant: each publish is refused and logged at warn,
    // the states stay awaiting and the loop keeps going.
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.add("gear-y", T1, KEY_A, change_ref(2));
    r.gateway
        .script
        .lock()
        .unwrap()
        .extend([PublishScript::Refuse, PublishScript::Accept]);
    let (buf, _guard) = capture();
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 }
    );
    let log = logged(&buf);
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(log.contains("gear-x") && log.contains(T1));
    assert_eq!(r.repo.row("gear-x", T1, KEY_A).unwrap().last_notified, None);
    assert!(
        r.repo
            .row("gear-y", T1, KEY_A)
            .unwrap()
            .last_notified
            .is_some()
    );
    assert_eq!(r.metrics.redeliveries.load(Ordering::SeqCst), 1);
    // Due again at the next tick, with nothing waiting for the interval.
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 }
    );
    assert!(
        r.repo
            .row("gear-x", T1, KEY_A)
            .unwrap()
            .last_notified
            .is_some()
    );
}

#[tokio::test]
async fn a_stalled_broker_ends_the_pass_after_one_timeout_and_releases_the_lock() {
    let r = rig();
    for (n, subscriber) in ["gear-a", "gear-b", "gear-c"].into_iter().enumerate() {
        r.repo.add(subscriber, T1, KEY_A, change_ref(n as u128 + 1));
    }
    r.gateway
        .script
        .lock()
        .unwrap()
        .extend([PublishScript::TimeOut, PublishScript::Accept]);
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Stalled);
    assert_eq!(
        r.gateway.script.lock().unwrap().len(),
        1,
        "one publish was attempted, the other pairs were not"
    );
    assert_eq!(
        r.scan.held.load(Ordering::SeqCst),
        0,
        "the lock is released"
    );
    for subscriber in ["gear-a", "gear-b", "gear-c"] {
        let row = r.repo.row(subscriber, T1, KEY_A).unwrap();
        assert_eq!(row.last_notified, None, "{subscriber} stays awaiting");
    }
    assert_eq!(r.metrics.redeliveries.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn three_refusals_in_a_row_end_the_pass_with_one_warn_for_the_pass() {
    let r = rig();
    for (n, subscriber) in ["gear-a", "gear-b", "gear-c", "gear-d"]
        .into_iter()
        .enumerate()
    {
        r.repo.add(subscriber, T1, KEY_A, change_ref(n as u128 + 1));
    }
    r.gateway.script.lock().unwrap().extend([
        PublishScript::Refuse,
        PublishScript::Refuse,
        PublishScript::Refuse,
        PublishScript::Accept,
    ]);
    let (buf, _guard) = capture();
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Stalled);
    assert_eq!(r.gateway.script.lock().unwrap().len(), 1, "three attempts");
    assert_eq!(r.scan.held.load(Ordering::SeqCst), 0);
    let log = logged(&buf);
    assert_eq!(log.matches("WARN").count(), 1, "one warn per pass: {log}");
}

#[tokio::test]
async fn an_accepted_publish_resets_the_run_of_refusals() {
    let r = rig();
    for (n, subscriber) in ["gear-a", "gear-b", "gear-c", "gear-d", "gear-e"]
        .into_iter()
        .enumerate()
    {
        r.repo.add(subscriber, T1, KEY_A, change_ref(n as u128 + 1));
    }
    r.gateway.script.lock().unwrap().extend([
        PublishScript::Refuse,
        PublishScript::Refuse,
        PublishScript::Accept,
        PublishScript::Refuse,
        PublishScript::Refuse,
    ]);
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 }
    );
}

#[tokio::test]
async fn a_broker_lost_during_the_pass_abandons_the_rest_and_keeps_every_state() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.add("gear-y", T1, KEY_A, change_ref(2));
    r.gateway.script.lock().unwrap().push(PublishScript::Refuse);
    r.gateway
        .broker_leaves_on_refusal
        .store(true, Ordering::SeqCst);
    let (buf, _guard) = capture();
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::BrokerLost);
    assert!(logged(&buf).contains("WARN"), "the loss is a warn");
    assert!(
        r.gateway.events().is_empty(),
        "the rest of the pass is abandoned"
    );
    assert_eq!(
        r.scan.held.load(Ordering::SeqCst),
        0,
        "the lock is released"
    );
    assert_eq!(r.repo.rows.lock().unwrap().len(), 2, "every state is kept");
    // The next tick on which a broker resolves delivers them.
    r.gateway.broker.store(true, Ordering::SeqCst);
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 2 }
    );
}

#[tokio::test]
async fn stop_during_a_pass_abandons_it_releases_the_lock_and_keeps_every_state() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.gateway.script.lock().unwrap().push(PublishScript::Hold);
    let cancel = no_cancel();
    let ticking = {
        let notifier = Arc::clone(&r.notifier);
        let cancel = cancel.clone();
        tokio::spawn(async move { notifier.tick(&cancel).await })
    };
    // The publish is in flight; the stop fires.
    r.gateway.gate.arrived.notified().await;
    cancel.cancel();
    assert_eq!(ticking.await.unwrap(), Tick::Cancelled);
    assert_eq!(r.scan.held.load(Ordering::SeqCst), 0);
    assert_eq!(r.scan.released.load(Ordering::SeqCst), 1);
    let row = r.repo.row("gear-x", T1, KEY_A).unwrap();
    assert_eq!(row.status, ActivationStatus::Awaiting);
    assert_eq!(
        row.last_notified, None,
        "the abandoned publish stamps nothing"
    );
}

#[tokio::test]
async fn a_row_overwritten_during_the_pass_keeps_its_stamp_absent() {
    // The pass read `change_ref` 1; a change overwrites the row to 2 while the
    // publish is in flight: the stamp is guarded and skips it.
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.gateway.script.lock().unwrap().push(PublishScript::Hold);
    let ticking = {
        let notifier = Arc::clone(&r.notifier);
        tokio::spawn(async move { notifier.tick(&CancellationToken::new()).await })
    };
    r.gateway.gate.arrived.notified().await;
    r.repo.overwrite("gear-x", T1, KEY_A, change_ref(2));
    r.gateway.gate.open.notify_one();
    assert_eq!(ticking.await.unwrap(), Tick::Complete { published: 1 });
    let row = r.repo.row("gear-x", T1, KEY_A).unwrap();
    assert_eq!(row.change_ref, change_ref(2));
    assert_eq!(
        row.last_notified, None,
        "the new expectation stays unstamped"
    );
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 },
        "and is published at the next tick"
    );
}

#[tokio::test]
async fn a_stamp_naming_another_change_ref_stamps_nothing() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    let stamped = r
        .repo
        .stamp_due(&[DueRow {
            pair: pair("gear-x", T1),
            instance_id: ONE_INSTANCE,
            change: NotifiedChange {
                key: KEY_A.to_owned(),
                change_ref: change_ref(9),
            },
            deliveries: 0,
        }])
        .unwrap();
    assert_eq!(
        stamped, 0,
        "a different change_ref names a different expectation"
    );
    let row = r.repo.row("gear-x", T1, KEY_A).unwrap();
    assert_eq!((row.last_notified, row.deliveries), (None, 0));
}

// ---------------------------------------------------------------------------
// Per instance: the counted deliveries, the call of silence, the forgetting
// ---------------------------------------------------------------------------

/// Instance `n` of `gear-x` for `key` at `tenant`.
fn instance(n: u128, key: &str, tenant: &str) -> InstanceKey {
    InstanceKey {
        subscriber: "gear-x".to_owned(),
        instance_id: uuid::Uuid::from_u128(0x100 + n),
        key: key.to_owned(),
        tenant: tenant.to_owned(),
    }
}

fn add(r: &crate::domain::activation::testing::Rig, of: &InstanceKey, n: u128) {
    r.repo.add_instance(
        &of.subscriber,
        of.instance_id,
        &of.tenant,
        &of.key,
        change_ref(n),
    );
}

fn set_deliveries(r: &crate::domain::activation::testing::Rig, of: &InstanceKey, deliveries: u32) {
    for row in r.repo.rows.lock().unwrap().iter_mut() {
        if row.instance_key() == *of {
            row.deliveries = deliveries;
        }
    }
}

/// One tick later: every `awaiting` row a tick stamped is due again, a stamp
/// exactly one interval old being due.
fn next_interval(r: &crate::domain::activation::testing::Rig) {
    r.repo.advance(INTERVAL);
}

#[tokio::test]
async fn deliveries_count_accepted_publications_only() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    // Refused: no delivery, and due again at once.
    r.gateway.script.lock().unwrap().push(PublishScript::Refuse);
    r.notifier.tick(&no_cancel()).await;
    let row = r.repo.row("gear-x", T1, KEY_A).unwrap();
    assert_eq!((row.deliveries, row.last_notified), (0, None));
    // Timed out: none either.
    r.gateway
        .script
        .lock()
        .unwrap()
        .push(PublishScript::TimeOut);
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Stalled);
    assert_eq!(r.repo.row("gear-x", T1, KEY_A).unwrap().deliveries, 0);
    // Accepted: one.
    r.notifier.tick(&no_cancel()).await;
    assert_eq!(r.repo.row("gear-x", T1, KEY_A).unwrap().deliveries, 1);
    // Not due within the interval: not published, not counted.
    r.repo.advance(INTERVAL / 2);
    r.notifier.tick(&no_cancel()).await;
    assert_eq!(r.repo.row("gear-x", T1, KEY_A).unwrap().deliveries, 1);
    // Due again: the second accepted publication.
    next_interval(&r);
    r.notifier.tick(&no_cancel()).await;
    assert_eq!(r.repo.row("gear-x", T1, KEY_A).unwrap().deliveries, 2);
    assert_eq!(r.gateway.events().len(), 2);
}

/// *The report timeout counts deliveries*, on the loop's own cadence: a state
/// its tick published is due at the next tick, so `K` deliveries take `K`
/// ticks — not twice as many — even when a tick comes a little early (the
/// timer's jitter, the pass's own time before its read), and the next tick
/// calls the unanswered state silent.
#[tokio::test]
async fn k_deliveries_accrue_in_k_ticks_whatever_the_jitter_of_a_tick() {
    // K = ceil(150 / 30) = 5.
    let r = rig_with_timeout(INTERVAL * 5);
    let k = r.notifier.silent_after();
    assert_eq!(k, 5);
    let a = one("gear-x", KEY_A, T1);
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    // The periods between ticks: exact, a little short, a little long.
    let jitter = [
        INTERVAL,
        INTERVAL.saturating_sub(Duration::from_millis(40)),
        INTERVAL + Duration::from_millis(25),
        INTERVAL.saturating_sub(Duration::from_millis(5)),
    ];
    for (tick, period) in (1..=k).zip(jitter.iter().cycle()) {
        assert_eq!(
            r.notifier.tick(&no_cancel()).await,
            Tick::Complete { published: 1 },
            "tick {tick} publishes"
        );
        assert_eq!(r.repo.row_of(&a).unwrap().deliveries, tick);
        r.repo.advance(*period);
    }
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 0 }
    );
    assert_eq!(
        r.repo.row_of(&a).unwrap().status,
        ActivationStatus::Silent,
        "called silent at tick K + 1"
    );
    assert_eq!(r.gateway.events().len(), 5);
}

/// The loop's slack is a tenth of the interval and no more: a state published
/// under nine tenths of an interval ago is not published again by a tick,
/// one published exactly nine tenths ago is.
#[tokio::test]
async fn the_loop_forgives_a_tenth_of_the_interval_and_no_more() {
    assert_eq!(super::tick_age(INTERVAL), INTERVAL * 9 / 10);
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.notifier.tick(&no_cancel()).await;
    r.repo
        .advance((INTERVAL * 9 / 10).saturating_sub(Duration::from_millis(1)));
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 0 }
    );
    r.repo.advance(Duration::from_millis(1));
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 }
    );
    assert_eq!(r.repo.row("gear-x", T1, KEY_A).unwrap().deliveries, 2);
}

#[tokio::test]
async fn broker_outage_does_not_count_and_cannot_silence() {
    // K = 2: an outage of many intervals counts nothing, so nothing is called
    // silent, and delivery resumes with the count where it was.
    let r = rig_with_timeout(INTERVAL * 2);
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.gateway.broker.store(false, Ordering::SeqCst);
    for _ in 0..10 {
        next_interval(&r);
        assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Idle);
    }
    let row = r.repo.row("gear-x", T1, KEY_A).unwrap();
    assert_eq!(row.status, ActivationStatus::Awaiting);
    assert_eq!(row.deliveries, 0);
    assert_eq!(r.metrics.silenced.load(Ordering::SeqCst), 0);
    r.gateway.broker.store(true, Ordering::SeqCst);
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 }
    );
    assert_eq!(r.repo.row("gear-x", T1, KEY_A).unwrap().deliveries, 1);
}

#[tokio::test]
async fn called_silent_after_exactly_k_accepted_deliveries() {
    // A timeout of 61 s at a 30 s interval: K = ceil(61 / 30) = 3.
    let r = rig_with_timeout(INTERVAL * 2 + Duration::from_secs(1));
    assert_eq!(r.notifier.silent_after(), 3);
    let a = one("gear-x", KEY_A, T1);
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    for delivered in 1..=3 {
        assert_eq!(
            r.notifier.tick(&no_cancel()).await,
            Tick::Complete { published: 1 }
        );
        let row = r.repo.row_of(&a).unwrap();
        assert_eq!(
            (row.status, row.deliveries),
            (ActivationStatus::Awaiting, delivered)
        );
        next_interval(&r);
    }
    // The fourth due tick calls it silent instead of publishing it.
    let at = r.repo.now();
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 0 }
    );
    let row = r.repo.row_of(&a).unwrap();
    assert_eq!(row.status, ActivationStatus::Silent);
    assert_eq!(row.silent_since, Some(at));
    assert_eq!(r.repo.silent_since_of(&a), Some(at));
    assert_eq!(r.gateway.events().len(), 3, "K publications, no more");
    assert_eq!(r.metrics.silenced.load(Ordering::SeqCst), 1);
    assert_eq!(r.metrics.incomplete.load(Ordering::SeqCst), 1);
    // A silent row is never due again until a re-entry re-arms it.
    for _ in 0..3 {
        next_interval(&r);
        assert_eq!(
            r.notifier.tick(&no_cancel()).await,
            Tick::Complete { published: 0 }
        );
    }
    assert_eq!(r.gateway.events().len(), 3);
    assert_eq!(r.metrics.silenced.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_silent_subscription_keeps_its_first_silent_since() {
    let r = rig();
    let a = one("gear-x", KEY_A, T1);
    r.repo.add("gear-x", T1, KEY_A, change_ref(2));
    // Silent for the key since an earlier change, never re-entered.
    let first = Duration::from_secs(5);
    r.repo
        .subscribed
        .lock()
        .unwrap()
        .insert(a.clone(), Some(first));
    set_deliveries(&r, &a, r.notifier.silent_after());
    r.repo.advance(Duration::from_secs(100));
    r.notifier.tick(&no_cancel()).await;
    let row = r.repo.row_of(&a).unwrap();
    assert_eq!(row.status, ActivationStatus::Silent);
    assert_eq!(row.silent_since, Some(Duration::from_secs(100)));
    assert_eq!(
        r.repo.silent_since_of(&a),
        Some(first),
        "the day before forgetting counts from the first call"
    );
}

#[tokio::test]
async fn a_call_of_silence_is_recorded_with_its_instance_under_its_change() {
    let r = rig();
    let a = instance(1, KEY_A, T1);
    add(&r, &a, 7);
    r.repo
        .labels
        .lock()
        .unwrap()
        .insert(a.clone(), "host-a".to_owned());
    set_deliveries(&r, &a, r.notifier.silent_after());
    let (buf, _guard) = capture();
    r.notifier.tick(&no_cancel()).await;
    let records = r.repo.silent_records.lock().unwrap().clone();
    assert_eq!(records.len(), 1, "one record for the call");
    assert_eq!(records[0].instance, a);
    assert_eq!(records[0].instance_label.as_deref(), Some("host-a"));
    assert_eq!(records[0].change_ref, change_ref(7));
    let log = logged(&buf);
    assert!(
        log.contains("called silent") && log.contains(&a.instance_id.to_string()),
        "{log}"
    );
}

#[tokio::test]
async fn a_call_of_silence_that_loses_to_a_report_writes_nothing() {
    // The state was settled between the scan and the call: the guard matches
    // nothing — modelled by a settled row the scan still reads as due.
    let r = rig();
    let a = one("gear-x", KEY_A, T1);
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    let due = DueRow {
        pair: pair("gear-x", T1),
        instance_id: a.instance_id,
        change: NotifiedChange {
            key: KEY_A.to_owned(),
            change_ref: change_ref(1),
        },
        deliveries: 99,
    };
    r.repo.set_status(&a, ActivationStatus::Succeeded);
    let called = crate::domain::activation::NotificationStore::silence(r.repo.as_ref(), &due)
        .await
        .unwrap();
    assert_eq!(called, None);
    assert_eq!(
        r.repo.silent_since_of(&a),
        None,
        "the subscription untouched"
    );
    assert!(
        r.repo.silent_records.lock().unwrap().is_empty(),
        "no record"
    );
}

#[tokio::test]
async fn incomplete_is_counted_once_per_transition_of_the_account() {
    let r = rig();
    let limit = r.notifier.silent_after();
    let [a, b, c] = [1, 2, 3].map(|n| instance(n, KEY_A, T1));
    for of in [&a, &b, &c] {
        add(&r, of, 1);
    }
    // `a` and `b` are called silent on one tick: the first turns the account
    // `incomplete`, the second finds it so already.
    set_deliveries(&r, &a, limit);
    set_deliveries(&r, &b, limit);
    r.notifier.tick(&no_cancel()).await;
    assert_eq!(r.metrics.silenced.load(Ordering::SeqCst), 2);
    assert_eq!(r.metrics.incomplete.load(Ordering::SeqCst), 1);
    // `a` and `b` re-enter (re-armed to `awaiting`); `c` is called silent:
    // a second transition, counted again.
    for of in [&a, &b] {
        r.repo.set_status(of, ActivationStatus::Awaiting);
        set_deliveries(&r, of, 0);
    }
    set_deliveries(&r, &c, limit);
    next_interval(&r);
    r.notifier.tick(&no_cancel()).await;
    assert_eq!(r.metrics.silenced.load(Ordering::SeqCst), 3);
    assert_eq!(r.metrics.incomplete.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_call_of_silence_on_a_failed_account_counts_no_transition() {
    let r = rig();
    let [a, b] = [1, 2].map(|n| instance(n, KEY_A, T1));
    add(&r, &a, 1);
    add(&r, &b, 1);
    r.repo.set_status(&a, ActivationStatus::Failed);
    set_deliveries(&r, &b, r.notifier.silent_after());
    r.notifier.tick(&no_cancel()).await;
    assert_eq!(r.repo.row_of(&b).unwrap().status, ActivationStatus::Silent);
    assert_eq!(r.metrics.silenced.load(Ordering::SeqCst), 1);
    assert_eq!(
        r.metrics.incomplete.load(Ordering::SeqCst),
        0,
        "failure wins: the account was failed before the call"
    );
}

#[tokio::test]
async fn the_sweep_runs_without_a_broker_and_neither_silences_nor_publishes() {
    let r = rig();
    let [gone, forgotten, waiting] = [1, 2, 3].map(|n| instance(n, KEY_A, T1));
    for of in [&gone, &forgotten, &waiting] {
        add(&r, of, 1);
    }
    r.repo.subscribed.lock().unwrap().remove(&gone);
    r.repo
        .subscribed
        .lock()
        .unwrap()
        .insert(forgotten.clone(), Some(Duration::ZERO));
    set_deliveries(&r, &waiting, r.notifier.silent_after() + 5);
    r.repo.advance(SILENT_FORGET + Duration::from_secs(1));
    r.gateway.broker.store(false, Ordering::SeqCst);
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Idle);
    assert!(r.repo.row_of(&gone).is_none(), "the orphan is swept");
    assert!(
        r.repo.row_of(&forgotten).is_none() && !r.repo.is_subscribed(&forgotten),
        "the silent subscription is forgotten with its state"
    );
    assert_eq!(r.metrics.forgotten.load(Ordering::SeqCst), 1);
    let row = r.repo.row_of(&waiting).unwrap();
    assert_eq!(row.status, ActivationStatus::Awaiting, "no call of silence");
    assert_eq!(r.metrics.silenced.load(Ordering::SeqCst), 0);
    assert!(r.gateway.events().is_empty());
}

#[tokio::test]
async fn forgetting_is_conditional_on_silent_since_and_writes_no_record() {
    let r = rig();
    let [old, young, reentered] = [1, 2, 3].map(|n| instance(n, KEY_A, T1));
    for of in [&old, &young, &reentered] {
        add(&r, of, 1);
        r.repo.set_status(of, ActivationStatus::Silent);
    }
    {
        let mut subscribed = r.repo.subscribed.lock().unwrap();
        subscribed.insert(old.clone(), Some(Duration::from_secs(10)));
        subscribed.insert(young.clone(), Some(Duration::from_secs(5_000)));
        subscribed.insert(reentered.clone(), Some(Duration::from_secs(10)));
    }
    // A report of `reentered` lands between the read and the forgetting.
    r.repo
        .reenter_after_read
        .lock()
        .unwrap()
        .push(reentered.clone());
    r.repo.advance(SILENT_FORGET + Duration::from_secs(100));
    r.notifier.tick(&no_cancel()).await;
    assert!(!r.repo.is_subscribed(&old) && r.repo.row_of(&old).is_none());
    assert!(
        r.repo.is_subscribed(&young) && r.repo.row_of(&young).is_some(),
        "silent for less than a day: kept"
    );
    assert!(
        r.repo.is_subscribed(&reentered) && r.repo.row_of(&reentered).is_some(),
        "its silence ended meanwhile: kept"
    );
    assert_eq!(r.metrics.forgotten.load(Ordering::SeqCst), 1);
    assert!(
        r.repo.silent_records.lock().unwrap().is_empty(),
        "the forgetting writes no audit record"
    );
}

#[tokio::test]
async fn the_republish_carries_a_key_while_any_instance_is_due_and_never_a_silent_row() {
    let r = rig();
    let [fresh, due, silent] = [1, 2, 3].map(|n| instance(n, KEY_A, T1));
    let only_silent = instance(4, KEY_B, T1);
    for of in [&fresh, &due, &silent, &only_silent] {
        let n = if of.key == KEY_A { 1 } else { 2 };
        add(&r, of, n);
    }
    r.repo.set_status(&silent, ActivationStatus::Silent);
    r.repo.set_status(&only_silent, ActivationStatus::Silent);
    // `fresh` was published just now, so it is awaiting but not due.
    r.repo.advance(INTERVAL * 3);
    for row in r.repo.rows.lock().unwrap().iter_mut() {
        if row.instance_key() == fresh {
            row.last_notified = Some(INTERVAL * 3);
            row.deliveries = 4;
        }
    }
    r.notifier.tick(&no_cancel()).await;
    let events = r.gateway.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        body_changes(&events[0]),
        [(KEY_A.to_owned(), change_ref(1).to_string())],
        "the key once, for the due instance; the silent-only key not at all"
    );
    let row = r.repo.row_of(&due).unwrap();
    assert_eq!((row.deliveries, row.last_notified), (1, Some(INTERVAL * 3)));
    let row = r.repo.row_of(&fresh).unwrap();
    assert_eq!(
        row.deliveries, 4,
        "a row that travelled only for its sibling is not counted"
    );
    for of in [&silent, &only_silent] {
        let row = r.repo.row_of(of).unwrap();
        assert_eq!((row.deliveries, row.last_notified), (0, None), "{of:?}");
    }
}

#[tokio::test]
async fn one_notification_per_pair_names_each_due_key_once_however_many_instances() {
    let r = rig();
    for n in 1..=3 {
        add(&r, &instance(n, KEY_A, T1), 1);
        add(&r, &instance(n, KEY_B, T1), 2);
    }
    r.notifier.tick(&no_cancel()).await;
    let events = r.gateway.events();
    assert_eq!(
        events.len(),
        1,
        "one per subscriber and tenant, not per instance"
    );
    assert_eq!(
        body_changes(&events[0]),
        [
            (KEY_A.to_owned(), change_ref(1).to_string()),
            (KEY_B.to_owned(), change_ref(2).to_string()),
        ]
    );
    assert!(
        r.repo
            .rows
            .lock()
            .unwrap()
            .iter()
            .all(|row| row.deliveries == 1),
        "every due row it bundled counts one delivery"
    );
}

#[tokio::test]
async fn a_split_notification_carries_each_due_key_once_and_counts_each_due_row_once() {
    let r = rig();
    let keys: Vec<String> = (0..220)
        .map(|i| {
            format!(
                "gts.cf.core.settings.setting_type.v1~acme.net.k{i:04}{}.v1~",
                "x".repeat(300)
            )
        })
        .collect();
    for (i, key) in keys.iter().enumerate() {
        for n in 1..=2 {
            add(&r, &instance(n, key, T1), i as u128 + 1);
        }
    }
    // One key's rows are not due: it is in no part.
    let quiet = keys[0].clone();
    for row in r.repo.rows.lock().unwrap().iter_mut() {
        if row.key == quiet {
            row.last_notified = Some(Duration::ZERO);
        }
    }
    r.notifier.tick(&no_cancel()).await;
    let events = r.gateway.events();
    assert!(events.len() > 1, "the bundle splits");
    let carried: Vec<String> = events
        .iter()
        .flat_map(|e| body_changes(e).into_iter().map(|(k, _)| k))
        .collect();
    let mut expected: Vec<String> = keys[1..].to_vec();
    expected.sort();
    assert_eq!(carried, expected, "each due key once, in key order");
    for row in r.repo.rows.lock().unwrap().iter() {
        let counted = u32::from(row.key != quiet);
        assert_eq!(row.deliveries, counted, "{}", row.key);
    }
}

#[tokio::test]
async fn a_silent_orphan_row_is_swept_too() {
    let r = rig();
    let a = instance(1, KEY_A, T1);
    add(&r, &a, 1);
    r.repo.set_status(&a, ActivationStatus::Silent);
    r.repo.subscribed.lock().unwrap().remove(&a);
    r.notifier.tick(&no_cancel()).await;
    assert!(r.repo.row_of(&a).is_none());
}

// ---------------------------------------------------------------------------
// Fairness: a pass that ended early does not starve the pairs after its end
// ---------------------------------------------------------------------------

/// How the broker answers a poisoned pair's publish.
#[derive(Clone, Copy)]
enum Poison {
    Refuse,
    TimeOut,
}

/// A gateway over [`crate::domain::activation::testing::FakeNotifications`]
/// whose broker answers the publishes of some subscribers' pairs with
/// [`Poison`], whatever their place in the pass, and records the subscriber of
/// every publish it is handed, accepted or not.
struct PoisonedGateway {
    inner: Arc<crate::domain::activation::testing::FakeNotifications>,
    poisoned: Arc<std::sync::Mutex<std::collections::HashMap<String, Poison>>>,
    attempts: Arc<std::sync::Mutex<Vec<String>>>,
}

struct PoisonedSession {
    inner: Box<dyn crate::domain::broker_sink::BroadcastSink>,
    poisoned: Arc<std::sync::Mutex<std::collections::HashMap<String, Poison>>>,
    attempts: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl crate::domain::broker_sink::BroadcastSink for PoisonedSession {
    async fn publish(
        &self,
        event: event_broker_sdk::EventV1,
    ) -> Result<(), crate::domain::broker_sink::PublishError> {
        let subscriber = event.data().unwrap()["subscriber"]
            .as_str()
            .unwrap()
            .to_owned();
        self.attempts.lock().unwrap().push(subscriber.clone());
        let poison = self.poisoned.lock().unwrap().get(&subscriber).copied();
        match poison {
            Some(Poison::Refuse) => Err(crate::domain::broker_sink::PublishError::Refused(
                "poisoned".to_owned(),
            )),
            Some(Poison::TimeOut) => Err(crate::domain::broker_sink::PublishError::TimedOut),
            None => self.inner.publish(event).await,
        }
    }
}

#[async_trait::async_trait]
impl crate::domain::activation::NotificationGateway for PoisonedGateway {
    fn broker_present(&self) -> bool {
        self.inner.broker_present()
    }

    fn root_tenant(&self) -> Option<uuid::Uuid> {
        self.inner.root_tenant()
    }

    fn registered(&self) -> bool {
        self.inner.registered()
    }

    async fn register(&self, cancel: &CancellationToken) -> bool {
        self.inner.register(cancel).await
    }

    fn session(&self) -> Option<Box<dyn crate::domain::broker_sink::BroadcastSink>> {
        self.inner.session().map(|inner| {
            Box::new(PoisonedSession {
                inner,
                poisoned: Arc::clone(&self.poisoned),
                attempts: Arc::clone(&self.attempts),
            }) as Box<dyn crate::domain::broker_sink::BroadcastSink>
        })
    }

    fn notifications(
        &self,
        pair: &crate::domain::activation::SubscriberTenant,
        changes: &[NotifiedChange],
        trace_parent: Option<String>,
    ) -> Vec<crate::domain::activation::BuiltNotification> {
        self.inner.notifications(pair, changes, trace_parent)
    }
}

/// A rig whose notifier publishes through a [`PoisonedGateway`], with one
/// state of `KEY_A` at `T1` for each of `subscribers`.
struct Fairness {
    rig: crate::domain::activation::testing::Rig,
    notifier: crate::domain::activation::ActivationNotifier,
    poisoned: Arc<std::sync::Mutex<std::collections::HashMap<String, Poison>>>,
    attempts: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Fairness {
    fn new(subscribers: &[&str]) -> Self {
        let rig = rig();
        for (n, subscriber) in subscribers.iter().enumerate() {
            rig.repo
                .add(subscriber, T1, KEY_A, change_ref(n as u128 + 1));
        }
        let poisoned = Arc::new(std::sync::Mutex::default());
        let attempts = Arc::new(std::sync::Mutex::default());
        let gateway = Arc::new(PoisonedGateway {
            inner: Arc::clone(&rig.gateway),
            poisoned: Arc::clone(&poisoned),
            attempts: Arc::clone(&attempts),
        });
        let notifier = crate::domain::activation::ActivationNotifier::new(
            Arc::clone(&rig.repo) as Arc<dyn crate::domain::activation::NotificationStore>,
            gateway as Arc<dyn crate::domain::activation::NotificationGateway>,
            Arc::clone(&rig.scan) as Arc<dyn crate::domain::activation::ScanLock>,
            Arc::clone(&rig.metrics) as Arc<dyn crate::domain::activation::NotifyMetrics>,
            INTERVAL,
        );
        Self {
            rig,
            notifier,
            poisoned,
            attempts,
        }
    }

    fn poison(&self, subscribers: &[&str], how: Poison) {
        let mut poisoned = self.poisoned.lock().unwrap();
        poisoned.clear();
        for subscriber in subscribers {
            poisoned.insert((*subscriber).to_owned(), how);
        }
    }

    /// One tick, one interval after the previous: the subscribers it
    /// attempted, in order, and how it ended.
    async fn tick(&self) -> (Tick, Vec<String>) {
        self.attempts.lock().unwrap().clear();
        let ended = self.notifier.tick(&no_cancel()).await;
        self.rig.repo.advance(INTERVAL);
        (ended, self.attempts.lock().unwrap().clone())
    }

    fn deliveries(&self, subscriber: &str) -> u32 {
        self.rig.repo.row(subscriber, T1, KEY_A).unwrap().deliveries
    }
}

#[tokio::test]
async fn three_refused_pairs_at_the_head_do_not_starve_the_pairs_after_them() {
    let f = Fairness::new(&["gear-a", "gear-b", "gear-c", "gear-d", "gear-e", "gear-f"]);
    f.poison(&["gear-a", "gear-b", "gear-c"], Poison::Refuse);
    let (ended, attempted) = f.tick().await;
    assert_eq!(ended, Tick::Stalled, "the third refusal in a row ends it");
    assert_eq!(attempted, ["gear-a", "gear-b", "gear-c"]);
    for _ in 0..3 {
        let (ended, attempted) = f.tick().await;
        assert_eq!(ended, Tick::Stalled);
        assert_eq!(
            attempted,
            ["gear-d", "gear-e", "gear-f", "gear-a", "gear-b", "gear-c"],
            "the pass starts after the pair the previous one ended at"
        );
    }
    for healthy in ["gear-d", "gear-e", "gear-f"] {
        assert_eq!(
            f.deliveries(healthy),
            3,
            "{healthy}: one per tick after the first"
        );
    }
    for poisoned in ["gear-a", "gear-b", "gear-c"] {
        assert_eq!(
            f.deliveries(poisoned),
            0,
            "{poisoned}: a refusal counts nothing"
        );
        assert_eq!(
            f.rig.repo.row(poisoned, T1, KEY_A).unwrap().last_notified,
            None
        );
    }
}

#[tokio::test]
async fn a_pair_that_times_out_does_not_starve_the_pairs_after_it() {
    let f = Fairness::new(&["gear-a", "gear-b", "gear-c", "gear-d"]);
    f.poison(&["gear-a"], Poison::TimeOut);
    assert_eq!(f.tick().await, (Tick::Stalled, vec!["gear-a".to_owned()]));
    for _ in 0..3 {
        let (ended, attempted) = f.tick().await;
        assert_eq!(ended, Tick::Stalled, "one timeout still ends the pass");
        assert_eq!(attempted, ["gear-b", "gear-c", "gear-d", "gear-a"]);
    }
    for healthy in ["gear-b", "gear-c", "gear-d"] {
        assert_eq!(f.deliveries(healthy), 3, "{healthy}");
    }
    assert_eq!(f.deliveries("gear-a"), 0);
}

#[tokio::test]
async fn the_next_pass_wraps_around_to_the_pairs_before_its_start() {
    let f = Fairness::new(&["gear-a", "gear-b", "gear-c", "gear-d", "gear-e", "gear-f"]);
    f.poison(&["gear-b", "gear-c", "gear-d"], Poison::Refuse);
    let (ended, attempted) = f.tick().await;
    assert_eq!(ended, Tick::Stalled);
    assert_eq!(attempted, ["gear-a", "gear-b", "gear-c", "gear-d"]);
    let (ended, attempted) = f.tick().await;
    assert_eq!(ended, Tick::Stalled);
    assert_eq!(
        attempted,
        ["gear-e", "gear-f", "gear-a", "gear-b", "gear-c", "gear-d"],
        "after the end of the order the pass goes on from its beginning"
    );
    assert_eq!(
        (
            f.deliveries("gear-a"),
            f.deliveries("gear-e"),
            f.deliveries("gear-f")
        ),
        (2, 1, 1)
    );
}

#[tokio::test]
async fn a_pass_that_runs_to_its_end_returns_the_next_to_the_pair_order() {
    let f = Fairness::new(&["gear-a", "gear-b", "gear-c", "gear-d", "gear-e"]);
    f.poison(&["gear-a", "gear-b", "gear-c"], Poison::Refuse);
    assert_eq!(f.tick().await.0, Tick::Stalled);
    // The broker recovers: the pass starts after the stall and publishes every
    // pair once.
    f.poison(&[], Poison::Refuse);
    let (ended, attempted) = f.tick().await;
    assert_eq!(ended, Tick::Complete { published: 5 });
    assert_eq!(
        attempted,
        ["gear-d", "gear-e", "gear-a", "gear-b", "gear-c"]
    );
    // A healthy pass in pair order, every pair once per interval.
    for _ in 0..2 {
        let (ended, attempted) = f.tick().await;
        assert_eq!(ended, Tick::Complete { published: 5 });
        assert_eq!(
            attempted,
            ["gear-a", "gear-b", "gear-c", "gear-d", "gear-e"]
        );
    }
    assert_eq!(
        ["gear-a", "gear-b", "gear-c", "gear-d", "gear-e"].map(|s| f.deliveries(s)),
        [3, 3, 3, 3, 3],
        "one counted delivery per pair and interval"
    );
}

// ---------------------------------------------------------------------------
// Spans of the tick
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_redelivered_notification_carries_the_pairs_republish_span_in_the_ticks_trace() {
    use crate::domain::activation::testing::{SpanCapture, gear_attributes};

    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    let spans = SpanCapture::start();
    assert_eq!(
        r.notifier.tick(&no_cancel()).await,
        Tick::Complete { published: 1 }
    );

    let tick = spans.one("activation.deliver");
    let republish = spans.one("change_notification.republish");
    assert_eq!(
        tick.parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "a tick is a root of its own"
    );
    assert_eq!(republish.parent_span_id, tick.span_context.span_id());
    assert_eq!(
        republish.span_context.trace_id(),
        tick.span_context.trace_id()
    );
    let events = r.gateway.events();
    assert_eq!(events.len(), 1);
    let carried = events[0]
        .trace_parent()
        .expect("a redelivered notification carries a trace_parent");
    assert_eq!(
        carried,
        format!(
            "00-{}-{}-01",
            republish.span_context.trace_id(),
            republish.span_context.span_id()
        ),
        "the publishing span is the pair's republish span"
    );
    assert_eq!(
        gear_attributes(&tick),
        [("end".to_owned(), "complete".to_owned())]
    );
    let mut attributes = gear_attributes(&republish);
    attributes.sort();
    assert_eq!(
        attributes,
        [
            ("subscriber".to_owned(), "gear-x".to_owned()),
            ("tenant".to_owned(), T1.to_owned()),
        ],
        "identifiers only: no key, change_ref, value or hash"
    );
}

#[tokio::test]
async fn a_tick_without_a_broker_ends_its_span_idle_and_republishes_nothing() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.gateway.broker.store(false, Ordering::SeqCst);
    let spans = crate::domain::activation::testing::SpanCapture::start();
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Idle);
    let tick = spans.one("activation.deliver");
    assert_eq!(
        crate::domain::activation::testing::gear_attributes(&tick),
        [("end".to_owned(), "idle".to_owned())]
    );
    assert!(spans.named("change_notification.republish").is_empty());
}
