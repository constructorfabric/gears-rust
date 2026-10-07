//! P-D-221 against a real broker: a transaction's enqueue wakes the outbox's sequencer after its
//! commit, and a rolled-back one wakes nothing.
//!
//! The broker is the event-broker gear itself, in process: its own test harness
//! (`event_broker::test_support`), as in pricing's bound-producer suite. Topics and event types
//! are seeded the way `types-registry` would hold them, publishes go through the gear's real
//! ingest, and what the broker stored is read back from its backend.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::infra::events::{self, enqueue_typed};
use crate::test_support::{authed_ctx, flat_in_enforcer, resolved_usage_types, test_db_with};
use axum::{Router, body::Body, http::Request};
use event_broker::test_support::{EventBrokerHarness, StaticTypesRegistry};
use event_broker_sdk::{Sequence, api::EventBrokerApi};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};
use toolkit_db::secure::TxConfig;
use tower::ServiceExt;

/// The partitions the products topic is configured with: the broker's default, which the
/// producer assumes when it declares none.
const PARTITIONS: u32 = 8;
/// How long a test transaction goes on after its enqueue: long enough for a sequencer woken at the
/// enqueue to run, find nothing committed and go back to sleep.
const HOLD: Duration = Duration::from_millis(300);
/// The bound on "at once": far below the cold reconciler's idle period (a minute), so a row
/// delivered within it was woken for, not found by the reconciler.
const AT_ONCE: Duration = Duration::from_secs(5);
/// How long nothing may move after a rolled-back transaction.
const QUIET: Duration = Duration::from_secs(2);

/// A products state over a database with four connections (the door fixtures pin one), bound to
/// a real broker that knows every products event type: the outbox's sequencer then reads on a
/// connection of its own while a transaction is still open, as it does in a deployment. The
/// start-up reconcile has run when it returns. The handle and the broker must outlive the state.
async fn bound() -> (
    Arc<crate::api::rest::ApiState>,
    Uuid,
    crate::test_support::TestDsn,
    event_broker_sdk::ProducerOutboxHandle,
    EventBrokerHarness,
) {
    let mut spec = vec![json!({"id": TOPIC, "partitions": PARTITIONS})];
    for (type_id, subject) in [
        (SkuPublished::TYPE_ID, SKU_SUBJECT_TYPE),
        (SkuChanged::TYPE_ID, SKU_SUBJECT_TYPE),
        (SkuRetired::TYPE_ID, SKU_SUBJECT_TYPE),
        (ApprovalUnitDecided::TYPE_ID, APPROVAL_UNIT_SUBJECT_TYPE),
        (ReferenceForceReleased::TYPE_ID, SKU_SUBJECT_TYPE),
    ] {
        spec.push(json!({
            "id": type_id,
            "topic": TOPIC,
            "data_schema": {"type": "object"},
            "allowed_subject_types": [subject],
        }));
    }
    let broker = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(Value::Array(spec)))
        .build()
        .await;
    let (db, _, tenant, dsn) = test_db_with(4).await;
    // The producer's registration table, which the gear's chain appends (`gear.rs`).
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db.db(),
        event_broker_sdk::producer_registration_migrations(),
    )
    .await
    .unwrap();
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn EventBrokerApi>(broker.broker());
    let (sink, handle) = bind_producer(
        &hub,
        db.db(),
        events::OUTBOX_TABLE_PREFIX,
        toolkit_db::outbox::Partitions::of(events::PARTITIONS),
    )
    .await
    .unwrap()
    .expect("a broker is registered");
    let state = Arc::new(crate::api::rest::ApiState {
        db,
        sink,
        usage_type_catalog: resolved_usage_types(),
        usage_type_catalog_source: "test",
        idempotency_retention_hours: 24,
        fence_ttl_minutes: 30,
        reference_principals: std::collections::BTreeMap::new(),
        hub: Arc::clone(&hub),
        actor_names: crate::api::rest::ApiState::names_from(&hub),
    });
    // Let the outbox's first reconcile pass run on the empty queue, so that only a wake, or the
    // reconciler's next pass a minute later, can move a row written after this.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    (state, tenant, dsn, handle, broker)
}

