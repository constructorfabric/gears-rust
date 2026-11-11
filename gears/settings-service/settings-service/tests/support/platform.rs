//! The platform around a replica of the gear, shared by the `PostgreSQL`
//! suites that run whole `SettingsService` replicas: a delivering Event Broker
//! double, a Types Registry that accepts what is registered, a two-level
//! Tenant Resolver, a policy decision point that allows everything, and the
//! `Replica` that wraps `init` and the managed lifecycle over one shared
//! database.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cf_gears_settings_service::SettingsService;
use cf_gears_settings_service::domain::activation::Subscription as Registered;
use cf_gears_settings_service::domain::category::DomainVisibility;
use cf_gears_settings_service::domain::error::DomainError;
use cf_gears_settings_service::domain::ports::StepUpOperation;
use cf_gears_settings_service::domain::stepup::USER_SUBJECT_TYPE;
use cf_gears_settings_service::domain::writes::{Change, Committed, WriteActor};
use cf_gears_settings_service::infra::storage::migrations::Migrator;
use chrono::Utc;
use event_broker_sdk::models::EventType;
use event_broker_sdk::{
    Ack, AssignedPartition, ConsumerGroup, ConsumerGroupId, ConsumerGroupKind, ConsumerGroupQuery,
    CreateConsumerGroupRequest, EventBrokerApi, EventBrokerError, EventV1, FrameStream,
    IngestOutcome, JoinRequest, Page, PartitionRange, ProducerCursors, ProducerId, ProducerMode,
    ResetScope, SeekPosition, SeekResult, Sequence, StoredPosition, Subscription,
    SubscriptionAssignment, SubscriptionId, Topic, TopicSegment, WireFrame,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement, Value as SeaValue};
use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use settings_service_sdk::SettingKey;
use settings_service_sdk::api::{SettingsContributionClient, SettingsReaderClient};
use settings_service_sdk::gts::{CACHE_INVALIDATE_EVENT_TYPE, CHANGE_NOTIFICATION_EVENT_TYPE};
use settings_service_sdk::models::{ContributedDeclaration, GetEffectiveRequest, ScopeClass};
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions,
    GetDescendantsResponse, GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantRef,
    TenantResolverClient, TenantResolverError, TenantStatus,
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;
use toolkit::contracts::RunnableCapability;
use toolkit::lifecycle::WithLifecycle;
use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::MockTypesRegistryClient;
use types_registry_sdk::{
    GtsInstance, GtsTypeId, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient,
};
use uuid::Uuid;

use super::event_broker_stubs::event_broker_impl;

const GEAR: &str = "settings-service";
pub const STRING_TYPE: &str = "gts.cf.core.settings.type_string.v1~";
pub const SECRET_TYPE: &str = "gts.cf.core.settings.type_secret_string.v1~";
pub const ROOT: Uuid = Uuid::from_u128(0xa0);
/// The instance id the harness registers and reports under: the one consumer
/// process these tests emulate. A test that needs two instances passes its own.
pub const CONSUMER_INSTANCE: Uuid = Uuid::from_u128(0x1457);
pub const CHILD: Uuid = Uuid::from_u128(0xc1);

/// How long a replica gets to join the cache topic.
pub const JOIN_DEADLINE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// PostgreSQL fixture
// ---------------------------------------------------------------------------

pub struct PgFixture {
    pub _container: super::Container,
    pub url: String,
    /// A plain connection for what the tests assert and move behind the gears'
    /// backs.
    pub conn: DatabaseConnection,
}

pub fn pool() -> ConnectOpts {
    ConnectOpts {
        max_conns: Some(5),
        min_conns: Some(1),
        ..Default::default()
    }
}

/// A container with the gear's schema applied once, as a stand's first start
/// applies it; every replica then finds nothing outstanding.
pub async fn pg_fixture(suite: &str) -> Option<PgFixture> {
    let container = super::start(suite).await?;
    let url = container.url.clone();
    let db = connect_db(&url, pool())
        .await
        .expect("the toolkit connects to the container");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");
    let conn = Database::connect(&url)
        .await
        .expect("a plain connection to the container");
    Some(PgFixture {
        _container: container,
        url,
        conn,
    })
}

/// One activation state as the table holds it, for assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRow {
    pub subscriber: String,
    pub tenant: String,
    pub expected_value: String,
    pub change_ref: String,
    pub status: String,
    /// Whether `last_notified_at` is set.
    pub notified: bool,
}

