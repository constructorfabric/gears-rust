#![cfg(feature = "integration")]
// The doubles, the fixture and the polls are plain helpers, which
// `allow-expect-in-tests` does not reach; a failed expectation there is the
// test failing, as in the sibling PostgreSQL suites.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Consumer activation, the delivery half, on a real `PostgreSQL`: the
//! `change_notification` at the end of a request, the delivery loop that
//! re-publishes what stays `awaiting`, and the report that settles it — the
//! Level 2 rows of DESIGN-activation §7 that belong to them.
//!
//! Each replica is a whole `SettingsService`: `init` over its own `ClientHub`
//! and connection pool — so its own advisory-lock session, the scan lock's — and
//! the managed lifecycle, so the delivery loop is the production one. What the
//! test hosts is the platform around them: the delivering Event Broker double of
//! `tests/support/platform.rs` (it records, refuses or parks a
//! `change_notification` on command), the Types Registry, the Tenant Resolver
//! and a policy decision point that allows everything. The consumer is the
//! test: it registers its subscription through the gear's subscription service
//! and reports through the Recorder, as the SDK's `report_outcome` does in
//! process.
//!
//! The assertions are on outcomes, never on how long anything took. The loop's
//! interval is a whole number of seconds, so a row that is due again at the next
//! tick is waited for, with a deadline that only a hang can exhaust, and
//! `pg_stat_database.deadlocks` is asserted unchanged.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_activation_delivery_test
//! ```

mod support;

use std::time::Duration;

use cf_gears_settings_service::domain::activation::{BackResponse, ReportResult};
use cf_gears_settings_service::domain::error::DomainError;
use cf_gears_settings_service::infra::storage::audit_store::AuditStore;
use event_broker_sdk::Event;
use serde_json::json;
use settings_service_sdk::SettingKey;
use settings_service_sdk::gts::{
    ACTIVATION_SUBJECT_TYPE, ACTIVATION_TOPIC_ID, CACHE_INVALIDATE_EVENT_TYPE,
    CHANGE_NOTIFICATION_EVENT_TYPE,
};
use settings_service_sdk::models::ScopeClass;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use support::platform::{CHILD, PgFixture, Platform, ROOT, Replica, pg_fixture, platform};

/// How long the test waits for something that must happen: far beyond any real
/// wait, so only a hang can exhaust it.
const MUST_HAPPEN: Duration = Duration::from_secs(40);
/// How long the test watches for something that must *not* have happened yet.
const STILL_WAITING: Duration = Duration::from_millis(600);

fn setting(name: &str) -> SettingKey {
    SettingKey::contributed("cf", "demo", "delivery", name, std::num::NonZeroU32::MIN)
        .expect("a valid key")
}

/// A subscriber: a gear's own security context.
fn consumer() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(ROOT)
        .subject_type("gts.cf.core.security.subject_service.v1~")
        .build()
        .expect("a context")
}

fn config(interval_seconds: u64) -> serde_json::Value {
    json!({ "redeliver_interval_seconds": interval_seconds })
}

