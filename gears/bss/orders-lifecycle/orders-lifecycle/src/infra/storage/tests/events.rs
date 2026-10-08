//! S2-08: typed events through the real managed producer, on real PostgreSQL (restricted
//! runtime role) and the real in-process Event Broker gear, whose topics and event types are
//! loaded from the real `types-registry` commit of the Orders contract.
//!
//! Test doubles are limited to the two inputs the platform does not ship locally: the broker's
//! PDP (scripted grants; the bundled rules provider cannot express the broker's
//! property-less event-type check) and the tenant resolver's root answer. Transport and
//! rate-limit faults are injected at the producer's `EventBrokerApi` client boundary around the
//! real broker.
use super::*;
use crate::authz::test_pdp::{ScriptedPdp, allow, deny};
use crate::infra::broker::{self, BindFailure, BoundProducer, ProducerSettings};
use crate::infra::events::payload::{
    LineRef, OrderCancelled, OrderHeld, OrderResumed, OrderSubmitted,
};
use crate::infra::events::test_support::{Root, root};
use crate::infra::events::test_support::{encoded_max_events, max_events, registry_client};
use crate::infra::events::{
    self as events, EventEnqueueError, EventKind, EventSink, OrderSummary, TOPIC, TxEvents,
};
use crate::infra::storage::repo::TransactionRunner;
use crate::infra::storage::repo::audit as writer;
use bss_orders_lifecycle_sdk::catalog::{OrderState, Trigger};
use event_broker::test_support::{
    DeliveredOutcome, DeliveredPublish, EventBrokerHarness, FaultInjectingBroker, TransientFault,
};
use event_broker_sdk::{EventBrokerApi, Sequence};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use tenant_resolver_sdk::TenantResolverClient;
use toolkit_security::AccessScope;

const BROKER_PARTITIONS: u32 = 4;
const ROOT: u128 = 1_000;
const PRODUCER: u128 = 1_100;

fn settings(partitions: u32) -> ProducerSettings {
    ProducerSettings {
        subject_id: u(PRODUCER),
        tenant_id: u(1_101),
        broker_partitions: partitions,
    }
}
fn fast_retry() -> toolkit_db::outbox::WorkerTuning {
    toolkit_db::outbox::WorkerTuning::processor_high_throughput()
        .retry_base(Duration::from_millis(20))
        .retry_max(Duration::from_millis(100))
}

