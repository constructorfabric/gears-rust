#![cfg(feature = "integration")]
// The doubles, the fixture and the polls are plain helpers, which
// `allow-expect-in-tests` does not reach; a failed expectation there is the
// test failing, as in the sibling PostgreSQL suites.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Consumer activation, the state half, on a real `PostgreSQL`: the key's
//! mutation lock between replicas and the activation state a change records in
//! its own transaction — the Level 2 rows of DESIGN-activation §7 that belong to
//! it.
//!
//! Each replica is a whole `SettingsService`: `init` over its own `ClientHub`
//! and its own connection pool — so its own advisory-lock session — over the one
//! database, which is what makes the lock the platform's and not the process's.
//! What the test hosts is the platform around them: the Types Registry (serves
//! the value types, accepts what is registered), the Tenant Resolver (a root, a
//! child and a grandchild), the Credential Store (an in-memory one that keeps
//! what it is given) and a policy decision point that allows everything.
//!
//! The concurrent rows run on separate committed connections. A write is held in
//! flight by the coordinator's test-only pause point, between its candidate step
//! and its commit; the assertions are on outcomes and never on how long anything
//! took, and `pg_stat_database.deadlocks` is asserted unchanged.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_activation_state_test
//! ```

mod support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cf_gears_settings_service::SettingsService;
use cf_gears_settings_service::domain::access::{AccessActor, AccessReadout, TenantAccess};
use cf_gears_settings_service::domain::activation::{
    KeyLocks, LockMetrics, Subscription, secret_hash, tenant_text,
};
use cf_gears_settings_service::domain::category::DomainVisibility;
use cf_gears_settings_service::domain::error::DomainError;
use cf_gears_settings_service::domain::ports::StepUpOperation;
use cf_gears_settings_service::domain::stepup::USER_SUBJECT_TYPE;
use cf_gears_settings_service::domain::writes::{Change, Committed, WriteActor};
use cf_gears_settings_service::infra::mutation_lock::{
    LockSchedule, MutationLock, clear_startup_markers,
};
use cf_gears_settings_service::infra::value_writes::{BatchChange, PausePoint};
use credstore_sdk::{
    CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef, SecretType, SecretValue,
    SharingMode, TenantId as StoreTenantId, WriteOptions, WritePrecondition,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement, Value as SeaValue};
use secrecy::SecretString;
use serde_json::{Value, json};
use settings_service_sdk::SettingKey;
use settings_service_sdk::api::SettingsContributionClient;
use settings_service_sdk::models::{ContributedDeclaration, ScopeClass};
use tenant_resolver_sdk::TenantResolverClient;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use toolkit::{ClientHub, Gear, GearCtx};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

use support::platform::{
    AllowAll, CHILD, CONSUMER_INSTANCE, FixedConfig, ROOT, RegistryDouble, SECRET_TYPE,
    STRING_TYPE, TenantTree,
};

const GEAR: &str = "settings-service";
const GRAND: Uuid = Uuid::from_u128(0xc2);
/// The tenants of the per-key bound test: this many children of the root, each
/// `BOUND_TENANT_BASE + n`.
const BOUND_TENANTS: u128 = 50;
const BOUND_TENANT_BASE: u128 = 0xe000;
/// The subscribers of the per-key bound test.
const BOUND_SUBSCRIBERS: u128 = 20;

/// How long the test waits for something that must happen: far beyond any real
/// wait, so only a hang can exhaust it.
const MUST_HAPPEN: Duration = Duration::from_secs(20);
/// How long the test watches for something that must *not* have happened yet.
const STILL_WAITING: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------------------
// PostgreSQL fixture
// ---------------------------------------------------------------------------

struct PgFixture {
    _container: support::Container,
    url: String,
    /// A plain connection for what the tests assert and move behind the gears'
    /// backs.
    conn: DatabaseConnection,
}

fn pool() -> ConnectOpts {
    ConnectOpts {
        max_conns: Some(8),
        min_conns: Some(1),
        ..Default::default()
    }
}

/// A container with the gear's schema applied once, as a stand's first start
/// applies it.
async fn pg_fixture() -> Option<PgFixture> {
    let container = support::start("activation").await?;
    let url = container.url.clone();
    let migrating = connect_db(&url, pool())
        .await
        .expect("the toolkit connects to the container");
    run_migrations_for_testing(
        &migrating,
        <cf_gears_settings_service::infra::storage::migrations::Migrator as sea_orm_migration::MigratorTrait>::migrations(),
    )
    .await
    .expect("the chain applies");
    let conn = Database::connect(&url)
        .await
        .expect("a plain connection to the container");
    Some(PgFixture {
        _container: container,
        url,
        conn,
    })
}