impl PgFixture {
    pub async fn rows(&self, sql: &str, values: Vec<SeaValue>) -> Vec<sea_orm::QueryResult> {
        self.conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                sql,
                values,
            ))
            .await
            .expect("the query runs")
    }

    pub async fn exec(&self, sql: &str, values: Vec<SeaValue>) -> u64 {
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

    pub async fn count(&self, sql: &str, values: Vec<SeaValue>) -> i64 {
        self.rows(sql, values).await[0]
            .try_get_by_index::<i64>(0)
            .expect("a count")
    }

    /// The activation states of one key: `(subscriber, tenant, expected_value,
    /// change_ref, status, last_notified)`, by subscriber and tenant, the last
    /// being whether `last_notified_at` is set.
    pub async fn states(&self, key: &SettingKey) -> Vec<StateRow> {
        self.rows(
            "SELECT subscriber, tenant, expected_value, change_ref::text AS change_ref, status,
                    (last_notified_at IS NOT NULL) AS notified
               FROM activation_state WHERE key = $1 ORDER BY subscriber, tenant",
            vec![key.as_str().into()],
        )
        .await
        .iter()
        .map(|row| StateRow {
            subscriber: row.try_get("", "subscriber").unwrap(),
            tenant: row.try_get("", "tenant").unwrap(),
            expected_value: row.try_get("", "expected_value").unwrap(),
            change_ref: row.try_get("", "change_ref").unwrap(),
            status: row.try_get("", "status").unwrap(),
            notified: row.try_get("", "notified").unwrap(),
        })
        .collect()
    }

    /// When the state of `(subscriber, key, tenant)` was last stamped, as
    /// microseconds since the epoch; `None` while the stamp is absent.
    pub async fn stamp_micros(&self, key: &SettingKey, tenant: Uuid) -> Option<i64> {
        self.rows(
            "SELECT floor(extract(epoch FROM last_notified_at) * 1000000)::bigint AS at
               FROM activation_state WHERE key = $1 AND tenant = $2",
            vec![key.as_str().into(), tenant.to_string().into()],
        )
        .await
        .first()
        .and_then(|row| row.try_get::<Option<i64>>("", "at").unwrap())
    }

    /// The id of the newest audit record of `key` at `tenant`, a value change's.
    pub async fn newest_change(&self, key: &SettingKey, tenant: Uuid) -> String {
        self.rows(
            "SELECT id::text AS id FROM audit_records
              WHERE declaration_key = $1 AND tenant_id = $2
                AND operation NOT IN ('secret_use', 'activation')
              ORDER BY occurred_at DESC, id DESC LIMIT 1",
            vec![key.as_str().into(), tenant.into()],
        )
        .await[0]
            .try_get::<String>("", "id")
            .unwrap()
    }

    /// The `activation` audit records of `key`: `(tenant, actor, outcome,
    /// change_ref, detail)`.
    pub async fn activation_records(
        &self,
        key: &SettingKey,
    ) -> Vec<(String, String, String, String, Option<String>)> {
        self.rows(
            "SELECT tenant_id::text AS tenant, actor, outcome, change_ref::text AS change_ref, detail
               FROM audit_records WHERE declaration_key = $1 AND operation = 'activation'
              ORDER BY occurred_at, id",
            vec![key.as_str().into()],
        )
        .await
        .iter()
        .map(|row| {
            (
                row.try_get("", "tenant").unwrap(),
                row.try_get("", "actor").unwrap(),
                row.try_get("", "outcome").unwrap(),
                row.try_get("", "change_ref").unwrap(),
                row.try_get("", "detail").unwrap(),
            )
        })
        .collect()
    }

    /// The state tag of a value row as a client reads it back.
    pub async fn row_tag(&self, key: &SettingKey, tenant: Uuid) -> String {
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

    /// Advisory locks currently granted in this database: the scan lock and the
    /// keys' mutation locks held by any replica's session.
    pub async fn advisory_locks(&self) -> i64 {
        self.count(
            "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND granted",
            vec![],
        )
        .await
    }

    /// The deadlocks the server has detected in this database so far.
    pub async fn deadlocks(&self) -> i64 {
        self.count(
            "SELECT deadlocks::bigint FROM pg_stat_database WHERE datname = current_database()",
            vec![],
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// The delivering broker double
// ---------------------------------------------------------------------------

pub type FrameResult = Result<WireFrame, EventBrokerError>;

/// One interest of a joined subscription: the tenant it asked for, exactly, and
/// the event types it matches (none matches every type), as the broker matches.
pub struct Interest {
    pub tenant: Uuid,
    pub types: Vec<String>,
}

impl Interest {
    fn wants(&self, event: &EventV1) -> bool {
        self.tenant == event.tenant_id()
            && (self.types.is_empty() || self.types.iter().any(|t| *event.event_type() == **t))
    }
}

/// One joined subscription: where its deliveries go and what it asked for.
pub struct Joined {
    pub interests: Vec<Interest>,
    pub deliveries: UnboundedSender<FrameResult>,
}

/// What the broker keeps and what it was asked, behind one lock.
#[derive(Default)]
pub struct BrokerState {
    pub groups: Vec<ConsumerGroupId>,
    pub subscriptions: HashMap<SubscriptionId, Joined>,
    pub pending: HashMap<SubscriptionId, tokio::sync::mpsc::UnboundedReceiver<FrameResult>>,
    pub published: Vec<EventV1>,
    pub delivered: usize,
    pub creates: usize,
    pub joins: usize,
    pub leaves: usize,
    pub deletes: usize,
}

/// An in-memory Event Broker that delivers: `publish` hands the event, stamped
/// with its stored position as the consumer reads it, to the open stream of every joined group
/// whose interest names the event's tenant. One group is one replica, so every
/// replica sees every broadcast, which is the fan-out the settings service
/// relies on (the broker has no broadcast mode).
///
/// Matches `EventBrokerApi` of `event-broker-sdk/src/api.rs`: the calls the
/// consumer makes (`create_consumer_group`, `join`, `seek`, `stream`, `leave`,
/// `delete_consumer_group`) and `publish`, which refuses what the broker's
/// ingest refuses (an unpublishable or oversized event); the frame is
/// `WireFrame::Event` carrying the stored `EventV1` (`api.rs`, `WireFrame`).
pub struct DeliveringBroker {
    pub state: Mutex<BrokerState>,
    /// While set, `publish` is accepted and discarded: no replica receives it.
    pub drop_publishes: AtomicBool,
    /// Subscriptions with an open stream: what the join barrier waits on.
    pub streaming: tokio::sync::watch::Sender<usize>,
    pub stream_opens: AtomicUsize,
    /// While set, a `change_notification` is refused, as a missing `produce`
    /// grant on the activation topic would refuse it; every other event is
    /// accepted.
    pub refuse_notifications: AtomicBool,
    /// How many `change_notification` publishes were refused.
    pub refused: AtomicUsize,
    /// While armed, a `change_notification` parks in `publish` until the test
    /// opens the gate — the pause the delivery rows run a write against.
    pub notification_gate: Mutex<Option<Arc<Gate>>>,
}

/// A gate a publish parks at: it signals that it arrived and goes on once the
/// test opens it. It parks the first `to_park` publishes and lets the rest by.
pub struct Gate {
    pub arrived: tokio::sync::Notify,
    pub open: tokio::sync::Notify,
    /// How many publishes reached the gate, parked or not.
    pub arrivals: AtomicUsize,
    to_park: AtomicUsize,
}

impl Gate {
    fn new(to_park: usize) -> Self {
        Self {
            arrived: tokio::sync::Notify::new(),
            open: tokio::sync::Notify::new(),
            arrivals: AtomicUsize::new(0),
            to_park: AtomicUsize::new(to_park),
        }
    }

    /// Whether this arrival is to be parked.
    fn parks(&self) -> bool {
        self.arrivals.fetch_add(1, Ordering::SeqCst);
        self.to_park
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }

    /// Let the parked publishes go on.
    pub fn release(&self) {
        self.open.notify_waiters();
        self.open.notify_one();
    }
}

impl DeliveringBroker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BrokerState::default()),
            drop_publishes: AtomicBool::new(false),
            streaming: tokio::sync::watch::channel(0).0,
            stream_opens: AtomicUsize::new(0),
            refuse_notifications: AtomicBool::new(false),
            refused: AtomicUsize::new(0),
            notification_gate: Mutex::new(None),
        })
    }

    pub fn drop_publishes(&self, on: bool) {
        self.drop_publishes.store(on, Ordering::SeqCst);
    }

    /// The join barrier: returns once `replicas` subscriptions hold an open
    /// stream, so a publish from now on reaches every one of them.
    pub async fn await_joined(&self, replicas: usize) {
        let mut rx = self.streaming.subscribe();
        tokio::time::timeout(JOIN_DEADLINE, rx.wait_for(|open| *open >= replicas))
            .await
            .expect("every replica joined the cache topic within the deadline")
            .expect("the barrier stays open");
    }

    /// Forget what was published so far, so a count starts at the write.
    pub fn forget_published(&self) {
        let mut state = self.state.lock().unwrap();
        state.published.clear();
        state.delivered = 0;
    }

    /// The `cache_invalidate` events published, in order.
    pub fn cache_invalidations(&self) -> Vec<EventV1> {
        self.state
            .lock()
            .unwrap()
            .published
            .iter()
            .filter(|e| *e.event_type() == CACHE_INVALIDATE_EVENT_TYPE)
            .cloned()
            .collect()
    }

    /// Every event the broker accepted, in order.
    pub fn published(&self) -> Vec<EventV1> {
        self.state.lock().unwrap().published.clone()
    }

    /// The `change_notification` events the broker accepted, in order.
    pub fn notifications(&self) -> Vec<EventV1> {
        self.state
            .lock()
            .unwrap()
            .published
            .iter()
            .filter(|e| *e.event_type() == CHANGE_NOTIFICATION_EVENT_TYPE)
            .cloned()
            .collect()
    }

    /// Refuse (or accept again) every `change_notification`.
    pub fn refuse_notifications(&self, on: bool) {
        self.refuse_notifications.store(on, Ordering::SeqCst);
    }

    /// Park the next `to_park` `change_notification` publishes at a gate,
    /// returned; later ones go through.
    pub fn gate_notifications(&self, to_park: usize) -> Arc<Gate> {
        let gate = Arc::new(Gate::new(to_park));
        *self.notification_gate.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    /// Let notification publishes through again.
    pub fn ungate_notifications(&self) {
        *self.notification_gate.lock().unwrap() = None;
    }

    pub fn delivered(&self) -> usize {
        self.state.lock().unwrap().delivered
    }

    /// `(created, joined, left, deleted, groups still held)`.
    pub fn lifecycle(&self) -> (usize, usize, usize, usize, usize) {
        let state = self.state.lock().unwrap();
        (
            state.creates,
            state.joins,
            state.leaves,
            state.deletes,
            state.groups.len(),
        )
    }

    pub fn open_streams(&self) -> usize {
        *self.streaming.borrow()
    }
}