/// Explicit grants at the broker's PEP: only the Orders producer may produce the Orders event
/// family, and only under the platform-root tenant. `granted` revokes the tenant grant.
fn grants(granted: Arc<AtomicBool>) -> Arc<ScriptedPdp> {
    ScriptedPdp::new(move |request| {
        let producer = request.subject.id == u(PRODUCER);
        let produce = request.action.name == "produce";
        let decision = match request.resource.resource_type.as_str() {
            "gts.cf.core.events.event_type.v1~" => {
                producer
                    && produce
                    && request
                        .resource
                        .properties
                        .get("event_type_id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|t| t.starts_with(events::EVENT_BASE))
            }
            "gts.cf.core.events.request.v1~" => {
                producer
                    && produce
                    && granted.load(Ordering::SeqCst)
                    && request
                        .resource
                        .properties
                        .get("owner_tenant_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(u(ROOT).to_string().as_str())
            }
            _ => false,
        };
        Some(if decision { allow(vec![]) } else { deny(None) })
    })
}

pub(super) struct Env {
    pub(super) pg: Pg,
    pub(super) db: Db,
    harness: EventBrokerHarness,
    faulty: Arc<FaultInjectingBroker>,
    pdp: Arc<ScriptedPdp>,
    granted: Arc<AtomicBool>,
}
impl Env {
    pub(super) async fn new() -> anyhow::Result<Self> {
        Self::with_broker_partitions(BROKER_PARTITIONS).await
    }
    async fn with_broker_partitions(partitions: u32) -> anyhow::Result<Self> {
        let pg = Pg::new().await?;
        let (db, _) = pg.role("runtime").await?;
        let granted = Arc::new(AtomicBool::new(true));
        let pdp = grants(Arc::clone(&granted));
        let harness = EventBrokerHarness::builder()
            .with_types_registry_client(registry_client(), &[(TOPIC, partitions)])
            .with_policy_enforcer(authz_resolver_sdk::PolicyEnforcer::new(
                Arc::clone(&pdp) as Arc<dyn authz_resolver_sdk::AuthZResolverApi>
            ))
            .build()
            .await;
        let faulty = FaultInjectingBroker::new(harness.broker());
        Ok(Self {
            pg,
            db,
            harness,
            faulty,
            pdp,
            granted,
        })
    }
    async fn bind_declaring(&self, partitions: u32) -> Result<BoundProducer, broker::BindError> {
        Box::pin(broker::bind(
            Some(Arc::clone(&self.faulty) as Arc<dyn EventBrokerApi>),
            Some(root(ROOT, None)),
            self.db.clone(),
            &settings(partitions),
            Duration::from_secs(10),
            Some(fast_retry()),
        ))
        .await
    }
    pub(super) async fn bind(&self) -> BoundProducer {
        self.bind_declaring(BROKER_PARTITIONS).await.unwrap()
    }
    pub(super) async fn stored(&self) -> Vec<event_broker_sdk::Event> {
        let ctx = self.harness.security_context();
        let mut all = Vec::new();
        for partition in 0..BROKER_PARTITIONS {
            all.extend(
                self.harness
                    .backend()
                    .read(ctx, TOPIC, partition, Sequence::NONE, 4_096)
                    .await
                    .unwrap(),
            );
        }
        all
    }
    pub(super) async fn wait_stored(&self, n: usize) -> Vec<event_broker_sdk::Event> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let stored = self.stored().await;
            if stored.len() >= n {
                return stored;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: {} of {n} events stored",
                stored.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn dead_letters(&self) -> i64 {
        self.pg
            .scalar("SELECT count(*) AS n FROM toolkit_outbox_dead_letters")
            .await
            .unwrap()
    }
    async fn wait_dead_letters(&self, n: i64) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while self.dead_letters().await < n {
            assert!(tokio::time::Instant::now() < deadline, "no dead letter");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn registration(&self) -> (Uuid, i64, String) {
        let row = self
            .pg
            .raw
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                "SELECT producer_id, generation, mode FROM event_broker_producer_registrations WHERE registration_key='bss-orders-events-v1'",
            ))
            .await
            .unwrap()
            .expect("durable producer registration");
        (
            row.try_get("", "producer_id").unwrap(),
            row.try_get("", "generation").unwrap(),
            row.try_get("", "mode").unwrap(),
        )
    }
}

fn scope(id: u128) -> AccessScope {
    AccessScope::for_resources(vec![u(id)])
}
fn summary_of(row: &entity::order::Model) -> OrderSummary {
    OrderSummary {
        order_id: row.order_id,
        order_version: row.current_version,
        occurred_at: now(),
        correlation_id: u(77),
        category: row.category.clone(),
        state: serde_json::from_value(serde_json::json!(row.state)).unwrap(),
        resource_tenant_id: row.resource_tenant_id,
        seller_tenant_id: row.seller_tenant_id,
        payer_tenant_id: row.payer_tenant_id,
        contract_id: row.contract_id,
        external_reference: None,
    }
}
async fn locked<T: TransactionRunner>(
    tx: &T,
    id: u128,
) -> anyhow::Result<repo::LockedOrder<'_, T>> {
    repo::LockedOrder::lock_current(tx, &scope(id), u(id))
        .await?
        .ok_or_else(|| anyhow::anyhow!("order {id} not visible"))
}
/// Apply `state` to the locked aggregate and enqueue `detail` describing that post-state.
async fn apply<T: TransactionRunner + Sync, K: EventKind>(
    tx: &T,
    events: &TxEvents,
    id: u128,
    state: &str,
    detail: K,
) -> anyhow::Result<()> {
    let mut locked = locked(tx, id).await?;
    let mut row = locked.row().clone();
    row.state = state.to_owned();
    locked.replace(&scope(id), row).await?;
    let summary = summary_of(locked.row());
    events.enqueue(&locked, summary, detail).await?;
    Ok(())
}
fn db_err(e: &anyhow::Error) -> Option<&sea_orm::DbErr> {
    e.downcast_ref::<sea_orm::DbErr>()
}
/// One transition transaction through the Orders event runner.
async fn transition<K: EventKind>(
    env: &Env,
    sink: &EventSink,
    id: u128,
    state: &'static str,
    detail: K,
) -> anyhow::Result<()> {
    events::transaction(
        &env.db,
        sink,
        repo::claims::transition_tx_config(),
        1,
        db_err,
        move |tx, events| {
            let detail = detail.clone();
            Box::pin(async move { apply(tx, &events, id, state, detail).await })
        },
    )
    .await
}
fn submitted() -> OrderSubmitted {
    OrderSubmitted {
        lines: vec![LineRef { line_id: u(5) }],
        acceptance: None,
    }
}
fn cancelled() -> OrderCancelled {
    OrderCancelled {
        cancelling_actor: u(40).to_string(),
        cancel_reason: "customer withdrew".into(),
        compensation_evidence: None,
    }
}
async fn count(pg: &Pg, sql: &str) -> i64 {
    pg.scalar(sql).await.unwrap()
}
/// Sequenced producer messages the leased processor has not yet acknowledged.
const UNACKNOWLEDGED: &str = "SELECT count(*) AS n FROM toolkit_outbox_outgoing o \
     LEFT JOIN toolkit_outbox_processor p ON p.partition_id = o.partition_id \
     WHERE o.seq > COALESCE(p.processed_seq, 0)";

/// D-200 / §4.4: a committed transition yields exactly one durable producer message, fired
/// after commit and delivered with the explicit root tenant, `bss-orders-lifecycle` source,
/// order-UUID subject and `/subject` routing; the registration is durable Chained.
#[tokio::test]
async fn committed_transitions_deliver_root_tenant_events_routed_by_order() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    assert_eq!(bound.sink().root().id(), u(ROOT));
    let (producer_id, generation, mode) = env.registration().await;
    assert_eq!((generation, mode.as_str()), (1, "chained"));
    let orders: Vec<u128> = (300..308).collect();
    for id in &orders {
        create(&env.db, *id).await?;
        transition(&env, bound.sink(), *id, "submitted", submitted()).await?;
    }
    let stored = env.wait_stored(orders.len()).await;
    assert_eq!(
        stored.len(),
        orders.len(),
        "one event per committed transition"
    );
    let mut partitions = std::collections::BTreeSet::new();
    for event in &stored {
        assert_eq!(
            event.type_id.as_ref(),
            <events::OrderEvent<OrderSubmitted> as event_broker_sdk::TypedEvent>::TYPE_ID
        );
        assert_eq!(
            event.tenant_id,
            u(ROOT),
            "explicit platform-root envelope tenant"
        );
        assert_eq!(event.source, "bss-orders-lifecycle");
        assert_eq!(event.subject_type.as_ref(), "gts.cf.bss.orders.order.v1~");
        let data = event.data.as_ref().unwrap();
        assert_eq!(data["orderId"], serde_json::json!(event.subject));
        assert_eq!(data["resourceTenantId"], serde_json::json!(u(10)));
        assert!(Uuid::parse_str(&event.subject).is_ok());
        partitions.insert(event.partition);
    }
    assert!(
        partitions.len() > 1,
        "routing follows /subject (order UUID), not the shared root tenant: {partitions:?}"
    );
    // Same order, later transition: same partition (per-order ordering).
    transition(&env, bound.sink(), 300, "cancelled", cancelled()).await?;
    let stored = env.wait_stored(orders.len() + 1).await;
    let of_300: Vec<_> = stored
        .iter()
        .filter(|e| e.subject == u(300).to_string())
        .collect();
    assert_eq!(of_300.len(), 2);
    assert_eq!(of_300[0].partition, of_300[1].partition);
    assert_eq!(env.dead_letters().await, 0);
    assert_eq!(env.registration().await.0, producer_id);
    bound.stop().await;
    Ok(())
}

