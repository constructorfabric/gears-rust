#![cfg(feature = "integration")]
// The doubles, the fixture and the polls are plain helpers, which
// `allow-expect-in-tests` does not reach; a failed expectation there is the
// test failing, as in the sibling PostgreSQL suites.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Two replicas of the gear over one `PostgreSQL`, joined by a delivering
//! broker: the Level 2 rows *Two-replica convergence* and *Dropped broadcast*
//! of DESIGN-activation §7.
//!
//! Each replica is a whole `SettingsService`: `init` over its own `ClientHub`,
//! then the managed lifecycle, so the end-of-request publish, the replica's
//! consumer and its cache are the production ones. What the test hosts is the
//! platform around them: the Event Broker (an in-memory `EventBrokerApi` that
//! fans every published event out to each joined group's open stream, with a
//! join barrier and a drop switch), the Types Registry (accepts what is
//! registered, serves the value type), the Tenant Resolver (a root with one
//! child) and the Credential Store the gear's init resolves.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_replica_coherence_test
//! ```

// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-tests:p1

mod support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cf_gears_settings_service::SettingsService;
use cf_gears_settings_service::domain::category::DomainVisibility;
use cf_gears_settings_service::domain::stepup::USER_SUBJECT_TYPE;
use cf_gears_settings_service::domain::writes::{Change, WriteActor};
use cf_gears_settings_service::infra::storage::migrations::Migrator;
use chrono::Utc;
use event_broker_sdk::models::EventType;
use event_broker_sdk::{
    AssignedPartition, ConsumerGroup, ConsumerGroupId, ConsumerGroupKind, ConsumerGroupQuery,
    CreateConsumerGroupRequest, Event, EventBrokerApi, EventBrokerError, FrameStream,
    IngestOutcome, JoinRequest, Page, PartitionRange, ProducerCursors, ProducerId, ProducerMode,
    ResetScope, SeekPosition, SeekResult, Sequence, Subscription, SubscriptionAssignment,
    SubscriptionId, Topic, TopicSegment, WireEvent, WireFrame,
};
use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use settings_service_sdk::SettingKey;
use settings_service_sdk::api::{SettingsContributionClient, SettingsReaderClient};
use settings_service_sdk::gts::CACHE_INVALIDATE_EVENT_TYPE;
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

const GEAR: &str = "settings-service";
const STRING_TYPE: &str = "gts.cf.core.settings.type_string.v1~";
const ROOT: Uuid = Uuid::from_u128(0xa0);
const CHILD: Uuid = Uuid::from_u128(0xc1);

/// How long a poll waits for a broadcast to converge a replica: far below the
/// default 30 s `cache_ttl_seconds`, so only the broadcast can explain a pass.
const CONVERGENCE_DEADLINE: Duration = Duration::from_secs(5);
/// How long a replica gets to join the cache topic.
const JOIN_DEADLINE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// PostgreSQL fixture
// ---------------------------------------------------------------------------

struct PgFixture {
    _container: support::Container,
    url: String,
}

fn pool() -> ConnectOpts {
    ConnectOpts {
        max_conns: Some(5),
        min_conns: Some(1),
        ..Default::default()
    }
}

/// A container with the gear's schema applied once, as a stand's first start
/// applies it; both replicas then find nothing outstanding.
async fn pg_fixture() -> Option<PgFixture> {
    let container = support::start("replica").await?;
    let url = container.url.clone();
    let db = connect_db(&url, pool())
        .await
        .expect("the toolkit connects to the container");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");
    Some(PgFixture {
        _container: container,
        url,
    })
}

// ---------------------------------------------------------------------------
// The delivering broker double
// ---------------------------------------------------------------------------

type FrameResult = Result<WireFrame, EventBrokerError>;

/// One joined subscription: where its deliveries go and the tenant it asked for.
struct Joined {
    tenant: Uuid,
    deliveries: UnboundedSender<FrameResult>,
}