impl PgFixture {
    async fn rows(&self, sql: &str, values: Vec<SeaValue>) -> Vec<sea_orm::QueryResult> {
        self.conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                sql,
                values,
            ))
            .await
            .expect("the query runs")
    }

    async fn exec(&self, sql: &str, values: Vec<SeaValue>) -> u64 {
        self.conn
            .execute_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                sql,
                values,
            ))
            .await
            .expect("the statement runs")
            .rows_affected()
    }

    async fn count(&self, sql: &str, values: Vec<SeaValue>) -> i64 {
        self.rows(sql, values).await[0]
            .try_get_by_index::<i64>(0)
            .expect("a count")
    }

    /// The deadlocks the server has detected in this database so far.
    async fn deadlocks(&self) -> i64 {
        self.count(
            "SELECT deadlocks::bigint FROM pg_stat_database WHERE datname = current_database()",
            vec![],
        )
        .await
    }

    /// The activation states of one key: `(subscriber, tenant, expected_value,
    /// change_ref, status)`, by subscriber and tenant.
    async fn states(&self, key: &SettingKey) -> Vec<(String, String, String, String, String)> {
        self.rows(
            "SELECT subscriber, tenant, expected_value, change_ref::text AS change_ref, status
               FROM activation_state WHERE key = $1 ORDER BY subscriber, tenant",
            vec![key.as_str().into()],
        )
        .await
        .iter()
        .map(|row| {
            (
                row.try_get::<String>("", "subscriber").unwrap(),
                row.try_get::<String>("", "tenant").unwrap(),
                row.try_get::<String>("", "expected_value").unwrap(),
                row.try_get::<String>("", "change_ref").unwrap(),
                row.try_get::<String>("", "status").unwrap(),
            )
        })
        .collect()
    }

    async fn subscriptions(&self, key: &SettingKey) -> i64 {
        self.count(
            "SELECT count(*) FROM subscriptions WHERE key = $1",
            vec![key.as_str().into()],
        )
        .await
    }

    async fn values(&self, key: &SettingKey) -> i64 {
        self.count(
            "SELECT count(*) FROM setting_values v
               JOIN setting_declarations d ON d.id = v.declaration_id WHERE d.key = $1",
            vec![key.as_str().into()],
        )
        .await
    }

    /// The id of the newest audit record of `key` at `tenant`.
    async fn newest_record(&self, key: &SettingKey, tenant: Uuid) -> String {
        self.rows(
            "SELECT id::text AS id FROM audit_records
              WHERE declaration_key = $1 AND tenant_id = $2 AND operation <> 'secret_use'
              ORDER BY occurred_at DESC, id DESC LIMIT 1",
            vec![key.as_str().into(), tenant.into()],
        )
        .await[0]
            .try_get::<String>("", "id")
            .unwrap()
    }

    /// The state tag of a row as a client reads it back: its `last_change_at` in
    /// nanoseconds.
    async fn row_tag(&self, key: &SettingKey, tenant: Uuid) -> String {
        let micros = self
            .count(
                "SELECT floor(extract(epoch FROM v.last_change_at) * 1000000)::bigint
                   FROM setting_values v
                   JOIN setting_declarations d ON d.id = v.declaration_id
                  WHERE d.key = $1 AND v.tenant_id = $2",
                vec![key.as_str().into(), tenant.into()],
            )
            .await;
        (i128::from(micros) * 1000).to_string()
    }

    async fn secret_uses(&self, key: &SettingKey) -> i64 {
        self.count(
            "SELECT count(*) FROM audit_records WHERE declaration_key = $1 AND operation = 'secret_use'",
            vec![key.as_str().into()],
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// The platform around the gears
// ---------------------------------------------------------------------------

/// The hierarchy fixture: the root, a child under it and a grandchild, and the
/// fan-out family of the per-key bound test: children of the root.
const THREE_LEVELS: TenantTree = TenantTree::new(
    |id| match id {
        ROOT => Some(None),
        CHILD => Some(Some(ROOT)),
        GRAND => Some(Some(CHILD)),
        other
            if (BOUND_TENANT_BASE..BOUND_TENANT_BASE + BOUND_TENANTS)
                .contains(&other.as_u128()) =>
        {
            Some(Some(ROOT))
        }
        _ => None,
    },
    &[CHILD, GRAND],
);

/// A Credential Store that keeps what it is given.
#[derive(Default)]
struct MemoryCredStore {
    entries: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryCredStore {
    fn response(value: Vec<u8>) -> GetSecretResponse {
        GetSecretResponse {
            value: SecretValue::new(value),
            id: Uuid::nil(),
            owner_tenant_id: StoreTenantId::nil(),
            sharing: SharingMode::default(),
            is_inherited: false,
            version: 1,
            secret_type: SecretType::generic().gts_id().to_owned(),
            expires_at: None,
        }
    }
}

#[async_trait]
impl CredStoreClientV1 for MemoryCredStore {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .get(key.as_ref())
            .cloned()
            .map(Self::response))
    }

    async fn put_opts(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        _sharing: SharingMode,
        _precondition: WritePrecondition,
        _opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        self.entries
            .lock()
            .unwrap()
            .insert(key.as_ref().to_owned(), value.as_bytes().to_vec());
        Ok(())
    }

    async fn create_opts(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        _sharing: SharingMode,
        _opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        self.entries
            .lock()
            .unwrap()
            .insert(key.as_ref().to_owned(), value.as_bytes().to_vec());
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
        _precondition: WritePrecondition,
    ) -> Result<(), CredStoreError> {
        self.entries.lock().unwrap().remove(key.as_ref());
        Ok(())
    }
}

struct Platform {
    registry: Arc<RegistryDouble>,
    store: Arc<MemoryCredStore>,
}

fn platform() -> Platform {
    Platform {
        registry: RegistryDouble::with_secret_type(),
        store: Arc::new(MemoryCredStore::default()),
    }
}

// ---------------------------------------------------------------------------
// A replica
// ---------------------------------------------------------------------------

struct Replica {
    gear: Arc<SettingsService>,
    hub: Arc<ClientHub>,
    db: Arc<DBProvider<DbError>>,
}

impl Replica {
    /// `init` over the shared database with `config` as the gear's section.
    async fn init(pg: &PgFixture, platform: &Platform, config: Value) -> Self {
        let hub = Arc::new(ClientHub::new());
        hub.register::<dyn TypesRegistryClient>(
            Arc::clone(&platform.registry) as Arc<dyn TypesRegistryClient>
        );
        hub.register::<dyn CredStoreClientV1>(
            Arc::clone(&platform.store) as Arc<dyn CredStoreClientV1>
        );
        hub.register::<dyn TenantResolverClient>(Arc::new(THREE_LEVELS));
        hub.register::<dyn authz_resolver_sdk::AuthZResolverApi>(Arc::new(AllowAll));
        let db = connect_db(&pg.url, pool())
            .await
            .expect("the replica connects to the container");
        let provider: DBProvider<DbError> = DBProvider::new(db);
        let ctx = GearCtx::new(
            GEAR,
            Uuid::new_v4(),
            Arc::new(FixedConfig(json!({ "config": config }))),
            Arc::clone(&hub),
            CancellationToken::new(),
        )
        .with_db(provider.clone());
        let gear = Arc::new(SettingsService::default());
        gear.init(&ctx).await.expect("init over a live context");
        Self {
            gear,
            hub,
            db: Arc::new(provider),
        }
    }

    async fn declare(&self, key: &SettingKey, class: ScopeClass, type_id: &str, default: &str) {
        let contributions = self
            .hub
            .get::<dyn SettingsContributionClient>()
            .expect("the contribution door is bound");
        let mut declaration =
            ContributedDeclaration::new(key.clone(), type_id, json!(default), class);
        declaration.requires_step_up = Some(false);
        let result = contributions
            .register_declarations(
                &SecurityContext::anonymous(),
                "activation-state-test".to_owned(),
                vec![declaration],
            )
            .await
            .expect("the declaration registers");
        assert!(result.errors.is_empty(), "refused: {:?}", result.errors);
    }

    async fn declare_plain(&self, key: &SettingKey, class: ScopeClass) {
        self.declare(key, class, STRING_TYPE, "default").await;
    }

    async fn declare_secret(&self, key: &SettingKey) {
        self.declare(key, ScopeClass::Cascading, SECRET_TYPE, "")
            .await;
    }

    fn actor() -> WriteActor {
        WriteActor {
            ctx: SecurityContext::builder()
                .subject_id(Uuid::new_v4())
                .subject_tenant_id(ROOT)
                .subject_type(USER_SUBJECT_TYPE)
                .build()
                .expect("a context"),
            request_id: "activation-state-test".to_owned(),
            step_up_token: None,
            visibility: DomainVisibility::Unrestricted,
        }
    }

    /// A single change through this replica's coordinator.
    async fn change(
        &self,
        key: &SettingKey,
        tenant: Uuid,
        change: Change,
        if_match: Option<&str>,
    ) -> Result<Committed, DomainError> {
        self.gear
            .writes()
            .expect("initialized")
            .change(
                &Self::actor(),
                key,
                Some(tenant),
                change,
                if_match,
                StepUpOperation::Set,
            )
            .await
    }

    async fn set(
        &self,
        key: &SettingKey,
        tenant: Uuid,
        value: &str,
        if_match: Option<&str>,
    ) -> Result<Committed, DomainError> {
        self.change(key, tenant, Change::Set(json!(value)), if_match)
            .await
    }

    async fn register(
        &self,
        subscriber: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<Subscription, DomainError> {
        self.gear
            .subscriptions()
            .expect("initialized")
            .register_subscription(subscriber, key, tenant, Some(CONSUMER_INSTANCE), None)
            .await
    }

    async fn unsubscribe(
        &self,
        subscriber: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<(), DomainError> {
        self.gear
            .subscriptions()
            .expect("initialized")
            .unsubscribe(subscriber, key, tenant, Some(CONSUMER_INSTANCE))
            .await
    }

    /// Hold this replica's next change of `key` at the pause point.
    fn pause_on(&self, key: &SettingKey) -> Arc<Pause> {
        let pause = Pause::new(key);
        self.gear
            .writes()
            .expect("initialized")
            .set_pause_point(Some(pause.point()));
        pause
    }

    /// A restriction change, as the surface runs it: its checks, then the key's
    /// lock around its transaction.
    async fn restrict(
        &self,
        key: &SettingKey,
        target: Uuid,
        access: TenantAccess,
    ) -> Result<AccessReadout, DomainError> {
        let access_service = self.gear.access().expect("initialized");
        let actor = AccessActor {
            ctx: Self::actor().ctx,
            request_id: "activation-state-test".to_owned(),
            visibility: DomainVisibility::Unrestricted,
        };
        let chains = access_service
            .authorized_hierarchy_chains(&actor, target)
            .await?;
        {
            let conn = self.db.conn().expect("connection");
            access_service
                .precheck_set(&conn, &actor, &chains, key, target, access)
                .await?;
        }
        access_service
            .locked(key, || {
                let service = Arc::clone(&access_service);
                let db = Arc::clone(&self.db);
                let (actor, key) = (actor.clone(), key.clone());
                async move {
                    db.db()
                        .transaction_ref_mapped::<_, AccessReadout, DomainError>(move |tx| {
                            Box::pin(async move {
                                service
                                    .set(tx, &actor, &chains, &key, target, access, Some("absent"))
                                    .await
                            })
                        })
                        .await
                }
            })
            .await
    }
}

/// A subscriber: a gear's own security context.
fn subscriber() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(ROOT)
        .subject_type("gts.cf.core.security.subject_service.v1~")
        .build()
        .expect("a context")
}

fn setting(name: &str) -> SettingKey {
    SettingKey::contributed("cf", "demo", "activation", name, std::num::NonZeroU32::MIN)
        .expect("a valid key")
}

/// A pause point that holds the first change of one key until released.
struct Pause {
    target: Mutex<Option<String>>,
    reached: Notify,
    proceed: Notify,
    held: AtomicUsize,
}

impl Pause {
    fn new(key: &SettingKey) -> Arc<Self> {
        Arc::new(Self {
            target: Mutex::new(Some(key.as_str().to_owned())),
            reached: Notify::new(),
            proceed: Notify::new(),
            held: AtomicUsize::new(0),
        })
    }

    fn point(self: &Arc<Self>) -> PausePoint {
        let this = Arc::clone(self);
        Arc::new(move |key: &str| {
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
                    this.held.fetch_add(1, Ordering::SeqCst);
                    this.reached.notify_one();
                    this.proceed.notified().await;
                }
            })
        })
    }

    /// Wait until a change is held at the pause point.
    async fn reached(&self) {
        tokio::time::timeout(MUST_HAPPEN, self.reached.notified())
            .await
            .expect("a change reached the pause point");
    }

    /// Let the held change go on to its transaction.
    fn release(&self) {
        self.proceed.notify_one();
    }
}

