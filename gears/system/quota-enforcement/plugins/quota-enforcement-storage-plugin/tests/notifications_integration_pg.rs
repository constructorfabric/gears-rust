#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `PostgreSQL`-backed suite of the notification pipeline.
//!
//! What only a real concurrent backend can show:
//!
//! - a cold database bootstraps with the outbox unbound, an event cannot be
//!   committed before delivery starts, and once it starts a committed debit's
//!   event reaches the delivery callback at once, on the commit's wake;
//! - a rejected event and an undecodable row land in the dead-letter store;
//! - two pipelines over one database never deliver one partition at the same
//!   time, and together deliver every event;
//! - a pipeline stopped while delivering loses nothing: another one delivers
//!   what it left.
//!
//! Requires Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test notifications_integration_pg`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use gts::GtsTypeId;
use quota_enforcement_sdk::engine::{
    EvaluationContext, EvaluationLimits, EvaluationOutcome, PolicySchemaSnapshot,
    TransactionEvaluator,
};
use quota_enforcement_sdk::testing::quota_draft;
use quota_enforcement_sdk::{
    ApplicableQuotas, AttributionDigest, Decision, DeliveryOutcome, EvaluatedMutation, EventId,
    IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, MetricId, NotificationDeliveryV1,
    NotificationEvent, NotificationEventKind, NotificationScope, OperationType, PayloadHash,
    PeriodType, PolicyDraft, PolicyScope, QuotaDebitPlan, QuotaDraft, QuotaType, SubjectRef,
    TenantId,
};
use sea_orm_migration::MigratorTrait as _;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::{DeadLetterFilter, DeadLetterScope, OutboxHandle, Records};
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use quota_enforcement_storage_plugin::infra::storage::Migrator;
use quota_enforcement_storage_plugin::{
    Actor, ConsumptionStore, NOTIFICATION_QUEUE, NotificationOutbox, QeOutbox, QuotaStore,
    SqlConsumptionStore, SqlPolicyStore, SqlQuotaStore, StoreError, start_notification_pipeline,
};

const USER_PROJECTION: &str = "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~";
const METRIC_TOKENS: &str = "gts.cf.core.qe.metric_type.v1~cf.genai.llm_gateway.ai_tokens_input.v1";
/// Well within the reconciler's idle minute: a delivery this fast came from
/// the commit's wake.
const PROMPTLY: Duration = Duration::from_secs(5);

fn tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(0x00ac_ce55))
}

fn actor() -> Actor {
    Actor {
        subject_id: Uuid::from_u128(0x5eed),
        subject_type: None,
    }
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("security context")
}

fn subject(id: &str) -> SubjectRef {
    SubjectRef {
        projection_type: GtsTypeId::try_new(USER_PROJECTION).expect("type id"),
        subject_id: id.to_owned(),
    }
}

/// A `quota-changed` event of `tenant`.
fn changed(tenant: TenantId) -> NotificationEvent {
    NotificationEvent {
        event_id: EventId::generate(),
        kind: NotificationEventKind::QuotaChanged,
        scope: NotificationScope::Tenant { tenant_id: tenant },
        quota_id: None,
        policy_id: None,
        subject: None,
        payload: serde_json::json!({ "change_kind": "created" }),
        emitted_at: OffsetDateTime::now_utc(),
    }
}

/// A daily consumption Quota that notifies at half its cap.
fn consumption(holder: &str, cap: u64) -> QuotaDraft {
    let mut draft = quota_draft(subject(holder), Some(cap));
    draft.tenant_id = tenant();
    draft.metric = MetricId::parse(METRIC_TOKENS).expect("metric");
    draft.quota_type = QuotaType::Consumption;
    draft.period = Some(PeriodType::Day);
    draft.notification_thresholds = vec![50];
    draft
}

fn limits() -> EvaluationLimits {
    EvaluationLimits {
        upper_timeout_ms: std::num::NonZeroU64::new(500).expect("nonzero"),
        cost_limit: std::num::NonZeroU64::new(100_000).expect("nonzero"),
    }
}

/// Allow the requested amount against every applicable Quota.
fn allow_all() -> Arc<TransactionEvaluator> {
    Arc::new(move |context: &EvaluationContext<'_>| {
        let plan = context
            .quotas
            .iter()
            .map(|quota| {
                (
                    quota.snapshot.quota_id,
                    QuotaDebitPlan {
                        amount: context.amount,
                    },
                )
            })
            .collect();
        Ok(EvaluationOutcome::validate(
            Decision::allowed_with_plan(plan),
            context,
        )?)
    })
}