pub fn unsupported() -> EventBrokerError {
    EventBrokerError::Internal("not supported by the delivering broker".to_owned())
}

event_broker_impl! {
    impl EventBrokerApi for DeliveringBroker {
        async fn publish(
            &self,
            _: &SecurityContext,
            event: &EventV1,
            _: Ack,
        ) -> Result<IngestOutcome, EventBrokerError> {
            event.check_publishable()?;
            event.check_size()?;
            if *event.event_type() == CHANGE_NOTIFICATION_EVENT_TYPE {
                if self.refuse_notifications.load(Ordering::SeqCst) {
                    self.refused.fetch_add(1, Ordering::SeqCst);
                    return Err(EventBrokerError::Internal(
                        "no produce grant on the activation topic".to_owned(),
                    ));
                }
                let gate = self.notification_gate.lock().unwrap().clone();
                if let Some(gate) = gate
                    && gate.parks()
                {
                    gate.arrived.notify_one();
                    gate.open.notified().await;
                }
            }
            let discard = self.drop_publishes.load(Ordering::SeqCst);
            let mut state = self.state.lock().unwrap();
            state.published.push(event.clone());
            if discard {
                return Ok(IngestOutcome::Accepted);
            }
            let sequence = i64::try_from(state.published.len()).unwrap_or(i64::MAX);
            let mut delivered = 0;
            for joined in state.subscriptions.values() {
                if !joined
                    .interests
                    .iter()
                    .any(|interest| interest.wants(event))
                {
                    continue;
                }
                let frame = WireFrame::Event(event.clone().stamp_position(StoredPosition {
                    partition: 0,
                    sequence: Sequence::assigned(sequence),
                    sequence_time: Utc::now(),
                }));
                if joined.deliveries.send(Ok(frame)).is_ok() {
                    delivered += 1;
                }
            }
            state.delivered += delivered;
            Ok(IngestOutcome::Persisted)
        }
        async fn create_consumer_group(
            &self,
            _: &SecurityContext,
            _: CreateConsumerGroupRequest,
        ) -> Result<ConsumerGroup, EventBrokerError> {
            let id = ConsumerGroupId::new(Uuid::new_v4());
            let mut state = self.state.lock().unwrap();
            state.creates += 1;
            state.groups.push(id);
            Ok(ConsumerGroup {
                id,
                tenant_id: ROOT,
                owner_principal_id: Uuid::nil(),
                kind: ConsumerGroupKind::Anonymous,
                description: None,
                created_at: Utc::now(),
            })
        }
        async fn delete_consumer_group(
            &self,
            _: &SecurityContext,
            id: &ConsumerGroupId,
        ) -> Result<(), EventBrokerError> {
            let mut state = self.state.lock().unwrap();
            state.deletes += 1;
            state.groups.retain(|g| g != id);
            Ok(())
        }
        async fn join(
            &self,
            _: &SecurityContext,
            req: JoinRequest,
        ) -> Result<SubscriptionAssignment, EventBrokerError> {
            let first = req
                .interests
                .first()
                .ok_or_else(|| EventBrokerError::Internal("a join names no interest".to_owned()))?;
            let topic = first.topic().clone();
            let interests: Vec<Interest> = req
                .interests
                .iter()
                .map(|interest| Interest {
                    tenant: interest.tenant_id(),
                    types: interest
                        .types()
                        .iter()
                        .map(|t| t.as_ref().to_owned())
                        .collect(),
                })
                .collect();
            let (tx, rx) = unbounded_channel();
            let subscription_id = SubscriptionId(Uuid::new_v4());
            let mut state = self.state.lock().unwrap();
            state.joins += 1;
            state.subscriptions.insert(
                subscription_id,
                Joined {
                    interests,
                    deliveries: tx,
                },
            );
            state.pending.insert(subscription_id, rx);
            Ok(SubscriptionAssignment {
                subscription_id,
                topology_version: 1,
                assigned: vec![AssignedPartition {
                    topic,
                    partition: 0,
                }],
            })
        }
        async fn leave(&self, _: &SecurityContext, id: SubscriptionId) -> Result<(), EventBrokerError> {
            let was_open = {
                let mut state = self.state.lock().unwrap();
                state.leaves += 1;
                state.pending.remove(&id);
                state.subscriptions.remove(&id).is_some()
            };
            if was_open {
                self.streaming
                    .send_modify(|open| *open = open.saturating_sub(1));
            }
            Ok(())
        }
        async fn stream(
            &self,
            _: &SecurityContext,
            id: SubscriptionId,
        ) -> Result<FrameStream, EventBrokerError> {
            let rx = self
                .state
                .lock()
                .unwrap()
                .pending
                .remove(&id)
                .ok_or_else(|| EventBrokerError::Internal("no such subscription".to_owned()))?;
            self.stream_opens.fetch_add(1, Ordering::SeqCst);
            self.streaming.send_modify(|open| *open += 1);
            Ok(Box::pin(futures_util::stream::unfold(
                rx,
                |mut rx| async move { rx.recv().await.map(|frame| (frame, rx)) },
            )))
        }
        async fn seek(
            &self,
            _: &SecurityContext,
            _: SubscriptionId,
            _: i64,
            _: &[SeekPosition],
        ) -> Result<Vec<SeekResult>, EventBrokerError> {
            // Deliveries start at the latest position by construction: a
            // subscription only receives what is published after its join.
            Ok(Vec::new())
        }
    }
    unsupported: [
        register_producer, publish_batch, get_producer_cursors, reset_producer_chain,
        get_consumer_group, list_consumer_groups, get_subscription, list_subscriptions,
        list_topics, list_topic_segments, list_event_types, get_event_type,
    ]
}