/// §4.4: order, sealed audit and outbox enqueue commit or roll back together; a retried attempt
/// discards its wake and leaves exactly one message; the enqueue API admits one event per
/// transition, only in the transaction holding the order's lock, describing its post-state.
#[tokio::test]
async fn enqueue_is_atomic_with_the_transition_and_once_per_attempt() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    let sink = bound.sink().clone();
    create(&env.db, 400).await?;
    let audits = || {
        count(
            &env.pg,
            "SELECT count(*) AS n FROM bss_orders__transition_audit",
        )
    };
    let bodies = || count(&env.pg, "SELECT count(*) AS n FROM toolkit_outbox_body");
    let (audits_before, bodies_before) = (audits().await, bodies().await);

    // 1. A failure after order mutation, sealed audit and enqueue rolls all three back.
    let failed: anyhow::Result<()> = events::transaction(
        &env.db,
        &sink,
        repo::claims::transition_tx_config(),
        1,
        db_err,
        |tx, events| {
            Box::pin(async move {
                let mut locked = locked(tx, 400).await?;
                let before = writer::order_facts(locked.row())?;
                let mut row = locked.row().clone();
                row.state = "cancelled".into();
                locked.replace(&scope(400), row).await?;
                let after = locked.audit_facts()?;
                let pending = super::audit::evidence(40).committed_transition(
                    Uuid::new_v4(),
                    crate::domain::audit::AuditTrigger::Public(Trigger::Cancel),
                    &before,
                    &after,
                    Some("customer withdrew"),
                )?;
                writer::append_committed(&mut locked, pending).await?;
                let summary = summary_of(locked.row());
                events.enqueue(&locked, summary, cancelled()).await?;
                anyhow::bail!("deliberate failure after audit and enqueue")
            })
        },
    )
    .await;
    assert!(failed.is_err());
    assert_eq!(
        count(
            &env.pg,
            &format!(
                "SELECT count(*) AS n FROM bss_orders__order WHERE order_id='{}' AND state='draft'",
                u(400)
            )
        )
        .await,
        1
    );
    assert_eq!(audits().await, audits_before);
    assert_eq!(bodies().await, bodies_before);

    // 2. A retryable failure after the enqueue: the retried attempt commits exactly one event.
    let attempts = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&attempts);
    events::transaction(
        &env.db,
        &sink,
        repo::claims::transition_tx_config(),
        3,
        db_err,
        move |tx, events| {
            let attempt = seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                apply(tx, &events, 400, "submitted", submitted()).await?;
                if attempt == 0 {
                    // The driver's serialization-failure answer: the runner rolls back and
                    // retries this attempt (`toolkit_db::contention`).
                    return Err(sea_orm::DbErr::Custom(
                        "could not serialize access due to concurrent update".into(),
                    )
                    .into());
                }
                anyhow::Ok(())
            })
        },
    )
    .await?;
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "the first attempt was retried"
    );
    let stored = env.wait_stored(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        env.stored().await.len(),
        1,
        "only the committed attempt's event exists"
    );
    assert_eq!(
        stored[0].data.as_ref().unwrap()["state"],
        serde_json::json!("submitted")
    );

    // 3. One event per transition; only in the lock's own transaction; only its post-state.
    create(&env.db, 401).await?;
    let second: anyhow::Result<()> = events::transaction(
        &env.db,
        &sink,
        repo::claims::transition_tx_config(),
        1,
        db_err,
        |tx, events| {
            Box::pin(async move {
                apply(tx, &events, 401, "submitted", submitted()).await?;
                let locked = locked(tx, 401).await?;
                let again = events
                    .enqueue(&locked, summary_of(locked.row()), submitted())
                    .await;
                assert!(matches!(again, Err(EventEnqueueError::AlreadyEnqueued)));
                anyhow::bail!("rolled back")
            })
        },
    )
    .await;
    assert!(second.is_err());
    let misdescribed: anyhow::Result<()> = events::transaction(
        &env.db,
        &sink,
        repo::claims::transition_tx_config(),
        1,
        db_err,
        |tx, events| {
            Box::pin(async move {
                let locked = locked(tx, 401).await?;
                let mut summary = summary_of(locked.row());
                summary.payer_tenant_id = u(31);
                let result = events.enqueue(&locked, summary, submitted()).await;
                assert!(matches!(result, Err(EventEnqueueError::NotLockedPostState)));
                let mut summary = summary_of(locked.row());
                summary.state = OrderState::Submitted;
                let result = events.enqueue(&locked, summary, submitted()).await;
                assert!(matches!(result, Err(EventEnqueueError::NotLockedPostState)));
                anyhow::bail!("rolled back")
            })
        },
    )
    .await;
    assert!(misdescribed.is_err());
    // A lock held by a different transaction cannot enqueue through this attempt's handle.
    let foreign_db = env.db.clone();
    let foreign: anyhow::Result<()> = events::transaction(
        &env.db,
        &sink,
        repo::claims::transition_tx_config(),
        1,
        db_err,
        move |_tx, events| {
            let foreign_db = foreign_db.clone();
            Box::pin(async move {
                foreign_db
                    .transaction_ref_mapped(move |other| {
                        Box::pin(async move {
                            let locked = locked(other, 401).await?;
                            let result = events
                                .enqueue(&locked, summary_of(locked.row()), submitted())
                                .await;
                            assert!(matches!(result, Err(EventEnqueueError::ForeignTransaction)));
                            anyhow::Ok(())
                        })
                    })
                    .await?;
                anyhow::bail!("rolled back")
            })
        },
    )
    .await;
    assert!(foreign.is_err());
    assert_eq!(
        bodies().await - bodies_before,
        1,
        "one message: the retried commit"
    );
    assert_eq!(env.dead_letters().await, 0);
    bound.stop().await;
    Ok(())
}