async fn wait_for_tcp(port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "postgres never listened"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Wait until `done` holds, or fail after `within`.
async fn eventually(within: Duration, what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A delivery callback that records every call, answers from a script, and
/// optionally takes `pause` per call.
#[derive(Default)]
struct RecordingDelivery {
    script: Mutex<VecDeque<DeliveryOutcome>>,
    seen: Mutex<Vec<(EventId, NotificationEventKind)>>,
    undeliverable: Mutex<Vec<String>>,
    pause: Duration,
    /// Deliveries in flight per partition, and the most ever seen at once.
    in_flight: Mutex<HashMap<u32, usize>>,
    overlap: Mutex<usize>,
}

impl RecordingDelivery {
    fn answering(outcomes: Vec<DeliveryOutcome>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(outcomes.into()),
            ..Self::default()
        })
    }

    fn pausing(pause: Duration) -> Arc<Self> {
        Arc::new(Self {
            pause,
            ..Self::default()
        })
    }

    fn seen(&self) -> Vec<(EventId, NotificationEventKind)> {
        self.seen.lock().expect("seen").clone()
    }

    fn ids(&self) -> HashSet<EventId> {
        self.seen().into_iter().map(|(id, _)| id).collect()
    }
}

#[async_trait]
impl NotificationDeliveryV1 for RecordingDelivery {
    async fn deliver(
        &self,
        event: NotificationEvent,
        _attempts: u16,
        _budget: Duration,
    ) -> DeliveryOutcome {
        let partition = match event.scope {
            NotificationScope::Tenant { tenant_id } => QeOutbox::partition_for(tenant_id),
            NotificationScope::Platform => 0,
        };
        {
            let mut in_flight = self.in_flight.lock().expect("in flight");
            let running = in_flight.entry(partition).or_default();
            *running += 1;
            let mut overlap = self.overlap.lock().expect("overlap");
            *overlap = (*overlap).max(*running);
        }
        if !self.pause.is_zero() {
            tokio::time::sleep(self.pause).await;
        }
        *self
            .in_flight
            .lock()
            .expect("in flight")
            .entry(partition)
            .or_default() -= 1;
        self.seen
            .lock()
            .expect("seen")
            .push((event.event_id, event.kind));
        self.script
            .lock()
            .expect("script")
            .pop_front()
            .unwrap_or(DeliveryOutcome::Delivered)
    }

    fn undeliverable(&self, _payload_type: &str, reason: &str) {
        self.undeliverable
            .lock()
            .expect("undeliverable")
            .push(reason.to_owned());
    }
}

struct PgHarness {
    db: toolkit_db::Db,
    outbox: Arc<QeOutbox>,
    quotas: SqlQuotaStore,
    store: SqlConsumptionStore,
    _container: ContainerAsync<Postgres>,
}