/// Assert that `handle` has not finished within [`STILL_WAITING`].
async fn still_waiting<T>(handle: &mut tokio::task::JoinHandle<T>) {
    assert!(
        tokio::time::timeout(STILL_WAITING, &mut *handle)
            .await
            .is_err(),
        "the operation finished while the write was held in flight"
    );
}

async fn finished<T>(handle: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(MUST_HAPPEN, handle)
        .await
        .expect("the operation finished once the write was released")
        .expect("the task did not panic")
}

/// A lock holder of its own, on its own connection, standing for another
/// replica or an operator holding a key.
async fn another_holder(pg: &PgFixture, budget: Duration) -> MutationLock {
    let db = connect_db(&pg.url, pool())
        .await
        .expect("another session connects");
    MutationLock::new(Arc::new(DBProvider::new(db)), budget, Arc::new(NoMetrics))
}

struct NoMetrics;

impl LockMetrics for NoMetrics {
    fn acquired(&self, _wait: Duration) {}
    fn exhausted(&self, _wait: Duration) {}
    fn error(&self) {}
}

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[derive(Default)]
struct CountingMetrics {
    acquired: Mutex<Vec<Duration>>,
    exhausted: Mutex<Vec<Duration>>,
    errors: AtomicUsize,
}

impl LockMetrics for CountingMetrics {
    fn acquired(&self, wait: Duration) {
        self.acquired.lock().unwrap().push(wait);
    }
    fn exhausted(&self, wait: Duration) {
        self.exhausted.lock().unwrap().push(wait);
    }
    fn error(&self) {
        self.errors.fetch_add(1, Ordering::SeqCst);
    }
}