/// Poll until `done`, with the must-happen deadline.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(MUST_HAPPEN, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// Poll an async condition until it holds.
async fn until_async<F, Fut>(what: &str, mut done: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(MUST_HAPPEN, async {
        while !done().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// The delivery loop's pass has ended: no advisory lock is held by any
/// replica's session, the scan lock's included.
async fn scan_idle(pg: &PgFixture) {
    until_async("the pass to release its lock", || async {
        pg.advisory_locks().await == 0
    })
    .await;
}

/// The loop registered the activation types with the registry.
async fn registered(platform: &Platform) {
    until("the loop to register the activation types", || {
        platform.registry.has_registered(ACTIVATION_TOPIC_ID)
            && platform
                .registry
                .has_registered(CHANGE_NOTIFICATION_EVENT_TYPE)
    })
    .await;
}

fn is_notification(event: &Event) -> bool {
    event.type_id.as_ref() == CHANGE_NOTIFICATION_EVENT_TYPE
}

/// The `(key, change_ref)` pairs a notification's body names.
fn named(event: &Event) -> Vec<(String, String)> {
    event.data.as_ref().expect("a body")["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .map(|c| {
            (
                c["key"].as_str().expect("key").to_owned(),
                c["change_ref"].as_str().expect("ref").to_owned(),
            )
        })
        .collect()
}

/// Report as the consumer's SDK does in process.
async fn report(
    replica: &Replica,
    who: &SecurityContext,
    key: &SettingKey,
    tenant: Uuid,
    change_ref: &str,
    status: &str,
    applied: Option<&str>,
) -> Result<ReportResult, DomainError> {
    replica
        .gear
        .recorder()
        .expect("bound")
        .receive_response(
            who,
            BackResponse {
                change_ref: change_ref.to_owned(),
                tenant: tenant.to_string(),
                key: key.as_str().to_owned(),
                status: status.to_owned(),
                applied_value: applied.map(str::to_owned),
                detail: None,
            },
        )
        .await
}

/// One replica over a fresh database with `key` declared `cascading` and the
/// consumer subscribed at the root and at the child.
struct Scene {
    pg: PgFixture,
    platform: Platform,
    replica: Replica,
    key: SettingKey,
    gear: SecurityContext,
}

async fn scene(name: &str, interval_seconds: u64, tenants: &[Uuid]) -> Option<Scene> {
    let pg = pg_fixture("delivery").await?;
    let platform = platform();
    let replica = Replica::init(&pg, &platform, config(interval_seconds)).await;
    let key = setting(name);
    replica
        .declare(&key, ScopeClass::Cascading, "default")
        .await;
    let gear = consumer();
    for tenant in tenants {
        replica
            .register(&gear, &key, *tenant)
            .await
            .expect("the subscription registers");
    }
    Some(Scene {
        pg,
        platform,
        replica,
        key,
        gear,
    })
}

impl Scene {
    fn subscriber(&self) -> String {
        self.gear.subject_id().to_string()
    }

    /// Run the lifecycle and wait for the replica's group to join the cache
    /// topic: the root tenant is known from then on.
    async fn serve(&self, replicas: usize) {
        self.replica.serve().await;
        self.platform.broker.await_joined(replicas).await;
    }

    async fn overwrite(&self, tenant: Uuid, value: &str) {
        let tag = self.pg.row_tag(&self.key, tenant).await;
        self.replica
            .set(&self.key, tenant, value, Some(&tag))
            .await
            .expect("the overwrite commits");
    }
}

// ---------------------------------------------------------------------------
// Publish and deliver
// ---------------------------------------------------------------------------

/// *Delivery for committed changes*, the notification half: the end of the
/// request that wrote the states publishes one `change_notification` per pair,
/// after its `cache_invalidate`, carrying identifiers only, and stamps what the
/// broker accepted.
#[tokio::test]
async fn a_committed_change_publishes_one_notification_per_pair_after_its_broadcast() {
    let Some(s) = scene("prompt", 1, &[ROOT, CHILD]).await else {
        return;
    };
    let deadlocks = s.pg.deadlocks().await;
    s.serve(1).await;
    registered(&s.platform).await;
    s.platform.broker.forget_published();

    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");

    let change = s.pg.newest_change(&s.key, ROOT).await;
    let events = s.platform.broker.published();
    assert_eq!(
        events[0].type_id.as_ref() as &str,
        CACHE_INVALIDATE_EVENT_TYPE,
        "the broadcast first"
    );
    let notifications: Vec<&Event> = events.iter().filter(|e| is_notification(e)).collect();
    assert_eq!(notifications.len(), 2, "one per pair the write touched");
    let mut tenants: Vec<Uuid> = notifications.iter().map(|e| e.tenant_id).collect();
    tenants.sort();
    assert_eq!(tenants, [ROOT, CHILD], "under each subscription's tenant");
    for event in notifications {
        assert_eq!(event.subject, event.tenant_id.to_string());
        assert_eq!(event.subject_type.as_ref() as &str, ACTIVATION_SUBJECT_TYPE);
        assert_eq!(event.source, "settings-service");
        let data = event.data.clone().expect("a body");
        let mut fields: Vec<_> = data.as_object().unwrap().keys().cloned().collect();
        fields.sort();
        assert_eq!(
            fields,
            ["changes", "subscriber", "tenant"],
            "identifiers only"
        );
        assert_eq!(data["subscriber"], s.subscriber());
        assert_eq!(data["tenant"], event.tenant_id.to_string());
        assert_eq!(named(event), [(s.key.as_str().to_owned(), change.clone())]);
    }
    let states = s.pg.states(&s.key).await;
    assert_eq!(states.len(), 2);
    assert!(
        states
            .iter()
            .all(|row| row.status == "awaiting" && row.notified),
        "the accepted publish stamped every state: {states:?}"
    );
    assert!(states.iter().all(|row| row.change_ref == change));

    s.replica.stop().await;
    assert_eq!(s.pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *State commits with the value*: a change committed with its publish
/// suppressed — the process that committed it never reached the end of its
/// request, here a replica that was not serving — leaves its states `awaiting`,
/// and the delivery loop of a replica that starts later publishes them.
#[tokio::test]
async fn a_state_committed_without_its_publish_is_published_by_the_loop() {
    let Some(s) = scene("crash", 1, &[ROOT]).await else {
        return;
    };
    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");
    let change = s.pg.newest_change(&s.key, ROOT).await;
    let states = s.pg.states(&s.key).await;
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].status, "awaiting");
    assert!(!states[0].notified, "nothing was published");
    assert!(s.platform.broker.notifications().is_empty());

    s.serve(1).await;
    until("the loop's publish", || {
        !s.platform.broker.notifications().is_empty()
    })
    .await;

    let notifications = s.platform.broker.notifications();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].tenant_id, ROOT);
    assert_eq!(
        named(&notifications[0]),
        [(s.key.as_str().to_owned(), change)]
    );
    scan_idle(&s.pg).await;
    assert!(s.pg.states(&s.key).await[0].notified);
    s.replica.stop().await;
}