// ---------------------------------------------------------------------------
// The platform around the gear: Types Registry, Tenant Resolver
// ---------------------------------------------------------------------------

/// A Types Registry that serves the value type the setting validates against
/// and accepts everything the gear registers (the cache topic and the event
/// type), recording it.
pub struct RegistryDouble {
    pub inner: MockTypesRegistryClient,
    pub registered: Mutex<Vec<Value>>,
}

impl RegistryDouble {
    pub fn new() -> Arc<Self> {
        Self::serving([Self::string_type()])
    }

    /// As [`Self::new`], serving the secret value type too.
    pub fn with_secret_type() -> Arc<Self> {
        let secret = GtsTypeSchema::try_new(
            GtsTypeId::new(SECRET_TYPE),
            json!({
                "$id": format!("gts://{SECRET_TYPE}"),
                "type": "string",
                "x-gts-traits": { "secret": true }
            }),
            None,
            None,
        )
        .expect("a valid root type");
        Self::serving([Self::string_type(), secret])
    }

    fn string_type() -> GtsTypeSchema {
        GtsTypeSchema::try_new(
            GtsTypeId::new(STRING_TYPE),
            json!({ "$id": format!("gts://{STRING_TYPE}"), "type": "string" }),
            None,
            None,
        )
        .expect("a valid root type")
    }