fn one_second_budget() -> Value {
    json!({ "mutation_lock_wait_seconds": 1 })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// *Subscribe during a write*: the registration waits for the write's commit;
/// what it reads then is the committed value, and the next write writes the
/// subscription's state.
#[tokio::test]
async fn a_registration_during_a_write_waits_for_its_commit_and_the_next_write_records_the_state() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("subscribe");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let deadlocks = pg.deadlocks().await;
    let gear = subscriber();

    let pause = a.pause_on(&key);
    let write = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let key = key.clone();
        tokio::spawn(async move { a.set(&key, ROOT, "one", Some("absent")).await })
    };
    pause.reached().await;

    let mut registration = {
        let (key, gear) = (key.clone(), gear.clone());
        let b = Replica {
            gear: Arc::clone(&b.gear),
            hub: Arc::clone(&b.hub),
            db: Arc::clone(&b.db),
        };
        tokio::spawn(async move { b.register(&gear, &key, ROOT).await })
    };
    still_waiting(&mut registration).await;
    assert_eq!(
        pg.subscriptions(&key).await,
        0,
        "nothing registered while the write is in flight"
    );

    pause.release();
    let committed = finished(write).await.expect("the write commits");
    finished(registration)
        .await
        .expect("then the registration commits");
    assert_eq!(pg.subscriptions(&key).await, 1);
    assert!(
        pg.states(&key).await.is_empty(),
        "the write was over before the subscription existed: no state for it"
    );

    // The next write writes the subscription's state.
    a.set(&key, ROOT, "two", Some(&committed.etag))
        .await
        .expect("the next write");
    let states = pg.states(&key).await;
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].2, "\"two\"");
    assert_eq!(pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Unsubscribe during a write*: the unsubscribe waits, then deletes the subscription and
/// the state the write committed.
#[tokio::test]
async fn an_unsubscribe_during_a_write_waits_then_deletes_the_state_the_write_committed() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("retire");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let gear = subscriber();
    a.register(&gear, &key, ROOT).await.expect("registered");
    let deadlocks = pg.deadlocks().await;

    let pause = a.pause_on(&key);
    let write = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let key = key.clone();
        tokio::spawn(async move { a.set(&key, ROOT, "one", Some("absent")).await })
    };
    pause.reached().await;
    let mut unsubscribe = {
        let (key, gear) = (key.clone(), gear.clone());
        let b = Replica {
            gear: Arc::clone(&b.gear),
            hub: Arc::clone(&b.hub),
            db: Arc::clone(&b.db),
        };
        tokio::spawn(async move { b.unsubscribe(&gear, &key, ROOT).await })
    };
    still_waiting(&mut unsubscribe).await;
    assert_eq!(
        pg.subscriptions(&key).await,
        1,
        "still subscribed while the write is in flight"
    );

    pause.release();
    finished(write).await.expect("the write commits");
    finished(unsubscribe)
        .await
        .expect("the unsubscribe follows");

    assert_eq!(pg.subscriptions(&key).await, 0);
    assert!(
        pg.states(&key).await.is_empty(),
        "the unsubscribe deleted the state the write committed"
    );
    assert_eq!(pg.values(&key).await, 1, "the write itself stands");
    assert_eq!(pg.deadlocks().await, deadlocks);
}

/// *Two writes, one key*: two scopes of one key written concurrently commit one
/// after the other, and the later expectation is the value in force at its
/// commit — an ancestor secret write included.
#[tokio::test]
async fn two_writes_of_one_key_commit_one_after_the_other_and_the_later_expectation_is_in_force() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("two_writes");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let gear = subscriber();
    a.register(&gear, &key, CHILD).await.expect("registered");
    let deadlocks = pg.deadlocks().await;

    // The first write (the ancestor's) is held; the second (the child's own)
    // starts meanwhile on another replica and must wait for it.
    let pause = a.pause_on(&key);
    let first = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let key = key.clone();
        tokio::spawn(async move { a.set(&key, ROOT, "ancestor", Some("absent")).await })
    };
    pause.reached().await;
    let mut second = {
        let b = Replica {
            gear: Arc::clone(&b.gear),
            hub: Arc::clone(&b.hub),
            db: Arc::clone(&b.db),
        };
        let key = key.clone();
        tokio::spawn(async move { b.set(&key, CHILD, "own", Some("absent")).await })
    };
    still_waiting(&mut second).await;
    assert_eq!(pg.values(&key).await, 0, "neither write has committed");

    pause.release();
    finished(first).await.expect("the first commits");
    finished(second).await.expect("the second commits after it");

    let states = pg.states(&key).await;
    assert_eq!(states.len(), 1);
    assert_eq!(
        states[0].2, "\"own\"",
        "the later expectation is the value in force"
    );
    assert_eq!(
        states[0].3,
        pg.newest_record(&key, CHILD).await,
        "and it names the later change"
    );
    assert_eq!(pg.deadlocks().await, deadlocks);
}

/// *Two writes, one key*, with an ancestor secret write: a revert of the child's
/// override waits for the ancestor's secret write, and the fallback it fetches
/// is the secret that write committed.
#[tokio::test]
async fn a_revert_waits_for_an_ancestor_secret_write_and_hashes_the_secret_it_committed() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("ancestor_secret");
    a.declare_secret(&key).await;
    let root_v1 = a
        .set(&key, ROOT, "root-v1", Some("absent"))
        .await
        .expect("the root secret");
    let own = a
        .set(&key, CHILD, "child-own", Some("absent"))
        .await
        .expect("the child's own secret");
    let gear = subscriber();
    a.register(&gear, &key, CHILD).await.expect("registered");
    a.register(&gear, &key, GRAND).await.expect("registered");
    let deadlocks = pg.deadlocks().await;

    // The ancestor's secret is rewritten and held; the revert starts on another
    // replica and must wait for it.
    let pause = a.pause_on(&key);
    let rewrite = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let key = key.clone();
        tokio::spawn(async move { a.set(&key, ROOT, "root-v2", Some(&root_v1.etag)).await })
    };
    pause.reached().await;
    let mut revert = {
        let b = Replica {
            gear: Arc::clone(&b.gear),
            hub: Arc::clone(&b.hub),
            db: Arc::clone(&b.db),
        };
        let (key, etag) = (key.clone(), own.etag.clone());
        tokio::spawn(async move { b.change(&key, CHILD, Change::Revert, Some(&etag)).await })
    };
    still_waiting(&mut revert).await;
    assert_eq!(
        pg.secret_uses(&key).await,
        0,
        "no fallback was fetched while the write is in flight"
    );

    pause.release();
    finished(rewrite)
        .await
        .expect("the ancestor's write commits");
    finished(revert).await.expect("the revert commits after it");

    let expected = |tenant: Uuid| {
        secret_hash(
            key.as_str(),
            &tenant_text(tenant),
            &SecretString::from("root-v2".to_owned()),
        )
        .expect("hashes")
    };
    let states = pg.states(&key).await;
    let by_tenant: HashMap<String, String> =
        states.iter().map(|s| (s.1.clone(), s.2.clone())).collect();
    assert_eq!(
        by_tenant.get(&tenant_text(CHILD)),
        Some(&expected(CHILD)),
        "the fallback is the secret the ancestor's write committed: {states:?}"
    );
    assert_eq!(by_tenant.get(&tenant_text(GRAND)), Some(&expected(GRAND)));
    assert_ne!(
        expected(CHILD),
        expected(GRAND),
        "salted by the state's own tenant"
    );
    assert_eq!(
        pg.secret_uses(&key).await,
        1,
        "one fetch, one `secret_use` record"
    );
    assert_eq!(pg.deadlocks().await, deadlocks);
}