/// D-200: a failed enqueue cannot commit, even if the work drops its error and returns success.
/// The runner rolls the attempt back without retrying it as contention, so no order state
/// commits without its producer message.
#[tokio::test]
async fn a_failed_enqueue_never_commits_even_if_the_error_is_dropped() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    let sink = bound.sink().clone();
    create(&env.db, 401).await?;
    let bodies = || count(&env.pg, "SELECT count(*) AS n FROM toolkit_outbox_body");
    let bodies_before = bodies().await;
    let attempts = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&attempts);
    let swallowed: anyhow::Result<()> = events::transaction(
        &env.db,
        &sink,
        repo::claims::transition_tx_config(),
        3,
        db_err,
        move |tx, events| {
            seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let mut locked = locked(tx, 401).await?;
                let mut row = locked.row().clone();
                row.state = "submitted".into();
                locked.replace(&scope(401), row).await?;
                let mut summary = summary_of(locked.row());
                summary.payer_tenant_id = u(31);
                let dropped = events.enqueue(&locked, summary, submitted()).await;
                assert!(matches!(
                    dropped,
                    Err(EventEnqueueError::NotLockedPostState)
                ));
                anyhow::Ok(())
            })
        },
    )
    .await;
    let error = swallowed.expect_err("a failed enqueue never commits");
    assert!(format!("{error:#}").contains("cannot commit"), "{error:#}");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "never retried as contention"
    );
    assert_eq!(
        count(
            &env.pg,
            &format!(
                "SELECT count(*) AS n FROM bss_orders__order WHERE order_id='{}' AND state='draft'",
                u(401)
            )
        )
        .await,
        1,
        "the order mutation rolled back"
    );
    assert_eq!(bodies().await, bodies_before, "no producer message");
    bound.stop().await;
    Ok(())
}

/// Broker outcomes acknowledge once: accepted, persisted, and a lost acknowledgement retried
/// into the broker's duplicate answer, keeping the original event identity.
#[tokio::test]
async fn accepted_persisted_and_duplicate_outcomes_acknowledge_once() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    for id in 500..503 {
        create(&env.db, id).await?;
    }
    transition(&env, bound.sink(), 500, "submitted", submitted()).await?;
    env.wait_stored(1).await;
    env.faulty.report_persisted(1);
    transition(&env, bound.sink(), 501, "submitted", submitted()).await?;
    env.wait_stored(2).await;
    let delivered = env.faulty.delivered();
    env.faulty.lose_publish_acks(1);
    transition(&env, bound.sink(), 502, "submitted", submitted()).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while env.faulty.delivered() < delivered + 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "lost ack never retried"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stored = env.stored().await;
    let of_502 = stored
        .iter()
        .filter(|e| e.subject == u(502).to_string())
        .count();
    assert_eq!(of_502, 1, "the redelivered message is a broker duplicate");
    assert_eq!(stored.len(), 3);
    // The retry is the same durable message: event id, producer id and `meta.sequence` are
    // unchanged, and the broker answers it as the duplicate of the append it already made.
    let attempts: Vec<DeliveredPublish> = env
        .faulty
        .delivered_publishes()
        .into_iter()
        .filter(|p| p.subject == u(502).to_string())
        .collect();
    assert_eq!(attempts.len(), 2, "{attempts:#?}");
    assert!(attempts[0].ack_lost && attempts[0].outcome == DeliveredOutcome::Accepted);
    assert_eq!(attempts[1].outcome, DeliveredOutcome::Duplicate);
    assert_same_message(&attempts[0], &attempts[1]);
    let stored_502 = stored
        .iter()
        .find(|e| e.subject == u(502).to_string())
        .unwrap();
    assert_eq!(stored_502.id, attempts[0].event_id);
    assert_eq!(env.dead_letters().await, 0);
    assert_eq!(
        count(&env.pg, UNACKNOWLEDGED).await,
        0,
        "every message acknowledged"
    );
    assert_eq!(
        count(&env.pg, "SELECT count(*) AS n FROM toolkit_outbox_outgoing").await,
        3,
        "each transition sequenced exactly one message"
    );
    bound.stop().await;
    Ok(())
}

/// Two deliveries carry the same durable message: event id, producer id and chain sequence.
fn assert_same_message(first: &DeliveredPublish, retry: &DeliveredPublish) {
    assert_eq!(retry.event_id, first.event_id, "original event id");
    let (a, b) = (first.meta.as_ref().unwrap(), retry.meta.as_ref().unwrap());
    assert_eq!(b.producer_id, a.producer_id, "stable producer identity");
    assert_eq!(b.sequence, a.sequence, "stable meta.sequence");
    assert_eq!(b.partition_hint, a.partition_hint);
}