    fn serving(types: impl IntoIterator<Item = GtsTypeSchema>) -> Arc<Self> {
        Arc::new(Self {
            inner: MockTypesRegistryClient::new().with_type_schemas(types),
            registered: Mutex::new(Vec::new()),
        })
    }

    /// Whether a document with this id was registered.
    pub fn has_registered(&self, id: &str) -> bool {
        self.registered.lock().unwrap().iter().any(|document| {
            ["$id", "id"].iter().any(|field| {
                document
                    .get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|v| v.trim_start_matches("gts://") == id)
            })
        })
    }

    pub fn accept(&self, documents: Vec<Value>) -> Vec<RegisterResult> {
        let mut registered = self.registered.lock().unwrap();
        documents
            .into_iter()
            .map(|document| {
                let id = ["$id", "id"]
                    .iter()
                    .find_map(|field| document.get(field).and_then(Value::as_str))
                    .unwrap_or("unnamed")
                    .trim_start_matches("gts://")
                    .to_owned();
                registered.push(document);
                RegisterResult::Ok { gts_id: id }
            })
            .collect()
    }
}

#[async_trait]
impl TypesRegistryClient for RegistryDouble {
    async fn register(&self, entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        Ok(self.accept(entities))
    }

    async fn register_type_schemas(
        &self,
        type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Ok(self.accept(type_schemas))
    }

    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        self.inner.get_type_schema(type_id).await
    }

    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        self.inner.get_type_schema_by_uuid(type_uuid).await
    }

    async fn get_type_schemas(
        &self,
        type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        self.inner.get_type_schemas(type_ids).await
    }

    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        self.inner.get_type_schemas_by_uuid(type_uuids).await
    }

    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        self.inner.list_type_schemas(query).await
    }

    async fn register_instances(
        &self,
        instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Ok(self.accept(instances))
    }

    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError> {
        self.inner.get_instance(id).await
    }

    async fn get_instance_by_uuid(&self, uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        self.inner.get_instance_by_uuid(uuid).await
    }

    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        self.inner.get_instances(ids).await
    }

    async fn get_instances_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        self.inner.get_instances_by_uuid(uuids).await
    }

    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        self.inner.list_instances(query).await
    }
}

