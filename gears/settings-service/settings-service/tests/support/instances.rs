//! A rig for the per-instance account of consumer activation on a real
//! `PostgreSQL`: one `SettingsService` replica that is *not* served — no
//! delivery loop, no consumer task — whose broker client the test has resolved
//! by hand, and a notifier over the replica's database and broker client whose
//! ticks the test drives one at a time.
//!
//! Driving the ticks is what makes the rows of DESIGN-activation §7 exact: the
//! interval is a number the test controls (`elapse` makes every stamped state
//! due again without a sleep), `K` is the number of ticks that publish, and a
//! call of silence or a forgetting happens at the tick the test runs, not when a
//! timer fires. A write's own end-of-request publish is refused while the write
//! runs, so no delivery is counted but the ones the test's ticks make.
//!
//! The races are ordered with the holds of
//! `infra::storage::activation_hold` (a statement of the repository parked until
//! the test releases it) and, for a write, the coordinator's pause point.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cf_gears_settings_service::domain::activation::{
    ActivationNotifier, BackResponse, DueRow, NotificationGateway, NotifiedChange, ReportResult,
    SubscriberTenant, Tick,
};
use cf_gears_settings_service::domain::error::DomainError;
use cf_gears_settings_service::infra::activation_metrics::OtelNotifyMetrics;
use cf_gears_settings_service::infra::mutation_lock::DeliveryScanLock;
use cf_gears_settings_service::infra::storage::notification_store::DbNotificationStore;
use cf_gears_settings_service::infra::value_writes::PausePoint;
use serde_json::json;
use settings_service_sdk::SettingKey;
use settings_service_sdk::models::ScopeClass;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::platform::{PgFixture, Platform, ROOT, Replica, pg_fixture, platform};

/// How long the test waits for something that must happen: far beyond any real
/// wait, so only a hang can exhaust it.
pub const MUST_HAPPEN: Duration = Duration::from_secs(40);
/// How long the test watches for something that must *not* have happened.
pub const STILL_WAITING: Duration = Duration::from_millis(500);

/// The tick interval the rig's notifier is built with, in seconds: long, so
/// that nothing is due again unless the test says an interval passed.
pub const INTERVAL_SECONDS: u64 = 60;
/// The report timeout: `K = ceil(120 / 60) = 2` accepted deliveries.
pub const REPORT_TIMEOUT_SECONDS: u64 = 120;
/// `K` of the rig.
pub const K: i32 = 2;

/// The `n`th consumer instance of a test.
pub fn instance(n: u128) -> Uuid {
    Uuid::from_u128(0x1_0000 + n)
}

/// A subscriber: a gear's own security context.
pub fn gear_context() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(ROOT)
        .subject_type("gts.cf.core.security.subject_service.v1~")
        .build()
        .expect("a context")
}

pub fn key_named(name: &str) -> SettingKey {
    SettingKey::contributed(
        "cf",
        "demo",
        "instances",
        &name.replace('-', "_"),
        std::num::NonZeroU32::MIN,
    )
    .expect("a valid key")
}

/// One activation state as the table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub status: String,
    pub deliveries: i32,
    pub change_ref: String,
    pub expected_value: String,
    pub silent_since: bool,
    pub notified: bool,
}

/// One subscription as the table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sub {
    pub silent: bool,
    pub label: Option<String>,
}

/// One `activation` audit record of an instance: `(outcome, change_ref,
/// tenant)`.
pub type Record = (String, String, String);

pub struct Rig {
    pub pg: PgFixture,
    pub platform: Platform,
    pub replica: Replica,
    pub key: SettingKey,
    pub gear: SecurityContext,
    pub notifier: Arc<ActivationNotifier>,
    pub cancel: CancellationToken,
    written: Mutex<HashSet<Uuid>>,
}

impl Rig {
    /// A rig whose broker is up: the replica's broker client has learned the
    /// root tenant, and the first tick registers the activation types.
    pub async fn new(name: &str) -> Option<Self> {
        Self::build(name, true).await
    }