/// DESIGN §4.4 *Required regression evidence*: the broker appends an event, the response is
/// lost and the producer stays disconnected until it restarts. After the restart the new
/// workers start with an empty cursor cache, recover the chain cursor from the broker and
/// retry the same durable message: stable event id, producer identity and `meta.sequence`,
/// one broker append, eventual acknowledgement and no dead letter. Committed queue work
/// survives the stop; the chain continues afterwards.
#[tokio::test]
async fn lost_acknowledgement_retries_after_restart_with_cursor_recovery() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    for id in 520..523 {
        create(&env.db, id).await?;
    }
    transition(&env, bound.sink(), 520, "submitted", submitted()).await?;
    env.wait_stored(1).await;
    let (producer_id, generation, _) = env.registration().await;

    env.faulty.lose_next_ack_then_disconnect();
    let reads_before = env.faulty.cursor_reads();
    transition(&env, bound.sink(), 521, "submitted", submitted()).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !env.faulty.is_disconnected() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the publish never reached the broker"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The broker holds the event; the producer saw a transport fault and keeps retrying
    // while disconnected, without dead-lettering or acknowledging the message.
    env.wait_stored(2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.dead_letters().await, 0);
    assert_eq!(
        count(&env.pg, UNACKNOWLEDGED).await,
        1,
        "the unacknowledged message is still queued"
    );
    bound.stop().await;

    // Restart: new workers, empty cursor cache, same durable registration.
    env.faulty.reconnect();
    let bound = env.bind().await;
    assert_eq!(
        env.registration().await,
        (producer_id, generation, "chained".to_owned())
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while count(&env.pg, UNACKNOWLEDGED).await > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the message was never acknowledged"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        env.faulty.cursor_reads() > reads_before,
        "the restarted processor recovered the chain cursor from the broker"
    );
    let attempts: Vec<DeliveredPublish> = env
        .faulty
        .delivered_publishes()
        .into_iter()
        .filter(|p| p.subject == u(521).to_string())
        .collect();
    assert_eq!(
        attempts.len(),
        2,
        "one lost append, one retry: {attempts:#?}"
    );
    assert!(attempts[0].ack_lost && attempts[0].outcome == DeliveredOutcome::Accepted);
    assert_eq!(
        attempts[1].outcome,
        DeliveredOutcome::Duplicate,
        "the recovered predecessor makes the retry the broker's idempotent duplicate"
    );
    assert_same_message(&attempts[0], &attempts[1]);
    let stored = env.stored().await;
    let of_521: Vec<_> = stored
        .iter()
        .filter(|e| e.subject == u(521).to_string())
        .collect();
    assert_eq!(of_521.len(), 1, "one broker append");
    assert_eq!(of_521[0].id, attempts[0].event_id);

    // The chain continues after the recovered message.
    transition(&env, bound.sink(), 522, "submitted", submitted()).await?;
    let stored = env.wait_stored(3).await;
    assert!(stored.iter().any(|e| e.subject == u(522).to_string()));
    assert_eq!(env.dead_letters().await, 0);
    bound.stop().await;
    Ok(())
}

/// Transient transport and rate-limit failures retry without dead letters: on the very first
/// message (empty producer cursor cache, so the initial cursor read fails) and later on
/// publish. Per-order FIFO holds across the retries.
#[tokio::test]
async fn initial_and_later_transient_failures_retry_in_order() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    create(&env.db, 600).await?;
    let reads = env.faulty.cursor_reads();
    env.faulty
        .fail_cursor_reads([TransientFault::Transport, TransientFault::RateLimited]);
    transition(&env, bound.sink(), 600, "submitted", submitted()).await?;
    let stored = env.wait_stored(1).await;
    assert!(
        env.faulty.cursor_reads() >= reads + 3,
        "two failed initial cursor reads, then a successful one"
    );
    assert_eq!(
        env.dead_letters().await,
        0,
        "a transient initial read is never a dead letter"
    );
    let first_id = stored[0].id;

    env.faulty.fail_publishes([
        TransientFault::RateLimited,
        TransientFault::Transport,
        TransientFault::RateLimited,
    ]);
    transition(
        &env,
        bound.sink(),
        600,
        "on_hold",
        OrderHeld {
            previous_state: OrderState::Submitted,
            hold_reason: None,
        },
    )
    .await?;
    transition(
        &env,
        bound.sink(),
        600,
        "submitted",
        OrderResumed {
            restored_state: OrderState::Submitted,
        },
    )
    .await?;
    let mut stored = env.wait_stored(3).await;
    stored.sort_by_key(|e| e.sequence);
    let states: Vec<_> = stored
        .iter()
        .map(|e| e.data.as_ref().unwrap()["state"].clone())
        .collect();
    assert_eq!(
        states,
        vec![
            serde_json::json!("submitted"),
            serde_json::json!("on_hold"),
            serde_json::json!("submitted")
        ],
        "partition FIFO is retained across transient retries"
    );
    assert_eq!(stored[0].id, first_id);
    assert_eq!(env.dead_letters().await, 0);
    bound.stop().await;
    Ok(())
}

/// Restart keeps the durable producer identity and chain; a broker that forgot the producer
/// permanently rejects the in-flight message (inspectable dead letter, unchanged payload and
/// event id), rotates the registration and lets later messages proceed. The order is untouched.
#[tokio::test]
async fn unknown_producer_restart_and_permanent_rejection() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    for id in 700..704 {
        create(&env.db, id).await?;
    }
    transition(&env, bound.sink(), 700, "submitted", submitted()).await?;
    env.wait_stored(1).await;
    let (first, generation, _) = env.registration().await;
    assert_eq!(generation, 1);
    bound.stop().await;

    // Restart: the stored registration is reused; the chain continues without rejection.
    let bound = env.bind().await;
    assert_eq!(
        env.registration().await.0,
        first,
        "durable producer identity"
    );
    transition(&env, bound.sink(), 701, "submitted", submitted()).await?;
    env.wait_stored(2).await;
    assert_eq!(env.dead_letters().await, 0);

    // The broker forgets the producer: the in-flight message is rejected, not retried forever.
    env.harness.forget_producer(first).await;
    transition(&env, bound.sink(), 702, "submitted", submitted()).await?;
    env.wait_dead_letters(1).await;
    let (rotated, generation, _) = env.registration().await;
    assert_ne!(
        rotated, first,
        "replacement registration for future enqueues"
    );
    assert_eq!(generation, 2);
    let letter = env
        .pg
        .raw
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT payload, last_error FROM toolkit_outbox_dead_letters",
        ))
        .await?
        .unwrap();
    let payload: Vec<u8> = letter.try_get("", "payload")?;
    let envelope: serde_json::Value = serde_json::from_slice(&payload)?;
    assert_eq!(envelope["subject"], serde_json::json!(u(702).to_string()));
    assert_eq!(envelope["tenant_id"], serde_json::json!(u(ROOT)));
    assert_eq!(envelope["data"]["orderId"], serde_json::json!(u(702)));
    assert!(Uuid::parse_str(envelope["event_id"].as_str().unwrap()).is_ok());
    let error: Option<String> = letter.try_get("", "last_error")?;
    assert!(error.unwrap_or_default().contains("unknown"));
    // The committed order is authoritative; a dead letter is never an order state.
    assert_eq!(
        count(&env.pg, &format!("SELECT count(*) AS n FROM bss_orders__order WHERE order_id='{}' AND state='submitted'", u(702))).await,
        1
    );
    // Later messages proceed under the rotated registration.
    transition(&env, bound.sink(), 703, "submitted", submitted()).await?;
    let stored = env.wait_stored(3).await;
    assert!(stored.iter().any(|e| e.subject == u(703).to_string()));
    assert!(!stored.iter().any(|e| e.subject == u(702).to_string()));
    assert_eq!(env.dead_letters().await, 1);
    bound.stop().await;
    Ok(())
}