/// A batch `set` adopting a staged secret fetches the staged plaintext once,
/// under the lock and before the transaction, and records per subscription the
/// hash salted by that subscription's tenant.
#[tokio::test]
async fn a_batch_set_adopting_a_staged_secret_fetches_it_once_and_hashes_per_tenant() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("staged");
    a.declare_secret(&key).await;
    let gear = subscriber();
    for tenant in [ROOT, CHILD] {
        a.register(&gear, &key, tenant).await.expect("registered");
    }
    let writes = a.gear.writes().expect("initialized");
    // A staged secret belongs to the subject that staged it.
    let admin = Replica::actor();
    let pending = writes
        .stage_secret(&admin, &key, Some(ROOT), json!("staged-value"))
        .await
        .expect("staged");
    let before = pg.secret_uses(&key).await;

    let outcome = writes
        .batch(
            &admin,
            vec![BatchChange {
                key: key.clone(),
                tenant: Some(ROOT),
                op: None,
                value: Some(json!({ "pending_id": pending.id.to_string() })),
                if_match: Some("absent".to_owned()),
            }],
        )
        .await
        .expect("the batch runs");
    assert!(outcome.results[0].is_ok(), "{:?}", outcome.results[0]);

    assert_eq!(
        pg.secret_uses(&key).await,
        before + 1,
        "one fetch, one record"
    );
    let states = pg.states(&key).await;
    assert_eq!(states.len(), 2);
    for (_, tenant, expected, _, _) in &states {
        let want = secret_hash(
            key.as_str(),
            tenant,
            &SecretString::from("staged-value".to_owned()),
        )
        .expect("hashes");
        assert_eq!(expected, &want, "at {tenant}");
    }
    assert_eq!(
        outcome.activation.pairs().len(),
        2,
        "the pairs the request touched"
    );
}

/// *Lock wait exhausted*: a held lock and a short budget.
#[tokio::test]
async fn a_lock_wait_that_runs_out_answers_unavailable_with_nothing_stored_and_the_lock_stays_held()
{
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, one_second_budget()).await;
    let key = setting("exhausted");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let gear = subscriber();
    a.register(&gear, &key, ROOT).await.expect("registered");
    let holder = another_holder(&pg, Duration::from_secs(1)).await;
    let held = holder
        .acquire(key.as_str(), None)
        .await
        .expect("the key is free");

    let outcome = a.set(&key, ROOT, "one", Some("absent")).await;

    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{outcome:?}"
    );
    assert_eq!(pg.values(&key).await, 0, "nothing stored");
    assert!(pg.states(&key).await.is_empty());
    // Still its owner's: another attempt is refused, until the owner lets go.
    assert!(
        holder
            .acquire(key.as_str(), Some(Duration::ZERO))
            .await
            .is_err()
    );
    held.release().await;
    a.set(&key, ROOT, "one", Some("absent"))
        .await
        .expect("once the owner lets go the write goes through");
    assert_eq!(pg.states(&key).await.len(), 1);
}

/// *Per-item `Unavailable`*: a batch whose middle change's key is held.
#[tokio::test]
async fn a_batch_item_whose_key_is_held_is_unavailable_and_its_neighbours_commit() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, one_second_budget()).await;
    let keys = [
        setting("batch_one"),
        setting("batch_two"),
        setting("batch_three"),
    ];
    for key in &keys {
        a.declare_plain(key, ScopeClass::Cascading).await;
    }
    let holder = another_holder(&pg, Duration::from_secs(1)).await;
    let held = holder.acquire(keys[1].as_str(), None).await.expect("free");

    let outcome = a
        .gear
        .writes()
        .unwrap()
        .batch(
            &Replica::actor(),
            keys.iter()
                .map(|key| BatchChange {
                    key: key.clone(),
                    tenant: Some(ROOT),
                    op: None,
                    value: Some(json!("v")),
                    if_match: Some("absent".to_owned()),
                })
                .collect(),
        )
        .await
        .expect("the batch runs");

    assert!(outcome.results[0].is_ok());
    assert!(
        matches!(outcome.results[1], Err(DomainError::Unavailable { .. })),
        "{:?}",
        outcome.results[1]
    );
    assert!(
        outcome.results[2].is_ok(),
        "later changes are still attempted"
    );
    assert_eq!(pg.values(&keys[0]).await, 1);
    assert_eq!(pg.values(&keys[1]).await, 0, "nothing stored for the item");
    assert_eq!(pg.values(&keys[2]).await, 1);
    held.release().await;
}

/// *Restriction change against a write*: the restriction takes the same lock, so
/// it commits after the write that was in flight.
#[tokio::test]
async fn a_restriction_change_against_a_write_waits_and_commits_after_it() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("restriction");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let deadlocks = pg.deadlocks().await;

    let pause = a.pause_on(&key);
    let write = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let key = key.clone();
        tokio::spawn(async move { a.set(&key, CHILD, "one", Some("absent")).await })
    };
    pause.reached().await;
    let mut restriction = {
        let b = Replica {
            gear: Arc::clone(&b.gear),
            hub: Arc::clone(&b.hub),
            db: Arc::clone(&b.db),
        };
        let key = key.clone();
        tokio::spawn(async move { b.restrict(&key, CHILD, TenantAccess::ReadOnly).await })
    };
    still_waiting(&mut restriction).await;
    assert_eq!(
        pg.count("SELECT count(*) FROM tenant_permissions", vec![])
            .await,
        0,
        "the restriction waits for the write in flight"
    );

    pause.release();
    finished(write).await.expect("the write commits first");
    finished(restriction)
        .await
        .expect("the restriction follows");

    assert_eq!(pg.values(&key).await, 1);
    assert_eq!(
        pg.count("SELECT count(*) FROM tenant_permissions", vec![])
            .await,
        1
    );
    assert_eq!(pg.deadlocks().await, deadlocks);
}

