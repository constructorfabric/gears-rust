#![cfg(feature = "integration")]
// The doubles, the fixture and the polls are plain helpers, which
// `allow-expect-in-tests` does not reach; a failed expectation there is the
// test failing, as in the sibling PostgreSQL suites.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Consumer activation, end to end, on a real `PostgreSQL`: a whole
//! `SettingsService` replica, the delivering Event Broker double of
//! `tests/support/platform.rs`, and an in-process consumer that is the SDK's
//! own client taken out of the replica's `ClientHub` — `subscribe`, the fresh
//! re-read, the handler, the report and `retire()` — the Level 2 end-to-end row
//! of DESIGN-activation §7.
//!
//! What the test hosts is the platform around the replica: the broker double
//! (which delivers a published event to every joined group whose interest names
//! its tenant and type, and whose log the test reads), the Types Registry, the
//! Tenant Resolver and a policy decision point that allows everything. The
//! assertions are on outcomes, never on how long anything took; the delivery
//! loop's interval is a whole second, so a row due again is waited for, with a
//! deadline that only a hang can exhaust.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_activation_e2e_test
//! ```

// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-tests:p1

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cf_gears_settings_service::infra::storage::audit_store::AuditStore;
use event_broker_sdk::EventBrokerApi;
use serde_json::{Value, json};
use settings_service_sdk::models::ScopeClass;
use settings_service_sdk::{
    ActivationOutcome, ChangeBundle, KeyState, ReportResult, RetireResult, SettingChangeHandler,
    SettingKey, SettingsActivationClient, SubscriptionHandle,
};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use support::platform::{CHILD, PgFixture, Platform, ROOT, Replica, pg_fixture, platform};

/// How long the test waits for something that must happen.
const MUST_HAPPEN: Duration = Duration::from_secs(40);
/// How long the test watches for something that must *not* have happened.
const STILL_WAITING: Duration = Duration::from_millis(800);

fn setting(name: &str) -> SettingKey {
    SettingKey::contributed("cf", "demo", "e2e", name, std::num::NonZeroU32::MIN)
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

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(MUST_HAPPEN, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

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

/// What the consumer's handler does with a bundle.
#[derive(Clone)]
enum Mode {
    /// Apply the fresh value and say so.
    Faithful,
    /// Report having applied this value instead.
    Wrong(Value),
    /// Report nothing: the application completes later.
    Silent,
}

struct Applier {
    mode: Mutex<Mode>,
    bundles: Mutex<Vec<ChangeBundle>>,
}

impl Applier {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode: Mutex::new(mode),
            bundles: Mutex::new(Vec::new()),
        })
    }

    fn bundles(&self) -> Vec<ChangeBundle> {
        self.bundles.lock().unwrap().clone()
    }

    /// The `(change_ref, value)` pairs the handler was handed, in order.
    fn heard(&self) -> Vec<(Option<Uuid>, Value)> {
        self.bundles()
            .iter()
            .flat_map(|b| {
                b.changes
                    .iter()
                    .map(|c| (c.change_ref, c.value.value.clone()))
            })
            .collect()
    }
}

#[async_trait]
impl SettingChangeHandler for Applier {
    async fn on_change(&self, bundle: ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)> {
        let mode = self.mode.lock().unwrap().clone();
        let outcomes = bundle
            .changes
            .iter()
            .filter_map(|c| match &mode {
                Mode::Faithful => Some((
                    c.key.clone(),
                    ActivationOutcome::Success {
                        applied_value: c.value.value.clone(),
                    },
                )),
                Mode::Wrong(other) => Some((
                    c.key.clone(),
                    ActivationOutcome::Success {
                        applied_value: other.clone(),
                    },
                )),
                Mode::Silent => None,
            })
            .collect();
        self.bundles.lock().unwrap().push(bundle);
        outcomes
    }
}