/// *Delivery backoff*: a refused publish leaves the state unstamped and due at
/// the next tick; once accepted, the state is published again only once the
/// interval has passed since its stamp, not at every tick.
#[tokio::test]
async fn a_refused_publish_is_retried_and_an_accepted_one_waits_out_the_interval() {
    let Some(s) = scene("backoff", 2, &[ROOT]).await else {
        return;
    };
    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");
    s.platform.broker.refuse_notifications(true);
    s.serve(1).await;

    // Two ticks refused: the gear keeps serving, the state stays awaiting.
    until("two refused ticks", || {
        s.platform
            .broker
            .refused
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 2
    })
    .await;
    assert!(s.platform.broker.notifications().is_empty());
    assert!(!s.pg.states(&s.key).await[0].notified);

    s.platform.broker.refuse_notifications(false);
    until("the first accepted publish", || {
        s.platform.broker.notifications().len() == 1
    })
    .await;
    scan_idle(&s.pg).await;
    let first = s.pg.stamp_micros(&s.key, ROOT).await.expect("stamped");

    // Not again within the interval, however many ticks pass.
    tokio::time::sleep(STILL_WAITING).await;
    assert_eq!(s.platform.broker.notifications().len(), 1);

    until("the second publish", || {
        s.platform.broker.notifications().len() == 2
    })
    .await;
    scan_idle(&s.pg).await;
    let second = s.pg.stamp_micros(&s.key, ROOT).await.expect("stamped");
    assert!(
        second - first >= 2_000_000,
        "republished {} us after the stamp, before the 2 s interval",
        second - first
    );
    s.replica.stop().await;
}