impl PgHarness {
    /// A migrated database, bootstrapped cold: the global policy is seeded
    /// while the outbox is still unbound.
    async fn up() -> Self {
        let container = test_containers::postgres()
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app")
            .start()
            .await
            .expect("start postgres");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("mapped port");
        wait_for_tcp(port).await;
        let dsn = format!("postgres://user:pass@127.0.0.1:{port}/app");
        let mut db = None;
        for _ in 0..20 {
            match connect_db(&dsn, ConnectOpts::default()).await {
                Ok(connected) => {
                    db = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
        let db = db.expect("connect to postgres");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");
        let outbox = Arc::new(QeOutbox::new());
        let enqueuer: Arc<dyn NotificationOutbox> = Arc::clone(&outbox) as _;
        SqlPolicyStore::new(db.clone(), Arc::clone(&enqueuer))
            .create_policy(
                &ctx(),
                PolicyDraft {
                    scope: PolicyScope::Global,
                    engine_id: "cel".to_owned(),
                    engine_config: serde_json::json!({}),
                    timeout_ms: Some(200),
                    description: None,
                    comment: None,
                    created_by: "test".to_owned(),
                    schema_snapshot: PolicySchemaSnapshot::default(),
                },
                &[],
            )
            .await
            .expect("a cold bootstrap seeds with the outbox unbound");
        let quotas = SqlQuotaStore::new(db.clone(), Arc::clone(&enqueuer));
        let store = SqlConsumptionStore::new(db.clone(), enqueuer);
        Self {
            db,
            outbox,
            quotas,
            store,
            _container: container,
        }
    }

    async fn start(&self, delivery: &Arc<RecordingDelivery>) -> OutboxHandle {
        start_notification_pipeline(self.db.clone(), &self.outbox, Arc::clone(delivery) as _)
            .await
            .expect("start delivery")
    }

    /// A second process over the same database: its own outbox handle and
    /// pipeline.
    async fn second_process(&self, delivery: &Arc<RecordingDelivery>) -> OutboxHandle {
        start_notification_pipeline(self.db.clone(), &QeOutbox::new(), Arc::clone(delivery) as _)
            .await
            .expect("start the second pipeline")
    }

    async fn create(
        &self,
        tenant: TenantId,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> Result<(), StoreError> {
        self.quotas
            .create_quota(
                &actor(),
                &AccessScope::for_tenant(tenant.as_uuid()),
                draft,
                events,
            )
            .await
            .map(|_| ())
    }

    async fn debit(&self, holder: &str, amount: u64) {
        let applicable = ApplicableQuotas {
            tenant_id: tenant(),
            subjects: vec![subject(holder)],
            metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
        };
        let idempotency = IdempotencyWrite {
            scope: IdempotencyScope {
                tenant_id: tenant(),
                subject_key: IdempotencySubjectKey::of(&applicable.subjects),
                operation_type: OperationType::Debit,
                key: format!("debit-{holder}-{amount}"),
            },
            payload_hash: PayloadHash::from_bytes([1; 32]),
        };
        let null = serde_json::Value::Null;
        self.store
            .apply_debit_plan(
                &ctx(),
                &AccessScope::for_tenant(tenant().as_uuid()),
                &EvaluatedMutation {
                    applicable: &applicable,
                    amount,
                    request: &null,
                    resource: &null,
                    user_projection: None,
                    limits: limits(),
                    idempotency: &idempotency,
                    authorized: AttributionDigest::from_bytes([7; 32]),
                    evaluate: allow_all(),
                },
                &[],
            )
            .await
            .expect("debit");
    }

    async fn dead_letter_reasons(&self, handle: &OutboxHandle) -> Vec<String> {
        let conn = self.db.conn().expect("connection");
        handle
            .outbox()
            .dead_letter_list(
                &conn,
                &DeadLetterFilter::from_scope(DeadLetterScope::default().queue(NOTIFICATION_QUEUE)),
            )
            .await
            .expect("dead letters")
            .into_iter()
            .filter_map(|letter| letter.last_error)
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cold_start_delivers_a_committed_debit_at_once_and_dead_letters_what_it_must() {
    let h = PgHarness::up().await;
    let err = h
        .create(tenant(), consumption("early", 100), &[changed(tenant())])
        .await
        .expect_err("an event cannot commit before delivery starts");
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");

    let delivery =
        RecordingDelivery::answering(vec![DeliveryOutcome::Reject("sink refused".to_owned())]);
    let handle = h.start(&delivery).await;
    let refused = changed(tenant());
    h.create(
        tenant(),
        consumption("u1", 100),
        std::slice::from_ref(&refused),
    )
    .await
    .expect("create");
    eventually(PROMPTLY, "the Quota's event", || delivery.seen().len() == 1).await;

    // Past half the cap: the debit's own transaction enqueues the crossing.
    h.debit("u1", 60).await;
    eventually(PROMPTLY, "the debit's event", || {
        delivery
            .seen()
            .iter()
            .any(|(_, kind)| *kind == NotificationEventKind::ThresholdCrossed)
    })
    .await;

    // A row no event decodes from, on the tenant's partition.
    {
        let conn = h.db.conn().expect("connection");
        handle
            .outbox()
            .enqueue_batch(
                &conn,
                Records::to(NOTIFICATION_QUEUE)
                    .payload_type("quota-changed")
                    .push(QeOutbox::partition_for(tenant()), b"not an event".to_vec())
                    .build()
                    .expect("record"),
            )
            .await
            .expect("enqueue garbage")
            .fire();
    }
    eventually(PROMPTLY, "the undecodable row", || {
        delivery.undeliverable.lock().expect("undeliverable").len() == 1
    })
    .await;

    let reasons = h.dead_letter_reasons(&handle).await;
    assert_eq!(reasons.len(), 2, "{reasons:?}");
    assert!(reasons.contains(&"sink refused".to_owned()), "{reasons:?}");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.starts_with("undecodable notification event")),
        "{reasons:?}"
    );
    assert_eq!(delivery.seen()[0].0, refused.event_id);
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_pipelines_never_share_a_partition_at_once_and_deliver_every_event() {
    let h = PgHarness::up().await;
    let delivery = RecordingDelivery::pausing(Duration::from_millis(20));
    let first = h.start(&delivery).await;
    let second = h.second_process(&delivery).await;

    let mut expected = HashSet::new();
    for index in 0..8u128 {
        let tenant = TenantId::new(Uuid::from_u128(0x7e_0000 + index));
        for holder in 0..3 {
            let event = changed(tenant);
            expected.insert(event.event_id);
            let mut draft = consumption(&format!("h{holder}"), 100);
            draft.tenant_id = tenant;
            h.create(tenant, draft, &[event]).await.expect("create");
        }
    }
    eventually(Duration::from_mins(1), "every event", || {
        delivery.ids() == expected
    })
    .await;
    assert_eq!(
        *delivery.overlap.lock().expect("overlap"),
        1,
        "the lease fences a partition to one pipeline at a time"
    );
    first.stop().await;
    second.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pipeline_stopped_while_delivering_loses_nothing() {
    let h = PgHarness::up().await;
    let slow = RecordingDelivery::pausing(Duration::from_millis(500));
    let first = h.start(&slow).await;
    let mut expected = HashSet::new();
    for holder in 0..5 {
        let event = changed(tenant());
        expected.insert(event.event_id);
        h.create(tenant(), consumption(&format!("h{holder}"), 100), &[event])
            .await
            .expect("create");
    }
    // Stop while the first event is being delivered.
    tokio::time::sleep(Duration::from_millis(200)).await;
    first.stop().await;

    let rest = RecordingDelivery::pausing(Duration::ZERO);
    let second = h.second_process(&rest).await;
    // The stopped pipeline's lease may have to run out first.
    eventually(Duration::from_secs(90), "the rest of the events", || {
        let mut seen = slow.ids();
        seen.extend(rest.ids());
        seen == expected
    })
    .await;
    second.stop().await;
}