/// What the broker keeps and what it was asked, behind one lock.
#[derive(Default)]
struct BrokerState {
    groups: Vec<ConsumerGroupId>,
    subscriptions: HashMap<SubscriptionId, Joined>,
    pending: HashMap<SubscriptionId, tokio::sync::mpsc::UnboundedReceiver<FrameResult>>,
    published: Vec<Event>,
    delivered: usize,
    creates: usize,
    joins: usize,
    leaves: usize,
    deletes: usize,
}

/// An in-memory Event Broker that delivers: `publish` hands the event, as the
/// `WireEvent` the consumer reads, to the open stream of every joined group
/// whose interest names the event's tenant. One group is one replica, so every
/// replica sees every broadcast, which is the fan-out the settings service
/// relies on (the broker has no broadcast mode).
///
/// Matches `EventBrokerApi` of `event-broker-sdk/src/api.rs`: the calls the
/// consumer makes (`create_consumer_group`, `join`, `seek`, `stream`, `leave`,
/// `delete_consumer_group`) and `publish`; the frame is `WireFrame::Event`
/// carrying a `WireEvent` (`api.rs`, `WireEvent`).
struct DeliveringBroker {
    state: Mutex<BrokerState>,
    /// While set, `publish` is accepted and discarded: no replica receives it.
    drop_publishes: AtomicBool,
    /// Subscriptions with an open stream: what the join barrier waits on.
    streaming: tokio::sync::watch::Sender<usize>,
    stream_opens: AtomicUsize,
}

impl DeliveringBroker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BrokerState::default()),
            drop_publishes: AtomicBool::new(false),
            streaming: tokio::sync::watch::channel(0).0,
            stream_opens: AtomicUsize::new(0),
        })
    }

    fn drop_publishes(&self, on: bool) {
        self.drop_publishes.store(on, Ordering::SeqCst);
    }

    /// The join barrier: returns once `replicas` subscriptions hold an open
    /// stream, so a publish from now on reaches every one of them.
    async fn await_joined(&self, replicas: usize) {
        let mut rx = self.streaming.subscribe();
        tokio::time::timeout(JOIN_DEADLINE, rx.wait_for(|open| *open >= replicas))
            .await
            .expect("every replica joined the cache topic within the deadline")
            .expect("the barrier stays open");
    }

    /// Forget what was published so far, so a count starts at the write.
    fn forget_published(&self) {
        let mut state = self.state.lock().unwrap();
        state.published.clear();
        state.delivered = 0;
    }

    /// The `cache_invalidate` events published, in order.
    fn cache_invalidations(&self) -> Vec<Event> {
        self.state
            .lock()
            .unwrap()
            .published
            .iter()
            .filter(|e| e.type_id.as_ref() == CACHE_INVALIDATE_EVENT_TYPE)
            .cloned()
            .collect()
    }

    fn delivered(&self) -> usize {
        self.state.lock().unwrap().delivered
    }

    /// `(created, joined, left, deleted, groups still held)`.
    fn lifecycle(&self) -> (usize, usize, usize, usize, usize) {
        let state = self.state.lock().unwrap();
        (
            state.creates,
            state.joins,
            state.leaves,
            state.deletes,
            state.groups.len(),
        )
    }

    fn open_streams(&self) -> usize {
        *self.streaming.borrow()
    }
}

fn unsupported() -> EventBrokerError {
    EventBrokerError::Internal("not supported by the delivering broker".to_owned())
}

#[async_trait]
impl EventBrokerApi for DeliveringBroker {
    async fn register_producer(
        &self,
        _: &SecurityContext,
        _: ProducerMode,
        _: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        Err(unsupported())
    }

    async fn publish(
        &self,
        _: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        let discard = self.drop_publishes.load(Ordering::SeqCst);
        let mut state = self.state.lock().unwrap();
        state.published.push(event.clone());
        if discard {
            return Ok(IngestOutcome::Accepted);
        }
        let sequence = i64::try_from(state.published.len()).unwrap_or(i64::MAX);
        let mut delivered = 0;
        for joined in state.subscriptions.values() {
            if joined.tenant != event.tenant_id {
                continue;
            }
            let frame = WireFrame::Event(WireEvent {
                id: event.id,
                type_id: event.type_id.clone(),
                tenant_id: event.tenant_id,
                subject: event.subject.clone(),
                subject_type: event.subject_type.clone(),
                partition: 0,
                sequence: Sequence::assigned(sequence),
                occurred_at: event.occurred_at,
                sequence_time: Utc::now(),
                trace_parent: event.trace_parent.clone(),
                data: event.data.clone().unwrap_or(Value::Null),
            });
            if joined.deliveries.send(Ok(frame)).is_ok() {
                delivered += 1;
            }
        }
        state.delivered += delivered;
        Ok(IngestOutcome::Persisted)
    }

