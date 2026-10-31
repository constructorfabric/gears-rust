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

use tokio_util::sync::CancellationToken;

use crate::domain::activation::testing::{
    INTERVAL, KEY_A, KEY_B, PublishScript, T1, T2, body_changes, capture, change_ref, logged, pair,
    rig,
};
use crate::domain::activation::{ActivationStatus, NotificationStore, NotifiedChange, Tick};
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
    r.gateway.broker.store(false, Ordering::SeqCst);
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Idle);
    assert_eq!(r.notifier.tick(&no_cancel()).await, Tick::Idle);
    assert_eq!(*r.metrics.awaiting.lock().unwrap(), [2, 2]);
    assert_eq!(*r.metrics.subscriptions.lock().unwrap(), [2, 2]);
    assert!(r.gateway.events().is_empty());
    assert_eq!(
        r.scan.attempts.load(Ordering::SeqCst),
        0,
        "no pass, no lock"
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
    assert_eq!(r.scan.attempts.load(Ordering::SeqCst), 0);
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
        "no publish, no scan meanwhile"
    );
    assert_eq!(r.scan.attempts.load(Ordering::SeqCst), 0);
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
            let data = e.data.clone().unwrap();
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
        assert!(ids.insert(event.id), "an id of its own");
        let data = event.data.clone().unwrap();
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
    r.repo.subscribed.lock().unwrap().remove(&(
        "gear-x".to_owned(),
        KEY_B.to_owned(),
        T1.to_owned(),
    ));
    r.repo.subscribed.lock().unwrap().remove(&(
        "gear-y".to_owned(),
        KEY_A.to_owned(),
        T2.to_owned(),
    ));
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
        .stamp(
            &pair("gear-x", T1),
            &[NotifiedChange {
                key: KEY_A.to_owned(),
                change_ref: change_ref(9),
            }],
        )
        .await
        .unwrap();
    assert_eq!(
        stamped, 0,
        "a different change_ref names a different expectation"
    );
}