/// *Different keys do not block*: a change of another key finishes while one is
/// held in flight.
#[tokio::test]
async fn a_change_of_another_key_does_not_wait_for_one_held_in_flight() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;
    let (held_key, free_key) = (setting("held"), setting("free"));
    a.declare_plain(&held_key, ScopeClass::Cascading).await;
    a.declare_plain(&free_key, ScopeClass::Cascading).await;
    let deadlocks = pg.deadlocks().await;

    let pause = a.pause_on(&held_key);
    let held = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let key = held_key.clone();
        tokio::spawn(async move { a.set(&key, ROOT, "one", Some("absent")).await })
    };
    pause.reached().await;

    // Completes while the other is held: a wait on its lock could not.
    tokio::time::timeout(MUST_HAPPEN, b.set(&free_key, ROOT, "two", Some("absent")))
        .await
        .expect("no wait on the other key's lock")
        .expect("the change of the free key commits");
    assert_eq!(pg.values(&free_key).await, 1);
    assert_eq!(
        pg.values(&held_key).await,
        0,
        "the held one has not committed"
    );

    pause.release();
    finished(held).await.expect("the held change commits");
    assert_eq!(pg.deadlocks().await, deadlocks);
}

/// *Unsubscribed subscription gets no row*: a subscription of the change's read set
/// deleted before the upsert; the other affected subscription gets its state.
#[tokio::test]
async fn a_subscription_deleted_before_the_upsert_gets_no_state_and_the_others_get_theirs() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("deleted_before_upsert");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let (kept, retired) = (subscriber(), subscriber());
    a.register(&kept, &key, ROOT).await.expect("registered");
    a.register(&retired, &key, ROOT).await.expect("registered");

    let pause = a.pause_on(&key);
    let write = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let key = key.clone();
        tokio::spawn(async move { a.set(&key, ROOT, "one", Some("absent")).await })
    };
    pause.reached().await;
    // Behind the lock's back, as a replica on an earlier build would.
    let removed = pg
        .exec(
            "DELETE FROM subscriptions WHERE key = $1 AND subscriber = $2",
            vec![key.as_str().into(), retired.subject_id().to_string().into()],
        )
        .await;
    assert_eq!(removed, 1);
    pause.release();
    finished(write).await.expect("the write commits");

    let states = pg.states(&key).await;
    assert_eq!(states.len(), 1, "{states:?}");
    assert_eq!(states[0].0, kept.subject_id().to_string());
}

/// *Lock released on rollback*: a change whose transaction rolls back leaves the
/// key's lock free for the next mutation.
#[tokio::test]
async fn a_rolled_back_change_releases_the_lock_for_the_next_mutation() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    // A one-second budget: a lock left held would fail the next write.
    let a = Replica::init(&pg, &platform, one_second_budget()).await;
    let key = setting("rollback");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let first = a
        .set(&key, ROOT, "one", Some("absent"))
        .await
        .expect("first");

    let pause = a.pause_on(&key);
    let stale = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let (key, etag) = (key.clone(), first.etag.clone());
        tokio::spawn(async move { a.set(&key, ROOT, "two", Some(&etag)).await })
    };
    pause.reached().await;
    // The row moves between the candidate step and the transaction.
    pg.exec(
        "UPDATE setting_values SET last_change_at = last_change_at + interval '1 second'
          WHERE declaration_id = (SELECT id FROM setting_declarations WHERE key = $1)",
        vec![key.as_str().into()],
    )
    .await;
    pause.release();
    let outcome = finished(stale).await;
    assert!(
        matches!(outcome, Err(DomainError::PreconditionFailed { .. })),
        "{outcome:?}"
    );

    let tag = pg.row_tag(&key, ROOT).await;
    a.set(&key, ROOT, "three", Some(&tag))
        .await
        .expect("the next mutation acquires the lock without waiting out a budget");
}

/// *`secret_use` kept on rollback*: the fetch's record stays when the change's
/// transaction rolls back, and nothing of the change is stored.
#[tokio::test]
async fn the_secret_use_record_of_a_fetch_stays_when_the_changes_transaction_rolls_back() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("secret_rollback");
    a.declare_secret(&key).await;
    a.set(&key, ROOT, "root-secret", Some("absent"))
        .await
        .expect("root");
    let own = a
        .set(&key, CHILD, "own-secret", Some("absent"))
        .await
        .expect("own");
    let gear = subscriber();
    a.register(&gear, &key, CHILD).await.expect("registered");

    let pause = a.pause_on(&key);
    let revert = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let (key, etag) = (key.clone(), own.etag.clone());
        tokio::spawn(async move { a.change(&key, CHILD, Change::Revert, Some(&etag)).await })
    };
    pause.reached().await;
    pg.exec(
        "UPDATE setting_values SET last_change_at = last_change_at + interval '1 second'
          WHERE tenant_id = $2
            AND declaration_id = (SELECT id FROM setting_declarations WHERE key = $1)",
        vec![key.as_str().into(), CHILD.into()],
    )
    .await;
    pause.release();
    let outcome = finished(revert).await;

    assert!(
        matches!(outcome, Err(DomainError::PreconditionFailed { .. })),
        "{outcome:?}"
    );
    assert_eq!(pg.secret_uses(&key).await, 1, "the fetch's record stays");
    assert_eq!(pg.values(&key).await, 2, "nothing of the change is stored");
    assert!(pg.states(&key).await.is_empty());
}

/// Each secret subscription gets the hash of its own tenant, in one
/// insert-select carrying a `CASE` over the tenants.
#[tokio::test]
async fn one_secret_change_writes_each_subscription_its_own_hash_on_postgresql() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("secret_hashes");
    a.declare_secret(&key).await;
    let (first, second) = (subscriber(), subscriber());
    for ctx in [&first, &second] {
        for tenant in [ROOT, CHILD, GRAND] {
            a.register(ctx, &key, tenant).await.expect("registered");
        }
    }

    a.set(&key, ROOT, "hunter2", Some("absent"))
        .await
        .expect("the secret");

    let states = pg.states(&key).await;
    assert_eq!(states.len(), 6);
    for (subscriber_id, tenant, expected, change_ref, status) in &states {
        let want = secret_hash(
            key.as_str(),
            tenant,
            &SecretString::from("hunter2".to_owned()),
        )
        .expect("hashes");
        assert_eq!(expected, &want, "{subscriber_id} at {tenant}");
        assert_eq!(status, "awaiting");
        assert_eq!(change_ref, &pg.newest_record(&key, ROOT).await);
    }
}