pub fn tenant_ref(id: Uuid, parent: Option<Uuid>) -> TenantRef {
    TenantRef {
        id: TenantId(id),
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: parent.map(TenantId),
        self_managed: false,
    }
}

/// The hierarchy fixture: a fixed set of tenants, each with its parent.
/// `descendants` lists, outermost first, the tenants a descendants query
/// reports; a tenant outside `parent_of` is unknown.
pub struct TenantTree {
    parent_of: fn(Uuid) -> Option<Option<Uuid>>,
    descendants: &'static [Uuid],
}

/// The root tenant and one child under it.
pub const TWO_LEVELS: TenantTree = TenantTree {
    parent_of: |id| match id {
        ROOT => Some(None),
        CHILD => Some(Some(ROOT)),
        _ => None,
    },
    descendants: &[CHILD],
};

impl TenantTree {
    /// A tree of the tenants `parent_of` knows; `descendants` are what a
    /// descendants query reports for the tenants above them.
    pub const fn new(
        parent_of: fn(Uuid) -> Option<Option<Uuid>>,
        descendants: &'static [Uuid],
    ) -> Self {
        Self {
            parent_of,
            descendants,
        }
    }

    pub fn parent_of(&self, id: Uuid) -> Result<Option<Uuid>, TenantResolverError> {
        (self.parent_of)(id).ok_or(TenantResolverError::TenantNotFound {
            tenant_id: TenantId(id),
        })
    }

    /// The ancestors of `id`, parent first.
    fn ancestors(&self, id: Uuid) -> Result<Vec<Uuid>, TenantResolverError> {
        let mut out = Vec::new();
        let mut cursor = self.parent_of(id)?;
        while let Some(parent) = cursor {
            out.push(parent);
            cursor = self.parent_of(parent)?;
        }
        Ok(out)
    }

    pub fn info(&self, id: Uuid) -> Result<TenantInfo, TenantResolverError> {
        Ok(TenantInfo {
            id: TenantId(id),
            name: "a tenant".to_owned(),
            status: TenantStatus::Active,
            tenant_type: None,
            parent_id: self.parent_of(id)?.map(TenantId),
            self_managed: false,
        })
    }

    fn reference(&self, id: Uuid) -> TenantRef {
        tenant_ref(id, self.parent_of(id).ok().flatten())
    }
}

#[async_trait]
impl TenantResolverClient for TenantTree {
    async fn get_tenant(
        &self,
        _: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        self.info(id.0)
    }

    async fn get_root_tenant(
        &self,
        _: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        self.info(ROOT)
    }

    async fn get_tenants(
        &self,
        _: &SecurityContext,
        ids: &[TenantId],
        _: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Ok(ids.iter().filter_map(|id| self.info(id.0).ok()).collect())
    }