/// Every event the broker stored on the products topic: its type and its data.
async fn stored(broker: &EventBrokerHarness) -> Vec<(String, Value)> {
    let mut all = Vec::new();
    for partition in 0..PARTITIONS {
        let events = broker
            .backend()
            .read(
                broker.security_context(),
                TOPIC,
                partition,
                Sequence::NONE,
                1024,
            )
            .await
            .unwrap();
        for event in events {
            all.push((event.type_id.to_string(), event.data.unwrap_or_default()));
        }
    }
    all
}

/// What the broker stored once it holds at least `enough` events, or when `within` runs out.
async fn delivered_within(
    broker: &EventBrokerHarness,
    within: Duration,
    enough: usize,
) -> Vec<(String, Value)> {
    let start = std::time::Instant::now();
    loop {
        let delivered = stored(broker).await;
        if delivered.len() >= enough || start.elapsed() >= within {
            return delivered;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A `SkuPublished` of `tenant` for a fresh SKU id, which the tests tell their events apart by.
/// Every event of a tenant lands in one outbox partition.
fn published(tenant: Uuid) -> SkuPublished {
    SkuPublished {
        tenant_id: tenant,
        sku_id: Uuid::new_v4(),
        published_version: 1,
        actor_ref: Uuid::new_v4(),
    }
}

/// The SKU ids of the delivered `SkuPublished` events.
fn skus(delivered: &[(String, Value)]) -> BTreeSet<String> {
    delivered
        .iter()
        .filter(|(t, _)| t == SkuPublished::TYPE_ID)
        .map(|(_, data)| data["skuId"].as_str().unwrap().to_owned())
        .collect()
}

#[derive(Debug, thiserror::Error)]
enum TxError {
    #[error(transparent)]
    Db(#[from] toolkit_db::DbError),
    #[error(transparent)]
    Event(#[from] events::EventsError),
    #[error(transparent)]
    Broker(#[from] event_broker_sdk::EventBrokerError),
    #[error("the act failed after its event was written")]
    Rollback,
    #[error("contended: {0}")]
    Contended(sea_orm::DbErr),
}

/// Enqueue `event` through the gear's event transaction (P-D-221), which goes on for `hold` after
/// the enqueue (the act's other writes), then commits, or rolls back when `commit` is false.
async fn enqueue(
    state: &crate::api::rest::ApiState,
    event: SkuPublished,
    commit: bool,
    hold: Duration,
) {
    let result = events::transaction::<(), TxError, _, _>(
        &state.db.db(),
        &state.sink,
        TxConfig::default(),
        |_| None,
        move |tx, outbox| {
            let event = event.clone();
            Box::pin(async move {
                enqueue_typed(&outbox, tx, event).await?;
                tokio::time::sleep(hold).await;
                if commit {
                    Ok(())
                } else {
                    Err(TxError::Rollback)
                }
            })
        },
    )
    .await;
    assert_eq!(result.is_ok(), commit, "{result:?}");
}

/// Commit `event` with its wake discarded on purpose, as a writer that never wakes the sequencer
/// leaves it: only a sequencer woken for its partition, or the cold reconciler, moves it on.
async fn commit_unwoken(state: &crate::api::rest::ApiState, event: SkuPublished) {
    let EventSink::Broker(producer) = &state.sink else {
        panic!("the broker producer is bound");
    };
    let producer = producer.clone();
    state
        .db
        .db()
        .transaction_with_retry::<(), TxError, _, _>(
            TxConfig::default(),
            |_| None,
            move |tx| {
                let (producer, event) = (producer.clone(), event.clone());
                Box::pin(async move {
                    producer.enqueue(tx, event).await?.discard();
                    Ok(())
                })
            },
        )
        .await
        .unwrap();
}

/// P-D-221: a transaction's enqueue wakes the outbox's sequencer after its commit. The
/// transaction goes on after the enqueue; a sequencer woken at the enqueue would find nothing
/// committed, and the row would then wait for the cold reconciler (a minute), far past
/// [`AT_ONCE`].
#[tokio::test]
async fn a_committed_enqueue_wakes_the_sequencer_at_once() {
    let (state, tenant, _dsn, _handle, broker) = bound().await;
    let event = published(tenant);
    enqueue(&state, event.clone(), true, HOLD).await;
    let delivered = delivered_within(&broker, AT_ONCE, 1).await;
    assert_eq!(
        skus(&delivered),
        BTreeSet::from([event.sku_id.to_string()]),
        "the committed event is delivered at once, not by the cold reconciler: {delivered:#?}"
    );
}

/// P-D-221: a rolled-back transaction wakes no sequencer. A row committed earlier with its wake
/// discarded sits in the same partition, so any wake for that partition would deliver it; nothing
/// moves until a committed transaction wakes the partition, and the rolled-back event never.
#[tokio::test]
async fn a_rolled_back_enqueue_wakes_nothing() {
    let (state, tenant, _dsn, _handle, broker) = bound().await;
    let unwoken = published(tenant);
    commit_unwoken(&state, unwoken.clone()).await;
    let rolled_back = published(tenant);
    enqueue(&state, rolled_back.clone(), false, HOLD).await;
    let delivered = delivered_within(&broker, QUIET, 1).await;
    assert!(
        delivered.is_empty(),
        "no wake fired: the unwoken row of the partition did not move: {delivered:#?}"
    );
    let committed = published(tenant);
    enqueue(&state, committed.clone(), true, Duration::ZERO).await;
    let delivered = delivered_within(&broker, AT_ONCE, 2).await;
    assert_eq!(
        skus(&delivered),
        BTreeSet::from([unwoken.sku_id.to_string(), committed.sku_id.to_string()]),
        "the committed wake drains the partition, and the rolled-back event is never delivered: \
         {delivered:#?}"
    );
}

/// P-D-221 on a retried transaction: the attempt that met contention rolled back, so the attempt
/// that commits carries none of its wakes. The committed attempt enqueues nothing, so nothing may
/// wake the partition of the unwoken row.
#[tokio::test]
async fn a_retried_attempt_drops_the_wakes_of_the_attempt_before() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let (state, tenant, _dsn, _handle, broker) = bound().await;
    let unwoken = published(tenant);
    commit_unwoken(&state, unwoken.clone()).await;
    let attempts = Arc::new(AtomicU32::new(0));
    let (seen, contended) = (attempts.clone(), published(tenant));
    events::transaction::<(), TxError, _, _>(
        &state.db.db(),
        &state.sink,
        TxConfig::default(),
        |e| match e {
            TxError::Contended(source) => Some(source),
            _ => None,
        },
        move |tx, outbox| {
            let (seen, contended) = (seen.clone(), contended.clone());
            Box::pin(async move {
                if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    enqueue_typed(&outbox, tx, contended).await?;
                    return Err(TxError::Contended(sea_orm::DbErr::Custom(
                        "error returned from database: (code: 5) database is locked".into(),
                    )));
                }
                Ok(())
            })
        },
    )
    .await
    .unwrap();
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "the contention was retried"
    );
    let delivered = delivered_within(&broker, QUIET, 1).await;
    assert!(
        delivered.is_empty(),
        "the retried attempt's wake was dropped with its rows: {delivered:#?}"
    );
}

/// One request through `app` as `ctx`, with an optional `If-Match`: its status, `ETag` and body.
async fn call(
    app: &Router,
    ctx: &toolkit_security::SecurityContext,
    method: &str,
    path: &str,
    body: Value,
    tag: Option<&str>,
) -> (u16, Option<String>, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/bss-products/v1{path}"))
        .extension(ctx.clone())
        .header("Content-Type", "application/json");
    if let Some(tag) = tag {
        request = request.header("If-Match", tag);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, etag, body)
}

/// P-D-221 at a real door: at quorum zero a SKU's submit publishes it at once and announces
/// `SkuPublished` and `ApprovalUnitDecided`; both are delivered at once after its commit.
#[tokio::test]
async fn a_door_wakes_the_sequencer_once_its_transaction_commits() {
    let (state, tenant, _dsn, _handle, broker) = bound().await;
    let o = toolkit::api::OpenApiRegistryImpl::new();
    let app = crate::api::rest::categories::router(state.clone(), &o)
        .merge(crate::api::rest::skus::router(state.clone(), &o))
        .merge(crate::api::rest::sku_governance::router(state.clone(), &o))
        .merge(crate::api::rest::approval_policy::router(state, &o))
        .layer(axum::Extension(flat_in_enforcer(tenant)));
    let author = authed_ctx(tenant);
    let (status, _, category) = call(
        &app,
        &author,
        "POST",
        "/categories",
        json!({"code":"c","name":"C"}),
        None,
    )
    .await;
    assert_eq!(status, 201, "{category}");
    let (status, _, sku) = call(
        &app,
        &author,
        "POST",
        "/skus",
        json!({"code":"SKU","name":"SKU","type":"recurring","category_id":category["id"]}),
        None,
    )
    .await;
    assert_eq!(status, 201, "{sku}");
    let (status, tag, policy) =
        call(&app, &author, "GET", "/approval-policy", json!({}), None).await;
    assert_eq!(status, 200, "{policy}");
    let (status, _, policy) = call(
        &app,
        &author,
        "PUT",
        "/approval-policy",
        json!({"quorum":0}),
        tag.as_deref(),
    )
    .await;
    assert_eq!(status, 200, "{policy}");
    let path = format!("/skus/{}/submit", sku["id"].as_str().unwrap());
    let (status, _, unit) = call(&app, &author, "POST", &path, json!({}), None).await;
    assert_eq!(status, 200, "{unit}");
    assert_eq!(unit["applied"], true, "{unit}");
    let delivered = delivered_within(&broker, AT_ONCE, 2).await;
    let types: BTreeSet<_> = delivered.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        types,
        BTreeSet::from([SkuPublished::TYPE_ID, ApprovalUnitDecided::TYPE_ID]),
        "{delivered:#?}"
    );
}

/// The files under `src/`, outside the test modules, whose code (comment lines dropped) contains
/// any of `needles`, relative to `src/`.
fn files_with(needles: &[&str]) -> BTreeSet<String> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        needles: &[&str],
        out: &mut BTreeSet<String>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                walk(root, &path, needles, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && !name.ends_with("_tests.rs")
                && name != "test_support.rs"
            {
                let text = std::fs::read_to_string(&path).unwrap();
                let code: String = text
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if needles.iter().any(|needle| code.contains(needle)) {
                    let relative = path.strip_prefix(root).unwrap();
                    out.insert(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = BTreeSet::new();
    walk(&root, &root, needles, &mut out);
    out
}

/// P-D-221's census: only `events::transaction` opens a `TxOutbox` and settles it, so every event
/// a door or an approval subject's apply enqueues wakes the outbox's sequencer after its
/// transaction commits, and never on a rollback.
#[test]
fn only_the_event_transaction_opens_and_settles_a_tx_outbox() {
    let events = BTreeSet::from(["infra/events.rs".to_owned()]);
    assert_eq!(
        files_with(&["TxOutbox::new("]),
        events,
        "a door opens no TxOutbox of its own: it runs its events through events::transaction"
    );
    assert_eq!(
        files_with(&[".fire()", "Wake::fire", ".discard()", "Wake::discard"]),
        events,
        "no door fires or discards a wake itself"
    );
    let code = include_str!("events.rs");
    let transaction = code.find("pub async fn transaction<").unwrap();
    assert_eq!(code.matches("TxOutbox::new(").count(), 1);
    assert!(
        code[transaction..].contains("TxOutbox::new("),
        "the one TxOutbox::new is the event transaction's"
    );
}