/// One replica over a fresh database, `key` declared `cascading`, the lifecycle
/// running, and the consumer's client.
struct Scene {
    pg: PgFixture,
    platform: Platform,
    replica: Replica,
    key: SettingKey,
    gear: SecurityContext,
    client: Arc<dyn SettingsActivationClient>,
}

async fn scene(name: &str, serve: bool) -> Option<Scene> {
    let pg = pg_fixture("activation-e2e").await?;
    let platform = platform();
    let replica = Replica::init(&pg, &platform, json!({ "redeliver_interval_seconds": 1 })).await;
    let key = setting(name);
    replica
        .declare(&key, ScopeClass::Cascading, "default")
        .await;
    let client = replica
        .hub
        .get::<dyn SettingsActivationClient>()
        .expect("the activation client is bound beside the reader");
    let s = Scene {
        pg,
        platform,
        replica,
        key,
        gear: consumer(),
        client,
    };
    if serve {
        s.serve().await;
    }
    Some(s)
}

impl Scene {
    fn subscriber(&self) -> String {
        self.gear.subject_id().to_string()
    }

    /// Run the lifecycle and wait for the replica's cache group to join.
    async fn serve(&self) {
        self.replica.serve().await;
        self.platform.broker.await_joined(1).await;
    }

    async fn subscribe(&self, at: Uuid, handler: &Arc<Applier>) -> SubscriptionHandle {
        self.client
            .subscribe(
                &self.gear,
                vec![self.key.clone()],
                at.to_string(),
                Arc::clone(handler) as Arc<dyn SettingChangeHandler>,
            )
            .await
            .expect("subscribe answers a handle")
    }

    /// Subscribe and wait until the key is registered and the consumer's group
    /// holds an open stream beside the replica's own.
    async fn subscribed(&self, at: Uuid, handler: &Arc<Applier>) -> SubscriptionHandle {
        let handle = self.subscribe(at, handler).await;
        until("the key to register", || {
            matches!(
                handle.states().get(&self.key),
                Some(KeyState::Registered { .. })
            )
        })
        .await;
        until("the consumer's group to join", || {
            self.platform.broker.open_streams() >= 2
        })
        .await;
        handle
    }

    async fn write(&self, tenant: Uuid, value: &str, tag: Option<&str>) {
        self.replica
            .set(&self.key, tenant, value, tag)
            .await
            .expect("the write commits");
    }

    async fn overwrite(&self, tenant: Uuid, value: &str) {
        let tag = self.pg.row_tag(&self.key, tenant).await;
        self.write(tenant, value, Some(&tag)).await;
    }

    async fn report(
        &self,
        change_ref: &str,
        tenant: Uuid,
        outcome: ActivationOutcome,
    ) -> ReportResult {
        self.client
            .report_outcome(
                &self.gear,
                Uuid::parse_str(change_ref).expect("a change_ref"),
                tenant.to_string(),
                self.key.clone(),
                outcome,
            )
            .await
            .expect("answered")
    }

    async fn status(&self) -> String {
        let states = self.pg.states(&self.key).await;
        assert_eq!(states.len(), 1, "{states:?}");
        states[0].status.clone()
    }

    async fn settled(&self, status: &str) {
        until_async("the state to settle", || async {
            self.pg
                .states(&self.key)
                .await
                .first()
                .is_some_and(|s| s.status == status)
        })
        .await;
    }

    async fn subscriptions(&self) -> i64 {
        self.pg
            .count(
                "SELECT count(*) FROM subscriptions WHERE key = $1",
                vec![self.key.as_str().into()],
            )
            .await
    }
}

/// Every event the broker accepted, as the text of its body.
fn published_bodies(platform: &Platform) -> Vec<(bool, Value)> {
    platform
        .broker
        .published()
        .iter()
        .map(|e| {
            (
                e.type_id.as_ref() == settings_service_sdk::gts::CHANGE_NOTIFICATION_EVENT_TYPE,
                e.data.clone().unwrap_or(Value::Null),
            )
        })
        .collect()
}