    /// A rig with no Event Broker in the hub: every tick is the pass that only
    /// forgets and sweeps.
    pub async fn without_broker(name: &str) -> Option<Self> {
        Self::build(name, false).await
    }

    async fn build(name: &str, broker: bool) -> Option<Self> {
        let pg = pg_fixture("activation-instances").await?;
        let platform = platform();
        let replica = if broker {
            Replica::init(&pg, &platform, json!({})).await
        } else {
            Replica::init_without_broker(&pg, &platform, json!({})).await
        };
        let key = key_named(name);
        replica
            .declare(&key, ScopeClass::Cascading, "default")
            .await;
        let cancel = CancellationToken::new();
        let client = replica.gear.event_broker().expect("initialized");
        if broker {
            assert!(
                client.resolve_once(&cancel).await,
                "the broker client learns the root tenant"
            );
        }
        let db = replica.gear.db().expect("initialized");
        let notifier = Arc::new(
            ActivationNotifier::new(
                Arc::new(DbNotificationStore::new(Arc::clone(&db))),
                client as Arc<dyn NotificationGateway>,
                Arc::new(DeliveryScanLock::new(db)),
                Arc::new(OtelNotifyMetrics::new()),
                Duration::from_secs(INTERVAL_SECONDS),
            )
            .with_report_timeout(Duration::from_secs(REPORT_TIMEOUT_SECONDS)),
        );
        assert_eq!(notifier.silent_after(), u32::try_from(K).unwrap());
        Some(Self {
            pg,
            platform,
            replica,
            key,
            gear: gear_context(),
            notifier,
            cancel,
            written: Mutex::new(HashSet::new()),
        })
    }

    pub fn subscriber(&self) -> String {
        self.gear.subject_id().to_string()
    }

    // -- writes and registrations -------------------------------------------

    /// Register `instance`'s subscription to the key at `tenant`.
    pub async fn register(&self, instance: Uuid, tenant: Uuid) {
        self.register_as(&self.gear, instance, tenant, None)
            .await
            .expect("the subscription registers");
    }

    pub async fn register_as(
        &self,
        ctx: &SecurityContext,
        instance: Uuid,
        tenant: Uuid,
        label: Option<&str>,
    ) -> Result<cf_gears_settings_service::domain::activation::Subscription, DomainError> {
        self.replica
            .gear
            .subscriptions()
            .expect("initialized")
            .register_subscription(ctx, &self.key, tenant, Some(instance), label)
            .await
    }

    pub async fn unsubscribe(
        &self,
        ctx: &SecurityContext,
        instance: Uuid,
        tenant: Uuid,
    ) -> Result<(), DomainError> {
        self.replica
            .gear
            .subscriptions()
            .expect("initialized")
            .unsubscribe(ctx, &self.key, tenant, Some(instance))
            .await
    }

    /// Set `value` at `tenant`, its own end-of-request publish refused so that
    /// no delivery is counted but the ticks' own.
    pub async fn write(&self, tenant: Uuid, value: &str) {
        let tag = if self.written.lock().unwrap().insert(tenant) {
            "absent".to_owned()
        } else {
            self.pg.row_tag(&self.key, tenant).await
        };
        self.platform.broker.refuse_notifications(true);
        let done = self.replica.set(&self.key, tenant, value, Some(&tag)).await;
        self.platform.broker.refuse_notifications(false);
        done.expect("the write commits");
    }

    /// The change a write left: the id of its audit record.
    pub async fn change(&self, tenant: Uuid) -> String {
        self.pg.newest_change(&self.key, tenant).await
    }

    // -- ticks ---------------------------------------------------------------

    pub async fn tick(&self) -> Tick {
        self.notifier.tick(&self.cancel).await
    }

    /// A tick that runs as a task, for a test that holds it.
    pub fn tick_task(&self) -> tokio::task::JoinHandle<Tick> {
        let notifier = Arc::clone(&self.notifier);
        let cancel = self.cancel.clone();
        tokio::spawn(async move { notifier.tick(&cancel).await })
    }

