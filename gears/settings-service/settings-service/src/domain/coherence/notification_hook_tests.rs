//! The end-of-request publish through the hook: the coherence publisher
//! carrying the activation notifier's notifications on the request's own
//! session.
//!
//! Level 1 rows of DESIGN-activation §7 (group B): *Publish builds
//! identifier-only payloads*, *Per-subscriber filtering*, *One notification per
//! subscriber and tenant*, *Publish timeout and skip*, *No broker*; and the
//! refused-publish rows of the lifecycle.
//!
//! Log-capture assertions assume one process per test (nextest, as the gear's
//! `make` targets run) and may flake under plain `cargo test`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use uuid::Uuid;

use crate::domain::activation::testing::{
    KEY_A, KEY_B, Rig, T1, T2, body_changes, capture, change_ref, logged, pair, rig,
};
use crate::domain::activation::{ActivationPairs, ActivationStatus};
use crate::domain::coherence::testing::{RecordingGateway, publisher};
use crate::domain::coherence::{BroadcastGateway, ChangeOutcome, CoherencePublisher};
use crate::domain::resolution::scope_class;

// ---------------------------------------------------------------------------
// The end-of-request publish, through the hook
// ---------------------------------------------------------------------------

/// A hook over a recording broadcast gateway with the notifier bound, and the
/// pairs a request touched.
fn hook_over(
    rig: &Rig,
    gateway: &Arc<RecordingGateway>,
) -> (
    Arc<CoherencePublisher>,
    Arc<crate::domain::coherence::testing::CountingMetrics>,
) {
    let (_, metrics) = publisher(gateway);
    let hook = Arc::new(
        CoherencePublisher::new(
            Arc::clone(gateway) as Arc<dyn BroadcastGateway>,
            Arc::clone(&metrics) as Arc<dyn crate::domain::coherence::BroadcastMetrics>,
        )
        .with_notifier(Arc::clone(&rig.notifier)),
    );
    (hook, metrics)
}

fn write(key: &str, tenant: Uuid) -> ChangeOutcome {
    ChangeOutcome::value_write(key, tenant, scope_class::LOCAL, true)
}

fn pairs_of(list: &[(&str, &str)]) -> ActivationPairs {
    let mut pairs = ActivationPairs::new();
    pairs.remember(list.iter().map(|(s, t)| pair(s, t)));
    pairs
}

#[tokio::test]
async fn a_notification_carries_identifiers_only_under_the_subscribers_tenant() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    let broadcast = Arc::new(RecordingGateway::ready(Uuid::from_u128(0xa0)));
    let (hook, _) = hook_over(&r, &broadcast);
    hook.publish_value_writes(
        [write(KEY_A, Uuid::from_u128(0xc1))],
        Uuid::new_v4(),
        &pairs_of(&[("gear-x", T1)]),
    )
    .await;

    let events = broadcast.events();
    // The broadcast first, the notification after it, on the same session.
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].event_type().as_ref() as &str,
        settings_service_sdk::gts::CACHE_INVALIDATE_EVENT_TYPE
    );
    let notification = &events[1];
    assert_eq!(
        notification.event_type().as_ref() as &str,
        settings_service_sdk::gts::CHANGE_NOTIFICATION_EVENT_TYPE
    );
    assert_eq!(notification.tenant_id(), Uuid::parse_str(T1).unwrap());
    assert_eq!(notification.subject(), T1);
    assert_eq!(
        notification.subject_type().as_ref() as &str,
        settings_service_sdk::gts::ACTIVATION_SUBJECT_TYPE
    );
    assert_eq!(notification.source(), Some("settings-service"));
    let body = notification.data().cloned().expect("a body");
    let mut names: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
    names.sort();
    assert_eq!(names, ["changes", "subscriber", "tenant"]);
    assert_eq!(body["tenant"], T1);
    assert_eq!(body["subscriber"], "gear-x");
    assert_eq!(
        body_changes(notification),
        [(KEY_A.to_owned(), change_ref(1).to_string())]
    );
    for change in body["changes"].as_array().unwrap() {
        let mut fields: Vec<_> = change.as_object().unwrap().keys().cloned().collect();
        fields.sort();
        assert_eq!(fields, ["change_ref", "key"], "no value, no hash, no actor");
    }
}