/// Grants: the producer publishes only under the root tenant it is authorized for. A missing
/// root grant is a permanent rejection (dead letter, cursor advances); restored, later
/// messages proceed. Business tenant axes never reach the envelope tenant.
#[tokio::test]
async fn root_tenant_grant_is_required_and_rejection_advances() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    for id in 800..802 {
        create(&env.db, id).await?;
    }
    env.granted.store(false, Ordering::SeqCst);
    transition(&env, bound.sink(), 800, "submitted", submitted()).await?;
    env.wait_dead_letters(1).await;
    env.granted.store(true, Ordering::SeqCst);
    transition(&env, bound.sink(), 801, "submitted", submitted()).await?;
    let stored = env.wait_stored(1).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].subject, u(801).to_string());
    let tenants: Vec<String> = env
        .pdp
        .requests
        .lock()
        .iter()
        .filter(|r| r.resource.resource_type == "gts.cf.core.events.request.v1~")
        .filter_map(|r| {
            r.resource
                .properties
                .get("owner_tenant_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert!(!tenants.is_empty());
    assert!(
        tenants.iter().all(|t| *t == u(ROOT).to_string()),
        "the broker authorized only the root envelope tenant: {tenants:?}"
    );
    bound.stop().await;
    Ok(())
}

/// Readiness requires each prerequisite; a failed binding enqueues nothing.
#[tokio::test]
async fn binding_fails_closed_without_each_prerequisite() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let limit = Duration::from_secs(5);
    let attempt = |broker: Option<Arc<dyn EventBrokerApi>>,
                   tenants: Option<Arc<dyn TenantResolverClient>>,
                   db: Db,
                   partitions: u32| async move {
        Box::pin(broker::bind(
            broker,
            tenants,
            db,
            &settings(partitions),
            limit,
            None,
        ))
        .await
        .err()
        .map(|e| e.failure)
    };
    let real = || Some(Arc::clone(&env.faulty) as Arc<dyn EventBrokerApi>);
    assert_eq!(
        attempt(None, Some(root(ROOT, None)), env.db.clone(), 4).await,
        Some(BindFailure::BrokerUnavailable)
    );
    assert_eq!(
        attempt(real(), None, env.db.clone(), 4).await,
        Some(BindFailure::TenantResolverUnavailable)
    );
    assert_eq!(
        attempt(real(), Some(Arc::new(Root(None))), env.db.clone(), 4).await,
        Some(BindFailure::RootTenantUnavailable)
    );
    assert_eq!(
        attempt(real(), Some(root(ROOT, Some(1))), env.db.clone(), 4).await,
        Some(BindFailure::RootTenantInvalid)
    );
    assert_eq!(
        attempt(real(), Some(root(ROOT, None)), env.db.clone(), 0).await,
        Some(BindFailure::ProducerRegistration),
        "no partition count is assumed"
    );
    // A broker that never loaded the Orders contract: schema preparation fails.
    let bare = EventBrokerHarness::builder().build().await;
    assert_eq!(
        attempt(
            Some(bare.broker()),
            Some(root(ROOT, None)),
            env.db.clone(),
            4
        )
        .await,
        Some(BindFailure::SchemaPreparation)
    );
    // No platform producer-registration table: durable registration fails.
    let unmigrated = Pg::bare().await?;
    assert_eq!(
        attempt(real(), Some(root(ROOT, None)), unmigrated.db.clone(), 4).await,
        Some(BindFailure::ProducerRegistration)
    );
    assert_eq!(
        count(
            &env.pg,
            "SELECT count(*) AS n FROM event_broker_producer_registrations"
        )
        .await,
        0,
        "no failed attempt registered a producer"
    );
    let bound = env.bind().await;
    assert_eq!(env.registration().await.1, 1);
    bound.stop().await;
    Ok(())
}

/// §6: every event type at its 200-line maximum passes the broker-prepared schema and the
/// toolkit 64 KiB payload limit through the real serializer; data beyond the published bounds
/// is refused by the prepared schema and rolls the transition back.
#[tokio::test]
async fn maximal_envelopes_fit_and_oversize_events_roll_back() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    let sink = bound.sink().clone();
    create(&env.db, 900).await?;
    let max = max_events(u(900));
    macro_rules! enqueue_max {
        ($($pair:expr),+ $(,)?) => {$(
            let (summary, detail) = $pair;
            events::transaction(&env.db, &sink, repo::claims::transition_tx_config(), 1, db_err,
                move |tx, events| {
                    let (summary, detail) = (summary.clone(), detail.clone());
                    Box::pin(async move {
                        let locked = locked(tx, 900).await?;
                        events.enqueue_unchecked(&locked, summary, detail).await?;
                        anyhow::Ok(())
                    })
                }).await?;
        )+};
    }
    enqueue_max!(
        max.submitted,
        max.approved,
        max.rejected,
        max.amended,
        max.held,
        max.resumed,
        max.cancelled,
        max.expired,
        max.completed,
        max.failed,
        max.forced,
        max.acceptance,
    );
    let largest = count(
        &env.pg,
        "SELECT COALESCE(max(octet_length(payload)), 0)::bigint AS n FROM toolkit_outbox_body",
    )
    .await;
    let largest_data = encoded_max_events(u(900))
        .into_iter()
        .map(|(_, d)| d.len())
        .max()
        .unwrap();
    assert!(
        largest > i64::try_from(largest_data)?,
        "the envelope wraps the data"
    );
    assert!(
        largest <= i64::try_from(events::MAX_ENVELOPE_BYTES)?,
        "largest serialized envelope {largest} exceeds 64 KiB"
    );
    // Beyond the published bounds: the prepared schema refuses; nothing is committed.
    let bodies = count(&env.pg, "SELECT count(*) AS n FROM toolkit_outbox_body").await;
    let (summary, mut over) = max_events(u(900)).completed;
    over.lines
        .push(crate::infra::events::payload::LineSubscription {
            line_id: u(99_999),
            subscription_id: u(99_998),
        });
    let refused: anyhow::Result<()> = events::transaction(
        &env.db,
        &sink,
        repo::claims::transition_tx_config(),
        1,
        db_err,
        move |tx, events| {
            let (summary, over) = (summary.clone(), over.clone());
            Box::pin(async move {
                let locked = locked(tx, 900).await?;
                events.enqueue_unchecked(&locked, summary, over).await?;
                anyhow::Ok(())
            })
        },
    )
    .await;
    let error = refused.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<EventEnqueueError>(),
        Some(EventEnqueueError::Producer(_))
    ));
    assert_eq!(
        count(&env.pg, "SELECT count(*) AS n FROM toolkit_outbox_body").await,
        bodies
    );
    // The checked path refuses the same event before the producer is reached.
    let (summary, mut over) = max_events(u(900)).completed;
    over.lines
        .push(crate::infra::events::payload::LineSubscription {
            line_id: u(99_999),
            subscription_id: u(99_998),
        });
    assert!(events::payload::EventKind::validate(&over, &summary).is_err());
    env.wait_stored(12).await;
    assert_eq!(
        env.dead_letters().await,
        0,
        "every maximal event is broker-valid"
    );
    bound.stop().await;
    Ok(())
}