/// The attribute names of an object, sorted.
fn names(value: &Value) -> Vec<&str> {
    let mut names: Vec<&str> = value
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    names.sort_unstable();
    names
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// *Full loop*, the write side: a set → a `change_notification` → the fresh
/// re-read → the handler → the report → the state `succeeded` with one
/// `activation` audit record carrying the change's `change_ref`, visible
/// through the audit store; no value in any published event.
#[tokio::test]
async fn a_set_is_notified_re_read_applied_and_reported_to_succeeded() {
    let Some(s) = scene("loop", true).await else {
        return;
    };
    let deadlocks = s.pg.deadlocks().await;
    let applier = Applier::new(Mode::Faithful);
    let handle = s.subscribed(ROOT, &applier).await;
    // The subscription's own fresh read: the default, nothing set yet.
    let KeyState::Registered { value: Some(value) } = handle.states()[&s.key].clone() else {
        panic!("registered with its fresh value");
    };
    assert_eq!(value.value, json!("default"));
    assert!(applier.bundles().is_empty(), "nothing changed yet");

    s.write(ROOT, "one", Some("absent")).await;
    let change = s.pg.newest_change(&s.key, ROOT).await;
    until("the handler", || !applier.bundles().is_empty()).await;
    s.settled("succeeded").await;

    // At-least-once delivery may redeliver: keep the first bundle per change_ref.
    let mut heard = applier.heard();
    heard.dedup_by_key(|h| h.0);
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].0.map(|c| c.to_string()), Some(change.clone()));
    assert_eq!(
        heard[0].1,
        json!("one"),
        "the fresh re-read saw the committed value"
    );
    let records = s.pg.activation_records(&s.key).await;
    assert_eq!(
        records,
        [(
            ROOT.to_string(),
            s.subscriber(),
            "success".to_owned(),
            change.clone(),
            None
        )],
        "one activation record under the change"
    );
    let conn = s.replica.db_conn();
    let listing = AuditStore
        .history(
            &conn,
            &AccessScope::allow_all(),
            s.key.as_str(),
            ROOT,
            &toolkit_odata::ODataQuery::default(),
        )
        .await
        .expect("history");
    let activation: Vec<_> = listing
        .items
        .iter()
        .filter(|r| r.operation.as_str() == "activation")
        .collect();
    assert_eq!(activation.len(), 1, "visible through the audit store");
    assert_eq!(
        activation[0].change_ref.map(|c| c.to_string()),
        Some(change)
    );
    // *No secret in stream*, as far as a plain value goes: no event carries it.
    // Asserted on the published JSON's key set, not on a substring of its text:
    // identifiers only, no attribute that could carry a value — for the
    // notification and for the replicas' cache broadcast alike.
    let bodies = published_bodies(&s.platform);
    assert!(
        bodies.iter().any(|(notification, _)| *notification),
        "the change was notified"
    );
    for (notification, body) in &bodies {
        if *notification {
            assert_eq!(
                names(body),
                ["changes", "subscriber", "tenant"],
                "a notification names its identifiers only: {body}"
            );
            for changed in body["changes"].as_array().expect("changes is an array") {
                assert_eq!(names(changed), ["change_ref", "key"], "{body}");
            }
        } else {
            assert_eq!(
                names(body),
                ["change_set_id", "changed_keys", "tenant"],
                "a broadcast names its identifiers only: {body}"
            );
        }
    }

    drop(handle);
    s.replica.stop().await;
    assert_eq!(s.pg.deadlocks().await, deadlocks, "no deadlock");
    let (_, _, left, deleted, held) = s.platform.broker.lifecycle();
    assert_eq!(left, 2, "the replica's group and the consumer's each left");
    assert_eq!(deleted, 2, "and each was deleted");
    assert_eq!(held, 0, "the broker holds no group after the stop");
}