/// *Subscription idempotency*: registering the same triple twice keeps one row.
#[tokio::test]
async fn registering_the_same_subscriber_key_and_tenant_twice_keeps_one_row() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("idempotent");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let gear = subscriber();

    let first = a.register(&gear, &key, ROOT).await.expect("first");
    let second = b
        .register(&gear, &key, ROOT)
        .await
        .expect("from another replica");

    assert_eq!(first.id, second.id);
    assert_eq!(
        pg.subscriptions(&key).await,
        1,
        "uq_sub_subscriber_instance_key_tenant"
    );
}

/// *`unsubscribe()` against a held lock*: `Unavailable` with nothing deleted, and a
/// second call deletes the subscription once the lock is free.
#[tokio::test]
async fn an_unsubscribe_against_a_held_lock_is_unavailable_and_a_second_call_deletes() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, one_second_budget()).await;
    let key = setting("retire_held");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let gear = subscriber();
    a.register(&gear, &key, ROOT).await.expect("registered");
    let holder = another_holder(&pg, Duration::from_secs(1)).await;
    let held = holder.acquire(key.as_str(), None).await.expect("free");

    let refused = a.unsubscribe(&gear, &key, ROOT).await;
    assert!(
        matches!(refused, Err(DomainError::Unavailable { .. })),
        "{refused:?}"
    );
    assert_eq!(pg.subscriptions(&key).await, 1, "still subscribed");

    held.release().await;
    a.unsubscribe(&gear, &key, ROOT)
        .await
        .expect("a second call deletes it");
    assert_eq!(pg.subscriptions(&key).await, 0);
    let again = a.unsubscribe(&gear, &key, ROOT).await;
    assert!(
        matches!(again, Err(DomainError::NotFound { .. })),
        "{again:?}"
    );
}

/// A declaration lifecycle change takes no mutation lock: re-registering a
/// declaration with a changed description finishes while its key is held.
#[tokio::test]
async fn a_declaration_lifecycle_change_takes_no_lock_while_its_key_is_held() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, one_second_budget()).await;
    let key = setting("lifecycle");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let holder = another_holder(&pg, Duration::from_secs(1)).await;
    let held = holder.acquire(key.as_str(), None).await.expect("free");

    let contributions = a
        .hub
        .get::<dyn SettingsContributionClient>()
        .expect("the contribution door is bound");
    let mut changed = ContributedDeclaration::new(
        key.clone(),
        STRING_TYPE,
        json!("default"),
        ScopeClass::Cascading,
    );
    changed.requires_step_up = Some(false);
    changed.description = Some("a changed description".to_owned());
    let result = tokio::time::timeout(
        MUST_HAPPEN,
        contributions.register_declarations(
            &SecurityContext::anonymous(),
            "activation-state-test".to_owned(),
            vec![changed],
        ),
    )
    .await
    .expect("no wait on the key's lock")
    .expect("the reconcile pass runs");
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    held.release().await;
}

/// *Lock metrics*: an acquisition and an exhausted wait are counted and their
/// waits observed; a lock session lost while the lock is held is silent:
/// `toolkit-db` reports it as an already released lock, so the release returns
/// `Ok` with no `warn` and no `error` count (the residual DESIGN-activation §6
/// records).
#[tokio::test]
async fn the_lock_metrics_count_acquired_and_exhausted_and_a_lost_session_is_silent() {
    let Some(pg) = pg_fixture().await else { return };
    let logs = LogBuf::default();
    let _log_guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    let metrics = Arc::new(CountingMetrics::default());
    let db = connect_db(&pg.url, pool()).await.expect("connects");
    let lock = MutationLock::new(
        Arc::new(DBProvider::new(db)),
        Duration::from_millis(150),
        Arc::clone(&metrics) as Arc<dyn LockMetrics>,
    )
    .with_schedule(LockSchedule {
        base_ms: 1,
        factor: 5,
        cap: Duration::from_millis(30),
        jitter: false,
    });
    let key = setting("metrics");

    let held = lock.acquire(key.as_str(), None).await.expect("a free key");
    assert_eq!(metrics.acquired.lock().unwrap().len(), 1);
    // Another session finds it held and runs out its wait.
    let other = another_holder(&pg, Duration::from_millis(150)).await;
    assert!(other.acquire(key.as_str(), None).await.is_err());
    // Its own counter is its own: this lock's sink saw one acquisition only.
    assert!(metrics.exhausted.lock().unwrap().is_empty());
    let exhausted_here = lock.acquire(key.as_str(), None).await;
    assert!(exhausted_here.is_err());
    assert_eq!(metrics.exhausted.lock().unwrap().len(), 1);
    assert!(
        metrics.exhausted.lock().unwrap()[0] >= Duration::from_millis(100),
        "the wait is observed"
    );

    // The lock's session is lost while the lock is held.
    let sessions = pg
        .rows(
            "SELECT pid FROM pg_locks WHERE locktype = 'advisory' AND granted",
            vec![],
        )
        .await;
    for row in &sessions {
        let pid: i32 = row.try_get("", "pid").unwrap();
        pg.exec("SELECT pg_terminate_backend($1)", vec![pid.into()])
            .await;
    }
    // `toolkit-db` treats a lost session as a lock already released: the
    // release returns `Ok`, logs nothing above `debug`, and the gear counts no
    // `error`; the session's death freed the key.
    let errors_before = metrics.errors.load(Ordering::SeqCst);
    held.release().await;
    assert_eq!(
        metrics.errors.load(Ordering::SeqCst),
        errors_before,
        "a lost session is not counted"
    );
    assert!(
        !logs.text().contains("WARN"),
        "a lost session is not warned about: {}",
        logs.text()
    );
    other
        .acquire(key.as_str(), None)
        .await
        .expect("the dead session's lock is free for another")
        .release()
        .await;
}

/// On `PostgreSQL` start-up removes no marker: there are none, and a lock a live
/// session holds is not a marker.
#[tokio::test]
async fn start_up_removes_no_lock_marker_on_postgresql() {
    let Some(pg) = pg_fixture().await else { return };
    let db = connect_db(&pg.url, pool()).await.expect("connects");
    let provider = Arc::new(DBProvider::new(db));
    let lock = MutationLock::new(
        Arc::clone(&provider),
        Duration::from_millis(50),
        Arc::new(NoMetrics),
    );
    let key = setting("markers");
    let held = lock.acquire(key.as_str(), None).await.expect("free");

    let removed = clear_startup_markers(&provider, [key.as_str().to_owned()])
        .await
        .expect("a no-op");

    assert_eq!(removed, 0);
    assert!(
        lock.acquire(key.as_str(), Some(Duration::ZERO))
            .await
            .is_err(),
        "the live holder's lock is untouched"
    );
    held.release().await;
}