/// Commit one transition per order, settling each message before the next commit so the
/// outcome is deterministic, and attribute every committed event: stored, dead-lettered, or
/// acknowledged without being stored, by the broker's last answer to its publish.
struct Probe {
    committed: usize,
    stored: usize,
    dead_letters: usize,
    /// Broker `DuplicateIgnore` for a different event: its `meta.sequence` equalled the head
    /// of the chain at the broker-selected partition (SDK maps `Duplicate` to `Ok`).
    broker_duplicate: Vec<DeliveredPublish>,
    /// Broker `SequenceViolation`, then the SDK's `handle_sequence_violation` refreshed the
    /// cursor of the *declared* partition and acknowledged `seq <= previous` as already
    /// sequenced, without publishing again.
    sdk_already_sequenced: Vec<DeliveredPublish>,
    unattributed: usize,
}
fn answer(outcome: &DeliveredOutcome) -> &str {
    match outcome {
        DeliveredOutcome::Accepted => "accepted",
        DeliveredOutcome::Persisted => "persisted",
        DeliveredOutcome::Duplicate => "duplicate",
        DeliveredOutcome::SequenceViolation => "sequence-violation",
        DeliveredOutcome::UnknownProducer => "unknown-producer",
        DeliveredOutcome::Refused(detail) => detail,
    }
}
fn describe(p: &DeliveredPublish) -> String {
    let meta = p.meta.as_ref();
    let num = |v: Option<i64>| v.map_or_else(|| "none".to_owned(), |v| v.to_string());
    format!(
        "{} seq={} previous={} declared_partition={} broker_answer={}",
        p.subject,
        num(meta.and_then(|m| m.sequence)),
        num(meta.and_then(|m| m.previous)),
        num(meta.and_then(|m| m.partition_hint).map(i64::from)),
        answer(&p.outcome),
    )
}
async fn settle_each(env: &Env, sink: &EventSink, orders: &[u128]) -> anyhow::Result<()> {
    let processed = || {
        count(
            &env.pg,
            "SELECT COALESCE(sum(processed_seq), 0)::bigint AS n FROM toolkit_outbox_processor",
        )
    };
    let base = processed().await;
    for (n, id) in orders.iter().enumerate() {
        create(&env.db, *id).await?;
        transition(env, sink, *id, "submitted", submitted()).await?;
        let target = base + i64::try_from(n)? + 1;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while processed().await < target {
            assert!(
                tokio::time::Instant::now() < deadline,
                "message {n} never settled"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    Ok(())
}
/// Broker acceptance precedes persistence (its ingest outbox persists asynchronously): wait
/// until every event the broker accepted is readable before classifying anything.
async fn stored_after_persistence(
    env: &Env,
    subjects: &[String],
    log: &[DeliveredPublish],
) -> std::collections::BTreeSet<String> {
    let accepted: Vec<&String> = subjects
        .iter()
        .filter(|subject| {
            log.iter().any(|p| {
                &p.subject == *subject
                    && matches!(
                        p.outcome,
                        DeliveredOutcome::Accepted | DeliveredOutcome::Persisted
                    )
            })
        })
        .collect();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let stored: std::collections::BTreeSet<String> =
            env.stored().await.into_iter().map(|e| e.subject).collect();
        if accepted.iter().all(|subject| stored.contains(*subject)) {
            return stored;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "accepted events never persisted"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
async fn dead_lettered(env: &Env) -> anyhow::Result<std::collections::BTreeSet<String>> {
    let letters = env
        .pg
        .raw
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT payload, last_error FROM toolkit_outbox_dead_letters",
        ))
        .await?;
    let mut dead = std::collections::BTreeSet::new();
    for letter in letters {
        let payload: Vec<u8> = letter.try_get("", "payload")?;
        let envelope: serde_json::Value = serde_json::from_slice(&payload)?;
        let subject = envelope["subject"].as_str().unwrap_or_default().to_owned();
        let error: Option<String> = letter.try_get("", "last_error")?;
        eprintln!(
            "  dead letter {subject} declared_partition={} error={}",
            envelope["broker_partition"],
            error.unwrap_or_default()
        );
        dead.insert(subject);
    }
    Ok(dead)
}
async fn sequential_probe(env: &Env, sink: &EventSink, orders: &[u128]) -> anyhow::Result<Probe> {
    settle_each(env, sink, orders).await?;
    let subjects: Vec<String> = orders.iter().map(|id| u(*id).to_string()).collect();
    let log = env.faulty.delivered_publishes();
    let stored = stored_after_persistence(env, &subjects, &log).await;
    let dead = dead_lettered(env).await?;
    for p in log.iter().filter(|p| dead.contains(&p.subject)) {
        eprintln!("    answer {}", describe(p));
    }
    let mut probe = Probe {
        committed: orders.len(),
        stored: 0,
        dead_letters: 0,
        broker_duplicate: Vec::new(),
        sdk_already_sequenced: Vec::new(),
        unattributed: 0,
    };
    for subject in &subjects {
        if stored.contains(subject) {
            probe.stored += 1;
        } else if dead.contains(subject) {
            probe.dead_letters += 1;
        } else {
            match log.iter().rev().find(|p| &p.subject == subject) {
                Some(p) if p.outcome == DeliveredOutcome::Duplicate => {
                    probe.broker_duplicate.push(p.clone());
                }
                Some(p) if p.outcome == DeliveredOutcome::SequenceViolation => {
                    probe.sdk_already_sequenced.push(p.clone());
                }
                _ => probe.unattributed += 1,
            }
        }
    }
    Ok(probe)
}

/// Coordinator-requested probe (DESIGN §3.7 "startup MUST fail" on a partition-count
/// mismatch): the producer declares 3 partitions while the broker is configured with 4.
///
/// Observed platform behavior, pinned here as evidence for the open deployment gate
/// (`upreq-event-broker-runtime`) and the upstream defect in `UPSTREAM_REQS` §2.7: binding cannot
/// detect the mismatch (no API reports the count). The SDK keys its chain cursor and
/// `partition_hint` by `murmur3(subject) % declared`, while the broker ignores the hint and
/// chains by `% configured`, so two producer chains feed one broker chain. Each lost event is
/// attributed: a broker `DuplicateIgnore` for a *different* event whose sequence equals the
/// head, or a `SequenceViolation` the SDK acknowledges as already sequenced after refreshing
/// the wrong partition's cursor. Neither leaves a retry, dead letter or error on the queue.
/// When a broker-level partition-count capability exists, replace this with a fail-closed
/// startup test.
#[tokio::test]
async fn a_wrong_declared_partition_count_silently_drops_events() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env
        .bind_declaring(3)
        .await
        .expect("no platform API lets binding detect the mismatch");
    let orders: Vec<u128> = (1_200..1_212).collect();
    let probe = sequential_probe(&env, bound.sink(), &orders).await?;
    let silent = probe.broker_duplicate.len() + probe.sdk_already_sequenced.len();
    eprintln!(
        "partition mismatch probe: committed={} stored={} dead_letters={} silently_acknowledged={silent} (broker_duplicate={}, sdk_already_sequenced={}) unattributed={}",
        probe.committed,
        probe.stored,
        probe.dead_letters,
        probe.broker_duplicate.len(),
        probe.sdk_already_sequenced.len(),
        probe.unattributed,
    );
    for lost in probe
        .broker_duplicate
        .iter()
        .chain(&probe.sdk_already_sequenced)
    {
        eprintln!("  lost {}", describe(lost));
    }
    assert_eq!(probe.unattributed, 0, "every loss is attributed");
    assert_eq!(probe.stored + probe.dead_letters + silent, probe.committed);
    let errors = count(
        &env.pg,
        "SELECT count(*) AS n FROM toolkit_outbox_processor WHERE last_error IS NOT NULL OR attempts > 0",
    )
    .await;
    assert_eq!(errors, 0, "no retry or error is visible on the queue");
    assert!(
        silent > 0,
        "expected silent loss under a mismatched count; the platform may have changed - revisit the gate"
    );
    bound.stop().await;
    Ok(())
}

/// Control for the probe above: with the declared count equal to the broker's, the same
/// sequential traffic stores every event, every publish is a first-time acceptance, and no
/// duplicate or chain-violation answer occurs. The loss needs the mismatch.
#[tokio::test]
async fn a_matching_partition_count_stores_every_event() -> anyhow::Result<()> {
    let env = Env::new().await?;
    let bound = env.bind().await;
    let orders: Vec<u128> = (1_300..1_312).collect();
    let probe = sequential_probe(&env, bound.sink(), &orders).await?;
    assert_eq!(probe.stored, probe.committed);
    assert_eq!(probe.dead_letters, 0);
    assert!(probe.broker_duplicate.is_empty() && probe.sdk_already_sequenced.is_empty());
    assert!(
        env.faulty
            .delivered_publishes()
            .iter()
            .all(|p| p.outcome == DeliveredOutcome::Accepted && !p.ack_lost),
        "no duplicate or sequence-violation answer without a mismatch"
    );
    bound.stop().await;
    Ok(())
}