#[tokio::test]
async fn each_pair_gets_its_own_awaiting_keys_and_no_other_pairs() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.add("gear-x", T1, KEY_B, change_ref(2));
    r.repo.add("gear-y", T1, KEY_A, change_ref(3));
    r.repo.add("gear-x", T2, KEY_A, change_ref(4));
    // A terminal state of the pair is not carried.
    r.repo.add(
        "gear-x",
        T1,
        "gts.cf.core.settings.setting_type.v1~acme.net.gamma.v1~",
        change_ref(5),
    );
    r.repo.rows.lock().unwrap().last_mut().unwrap().status = ActivationStatus::Succeeded;
    let broadcast = Arc::new(RecordingGateway::ready(Uuid::from_u128(0xa0)));
    let (hook, _) = hook_over(&r, &broadcast);
    hook.publish_value_writes(
        [write(KEY_A, Uuid::from_u128(0xc1))],
        Uuid::new_v4(),
        &pairs_of(&[("gear-x", T1), ("gear-y", T1)]),
    )
    .await;

    let notifications: Vec<_> = broadcast.events().into_iter().skip(1).collect();
    // One notification per pair, in the order the request first touched them.
    assert_eq!(notifications.len(), 2);
    let first = notifications[0].data().cloned().unwrap();
    assert_eq!(first["subscriber"], "gear-x");
    assert_eq!(
        body_changes(&notifications[0]),
        [
            (KEY_A.to_owned(), change_ref(1).to_string()),
            (KEY_B.to_owned(), change_ref(2).to_string()),
        ],
        "every due key of the pair, in key order, not only the ones this request changed"
    );
    assert_eq!(
        notifications[1].data().cloned().unwrap()["subscriber"],
        "gear-y"
    );
    assert_eq!(
        body_changes(&notifications[1]),
        [(KEY_A.to_owned(), change_ref(3).to_string())]
    );
}

#[tokio::test]
async fn an_accepted_publish_stamps_the_bundled_rows_guarded_by_their_change_ref() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.add("gear-x", T2, KEY_A, change_ref(2));
    r.repo.advance(Duration::from_secs(5));
    let broadcast = Arc::new(RecordingGateway::ready(Uuid::from_u128(0xa0)));
    let (hook, _) = hook_over(&r, &broadcast);
    hook.publish_value_writes(
        [write(KEY_A, Uuid::from_u128(0xc1))],
        Uuid::new_v4(),
        &pairs_of(&[("gear-x", T1)]),
    )
    .await;
    let row = r.repo.row("gear-x", T1, KEY_A).unwrap();
    assert_eq!(row.last_notified, Some(Duration::from_secs(5)));
    assert_eq!(
        row.deliveries, 1,
        "the accepted publish counts one delivery"
    );
    let row = r.repo.row("gear-x", T2, KEY_A).unwrap();
    assert_eq!(
        (row.last_notified, row.deliveries),
        (None, 0),
        "another tenant's row of the subscriber is neither stamped nor counted"
    );
}

#[tokio::test]
async fn the_end_of_request_bundle_names_each_due_key_once_and_counts_only_the_due_rows() {
    // Three instances of one subscriber at one tenant: `a` and `b` due for
    // KEY_A, `c` published a moment ago so awaiting but not due, and `c`'s
    // KEY_B row not due either — its key is in no bundle.
    let r = rig();
    let instance = |n: u128| Uuid::from_u128(0x100 + n);
    for n in 1..=3 {
        r.repo
            .add_instance("gear-x", instance(n), T1, KEY_A, change_ref(1));
    }
    r.repo
        .add_instance("gear-x", instance(3), T1, KEY_B, change_ref(2));
    r.repo.advance(Duration::from_secs(10));
    for row in r.repo.rows.lock().unwrap().iter_mut() {
        if row.instance_id == instance(3) {
            row.last_notified = Some(Duration::from_secs(10));
            row.deliveries = 2;
        }
    }
    let broadcast = Arc::new(RecordingGateway::ready(Uuid::from_u128(0xa0)));
    let (hook, _) = hook_over(&r, &broadcast);
    hook.publish_value_writes(
        [write(KEY_A, Uuid::from_u128(0xc1))],
        Uuid::new_v4(),
        &pairs_of(&[("gear-x", T1)]),
    )
    .await;
    let notifications: Vec<_> = broadcast.events().into_iter().skip(1).collect();
    assert_eq!(notifications.len(), 1, "one per subscriber and tenant");
    assert_eq!(
        body_changes(&notifications[0]),
        [(KEY_A.to_owned(), change_ref(1).to_string())],
        "the key once; the not-due key not at all"
    );
    for row in r.repo.rows.lock().unwrap().iter() {
        let expected = if row.instance_id == instance(3) {
            (Some(Duration::from_secs(10)), 2)
        } else {
            (Some(Duration::from_secs(10)), 1)
        };
        assert_eq!(
            (row.last_notified, row.deliveries),
            expected,
            "the row that travelled only for its siblings is not counted: {row:?}"
        );
    }
}

#[tokio::test]
async fn a_failed_broadcast_skips_the_notifications_too() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    let mut refusing = RecordingGateway::ready(Uuid::from_u128(0xa0));
    refusing.refuse = true;
    let broadcast = Arc::new(refusing);
    let (hook, metrics) = hook_over(&r, &broadcast);
    hook.publish_value_writes(
        [write(KEY_A, Uuid::from_u128(0xc1))],
        Uuid::new_v4(),
        &pairs_of(&[("gear-x", T1)]),
    )
    .await;
    // Only the broadcast was attempted: the request pays one refusal.
    assert_eq!(broadcast.events().len(), 1);
    assert_eq!(metrics.all(), ["failed"]);
    assert_eq!(
        r.repo.row("gear-x", T1, KEY_A).unwrap().last_notified,
        None,
        "the row stays awaiting for the delivery loop"
    );
}