    async fn get_ancestors(
        &self,
        _: &SecurityContext,
        id: TenantId,
        _: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        let ancestors = self
            .ancestors(id.0)?
            .into_iter()
            .map(|a| self.reference(a))
            .collect();
        Ok(GetAncestorsResponse {
            tenant: self.reference(id.0),
            ancestors,
        })
    }

    async fn get_descendants(
        &self,
        _: &SecurityContext,
        id: TenantId,
        options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        self.parent_of(id.0)?;
        let wanted = options.status.is_empty() || options.status.contains(&TenantStatus::Active);
        let descendants = if wanted {
            self.descendants
                .iter()
                .filter(|t| **t != id.0 && self.ancestors(**t).is_ok_and(|a| a.contains(&id.0)))
                .map(|t| self.reference(*t))
                .collect()
        } else {
            Vec::new()
        };
        Ok(GetDescendantsResponse {
            tenant: self.reference(id.0),
            descendants,
        })
    }

    async fn is_ancestor(
        &self,
        _: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        let _ = matches!(options.barrier_mode, BarrierMode::Respect);
        self.parent_of(ancestor_id.0)?;
        Ok(self.ancestors(descendant_id.0)?.contains(&ancestor_id.0))
    }
}

// ---------------------------------------------------------------------------
// A replica: one `SettingsService`, its hub and its lifecycle
// ---------------------------------------------------------------------------

pub struct FixedConfig(pub Value);

impl ConfigProvider for FixedConfig {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        (gear == GEAR).then_some(&self.0)
    }
}

/// A policy decision point that allows everything, with no constraints.
pub struct AllowAll;

#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for AllowAll {
    async fn evaluate(
        &self,
        _ctx: toolkit_security::PlatformSecurityContext,
        _request: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::models::EvaluationResponse, CanonicalError> {
        Ok(authz_resolver_sdk::models::EvaluationResponse {
            decision: true,
            context: authz_resolver_sdk::models::EvaluationResponseContext::default(),
        })
    }
}

/// The platform the replicas share: one broker, one registry, one tenant
/// tree. Each replica gets its own `ClientHub` holding the same instances.
pub struct Platform {
    pub broker: Arc<DeliveringBroker>,
    pub registry: Arc<RegistryDouble>,
}

pub struct Replica {
    pub db: Arc<DBProvider<DbError>>,
    pub gear: Arc<SettingsService>,
    pub hub: Arc<ClientHub>,
    pub lifecycle: WithLifecycle<SettingsService>,
    pub cancel: CancellationToken,
}

impl Replica {
    /// `init` over the shared database with `config` as the gear's section.
    pub async fn init(pg: &PgFixture, platform: &Platform, config: Value) -> Self {
        let replica = Self::init_without_broker(pg, platform, config).await;
        replica.plug_broker(platform);
        replica
    }

    /// The Event Broker appears in this replica's hub: what a deployment that
    /// starts its broker later looks like.
    pub fn plug_broker(&self, platform: &Platform) {
        self.hub
            .register::<dyn EventBrokerApi>(Arc::clone(&platform.broker) as Arc<dyn EventBrokerApi>);
    }

    /// `init` as `init` does, with no Event Broker in the hub.
    pub async fn init_without_broker(pg: &PgFixture, platform: &Platform, config: Value) -> Self {
        let hub = Arc::new(ClientHub::new());
        hub.register::<dyn TypesRegistryClient>(
            Arc::clone(&platform.registry) as Arc<dyn TypesRegistryClient>
        );
        hub.register::<dyn credstore_sdk::CredStoreClientV1>(Arc::new(
            credstore_sdk::test_util::MockCredStoreClient::empty(),
        ));
        hub.register::<dyn TenantResolverClient>(Arc::new(TWO_LEVELS));
        hub.register::<dyn authz_resolver_sdk::AuthZResolverApi>(Arc::new(AllowAll));
        let db = connect_db(&pg.url, pool())
            .await
            .expect("the replica connects to the container");
        let provider: DBProvider<DbError> = DBProvider::new(db);
        let provider_for_asserts = provider.clone();
        let cancel = CancellationToken::new();
        let ctx = GearCtx::new(
            GEAR,
            Uuid::new_v4(),
            Arc::new(FixedConfig(json!({ "config": config }))),
            Arc::clone(&hub),
            cancel.clone(),
        )
        .with_db(provider);
        let gear = Arc::new(SettingsService::default());
        gear.init(&ctx).await.expect("init over a live context");
        let lifecycle = WithLifecycle::from_arc_with_name(Arc::clone(&gear), GEAR);
        Self {
            db: Arc::new(provider_for_asserts),
            gear,
            hub,
            lifecycle,
            cancel,
        }
    }