/// *Value mismatch → failed*: a success that reports another value than the
/// one recorded when the change committed settles the state `failed`, with its
/// `activation` record.
#[tokio::test]
async fn a_wrong_value_settles_failed_with_its_audit_record() {
    let Some(s) = scene("wrong", true).await else {
        return;
    };
    let applier = Applier::new(Mode::Wrong(json!("not-what-was-set")));
    let _handle = s.subscribed(ROOT, &applier).await;

    s.write(ROOT, "one", Some("absent")).await;
    let change = s.pg.newest_change(&s.key, ROOT).await;
    s.settled("failed").await;

    let records = s.pg.activation_records(&s.key).await;
    assert_eq!(records.len(), 1);
    assert_eq!(
        (
            records[0].1.as_str(),
            records[0].2.as_str(),
            records[0].3.as_str()
        ),
        (s.subscriber().as_str(), "failure", change.as_str())
    );
    s.replica.stop().await;
}

/// A failed outcome carries the consumer's detail into the record.
#[tokio::test]
async fn a_failure_the_consumer_reports_settles_failed_with_its_detail() {
    let Some(s) = scene("detail", true).await else {
        return;
    };
    let applier = Applier::new(Mode::Silent);
    let _handle = s.subscribed(ROOT, &applier).await;
    s.write(ROOT, "one", Some("absent")).await;
    let change = s.pg.newest_change(&s.key, ROOT).await;
    until("the handler", || !applier.bundles().is_empty()).await;

    let answer = s
        .report(
            &change,
            ROOT,
            ActivationOutcome::Failed {
                detail: "the pool could not be rebuilt".to_owned(),
            },
        )
        .await;

    assert_eq!(answer, ReportResult::Applied);
    assert_eq!(s.status().await, "failed");
    let records = s.pg.activation_records(&s.key).await;
    assert_eq!(
        records[0].4.as_deref(),
        Some("the pool could not be rebuilt")
    );
    s.replica.stop().await;
}

/// A report on a replaced `change_ref` is `ignored` and changes nothing; the
/// report of the current expectation settles it.
#[tokio::test]
async fn a_report_on_a_replaced_expectation_is_ignored() {
    let Some(s) = scene("stale", true).await else {
        return;
    };
    let applier = Applier::new(Mode::Silent);
    let _handle = s.subscribed(ROOT, &applier).await;
    s.write(ROOT, "one", Some("absent")).await;
    let first = s.pg.newest_change(&s.key, ROOT).await;
    until("the first notification", || !applier.heard().is_empty()).await;
    s.overwrite(ROOT, "two").await;
    let second = s.pg.newest_change(&s.key, ROOT).await;
    assert_ne!(first, second);
    until("the second notification", || applier.heard().len() >= 2).await;
    let (_, value) = applier.heard().pop().unwrap();
    assert_eq!(
        value,
        json!("two"),
        "the re-read after the overwrite is fresh"
    );

    let stale = s
        .report(
            &first,
            ROOT,
            ActivationOutcome::Failed {
                detail: "late".to_owned(),
            },
        )
        .await;

    assert_eq!(
        stale,
        ReportResult::Ignored,
        "it answers the replaced expectation"
    );
    assert_eq!(s.status().await, "awaiting", "and changes nothing");
    assert!(
        s.pg.activation_records(&s.key).await.is_empty(),
        "no record is written"
    );
    let current = s
        .report(
            &second,
            ROOT,
            ActivationOutcome::Success {
                applied_value: json!("two"),
            },
        )
        .await;
    assert_eq!(current, ReportResult::Applied);
    assert_eq!(s.status().await, "succeeded");
    s.replica.stop().await;
}