#[tokio::test]
async fn a_refused_notification_is_one_warn_skips_the_rest_and_leaves_the_rows_unstamped() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.add("gear-y", T1, KEY_A, change_ref(2));
    // The broadcast is accepted; every publish of the session is refused after
    // it: the first notification fails, the second pair is skipped.
    let mut gateway = RecordingGateway::ready(Uuid::from_u128(0xa0));
    gateway.refuse = true;
    gateway.state.types_registered = false;
    let broadcast = Arc::new(gateway);
    let (hook, _) = hook_over(&r, &broadcast);
    let (buf, _guard) = capture();
    hook.publish_value_writes(
        [write(KEY_A, Uuid::from_u128(0xc1))],
        Uuid::new_v4(),
        &pairs_of(&[("gear-x", T1), ("gear-y", T1)]),
    )
    .await;
    assert_eq!(broadcast.events().len(), 1, "one attempt, then skipped");
    let log = logged(&buf);
    assert_eq!(log.matches("WARN").count(), 1, "one warn: {log}");
    assert!(log.contains("gear-x") && log.contains(T1), "{log}");
    assert!(!log.contains("gear-y"), "the skipped pair is not attempted");
    assert_eq!(r.repo.row("gear-x", T1, KEY_A).unwrap().last_notified, None);
    assert_eq!(r.repo.row("gear-y", T1, KEY_A).unwrap().last_notified, None);
}

#[tokio::test]
async fn no_broker_types_unregistered_or_root_unknown_publish_no_notification() {
    for variant in ["no broker", "unregistered", "root unknown"] {
        let r = rig();
        r.repo.add("gear-x", T1, KEY_A, change_ref(1));
        let mut gateway = RecordingGateway::ready(Uuid::from_u128(0xa0));
        match variant {
            "no broker" => gateway.broker = false,
            "root unknown" => {
                gateway.root = None;
                gateway.state.root_tenant_known = false;
            }
            _ => {}
        }
        let broadcast = Arc::new(gateway);
        let (_, metrics) = publisher(&broadcast);
        if variant == "unregistered" {
            r.gateway.registered.store(false, Ordering::SeqCst);
        }
        let hook = Arc::new(
            CoherencePublisher::new(
                Arc::clone(&broadcast) as Arc<dyn BroadcastGateway>,
                metrics as Arc<dyn crate::domain::coherence::BroadcastMetrics>,
            )
            .with_notifier(Arc::clone(&r.notifier)),
        );
        hook.publish_value_writes(
            [write(KEY_A, Uuid::from_u128(0xc1))],
            Uuid::new_v4(),
            &pairs_of(&[("gear-x", T1)]),
        )
        .await;
        let notifications = broadcast
            .events()
            .into_iter()
            .filter(|e| {
                e.event_type().as_ref() as &str
                    == settings_service_sdk::gts::CHANGE_NOTIFICATION_EVENT_TYPE
            })
            .count();
        assert_eq!(notifications, 0, "{variant}");
        let row = r.repo.row("gear-x", T1, KEY_A).unwrap();
        assert_eq!(row.status, ActivationStatus::Awaiting, "{variant}");
        assert_eq!(row.last_notified, None, "{variant}");
    }
}

#[tokio::test]
async fn a_request_that_wrote_no_state_publishes_no_notification() {
    let r = rig();
    let broadcast = Arc::new(RecordingGateway::ready(Uuid::from_u128(0xa0)));
    let (hook, _) = hook_over(&r, &broadcast);
    hook.publish_value_writes(
        [write(KEY_A, Uuid::from_u128(0xc1))],
        Uuid::new_v4(),
        &ActivationPairs::new(),
    )
    .await;
    assert_eq!(broadcast.events().len(), 1, "the broadcast only");
}

#[tokio::test]
async fn a_store_failure_at_the_end_of_a_request_leaves_the_states_to_the_loop() {
    let r = rig();
    r.repo.add("gear-x", T1, KEY_A, change_ref(1));
    r.repo.failing.store(true, Ordering::SeqCst);
    let broadcast = Arc::new(RecordingGateway::ready(Uuid::from_u128(0xa0)));
    let (hook, _) = hook_over(&r, &broadcast);
    let (buf, _guard) = capture();
    hook.publish_value_writes(
        [write(KEY_A, Uuid::from_u128(0xc1))],
        Uuid::new_v4(),
        &pairs_of(&[("gear-x", T1)]),
    )
    .await;
    assert_eq!(broadcast.events().len(), 1);
    assert!(logged(&buf).contains("WARN"), "the failure is a warn");
}