/// *Lock hold time at the per-key bound* (NFR verification mapping, DESIGN
/// Concurrency Testing): the key has the design's bound of subscriptions —
/// 20 subscribers × 50 tenants, 1,000 in all — and a cascading write at the root
/// affects every one. The write is held in flight at the pause point, a mutation
/// of its own queues on the key's lock, and once the write goes on, the wait that
/// mutation observed through its lock metrics (the instrument behind
/// `settings_mutation_lock_wait_seconds`) says how long the change held the key.
/// The time is reported; the assertions are on outcomes — every subscription got
/// its state, and the wait ended within the budget.
#[tokio::test]
async fn a_change_at_the_per_key_bound_holds_its_lock_for_a_measured_time() {
    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({ "mutation_lock_wait_seconds": 60 })).await;
    let key = setting("bound");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let deadlocks = pg.deadlocks().await;
    // 20 subscribers × 50 tenants, seeded the way registration would write them.
    let seeded = pg
        .exec(
            "INSERT INTO subscriptions (id, subscriber, instance_id, key, tenant)
             SELECT gen_random_uuid(), 'subscriber-' || s, gen_random_uuid(), $1,
                    '00000000-0000-0000-0000-' || lpad(to_hex($2::bigint + t), 12, '0')
               FROM generate_series(1, $3::bigint) AS s, generate_series(0, $4::bigint - 1) AS t",
            vec![
                key.as_str().into(),
                i64::try_from(BOUND_TENANT_BASE).unwrap().into(),
                i64::try_from(BOUND_SUBSCRIBERS).unwrap().into(),
                i64::try_from(BOUND_TENANTS).unwrap().into(),
            ],
        )
        .await;
    assert_eq!(seeded, 1000, "the per-key bound of subscriptions");

    let pause = a.pause_on(&key);
    let write = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let key = key.clone();
        tokio::spawn(async move { a.set(&key, ROOT, "bound", Some("absent")).await })
    };
    pause.reached().await;

    // Another mutation of the key queues on the lock and counts its wait.
    let metrics = Arc::new(CountingMetrics::default());
    let contender_db = connect_db(&pg.url, pool())
        .await
        .expect("another session connects");
    let contender = MutationLock::new(
        Arc::new(DBProvider::new(contender_db)),
        Duration::from_secs(60),
        Arc::clone(&metrics) as Arc<dyn LockMetrics>,
    );
    let queued = std::time::Instant::now();
    let name = key.as_str().to_owned();
    let mut waiting = tokio::spawn(async move {
        let held = contender
            .acquire(&name, None)
            .await
            .expect("the lock frees");
        held.release().await;
    });
    still_waiting(&mut waiting).await;
    let before_release = queued.elapsed();

    pause.release();
    finished(write)
        .await
        .expect("the change at the bound commits");
    finished(waiting).await;

    let waits = metrics.acquired.lock().unwrap().clone();
    assert_eq!(waits.len(), 1, "the queued mutation acquired the lock once");
    let waited = waits[0];
    // No lower bound against `before_release`: that was measured from before the
    // contender's `acquire` started, the wait from inside it, so the two clocks
    // do not compare.
    let hold_after_pause = waited.saturating_sub(before_release);
    eprintln!(
        "per-key bound: 1000 subscriptions; the change held the key {} ms after its pause point \
         (queued wait {} ms in all)",
        hold_after_pause.as_millis(),
        waited.as_millis()
    );
    assert!(
        waited < Duration::from_secs(60),
        "the wait ended within the budget: {waited:?}"
    );
    assert_eq!(
        pg.states(&key).await.len(),
        1000,
        "every subscription of the key got its state in the one transaction"
    );
    assert_eq!(pg.deadlocks().await, deadlocks, "no deadlock");
}

/// *Subscribe during a write*, through the SDK: the registration waits for the
/// write's commit, and the fresh read the client makes after it returns the
/// committed value — the one the consumer materializes from — while the next
/// write writes the subscription's state.
#[tokio::test]
async fn a_subscribe_during_a_write_reads_the_committed_value_fresh() {
    use settings_service_sdk::{
        ActivationOutcome, ChangeBundle, KeyState, SettingChangeHandler, SettingsActivationClient,
    };

    struct Quiet;
    #[async_trait]
    impl SettingChangeHandler for Quiet {
        async fn on_change(&self, _: ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)> {
            Vec::new()
        }
    }

    let Some(pg) = pg_fixture().await else { return };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;
    let key = setting("sdk_subscribe");
    a.declare_plain(&key, ScopeClass::Cascading).await;
    let gear = subscriber();
    let client = b
        .hub
        .get::<dyn SettingsActivationClient>()
        .expect("the client is bound beside the reader");

    let pause = a.pause_on(&key);
    let write = {
        let a = Replica {
            gear: Arc::clone(&a.gear),
            hub: Arc::clone(&a.hub),
            db: Arc::clone(&a.db),
        };
        let key = key.clone();
        tokio::spawn(async move { a.set(&key, ROOT, "one", Some("absent")).await })
    };
    pause.reached().await;
    let handle = client
        .subscribe(
            &gear,
            vec![key.clone()],
            ROOT.to_string(),
            Arc::new(Quiet) as Arc<dyn SettingChangeHandler>,
        )
        .await
        .expect("a handle at once");
    tokio::time::sleep(STILL_WAITING).await;
    assert!(
        matches!(handle.states()[&key], KeyState::Pending { .. }),
        "the registration waits for the write: the key is not registered yet"
    );
    assert_eq!(pg.subscriptions(&key).await, 0);

    pause.release();
    let committed = finished(write).await.expect("the write commits");
    tokio::time::timeout(MUST_HAPPEN, async {
        while !matches!(handle.states()[&key], KeyState::Registered { .. }) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the key registers once the write is over");
    let KeyState::Registered { value: Some(value) } = handle.states()[&key].clone() else {
        panic!("registered with its fresh value");
    };
    assert_eq!(
        value.value,
        json!("one"),
        "the fresh read after the registration returns the committed value"
    );
    assert!(
        pg.states(&key).await.is_empty(),
        "no state for a change that preceded it"
    );

    a.set(&key, ROOT, "two", Some(&committed.etag))
        .await
        .expect("the next write");
    assert_eq!(
        pg.states(&key).await.len(),
        1,
        "it writes the subscription's state"
    );
    drop(handle);
}