    /// Every stamped `awaiting` state is due again, as after an interval.
    pub async fn elapse(&self) {
        self.pg
            .exec(
                "UPDATE activation_state SET last_notified_at = now() - interval '10 minutes'
                  WHERE key = $1 AND last_notified_at IS NOT NULL",
                vec![self.key.as_str().into()],
            )
            .await;
    }

    /// An interval passes and one tick runs.
    pub async fn tick_due(&self) -> Tick {
        self.elapse().await;
        self.tick().await
    }

    /// `n` ticks, each after an interval; each must publish.
    pub async fn deliveries(&self, n: i32) {
        for _ in 0..n {
            let tick = self.tick_due().await;
            assert!(
                matches!(tick, Tick::Complete { published } if published > 0),
                "the tick published: {tick:?}"
            );
        }
    }

    /// The `K` deliveries and the tick that calls the unanswered silent.
    pub async fn run_to_silence(&self) {
        self.deliveries(K).await;
        let tick = self.tick_due().await;
        assert!(matches!(tick, Tick::Complete { .. }), "{tick:?}");
    }

    // -- reports -------------------------------------------------------------

    pub async fn report(
        &self,
        instance: Uuid,
        tenant: Uuid,
        change_ref: &str,
        status: &str,
        applied: Option<&str>,
    ) -> Result<ReportResult, DomainError> {
        Self::report_with(
            &self.replica,
            &self.gear,
            &self.key,
            instance,
            tenant,
            change_ref,
            status,
            applied,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn report_with(
        replica: &Replica,
        who: &SecurityContext,
        key: &SettingKey,
        instance: Uuid,
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
                    instance_id: Some(instance),
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

    /// A report as a task, for a test that holds it.
    pub fn report_task(
        &self,
        instance: Uuid,
        tenant: Uuid,
        change_ref: String,
        status: &'static str,
        applied: Option<String>,
    ) -> tokio::task::JoinHandle<Result<ReportResult, DomainError>> {
        let recorder = self.replica.gear.recorder().expect("bound");
        let (who, key) = (self.gear.clone(), self.key.clone());
        tokio::spawn(async move {
            recorder
                .receive_response(
                    &who,
                    BackResponse {
                        instance_id: Some(instance),
                        change_ref,
                        tenant: tenant.to_string(),
                        key: key.as_str().to_owned(),
                        status: status.to_owned(),
                        applied_value: applied,
                        detail: None,
                    },
                )
                .await
        })
    }

    /// A registration as a task, for a test that holds it.
    pub fn register_task(
        &self,
        instance: Uuid,
        tenant: Uuid,
    ) -> tokio::task::JoinHandle<Result<(), DomainError>> {
        let service = self.replica.gear.subscriptions().expect("initialized");
        let (who, key) = (self.gear.clone(), self.key.clone());
        tokio::spawn(async move {
            service
                .register_subscription(&who, &key, tenant, Some(instance), None)
                .await
                .map(|_| ())
        })
    }

    /// The faithful report: the instance's current `change_ref` and expected
    /// value, `success`.
    pub async fn confirm(&self, instance: Uuid, tenant: Uuid) -> ReportResult {
        let row = self
            .row(instance, tenant)
            .await
            .expect("a state to confirm");
        self.report(
            instance,
            tenant,
            &row.change_ref,
            "success",
            Some(row.expected_value.as_str()),
        )
        .await
        .expect("the report is answered")
    }

    /// A stale report: an earlier change's `change_ref`, another value.
    pub async fn stale(&self, instance: Uuid, tenant: Uuid) -> ReportResult {
        self.report(
            instance,
            tenant,
            &Uuid::new_v4().to_string(),
            "success",
            Some("\"another\""),
        )
        .await
        .expect("the report is answered")
    }

    // -- what the tables hold --------------------------------------------------

    pub async fn row(&self, instance: Uuid, tenant: Uuid) -> Option<Row> {
        self.pg
            .rows(
                "SELECT status, deliveries, change_ref::text AS change_ref, expected_value,
                        (silent_since IS NOT NULL) AS silent, (last_notified_at IS NOT NULL) AS notified
                   FROM activation_state
                  WHERE key = $1 AND instance_id = $2 AND tenant = $3",
                vec![
                    self.key.as_str().into(),
                    instance.into(),
                    tenant.to_string().into(),
                ],
            )
            .await
            .first()
            .map(|r| Row {
                status: r.try_get("", "status").unwrap(),
                deliveries: r.try_get("", "deliveries").unwrap(),
                change_ref: r.try_get("", "change_ref").unwrap(),
                expected_value: r.try_get("", "expected_value").unwrap(),
                silent_since: r.try_get("", "silent").unwrap(),
                notified: r.try_get("", "notified").unwrap(),
            })
    }

    pub async fn status(&self, instance: Uuid, tenant: Uuid) -> String {
        self.row(instance, tenant)
            .await
            .map_or_else(|| "none".to_owned(), |r| r.status)
    }

    pub async fn sub(&self, instance: Uuid, tenant: Uuid) -> Option<Sub> {
        self.pg
            .rows(
                "SELECT (silent_since IS NOT NULL) AS silent, instance_label
                   FROM subscriptions WHERE key = $1 AND instance_id = $2 AND tenant = $3",
                vec![
                    self.key.as_str().into(),
                    instance.into(),
                    tenant.to_string().into(),
                ],
            )
            .await
            .first()
            .map(|r| Sub {
                silent: r.try_get("", "silent").unwrap(),
                label: r.try_get("", "instance_label").unwrap(),
            })
    }

    pub async fn silent(&self, instance: Uuid, tenant: Uuid) -> bool {
        self.sub(instance, tenant)
            .await
            .expect("a subscription")
            .silent
    }

    /// How many states the key holds, at every tenant.
    pub async fn rows(&self) -> i64 {
        self.pg
            .count(
                "SELECT count(*) FROM activation_state WHERE key = $1",
                vec![self.key.as_str().into()],
            )
            .await
    }

    /// The instances holding a state of the key at `tenant`.
    pub async fn holders(&self, tenant: Uuid) -> Vec<Uuid> {
        let mut holders: Vec<Uuid> = self
            .pg
            .rows(
                "SELECT instance_id FROM activation_state WHERE key = $1 AND tenant = $2",
                vec![self.key.as_str().into(), tenant.to_string().into()],
            )
            .await
            .iter()
            .map(|r| r.try_get::<Uuid>("", "instance_id").unwrap())
            .collect();
        holders.sort();
        holders
    }

    /// How many subscriptions the key holds, at every tenant.
    pub async fn subs(&self) -> i64 {
        self.pg
            .count(
                "SELECT count(*) FROM subscriptions WHERE key = $1",
                vec![self.key.as_str().into()],
            )
            .await
    }

    /// The `activation` records of one instance, oldest first.
    pub async fn records(&self, instance: Uuid) -> Vec<Record> {
        self.pg
            .rows(
                "SELECT outcome, change_ref::text AS change_ref, tenant_id::text AS tenant
                   FROM audit_records
                  WHERE declaration_key = $1 AND operation = 'activation' AND instance_id = $2
                  ORDER BY occurred_at, id",
                vec![self.key.as_str().into(), instance.into()],
            )
            .await
            .iter()
            .map(|r| {
                (
                    r.try_get("", "outcome").unwrap(),
                    r.try_get("", "change_ref").unwrap(),
                    r.try_get("", "tenant").unwrap(),
                )
            })
            .collect()
    }

    /// The outcomes of one instance's records.
    pub async fn outcomes(&self, instance: Uuid) -> Vec<String> {
        self.records(instance)
            .await
            .into_iter()
            .map(|r| r.0)
            .collect()
    }

    /// Every `activation` record of the key.
    pub async fn all_records(&self) -> i64 {
        self.pg
            .count(
                "SELECT count(*) FROM audit_records
                  WHERE declaration_key = $1 AND operation = 'activation'",
                vec![self.key.as_str().into()],
            )
            .await
    }

    /// The `change_notification`s of the broker double that name the key.
    pub fn notified(&self) -> usize {
        self.platform.broker.notifications().len()
    }

    /// The subscription of `instance` has been silent for `hours`.
    pub async fn silent_for(&self, instance: Uuid, tenant: Uuid, hours: i32) {
        let changed = self
            .pg
            .exec(
                "UPDATE subscriptions SET silent_since = now() - make_interval(hours => $4)
                  WHERE key = $1 AND instance_id = $2 AND tenant = $3 AND silent_since IS NOT NULL",
                vec![
                    self.key.as_str().into(),
                    instance.into(),
                    tenant.to_string().into(),
                    hours.into(),
                ],
            )
            .await;
        assert_eq!(changed, 1, "a silent subscription to age");
    }

    /// Hours since the subscription's `silent_since`, rounded down.
    pub async fn silent_hours(&self, instance: Uuid, tenant: Uuid) -> i64 {
        self.pg
            .count(
                "SELECT floor(extract(epoch FROM now() - silent_since) / 3600)::bigint
                   FROM subscriptions WHERE key = $1 AND instance_id = $2 AND tenant = $3",
                vec![
                    self.key.as_str().into(),
                    instance.into(),
                    tenant.to_string().into(),
                ],
            )
            .await
    }
}

/// Assert that `handle` has not finished within [`STILL_WAITING`].
pub async fn still_waiting<T>(handle: &mut tokio::task::JoinHandle<T>) {
    assert!(
        tokio::time::timeout(STILL_WAITING, &mut *handle)
            .await
            .is_err(),
        "the operation finished while its rival was held"
    );
}

/// A held task's result, within [`MUST_HAPPEN`].
pub async fn finished<T>(handle: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(MUST_HAPPEN, handle)
        .await
        .expect("the operation finished once its rival was released")
        .expect("the task did not panic")
}

/// Wait for a hold to be reached.
pub async fn reached(hold: &cf_gears_settings_service::infra::storage::activation_hold::Hold) {
    tokio::time::timeout(MUST_HAPPEN, hold.arrived())
        .await
        .expect("a writer reached the hold");
}

/// A pause point that holds the first change of one key until released: the
/// coordinator's own, between the candidate step and the commit.
pub struct WritePause {
    target: Mutex<Option<String>>,
    reached: Notify,
    proceed: Notify,
}

impl WritePause {
    pub fn install(rig: &Rig) -> Arc<Self> {
        let pause = Arc::new(Self {
            target: Mutex::new(Some(rig.key.as_str().to_owned())),
            reached: Notify::new(),
            proceed: Notify::new(),
        });
        let this = Arc::clone(&pause);
        let point: PausePoint = Arc::new(move |key: &str| {
            let this = Arc::clone(&this);
            let key = key.to_owned();
            Box::pin(async move {
                let hold = {
                    let mut target = this.target.lock().unwrap();
                    if target.as_deref() == Some(key.as_str()) {
                        target.take();
                        true
                    } else {
                        false
                    }
                };
                if hold {
                    this.reached.notify_one();
                    this.proceed.notified().await;
                }
            })
        });
        rig.replica
            .gear
            .writes()
            .expect("initialized")
            .set_pause_point(Some(point));
        pause
    }

    pub async fn reached(&self) {
        tokio::time::timeout(MUST_HAPPEN, self.reached.notified())
            .await
            .expect("a change reached the pause point");
    }

    pub fn release(&self) {
        self.proceed.notify_one();
    }
}

/// A write as a task, held by a [`WritePause`]: its own end-of-request publish
/// refused.
pub fn write_task(
    rig: &Rig,
    tenant: Uuid,
    value: &'static str,
    tag: String,
) -> tokio::task::JoinHandle<()> {
    let gear = Arc::clone(&rig.replica.gear);
    let key = rig.key.clone();
    rig.platform.broker.refuse_notifications(true);
    tokio::spawn(async move {
        gear.writes()
            .expect("initialized")
            .change(
                &Replica::actor(),
                &key,
                Some(tenant),
                cf_gears_settings_service::domain::writes::Change::Set(json!(value)),
                Some(&tag),
                cf_gears_settings_service::domain::ports::StepUpOperation::Set,
            )
            .await
            .expect("the held write commits");
    })
}

// ---------------------------------------------------------------------------
// The lock bench: the row-lock order of the multi-row statements
// ---------------------------------------------------------------------------

/// The planner settings of the bench's toolkit connections, URL-encoded into
/// the libpq `options` parameter: index scans and nested loops, no memoize.
/// On a few rows the planner would scan the heap, which visits every row in
/// one physical order whatever the statement; these settings make each
/// statement walk the index or the join it would take on a large table, so
/// statements that leave the lock order to their plan lock in different
/// orders, as there.
const BENCH_PLANNER: &str = "-c%20enable_seqscan%3Doff%20-c%20enable_hashjoin%3Doff\
                             %20-c%20enable_mergejoin%3Doff%20-c%20enable_memoize%3Doff";

/// One activation state's identity, `uq_activation_state`.
pub type Ident = cf_gears_settings_service::domain::activation::InstanceKey;

/// The identity of a state of `subscriber`'s instance `n` for `key` at `ROOT`.
pub fn ident(subscriber: &str, n: u128, key: &str) -> Ident {
    Ident {
        subscriber: subscriber.to_owned(),
        instance_id: instance(n),
        key: key.to_owned(),
        tenant: ROOT.to_string(),
    }
}

/// A bare database with the gear's schema, the repository's statements run on
/// toolkit connections with [`BENCH_PLANNER`], and the rows put in place by
/// plain SQL, so the heap holds them in the order the test inserts them: the
/// order an index walk visits rows of equal index keys.
pub struct LockBench {
    pub pg: PgFixture,
    pub db: toolkit_db::DBProvider<toolkit_db::DbError>,
}

impl LockBench {
    pub async fn new(suite: &str) -> Option<Self> {
        let pg = pg_fixture(suite).await?;
        let url = format!("{}?options={BENCH_PLANNER}", pg.url);
        let db = toolkit_db::DBProvider::new(
            toolkit_db::connect_db(&url, super::platform::pool())
                .await
                .expect("the toolkit connects with the bench's planner settings"),
        );
        Some(Self { pg, db })
    }

    /// A subscription of `row`, inserted after every earlier one. Returns its
    /// id.
    pub async fn subscribe(&self, row: &Ident) -> Uuid {
        let id = Uuid::now_v7();
        self.pg
            .exec(
                "INSERT INTO subscriptions (id, subscriber, instance_id, key, tenant)
                 VALUES ($1, $2, $3, $4, $5)",
                vec![
                    id.into(),
                    row.subscriber.clone().into(),
                    row.instance_id.into(),
                    row.key.clone().into(),
                    row.tenant.clone().into(),
                ],
            )
            .await;
        id
    }

    /// An `awaiting` state of `row` for `change`, never published, inserted
    /// after every earlier one.
    pub async fn state(&self, row: &Ident, change: Uuid) {
        self.pg
            .exec(
                "INSERT INTO activation_state
                        (subscriber, instance_id, key, tenant, change_ref, changed_at, expected_value)
                 VALUES ($1, $2, $3, $4, $5, now(), '\"v\"')",
                vec![
                    row.subscriber.clone().into(),
                    row.instance_id.into(),
                    row.key.clone().into(),
                    row.tenant.clone().into(),
                    change.into(),
                ],
            )
            .await;
        self.pg
            .exec("ANALYZE subscriptions, activation_state", vec![])
            .await;
    }

    /// The due row of `row`'s state, as the scan reads it: `change`, never
    /// published.
    pub fn due(row: &Ident, change: Uuid) -> DueRow {
        DueRow {
            pair: SubscriberTenant {
                subscriber: row.subscriber.clone(),
                tenant: row.tenant.clone(),
            },
            instance_id: row.instance_id,
            change: NotifiedChange {
                key: row.key.clone(),
                change_ref: change,
            },
            deliveries: 0,
        }
    }

    /// A transaction of another writer holding `row`'s lock — a report or a
    /// call of silence in flight — until the test commits it.
    pub async fn hold(&self, row: &Ident) -> sea_orm::DatabaseTransaction {
        use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
        let tx = self.pg.conn.begin().await.expect("a transaction");
        let held = tx
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "UPDATE activation_state SET detail = detail
                  WHERE subscriber = $1 AND instance_id = $2 AND key = $3 AND tenant = $4",
                vec![
                    row.subscriber.clone().into(),
                    row.instance_id.into(),
                    row.key.clone().into(),
                    row.tenant.clone().into(),
                ],
            ))
            .await
            .expect("the holder's update runs")
            .rows_affected();
        assert_eq!(held, 1, "the holder holds one row");
        tx
    }

    /// Wait until `n` sessions of the database wait on a lock.
    pub async fn blocked(&self, n: i64) {
        let waiting = || async {
            self.pg
                .count(
                    "SELECT count(*) FROM pg_stat_activity
                      WHERE datname = current_database() AND wait_event_type = 'Lock'",
                    vec![],
                )
                .await
        };
        tokio::time::timeout(MUST_HAPPEN, async {
            while waiting().await != n {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{n} sessions wait on a lock"));
    }

    /// Whether another transaction holds `row`'s lock: a `FOR UPDATE NOWAIT`
    /// of it, from a transaction rolled back at once, cannot take it.
    pub async fn locked(&self, row: &Ident) -> bool {
        use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
        let tx = self.pg.conn.begin().await.expect("a transaction");
        let probe = tx
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT 1 FROM activation_state
                  WHERE subscriber = $1 AND instance_id = $2 AND key = $3 AND tenant = $4
                    FOR UPDATE NOWAIT",
                vec![
                    row.subscriber.clone().into(),
                    row.instance_id.into(),
                    row.key.clone().into(),
                    row.tenant.clone().into(),
                ],
            ))
            .await;
        tx.rollback().await.expect("the probe rolls back");
        match probe {
            Ok(rows) => {
                assert_eq!(rows.len(), 1, "the probed state exists");
                false
            }
            Err(err) if err.to_string().contains("could not obtain lock") => true,
            Err(err) => panic!("the probe failed: {err}"),
        }
    }

    /// Whether the state of `row` is still there.
    pub async fn exists(&self, row: &Ident) -> bool {
        self.pg
            .count(
                "SELECT count(*) FROM activation_state
                  WHERE subscriber = $1 AND instance_id = $2 AND key = $3 AND tenant = $4",
                vec![
                    row.subscriber.clone().into(),
                    row.instance_id.into(),
                    row.key.clone().into(),
                    row.tenant.clone().into(),
                ],
            )
            .await
            == 1
    }

    /// The `(change_ref, deliveries)` of `row`'s state.
    pub async fn stamp_of(&self, row: &Ident) -> (Uuid, i32) {
        let rows = self
            .pg
            .rows(
                "SELECT change_ref, deliveries FROM activation_state
                  WHERE subscriber = $1 AND instance_id = $2 AND key = $3 AND tenant = $4",
                vec![
                    row.subscriber.clone().into(),
                    row.instance_id.into(),
                    row.key.clone().into(),
                    row.tenant.clone().into(),
                ],
            )
            .await;
        let row = rows.first().expect("the state exists");
        (
            row.try_get("", "change_ref").unwrap(),
            row.try_get("", "deliveries").unwrap(),
        )
    }
}