    /// A connection to the replica's database.
    pub fn db_conn(&self) -> toolkit_db::DbConn<'_> {
        self.db.conn().expect("a connection")
    }

    /// Run the managed lifecycle: the replica's consumer starts joining.
    pub async fn serve(&self) {
        self.lifecycle
            .start(self.cancel.clone())
            .await
            .expect("the lifecycle starts");
    }

    /// Stop through the lifecycle, as the runtime does: the consumer leaves
    /// and deletes its group before this returns.
    pub async fn stop(&self) {
        self.lifecycle
            .stop(CancellationToken::new())
            .await
            .expect("the lifecycle stops");
    }

    pub async fn declare(&self, key: &SettingKey, class: ScopeClass, default: &str) {
        let contributions = self
            .hub
            .get::<dyn SettingsContributionClient>()
            .expect("the contribution door is bound");
        let mut declaration =
            ContributedDeclaration::new(key.clone(), STRING_TYPE, json!(default), class);
        declaration.requires_step_up = Some(false);
        let result = contributions
            .register_declarations(
                &SecurityContext::anonymous(),
                "replica-coherence-test".to_owned(),
                vec![declaration],
            )
            .await
            .expect("the declaration registers");
        assert!(result.errors.is_empty(), "refused: {:?}", result.errors);
    }

    /// A user's write context.
    pub fn actor() -> WriteActor {
        WriteActor {
            ctx: SecurityContext::builder()
                .subject_id(Uuid::new_v4())
                .subject_tenant_id(ROOT)
                .subject_type(USER_SUBJECT_TYPE)
                .build()
                .expect("a context"),
            request_id: "replica-test".to_owned(),
            step_up_token: None,
            visibility: DomainVisibility::Unrestricted,
        }
    }

    /// One change through this replica's writer.
    pub async fn change(
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

    /// Set `value` at `tenant`.
    pub async fn set(
        &self,
        key: &SettingKey,
        tenant: Uuid,
        value: &str,
        if_match: Option<&str>,
    ) -> Result<Committed, DomainError> {
        self.change(key, tenant, Change::Set(json!(value)), if_match)
            .await
    }

    /// Write `value` at the root tenant through this replica's writer. Each key
    /// is written once, over no row, so the precondition is the absent tag.
    pub async fn write_at_root(&self, key: &SettingKey, value: &str) {
        self.set(key, ROOT, value, Some("absent"))
            .await
            .expect("the write commits");
    }

    /// Register `subscriber`'s subscription to `key` at `tenant`.
    pub async fn register(
        &self,
        subscriber: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> Result<Registered, DomainError> {
        self.gear
            .subscriptions()
            .expect("initialized")
            .register_subscription(subscriber, key, tenant, Some(CONSUMER_INSTANCE), None)
            .await
    }

    /// Retire `subscriber`'s subscription to `key` at `tenant`.
    pub async fn unsubscribe(
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

    /// The effective value this replica serves for `key` at `tenant`.
    pub async fn read(&self, key: &SettingKey, tenant: Uuid) -> String {
        let scope = if tenant == ROOT {
            "/".to_owned()
        } else {
            format!("/tenants/{tenant}")
        };
        let reader = self
            .hub
            .get::<dyn SettingsReaderClient>()
            .expect("the reader is bound");
        let response = reader
            .get_effective(
                &SecurityContext::anonymous(),
                GetEffectiveRequest {
                    key: key.clone(),
                    scope,
                    freshness: settings_service_sdk::models::Freshness::default(),
                },
            )
            .await
            .expect("the read resolves");
        response.value.as_str().expect("a string value").to_owned()
    }

    /// Poll until this replica serves `expected`, never sleeping as the
    /// assertion: the deadline bounds the wait, the answer is the check.
    /// Returns how long it took.
    pub async fn serves(
        &self,
        key: &SettingKey,
        tenant: Uuid,
        expected: &str,
        deadline: Duration,
    ) -> Result<Duration, String> {
        let started = Instant::now();
        loop {
            let seen = self.read(key, tenant).await;
            if seen == expected {
                return Ok(started.elapsed());
            }
            if started.elapsed() >= deadline {
                return Err(format!(
                    "still serving `{seen}` after {deadline:?}, expected `{expected}`"
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

pub fn platform() -> Platform {
    Platform {
        broker: DeliveringBroker::new(),
        registry: RegistryDouble::new(),
    }
}