    async fn publish_batch(
        &self,
        _: &SecurityContext,
        _: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        Err(unsupported())
    }

    async fn get_producer_cursors(
        &self,
        _: &SecurityContext,
        _: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        Err(unsupported())
    }

    async fn reset_producer_chain(
        &self,
        _: &SecurityContext,
        _: ProducerId,
        _: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        Err(unsupported())
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

    async fn get_consumer_group(
        &self,
        _: &SecurityContext,
        _: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        Err(unsupported())
    }

    async fn list_consumer_groups(
        &self,
        _: &SecurityContext,
        _: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        Err(unsupported())
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
        let interest = req
            .interests
            .first()
            .ok_or_else(|| EventBrokerError::Internal("a join names no interest".to_owned()))?;
        let (tx, rx) = unbounded_channel();
        let subscription_id = SubscriptionId(Uuid::new_v4());
        let mut state = self.state.lock().unwrap();
        state.joins += 1;
        state.subscriptions.insert(
            subscription_id,
            Joined {
                tenant: interest.tenant_id(),
                deliveries: tx,
            },
        );
        state.pending.insert(subscription_id, rx);
        Ok(SubscriptionAssignment {
            subscription_id,
            topology_version: 1,
            assigned: vec![AssignedPartition {
                topic: interest.topic().clone(),
                partition: 0,
            }],
        })
    }

    async fn get_subscription(
        &self,
        _: &SecurityContext,
        _: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        Err(unsupported())
    }

    async fn list_subscriptions(
        &self,
        _: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        Err(unsupported())
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

    async fn list_topics(&self, _: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        Err(unsupported())
    }

    async fn list_topic_segments(
        &self,
        _: &SecurityContext,
        _: &str,
        _: u32,
        _: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError> {
        Err(unsupported())
    }

    async fn list_event_types(
        &self,
        _: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        Err(unsupported())
    }

    async fn get_event_type(
        &self,
        _: &SecurityContext,
        _: &str,
    ) -> Result<EventType, EventBrokerError> {
        Err(unsupported())
    }
}

// ---------------------------------------------------------------------------
// The platform around the gear: Types Registry, Tenant Resolver
// ---------------------------------------------------------------------------

/// A Types Registry that serves the value type the setting validates against
/// and accepts everything the gear registers (the cache topic and the event
/// type), recording it.
struct RegistryDouble {
    inner: MockTypesRegistryClient,
    registered: Mutex<Vec<Value>>,
}

impl RegistryDouble {
    fn new() -> Arc<Self> {
        let string = GtsTypeSchema::try_new(
            GtsTypeId::new(STRING_TYPE),
            json!({ "$id": format!("gts://{STRING_TYPE}"), "type": "string" }),
            None,
            None,
        )
        .expect("a valid root type");
        Arc::new(Self {
            inner: MockTypesRegistryClient::new().with_type_schemas([string]),
            registered: Mutex::new(Vec::new()),
        })
    }

    fn accept(&self, documents: Vec<Value>) -> Vec<RegisterResult> {
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

fn tenant_ref(id: Uuid, parent: Option<Uuid>) -> TenantRef {
    TenantRef {
        id: TenantId(id),
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: parent.map(TenantId),
        self_managed: false,
    }
}

/// The hierarchy fixture: the root tenant and one child under it.
struct TwoLevelTree;

impl TwoLevelTree {
    fn parent_of(id: Uuid) -> Result<Option<Uuid>, TenantResolverError> {
        match id {
            ROOT => Ok(None),
            CHILD => Ok(Some(ROOT)),
            other => Err(TenantResolverError::TenantNotFound {
                tenant_id: TenantId(other),
            }),
        }
    }

    fn info(id: Uuid) -> Result<TenantInfo, TenantResolverError> {
        Ok(TenantInfo {
            id: TenantId(id),
            name: "a tenant".to_owned(),
            status: TenantStatus::Active,
            tenant_type: None,
            parent_id: Self::parent_of(id)?.map(TenantId),
            self_managed: false,
        })
    }
}

#[async_trait]
impl TenantResolverClient for TwoLevelTree {
    async fn get_tenant(
        &self,
        _: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        Self::info(id.0)
    }

    async fn get_root_tenant(
        &self,
        _: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        Self::info(ROOT)
    }

    async fn get_tenants(
        &self,
        _: &SecurityContext,
        ids: &[TenantId],
        _: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Ok(ids.iter().filter_map(|id| Self::info(id.0).ok()).collect())
    }

    async fn get_ancestors(
        &self,
        _: &SecurityContext,
        id: TenantId,
        _: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        let parent = Self::parent_of(id.0)?;
        Ok(GetAncestorsResponse {
            tenant: tenant_ref(id.0, parent),
            ancestors: parent
                .map(|p| vec![tenant_ref(p, None)])
                .unwrap_or_default(),
        })
    }

    async fn get_descendants(
        &self,
        _: &SecurityContext,
        id: TenantId,
        options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        let parent = Self::parent_of(id.0)?;
        let wanted = options.status.is_empty() || options.status.contains(&TenantStatus::Active);
        let descendants = if id.0 == ROOT && wanted {
            vec![tenant_ref(CHILD, Some(ROOT))]
        } else {
            Vec::new()
        };
        Ok(GetDescendantsResponse {
            tenant: tenant_ref(id.0, parent),
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
        Self::parent_of(ancestor_id.0)?;
        Ok(Self::parent_of(descendant_id.0)? == Some(ancestor_id.0))
    }
}

// ---------------------------------------------------------------------------
// A replica: one `SettingsService`, its hub and its lifecycle
// ---------------------------------------------------------------------------

struct FixedConfig(Value);

impl ConfigProvider for FixedConfig {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        (gear == GEAR).then_some(&self.0)
    }
}

/// The platform the two replicas share: one broker, one registry, one tenant
/// tree. Each replica gets its own `ClientHub` holding the same instances.
struct Platform {
    broker: Arc<DeliveringBroker>,
    registry: Arc<RegistryDouble>,
}

struct Replica {
    gear: Arc<SettingsService>,
    hub: Arc<ClientHub>,
    lifecycle: WithLifecycle<SettingsService>,
    cancel: CancellationToken,
}

impl Replica {
    /// `init` over the shared database with `config` as the gear's section.
    async fn init(pg: &PgFixture, platform: &Platform, config: Value) -> Self {
        let hub = Arc::new(ClientHub::new());
        hub.register::<dyn TypesRegistryClient>(
            Arc::clone(&platform.registry) as Arc<dyn TypesRegistryClient>
        );
        hub.register::<dyn credstore_sdk::CredStoreClientV1>(Arc::new(
            credstore_sdk::test_util::MockCredStoreClient::empty(),
        ));
        hub.register::<dyn TenantResolverClient>(Arc::new(TwoLevelTree));
        hub.register::<dyn EventBrokerApi>(Arc::clone(&platform.broker) as Arc<dyn EventBrokerApi>);
        let db = connect_db(&pg.url, pool())
            .await
            .expect("the replica connects to the container");
        let provider: DBProvider<DbError> = DBProvider::new(db);
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
            gear,
            hub,
            lifecycle,
            cancel,
        }
    }

    /// Run the managed lifecycle: the replica's consumer starts joining.
    async fn serve(&self) {
        self.lifecycle
            .start(self.cancel.clone())
            .await
            .expect("the lifecycle starts");
    }

    /// Stop through the lifecycle, as the runtime does: the consumer leaves
    /// and deletes its group before this returns.
    async fn stop(&self) {
        self.lifecycle
            .stop(CancellationToken::new())
            .await
            .expect("the lifecycle stops");
    }

    async fn declare(&self, key: &SettingKey, class: ScopeClass, default: &str) {
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

    /// Write `value` at the root tenant through this replica's writer. Each key
    /// is written once, over no row, so the precondition is the absent tag.
    async fn write_at_root(&self, key: &SettingKey, value: &str) {
        let actor = WriteActor {
            ctx: SecurityContext::builder()
                .subject_id(Uuid::new_v4())
                .subject_tenant_id(ROOT)
                .subject_type(USER_SUBJECT_TYPE)
                .build()
                .expect("a context"),
            request_id: "replica-coherence-test".to_owned(),
            step_up_token: None,
            visibility: DomainVisibility::Unrestricted,
        };
        self.gear
            .writes()
            .expect("initialized")
            .change(
                &actor,
                key,
                Some(ROOT),
                Change::Set(json!(value)),
                Some("absent"),
                "set",
            )
            .await
            .expect("the write commits");
    }

    /// The effective value this replica serves for `key` at `tenant`.
    async fn read(&self, key: &SettingKey, tenant: Uuid) -> String {
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
                },
            )
            .await
            .expect("the read resolves");
        response.value.as_str().expect("a string value").to_owned()
    }

    /// Poll until this replica serves `expected`, never sleeping as the
    /// assertion: the deadline bounds the wait, the answer is the check.
    /// Returns how long it took.
    async fn serves(
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

fn setting(name: &str) -> SettingKey {
    SettingKey::contributed("cf", "demo", "coherence", name, std::num::NonZeroU32::MIN)
        .expect("a valid key")
}

/// The tenant and keys a `cache_invalidate` event names.
fn named(event: &Event) -> (String, Vec<String>) {
    let data = event.data.as_ref().expect("the event carries a body");
    let tenant = data["tenant"].as_str().expect("a tenant").to_owned();
    let keys = data["changed_keys"]
        .as_array()
        .expect("a key list")
        .iter()
        .map(|k| k.as_str().expect("a key").to_owned())
        .collect();
    (tenant, keys)
}

/// Both replicas are down and each left and deleted its one group.
async fn assert_clean_stop(platform: &Platform, replicas: &[&Replica]) {
    for replica in replicas {
        replica.stop().await;
    }
    let (created, joined, left, deleted, held) = platform.broker.lifecycle();
    assert_eq!(created, replicas.len(), "one group per replica");
    assert_eq!(joined, replicas.len(), "one join per replica");
    assert_eq!(left, replicas.len(), "each replica left its subscription");
    assert_eq!(deleted, replicas.len(), "each replica deleted its group");
    assert_eq!(held, 0, "the broker holds no group after the stop");
    assert_eq!(platform.broker.open_streams(), 0);
}

fn platform() -> Platform {
    Platform {
        broker: DeliveringBroker::new(),
        registry: RegistryDouble::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// *Two-replica convergence*: after a write on A, B stops serving the old
/// value long before `cache_ttl_seconds` (the default 30 s) could have
/// expired it, for a `local` key at the written tenant and for a `cascading`
/// key at the descendant. Only the broadcast can explain the pass.
#[tokio::test]
async fn a_write_on_one_replica_converges_the_other_within_the_poll_deadline() {
    let Some(pg) = pg_fixture().await else {
        return;
    };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;

    let cascading = setting("cascading_flag");
    let local = setting("local_flag");
    a.declare(&cascading, ScopeClass::Cascading, "v0").await;
    a.declare(&local, ScopeClass::Local, "v0").await;

    // The warm read: B holds the previous value in its cache.
    assert_eq!(b.read(&cascading, CHILD).await, "v0");
    assert_eq!(b.read(&local, ROOT).await, "v0");

    a.serve().await;
    b.serve().await;
    platform.broker.await_joined(2).await;
    platform.broker.forget_published();

    a.write_at_root(&cascading, "v1").await;
    let converged = b
        .serves(&cascading, CHILD, "v1", CONVERGENCE_DEADLINE)
        .await
        .expect("B serves the cascading value at the descendant");
    a.write_at_root(&local, "v1").await;
    let converged_local = b
        .serves(&local, ROOT, "v1", CONVERGENCE_DEADLINE)
        .await
        .expect("B serves the local value at the written tenant");
    eprintln!(
        "convergence: cascading {} ms, local {} ms",
        converged.as_millis(),
        converged_local.as_millis()
    );

    // One broadcast per write, under the root tenant, naming the key.
    let broadcasts = platform.broker.cache_invalidations();
    assert_eq!(broadcasts.len(), 2, "one cache_invalidate per write");
    for (event, key) in broadcasts.iter().zip([&cascading, &local]) {
        assert_eq!(event.tenant_id, ROOT, "published under the root tenant");
        let (tenant, keys) = named(event);
        assert_eq!(tenant, ROOT.to_string());
        assert_eq!(keys, vec![key.as_str().to_owned()]);
        assert!(
            event.data.as_ref().unwrap().get("value").is_none(),
            "identifiers only"
        );
    }
    assert!(
        platform.broker.delivered() >= 4,
        "each broadcast reached both replicas"
    );

    assert_clean_stop(&platform, &[&a, &b]).await;
}

/// *Dropped broadcast*: the broker accepts the publish and delivers it to
/// nobody. B still serves the previous value before its TTL elapses and the
/// committed one after it, so `cache_ttl_seconds` alone bounds the staleness.
// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-ttl-backstop:p1
#[tokio::test]
async fn a_dropped_broadcast_leaves_the_peer_stale_for_at_most_the_ttl() {
    const TTL: Duration = Duration::from_secs(2);
    /// What the TTL may overshoot by before the new value must be served.
    const MARGIN: Duration = Duration::from_secs(2);

    let Some(pg) = pg_fixture().await else {
        return;
    };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(
        &pg,
        &platform,
        json!({ "cache_ttl_seconds": TTL.as_secs() }),
    )
    .await;

    let key = setting("dropped_flag");
    let warmup = setting("dropped_warmup");
    a.declare(&key, ScopeClass::Cascading, "v0").await;
    a.declare(&warmup, ScopeClass::Cascading, "w0").await;

    a.serve().await;
    b.serve().await;
    platform.broker.await_joined(2).await;
    // A's write path is warmed by one throwaway write of another key, so the
    // first write the clock below times is not also A's first (connection,
    // statements, the publisher's first broker session). Its broadcast is
    // delivered to both replicas before the counters are reset and the drop
    // switch goes on.
    a.write_at_root(&warmup, "w1").await;
    tokio::time::timeout(CONVERGENCE_DEADLINE, async {
        while platform.broker.delivered() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the warm-up broadcast reached both replicas");
    platform.broker.forget_published();
    platform.broker.drop_publishes(true);

    // The warm read starts B's TTL clock; the write follows at once.
    assert_eq!(b.read(&key, CHILD).await, "v0");
    let warmed = Instant::now();
    a.write_at_root(&key, "v1").await;

    // Before the TTL: the old value, from B's cache.
    let seen_before = b.read(&key, CHILD).await;
    let elapsed = warmed.elapsed();
    assert!(
        elapsed < TTL,
        "the write and the read took {elapsed:?}, not under the {TTL:?} TTL: \
         the stale read proves nothing"
    );
    assert_eq!(
        seen_before, "v0",
        "B serves the previous value before the TTL"
    );

    // After the TTL plus the margin: the committed value, with no broadcast.
    b.serves(&key, CHILD, "v1", (TTL + MARGIN).saturating_sub(elapsed))
        .await
        .expect("B serves the new value once its TTL elapsed");
    let converged_after = warmed.elapsed();
    eprintln!(
        "dropped broadcast: converged {} ms after the warm read",
        converged_after.as_millis()
    );
    assert!(
        converged_after + Duration::from_millis(100) >= TTL,
        "converged after {converged_after:?}, before the {TTL:?} TTL could expire the entry: \
         something delivered the broadcast"
    );

    // The broker saw the publish and discarded it.
    assert_eq!(platform.broker.cache_invalidations().len(), 1);
    assert_eq!(platform.broker.delivered(), 0, "nothing was delivered");

    assert_clean_stop(&platform, &[&a, &b]).await;
}