/// *Concurrent replicas do not duplicate*: two loops tick together; the one
/// that holds the scan lock publishes, the other skips its pass, so a state is
/// published once per interval.
#[tokio::test]
async fn two_loops_on_one_tick_publish_a_state_once() {
    let Some(s) = scene("twin", 2, &[ROOT]).await else {
        return;
    };
    let second = Replica::init(&s.pg, &s.platform, config(2)).await;
    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");
    let gate = s.platform.broker.gate_notifications(1);

    s.replica.serve().await;
    second.serve().await;
    s.platform.broker.await_joined(2).await;

    // One replica's pass is parked in its publish, holding the scan lock.
    tokio::time::timeout(MUST_HAPPEN, gate.arrived.notified())
        .await
        .expect("a pass reached its publish");
    assert_eq!(s.pg.advisory_locks().await, 1, "the scan lock, held");
    // The other replica's tick falls in the same interval: its single attempt
    // on the lock fails and it skips the pass.
    tokio::time::sleep(STILL_WAITING).await;
    assert_eq!(
        gate.arrivals.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "only one replica reached the publish"
    );

    gate.release();
    until("the publish", || {
        !s.platform.broker.notifications().is_empty()
    })
    .await;
    scan_idle(&s.pg).await;
    tokio::time::sleep(STILL_WAITING).await;
    assert_eq!(
        s.platform.broker.notifications().len(),
        1,
        "once, not twice"
    );

    s.replica.stop().await;
    second.stop().await;
}

/// *Scan during an overwrite*: a change overwrites a state while the scan's
/// publish of it is in flight; the scan's stamp is guarded by the `change_ref`
/// it read, so the new expectation keeps its stamp absent.
#[tokio::test]
async fn an_overwrite_during_the_scan_keeps_the_new_state_unstamped() {
    let Some(s) = scene("scan_overwrite", 3, &[ROOT]).await else {
        return;
    };
    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");
    let first = s.pg.newest_change(&s.key, ROOT).await;
    let gate = s.platform.broker.gate_notifications(1);
    s.serve(1).await;
    tokio::time::timeout(MUST_HAPPEN, gate.arrived.notified())
        .await
        .expect("the scan reached its publish");

    // The overwrite lands while the scan's publish of the first change is
    // parked; its own end-of-request publish is refused, so only the scan's
    // stamp is in question.
    s.platform.broker.refuse_notifications(true);
    s.overwrite(ROOT, "two").await;
    s.platform.broker.refuse_notifications(false);
    let second = s.pg.newest_change(&s.key, ROOT).await;
    assert_ne!(first, second);

    gate.release();
    until("the scan's publish", || {
        !s.platform.broker.notifications().is_empty()
    })
    .await;
    scan_idle(&s.pg).await;

    let states = s.pg.states(&s.key).await;
    assert_eq!(states.len(), 1);
    assert_eq!(
        states[0].change_ref, second,
        "the state names the newer change"
    );
    assert!(
        !states[0].notified,
        "the scan's stamp skipped the replaced row"
    );
    s.replica.stop().await;
}

/// *Overwrite clears the delivery stamp*: a stamped state that a change
/// overwrites is `awaiting` again with no stamp, and the next tick publishes the
/// new expectation.
#[tokio::test]
async fn an_overwrite_clears_the_stamp_and_the_next_tick_publishes_the_new_change() {
    let Some(s) = scene("clear_stamp", 3, &[ROOT]).await else {
        return;
    };
    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");
    s.serve(1).await;
    until("the first publish", || {
        s.platform.broker.notifications().len() == 1
    })
    .await;
    scan_idle(&s.pg).await;
    assert!(s.pg.states(&s.key).await[0].notified);

    s.platform.broker.refuse_notifications(true);
    s.overwrite(ROOT, "two").await;
    s.platform.broker.refuse_notifications(false);
    let second = s.pg.newest_change(&s.key, ROOT).await;
    let states = s.pg.states(&s.key).await;
    assert_eq!(states[0].change_ref, second);
    assert_eq!(states[0].status, "awaiting");
    assert!(!states[0].notified, "the overwrite cleared the stamp");

    until("the new change's publish", || {
        s.platform.broker.notifications().len() == 2
    })
    .await;
    let notifications = s.platform.broker.notifications();
    assert_eq!(
        named(&notifications[1]),
        [(s.key.as_str().to_owned(), second)]
    );
    s.replica.stop().await;
}