/// `retire()` deletes the subscription and its state; a late report is
/// `ignored` and writes no record; the handle's group goes with it.
#[tokio::test]
async fn retire_deletes_the_rows_and_a_late_report_is_ignored() {
    let Some(s) = scene("retire", true).await else {
        return;
    };
    let applier = Applier::new(Mode::Silent);
    let handle = s.subscribed(ROOT, &applier).await;
    s.write(ROOT, "one", Some("absent")).await;
    let change = s.pg.newest_change(&s.key, ROOT).await;
    until("the notification", || !applier.heard().is_empty()).await;
    assert_eq!(s.status().await, "awaiting");
    assert_eq!(s.subscriptions().await, 1);

    let answers = handle.retire(&s.gear).await.expect("answers");

    assert_eq!(answers[&s.key], RetireResult::Retired);
    assert!(handle.states().is_empty());
    assert_eq!(s.subscriptions().await, 0, "the subscription is gone");
    assert!(s.pg.states(&s.key).await.is_empty(), "with its state");
    let late = s
        .report(
            &change,
            ROOT,
            ActivationOutcome::Success {
                applied_value: json!("one"),
            },
        )
        .await;
    assert_eq!(late, ReportResult::Ignored);
    assert!(s.pg.activation_records(&s.key).await.is_empty());
    until("the consumer's group to be deleted", || {
        s.platform.broker.lifecycle().3 >= 1
    })
    .await;
    s.replica.stop().await;
}

/// *Restart re-publish*: the consumer goes away with a state `awaiting`; its
/// replacement subscribes again, and the delivery loop re-publishes after its
/// interval, the replacement re-reads, applies, reports, and the state settles.
#[tokio::test]
async fn a_consumer_restart_is_caught_up_by_the_delivery_loop() {
    let Some(s) = scene("restart", true).await else {
        return;
    };
    let first = Applier::new(Mode::Silent);
    let handle = s.subscribed(ROOT, &first).await;
    s.write(ROOT, "one", Some("absent")).await;
    let change = s.pg.newest_change(&s.key, ROOT).await;
    until("the first consumer to hear it", || {
        !first.heard().is_empty()
    })
    .await;
    assert_eq!(s.status().await, "awaiting");

    // The consumer goes away: its handle is dropped, which retires nothing, and
    // its group goes with it. Nothing reports.
    drop(handle);
    until("the group to be deleted", || {
        s.platform.broker.lifecycle().3 >= 1
    })
    .await;
    assert_eq!(
        s.subscriptions().await,
        1,
        "the subscription survives the drop"
    );
    assert_eq!(s.status().await, "awaiting");

    // The replacement subscribes; the loop re-publishes the awaiting state.
    let second = Applier::new(Mode::Faithful);
    let _again = s.subscribe(ROOT, &second).await;
    until("the replacement to hear it", || !second.heard().is_empty()).await;
    s.settled("succeeded").await;

    let heard = second.heard();
    assert_eq!(heard[0].0.map(|c| c.to_string()), Some(change.clone()));
    assert_eq!(heard[0].1, json!("one"));
    assert_eq!(s.pg.activation_records(&s.key).await.len(), 1);
    assert_eq!(
        s.subscriptions().await,
        1,
        "a repeated subscribe is a new handle over the one subscription"
    );
    s.replica.stop().await;
}

/// A repeated `change_notification` is re-read, re-applied and re-reported like
/// the first; the repeat's report answers `ignored` once the first settled the
/// state, and no second audit record is written.
#[tokio::test]
async fn a_repeated_notification_is_re_applied_and_its_report_writes_no_second_record() {
    let Some(s) = scene("repeat", true).await else {
        return;
    };
    let applier = Applier::new(Mode::Faithful);
    let _handle = s.subscribed(ROOT, &applier).await;
    s.write(ROOT, "one", Some("absent")).await;
    s.settled("succeeded").await;
    // A redelivered duplicate between polls is tolerated (at-least-once).
    assert!(!applier.heard().is_empty());

    // The broker delivers the same notification again, as at-least-once does.
    let again = s
        .platform
        .broker
        .notifications()
        .first()
        .cloned()
        .expect("the notification published for the write");
    s.platform
        .broker
        .publish(&s.gear, &again)
        .await
        .expect("the broker takes the repeat");
    until("the repeat to be handled", || applier.heard().len() == 2).await;
    tokio::time::sleep(STILL_WAITING).await;

    assert_eq!(
        applier.heard()[0],
        applier.heard()[1],
        "re-read and re-applied alike"
    );
    assert_eq!(s.status().await, "succeeded", "the state ends as it did");
    assert_eq!(
        s.pg.activation_records(&s.key).await.len(),
        1,
        "the repeat's report answered ignored: no second record"
    );
    s.replica.stop().await;
}

/// *No broker*: a write commits with its state `awaiting` and nothing
/// published; the loop idles; once the broker appears the loop registers the
/// types and delivers, and the consumer's group joins, reads and reports.
#[tokio::test]
async fn a_write_without_a_broker_stays_awaiting_until_the_broker_appears() {
    let Some(pg) = pg_fixture("activation-e2e").await else {
        return;
    };
    let platform = platform();
    let replica =
        Replica::init_without_broker(&pg, &platform, json!({ "redeliver_interval_seconds": 1 }))
            .await;
    let key = setting("nobroker");
    replica
        .declare(&key, ScopeClass::Cascading, "default")
        .await;
    let client = replica
        .hub
        .get::<dyn SettingsActivationClient>()
        .expect("bound");
    let gear = consumer();
    let applier = Applier::new(Mode::Faithful);
    let handle = client
        .subscribe(
            &gear,
            vec![key.clone()],
            ROOT.to_string(),
            Arc::clone(&applier) as Arc<dyn SettingChangeHandler>,
        )
        .await
        .expect("a handle");
    until("the key to register", || {
        matches!(handle.states().get(&key), Some(KeyState::Registered { .. }))
    })
    .await;
    replica.serve().await;

    replica
        .set(&key, ROOT, "one", Some("absent"))
        .await
        .expect("the write commits without a broker");
    tokio::time::sleep(STILL_WAITING + Duration::from_millis(1200)).await;

    let states = pg.states(&key).await;
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].status, "awaiting");
    assert!(!states[0].notified, "nothing was published");
    assert!(platform.broker.published().is_empty());
    assert!(applier.bundles().is_empty());
    assert!(
        matches!(handle.states().get(&key), Some(KeyState::Registered { .. })),
        "no broker is not a pending key"
    );
    assert!(
        !platform
            .registry
            .has_registered(settings_service_sdk::gts::ACTIVATION_TOPIC_ID),
        "the loop idles until a broker resolves"
    );

    replica.plug_broker(&platform);
    until("the consumer to hear the change", || {
        !applier.bundles().is_empty()
    })
    .await;
    until_async("the state to settle", || async {
        pg.states(&key)
            .await
            .first()
            .is_some_and(|s| s.status == "succeeded")
    })
    .await;
    assert!(
        platform
            .registry
            .has_registered(settings_service_sdk::gts::CHANGE_NOTIFICATION_EVENT_TYPE),
        "the loop registered the activation types once the broker resolved"
    );
    assert_eq!(applier.heard()[0].1, json!("one"));
    replica.stop().await;
}

/// A subscription at a descendant is notified for a write at its ancestor that
/// changes the value it inherits, under the descendant's tenant.
#[tokio::test]
async fn a_descendants_subscription_hears_the_ancestors_write() {
    let Some(s) = scene("inherit", true).await else {
        return;
    };
    let applier = Applier::new(Mode::Faithful);
    let _handle = s.subscribed(CHILD, &applier).await;

    s.write(ROOT, "inherited", Some("absent")).await;
    until("the handler", || !applier.bundles().is_empty()).await;
    s.settled("succeeded").await;

    let bundle = &applier.bundles()[0];
    assert_eq!(
        bundle.tenant,
        CHILD.to_string(),
        "the bundle is at the subscription's tenant"
    );
    assert_eq!(applier.heard()[0].1, json!("inherited"));
    let states = s.pg.states(&s.key).await;
    assert_eq!(states[0].tenant, CHILD.to_string());
    s.replica.stop().await;
}