/// The loop sweeps a state whose subscription is gone, in a tick of its own,
/// and publishes nothing for it.
#[tokio::test]
async fn the_loop_sweeps_a_state_whose_subscription_vanished() {
    let Some(s) = scene("orphan", 1, &[ROOT]).await else {
        return;
    };
    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");
    assert_eq!(s.pg.states(&s.key).await.len(), 1);
    s.pg.exec(
        "DELETE FROM subscriptions WHERE key = $1",
        vec![s.key.as_str().into()],
    )
    .await;

    s.serve(1).await;
    until_async("the sweep", || async {
        s.pg.states(&s.key).await.is_empty()
    })
    .await;
    assert!(
        s.platform.broker.notifications().is_empty(),
        "swept, not published"
    );
    s.replica.stop().await;
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// *Report recorded in history*: the report that settles a state appends one
/// `activation` record naming the change, and the history lists it as an
/// ordinary record of its subscription tenant.
#[tokio::test]
async fn a_settling_report_is_recorded_under_the_change_it_answers() {
    let Some(s) = scene("history", 30, &[CHILD]).await else {
        return;
    };
    let deadlocks = s.pg.deadlocks().await;
    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");
    let change = s.pg.newest_change(&s.key, ROOT).await;

    let outcome = report(
        &s.replica,
        &s.gear,
        &s.key,
        CHILD,
        &change,
        "success",
        Some("\"one\""),
    )
    .await
    .expect("answered");
    assert_eq!(outcome, ReportResult::Applied);

    let states = s.pg.states(&s.key).await;
    assert_eq!(states[0].status, "succeeded");
    let records = s.pg.activation_records(&s.key).await;
    assert_eq!(
        records,
        [(
            CHILD.to_string(),
            s.subscriber(),
            "success".to_owned(),
            change.clone(),
            None
        )]
    );
    // The history of the subscription tenant lists it as an ordinary record.
    let conn = s.replica.db_conn();
    let listing = AuditStore
        .history(
            &conn,
            &AccessScope::allow_all(),
            s.key.as_str(),
            CHILD,
            &toolkit_odata::ODataQuery::default(),
        )
        .await
        .expect("history");
    let activation: Vec<_> = listing
        .items
        .iter()
        .filter(|r| r.operation.as_str() == "activation")
        .collect();
    assert_eq!(activation.len(), 1);
    assert_eq!(
        activation[0].change_ref.map(|c| c.to_string()),
        Some(change)
    );
    assert_eq!(activation[0].actor, s.subscriber());
    assert_eq!(s.pg.deadlocks().await, deadlocks);
}

/// *Report idempotency*: the consumer's repeat of a report changes nothing and
/// writes no second record; another subscriber's report on the row is ignored.
#[tokio::test]
async fn a_repeated_report_applies_once() {
    let Some(s) = scene("idempotent", 30, &[ROOT]).await else {
        return;
    };
    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");
    let change = s.pg.newest_change(&s.key, ROOT).await;
    let strangers = consumer();

    let stranger = report(
        &s.replica,
        &strangers,
        &s.key,
        ROOT,
        &change,
        "success",
        Some("\"one\""),
    )
    .await
    .expect("answered");
    assert_eq!(
        stranger,
        ReportResult::Ignored,
        "no state of that subscriber"
    );
    let first = report(
        &s.replica,
        &s.gear,
        &s.key,
        ROOT,
        &change,
        "success",
        Some("\"one\""),
    )
    .await
    .expect("answered");
    let repeat = report(
        &s.replica,
        &s.gear,
        &s.key,
        ROOT,
        &change,
        "success",
        Some("\"one\""),
    )
    .await
    .expect("answered");
    assert_eq!(
        (first, repeat),
        (ReportResult::Applied, ReportResult::Ignored)
    );
    assert_eq!(s.pg.activation_records(&s.key).await.len(), 1);
    assert_eq!(s.pg.states(&s.key).await[0].status, "succeeded");
}

/// *Concurrent report and overwrite*, in both orders: a report on the
/// expectation a change replaced is ignored with no record; one that settled
/// the state first is recorded, and the overwrite resets the state to
/// `awaiting` under the newer change.
#[tokio::test]
async fn a_report_and_an_overwrite_in_either_order_leave_the_state_on_the_newer_change() {
    let Some(s) = scene("order", 30, &[ROOT]).await else {
        return;
    };
    s.replica
        .set(&s.key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits");
    let first = s.pg.newest_change(&s.key, ROOT).await;

    // Overwrite first: the report on the replaced expectation is ignored.
    s.overwrite(ROOT, "two").await;
    let second = s.pg.newest_change(&s.key, ROOT).await;
    let late = report(
        &s.replica,
        &s.gear,
        &s.key,
        ROOT,
        &first,
        "success",
        Some("\"one\""),
    )
    .await
    .expect("answered");
    assert_eq!(late, ReportResult::Ignored);
    assert!(
        s.pg.activation_records(&s.key).await.is_empty(),
        "no record"
    );
    let states = s.pg.states(&s.key).await;
    assert_eq!(
        (states[0].change_ref.as_str(), states[0].status.as_str()),
        (second.as_str(), "awaiting")
    );

    // Report first: it settles the state, and the next overwrite resets it.
    let settled = report(
        &s.replica,
        &s.gear,
        &s.key,
        ROOT,
        &second,
        "success",
        Some("\"two\""),
    )
    .await
    .expect("answered");
    assert_eq!(settled, ReportResult::Applied);
    s.overwrite(ROOT, "three").await;
    let third = s.pg.newest_change(&s.key, ROOT).await;
    let states = s.pg.states(&s.key).await;
    assert_eq!(
        (states[0].change_ref.as_str(), states[0].status.as_str()),
        (third.as_str(), "awaiting")
    );
    assert_eq!(
        s.pg.activation_records(&s.key).await.len(),
        1,
        "the one settling report"
    );
}

/// The same race, genuinely concurrent: a report and an overwrite of one state
/// at once, many times. Whatever the interleaving, the state ends on the newer
/// change, awaiting, and there is exactly one `activation` record per report
/// that answered `applied`.
#[tokio::test]
async fn a_concurrent_report_and_overwrite_never_overwrite_the_newer_expectation() {
    let Some(pg) = pg_fixture("delivery").await else {
        return;
    };
    let platform = platform();
    let replica = Replica::init(&pg, &platform, config(30)).await;
    let gear = consumer();
    let deadlocks = pg.deadlocks().await;
    for round in 0..12 {
        let key = setting(&format!("race{round}"));
        replica
            .declare(&key, ScopeClass::Cascading, "default")
            .await;
        replica
            .register(&gear, &key, ROOT)
            .await
            .expect("registered");
        replica
            .set(&key, ROOT, "one", Some("absent"))
            .await
            .expect("the write commits");
        let first = pg.newest_change(&key, ROOT).await;
        let tag = pg.row_tag(&key, ROOT).await;

        let reporting = {
            let (replica, gear, key, first) = (&replica, &gear, &key, &first);
            report(replica, gear, key, ROOT, first, "success", Some("\"one\""))
        };
        let overwriting = replica.set(&key, ROOT, "two", Some(&tag));
        let (reported, overwritten) = tokio::join!(reporting, overwriting);
        overwritten.expect("the overwrite commits");
        let applied = reported.expect("answered") == ReportResult::Applied;

        let second = pg.newest_change(&key, ROOT).await;
        let states = pg.states(&key).await;
        assert_eq!(states.len(), 1, "round {round}");
        assert_eq!(
            states[0].change_ref, second,
            "round {round}: the newer change"
        );
        assert_eq!(states[0].status, "awaiting", "round {round}");
        let records = pg.activation_records(&key).await;
        assert_eq!(records.len(), usize::from(applied), "round {round}");
        if applied {
            assert_eq!(
                records[0].3, first,
                "the record names the change it answered"
            );
        }
    }
    assert_eq!(pg.deadlocks().await, deadlocks, "no deadlock");
}
