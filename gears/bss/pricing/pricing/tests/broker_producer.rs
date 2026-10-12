//! With an `EventBrokerApi` in the `ClientHub`, pricing binds the broker SDK's `DbProducer`
//! to its outbox queue (D-400, Products' pattern): committed events reach the broker, an
//! interrupted dispatch is retried from the durable envelope, and a rolled-back transaction
//! delivers nothing.
//!
//! The broker is the event-broker gear itself, in process: its own test harness
//! (`event_broker::test_support`), which replaced the SDK's `MockBroker`. Topics and event types
//! are seeded the way `types-registry` would hold them, publishes go through the gear's real
//! ingest, and what the broker stored is read back from its backend.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;
use bss_pricing::{
    api::rest::authoring::AuthoringState,
    infra::{
        events::{self, APPROVAL_UNIT_SUBJECT_TYPE, EventSink, PRICE_BOOK_SUBJECT_TYPE, TOPIC},
        reference_events::{PlanReferenceLost, PriceBookEntryReferenceLost},
    },
};
use entry_support::policy_support;
use entry_support::{Script, app_for, request, test_db, user_of};
use event_broker::test_support::{EventBrokerHarness, StaticTypesRegistry};
use event_broker_sdk::{Sequence, TypedEvent, api::EventBrokerApi};
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use uuid::Uuid;

const PUBLISHED: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.prices_published.v1~";
const DECIDED: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.approval_unit_decided.v1~";
const ENTRY_SUBJECT_TYPE: &str =
    "gts.cf.core.events.subject.v1~cf.bss.pricing.price_book_entry.v1~";
/// The partitions pricing's topic is configured with: the broker's default, which the producer
/// assumes when it declares none.
const PARTITIONS: u32 = 8;

/// A real in-process broker that knows pricing's topic and each `(type, subject type)` of
/// `types`, every one over an object payload. The harness must outlive every publish.
async fn broker_knowing(types: &[(&str, &str)]) -> EventBrokerHarness {
    let mut spec = vec![json!({"id": TOPIC, "partitions": PARTITIONS})];
    for (type_id, subject) in types {
        spec.push(json!({
            "id": type_id,
            "topic": TOPIC,
            "data_schema": {"type": "object"},
            "allowed_subject_types": [subject],
        }));
    }
    EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(serde_json::Value::Array(spec)))
        .build()
        .await
}

/// The retries pricing's outbox processor has recorded, over its partitions: a dispatch the
/// broker refused is retried, and counted there.
async fn processor_retries(dsn: &str) -> i64 {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let db = Database::connect(dsn).await.unwrap();
    db.query_one_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT COALESCE(SUM(attempts), 0) AS n FROM bss_pricing_outbox_processor".to_owned(),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "n")
    .unwrap()
}
/// Every event the broker stored on pricing's topic: its type and its data.
async fn stored(broker: &EventBrokerHarness) -> Vec<(String, serde_json::Value)> {
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
fn lost(tenant: Uuid) -> PriceBookEntryReferenceLost {
    PriceBookEntryReferenceLost {
        tenant_id: tenant,
        price_book_entry_id: Uuid::new_v4(),
        sku_id: Uuid::new_v4(),
        reservation_id: Uuid::new_v4(),
        actor_ref: Uuid::new_v4(),
    }
}
/// Enqueue one event through the gear's event transaction (D-455), which goes on for `hold` after
/// the enqueue (the act's other writes), then commits, or rolls back when `commit` is false.
async fn enqueue(
    state: &AuthoringState,
    event: PriceBookEntryReferenceLost,
    commit: bool,
    hold: Duration,
) {
    let result = events::transaction(
        &state.db.db(),
        &state.outbox,
        toolkit_db::secure::TxConfig::default(),
        |_: &anyhow::Error| None,
        move |tx, outbox| {
            let event = event.clone();
            Box::pin(async move {
                events::enqueue(&outbox, tx, &event, time::OffsetDateTime::now_utc()).await?;
                tokio::time::sleep(hold).await;
                if commit {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "the act failed after its event was written"
                    ))
                }
            })
        },
    )
    .await;
    assert_eq!(result.is_ok(), commit);
}

#[tokio::test]
async fn a_bound_producer_delivers_committed_events_retries_dispatch_and_drops_rollbacks() {
    let broker = broker_knowing(&[
        (PriceBookEntryReferenceLost::TYPE_ID, ENTRY_SUBJECT_TYPE),
        (PlanReferenceLost::TYPE_ID, PlanReferenceLost::SUBJECT_TYPE),
        (PUBLISHED, PRICE_BOOK_SUBJECT_TYPE),
        (
            events::PlanRevisionPublished::TYPE_ID,
            events::PlanRevisionPublished::SUBJECT_TYPE,
        ),
        (DECIDED, APPROVAL_UNIT_SUBJECT_TYPE),
    ])
    .await;
    let (db, _, tenant, dsn) = test_db().await;
    let hub = Arc::new(toolkit::ClientHub::default());
    hub.register::<bss_products_sdk::PricingReferenceRegistry>(Arc::new(
        bss_products_sdk::PricingReferenceRegistry(Arc::new(Script::default())),
    ));
    hub.register::<dyn EventBrokerApi>(broker.broker());
    let state = Arc::new(AuthoringState::new(db, hub).await.unwrap());

    // The broker refuses every dispatch for now (a real `RateLimited` on its publish path):
    // the committed envelope stays durable.
    broker.set_publish_rate_limited(true);
    let committed = lost(tenant);
    enqueue(&state, committed.clone(), true, Duration::ZERO).await;
    let rolled_back = lost(tenant);
    enqueue(&state, rolled_back.clone(), false, Duration::ZERO).await;

    // A real door's transaction: quorum zero applies at submit and announces both events.
    submit_a_price_at_quorum_zero(&state, tenant).await;

    // Evidence that a dispatch was attempted and refused, not only that time passed (PT-17): the
    // outbox's processor records each retry of its partition.
    let mut retried = 0;
    for _ in 0..300 {
        retried = processor_retries(&dsn).await;
        if retried > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(retried > 0, "the processor retried a refused dispatch");
    assert!(
        stored(&broker).await.is_empty(),
        "no dispatch succeeded while the broker refused"
    );
    // Delivery resumes: the durable envelopes are retried.
    broker.set_publish_rate_limited(false);
    let mut delivered = Vec::new();
    for _ in 0..300 {
        delivered = stored(&broker).await;
        if delivered.len() >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let types: Vec<_> = delivered.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(delivered.len(), 3, "{delivered:#?}");
    assert!(types.contains(&PUBLISHED), "{types:?}");
    assert!(types.contains(&DECIDED), "{types:?}");
    let lost_events: Vec<_> = delivered
        .iter()
        .filter(|(t, _)| t == PriceBookEntryReferenceLost::TYPE_ID)
        .collect();
    assert_eq!(lost_events.len(), 1, "{delivered:#?}");
    assert_eq!(
        lost_events[0].1["priceBookEntryId"],
        committed.price_book_entry_id.to_string(),
        "the committed event, retried after the refused dispatch"
    );
    assert!(
        !delivered.iter().any(
            |(_, data)| data["priceBookEntryId"] == rolled_back.price_book_entry_id.to_string()
        ),
        "a rolled-back transaction delivers nothing"
    );
}

/// A real door's transaction: at quorum zero a price's submit applies it at once and announces
/// `PricesPublished` and `ApprovalUnitDecided` in its own transaction.
async fn submit_a_price_at_quorum_zero(state: &Arc<AuthoringState>, tenant: Uuid) {
    state
        .hub
        .register::<dyn bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1>(Arc::new(
            entry_support::policy_support::MeterProvider::default(),
        ));
    let (app, ctx) = (app_for(state.clone(), tenant), user_of(tenant));
    let call = |method: &'static str,
                path: String,
                body: serde_json::Value,
                tag: Option<String>,
                key: Option<&'static str>| {
        let (app, ctx) = (app.clone(), ctx.clone());
        async move { request(&app, &ctx, method, &path, body, tag.as_deref(), key).await }
    };
    let (_, _, tag) = call("GET", "/approval-policy".into(), json!({}), None, None).await;
    assert_eq!(
        call(
            "PUT",
            "/approval-policy".into(),
            json!({"quorum":0}),
            Some(tag),
            None
        )
        .await
        .0,
        200
    );
    let book = call(
        "POST",
        "/price-books".into(),
        json!({"code":"standard","name":"Standard","currency":"EUR"}),
        None,
        Some("book"),
    )
    .await;
    assert_eq!(book.0, 201, "{book:?}");
    let entry = call(
        "POST",
        format!("/price-books/{}/entries", book.1["id"].as_str().unwrap()),
        json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"model":"per_unit"}),
        None,
        Some("entry"),
    )
    .await;
    assert_eq!(entry.0, 201, "{entry:?}");
    let prices = call(
        "POST",
        format!(
            "/price-book-entries/{}/prices",
            entry.1["id"].as_str().unwrap()
        ),
        json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":"2031-03-01"}),
        None,
        Some("price"),
    )
    .await;
    assert_eq!(prices.0, 201, "{prices:?}");
    let submitted = call(
        "POST",
        format!(
            "/prices/{}/submit",
            prices.1["items"][0]["id"].as_str().unwrap()
        ),
        json!({}),
        None,
        Some("submit"),
    )
    .await;
    assert_eq!(submitted.0, 201, "{submitted:?}");
}

/// How long a test transaction goes on after its enqueue: long enough for a sequencer woken at the
/// enqueue to run, find nothing committed and go back to sleep.
const HOLD: Duration = Duration::from_millis(300);
/// The bound on "at once": far below the cold reconciler's idle period (a minute), so a row
/// delivered within it was woken for, not found by the reconciler.
const AT_ONCE: Duration = Duration::from_secs(5);
/// How long nothing may move after a rolled-back transaction.
const QUIET: Duration = Duration::from_secs(2);

/// A pricing state over a migrated database of its own with four connections (the shared fixture
/// pins one), with a broker that knows every pricing event type. The outbox's sequencer then reads
/// on a connection of its own while a transaction is still open, as it does in a deployment,
/// instead of queueing behind that transaction. The start-up reconcile has run when it returns.
async fn pooled_state() -> (
    Arc<AuthoringState>,
    Uuid,
    entry_support::TestDsn,
    EventBrokerHarness,
) {
    use toolkit::contracts::DatabaseCapability as _;
    let known: Vec<(&str, &str)> = EVENT_TYPES
        .iter()
        .map(|(_, type_id, subject)| (*type_id, *subject))
        .collect();
    let broker = broker_knowing(&known).await;
    let dsn = entry_support::TestDsn::new("pricing-wake-");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(4),
            min_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        bss_pricing::module::BssPricingGear::default().migrations(),
    )
    .await
    .unwrap();
    let hub = Arc::new(toolkit::ClientHub::default());
    hub.register::<bss_products_sdk::PricingReferenceRegistry>(Arc::new(
        bss_products_sdk::PricingReferenceRegistry(Arc::new(Script::default())),
    ));
    hub.register::<dyn EventBrokerApi>(broker.broker());
    let state = Arc::new(
        AuthoringState::new(toolkit_db::DBProvider::new(db), hub)
            .await
            .unwrap(),
    );
    // Let the outbox's first reconcile pass run on the empty queue, so that only a wake, or the
    // reconciler's next pass a minute later, can move a row written after this.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    (state, Uuid::new_v4(), dsn, broker)
}

/// Commit `event` with its wake discarded on purpose, as a writer that never wakes the sequencer
/// leaves it: only a sequencer woken for its partition, or the cold reconciler, moves it on.
async fn commit_unwoken(state: &AuthoringState, event: PriceBookEntryReferenceLost) {
    let EventSink::Broker(producer) = &state.outbox else {
        panic!("the broker producer is bound");
    };
    let producer = producer.clone();
    state
        .db
        .db()
        .transaction_with_retry(
            toolkit_db::secure::TxConfig::default(),
            |_: &anyhow::Error| None,
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

/// What the broker stored once it holds at least `enough` events, or when `within` runs out.
async fn delivered_within(
    broker: &EventBrokerHarness,
    within: Duration,
    enough: usize,
) -> Vec<(String, serde_json::Value)> {
    let start = std::time::Instant::now();
    loop {
        let delivered = stored(broker).await;
        if delivered.len() >= enough || start.elapsed() >= within {
            return delivered;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The reservation ids of the delivered `PriceBookEntryReferenceLost` events: the tests tell their
/// events apart by them. Every event of a tenant lands in one outbox partition.
fn reservations(delivered: &[(String, serde_json::Value)]) -> BTreeSet<String> {
    delivered
        .iter()
        .filter(|(t, _)| t == PriceBookEntryReferenceLost::TYPE_ID)
        .map(|(_, data)| data["reservationId"].as_str().unwrap().to_owned())
        .collect()
}

/// D-455: a transaction's enqueue wakes the outbox's sequencer after its commit. The transaction
/// goes on after the enqueue; a sequencer woken at the enqueue would find nothing committed, and
/// the row would then wait for the cold reconciler (a minute), far past [`AT_ONCE`].
#[tokio::test]
async fn a_committed_enqueue_wakes_the_sequencer_at_once() {
    let (state, tenant, _dsn, broker) = pooled_state().await;
    let event = lost(tenant);
    enqueue(&state, event.clone(), true, HOLD).await;
    let delivered = delivered_within(&broker, AT_ONCE, 1).await;
    assert_eq!(
        reservations(&delivered),
        BTreeSet::from([event.reservation_id.to_string()]),
        "the committed event is delivered at once, not by the cold reconciler: {delivered:#?}"
    );
}

/// D-455: a rolled-back transaction wakes no sequencer. A row committed earlier with its wake
/// discarded sits in the same partition, so any wake for that partition would deliver it; nothing
/// moves until a committed transaction wakes the partition, and the rolled-back event never.
#[tokio::test]
async fn a_rolled_back_enqueue_wakes_nothing() {
    let (state, tenant, _dsn, broker) = pooled_state().await;
    let unwoken = lost(tenant);
    commit_unwoken(&state, unwoken.clone()).await;
    let rolled_back = lost(tenant);
    enqueue(&state, rolled_back.clone(), false, HOLD).await;
    let delivered = delivered_within(&broker, QUIET, 1).await;
    assert!(
        delivered.is_empty(),
        "no wake fired: the unwoken row of the partition did not move: {delivered:#?}"
    );
    let committed = lost(tenant);
    enqueue(&state, committed.clone(), true, Duration::ZERO).await;
    let delivered = delivered_within(&broker, AT_ONCE, 2).await;
    assert_eq!(
        reservations(&delivered),
        BTreeSet::from([
            unwoken.reservation_id.to_string(),
            committed.reservation_id.to_string()
        ]),
        "the committed wake drains the partition, and the rolled-back event is never delivered: \
         {delivered:#?}"
    );
}

/// D-455 on a retried transaction: the attempt that met contention rolled back, so the attempt
/// that commits carries none of its wakes. The committed attempt enqueues nothing, so nothing may
/// wake the partition of the unwoken row.
#[tokio::test]
async fn a_retried_attempt_drops_the_wakes_of_the_attempt_before() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let (state, tenant, _dsn, broker) = pooled_state().await;
    let unwoken = lost(tenant);
    commit_unwoken(&state, unwoken.clone()).await;
    let attempts = Arc::new(AtomicU32::new(0));
    let (seen, contended) = (attempts.clone(), lost(tenant));
    events::transaction(
        &state.db.db(),
        &state.outbox,
        toolkit_db::secure::TxConfig::default(),
        |e: &anyhow::Error| e.downcast_ref::<sea_orm::DbErr>(),
        move |tx, outbox| {
            let (seen, contended) = (seen.clone(), contended.clone());
            Box::pin(async move {
                if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    let now = time::OffsetDateTime::now_utc();
                    events::enqueue(&outbox, tx, &contended, now).await?;
                    return Err(anyhow::Error::new(sea_orm::DbErr::Custom(
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

/// D-455 at a real door: the quorum-zero submit's events are delivered at once after its commit.
#[tokio::test]
async fn a_door_wakes_the_sequencer_once_its_transaction_commits() {
    let (state, tenant, _dsn, broker) = pooled_state().await;
    submit_a_price_at_quorum_zero(&state, tenant).await;
    let delivered = delivered_within(&broker, AT_ONCE, 2).await;
    let types: BTreeSet<_> = delivered.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        types,
        BTreeSet::from([PUBLISHED, DECIDED]),
        "{delivered:#?}"
    );
}

/// Every pricing event type, by the struct that implements `TypedEvent`: the census the bound
/// producer is held to.
const EVENT_TYPES: [(&str, &str, &str); 5] = [
    (
        "PriceBookEntryReferenceLost",
        PriceBookEntryReferenceLost::TYPE_ID,
        PriceBookEntryReferenceLost::SUBJECT_TYPE,
    ),
    (
        "PlanReferenceLost",
        PlanReferenceLost::TYPE_ID,
        PlanReferenceLost::SUBJECT_TYPE,
    ),
    (
        "PricesPublished",
        events::PricesPublished::TYPE_ID,
        events::PricesPublished::SUBJECT_TYPE,
    ),
    (
        "PlanRevisionPublished",
        events::PlanRevisionPublished::TYPE_ID,
        events::PlanRevisionPublished::SUBJECT_TYPE,
    ),
    (
        "ApprovalUnitDecided",
        events::ApprovalUnitDecided::TYPE_ID,
        events::ApprovalUnitDecided::SUBJECT_TYPE,
    ),
];

/// The names of every `impl TypedEvent for <Name>` under `src/`, so a new event type fails the
/// census until it is listed (and so prepared at bind).
fn typed_events_in_src() -> std::collections::BTreeSet<String> {
    fn walk(dir: &std::path::Path, out: &mut std::collections::BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for (at, _) in text.match_indices("impl TypedEvent for ") {
                    let rest = &text[at + "impl TypedEvent for ".len()..];
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    out.insert(name);
                }
            }
        }
    }
    let mut out = std::collections::BTreeSet::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut out,
    );
    out
}

/// A pricing state over a fresh database with a broker that knows `types`.
/// The state comes with its database's DSN, which holds the database's temporary directory,
/// and with the broker, which must outlive the state.
async fn bind_with(
    types: &[(&str, &str, &str)],
) -> anyhow::Result<(
    Arc<AuthoringState>,
    entry_support::TestDsn,
    EventBrokerHarness,
)> {
    let known: Vec<(&str, &str)> = types
        .iter()
        .map(|(_, type_id, subject)| (*type_id, *subject))
        .collect();
    let broker = broker_knowing(&known).await;
    let (db, _, _, dsn) = test_db().await;
    let hub = Arc::new(toolkit::ClientHub::default());
    hub.register::<bss_products_sdk::PricingReferenceRegistry>(Arc::new(
        bss_products_sdk::PricingReferenceRegistry(Arc::new(Script::default())),
    ));
    hub.register::<dyn EventBrokerApi>(broker.broker());
    AuthoringState::new(db, hub)
        .await
        .map(|state| (Arc::new(state), dsn, broker))
}

/// The event census (run 3.5): every type pricing implements is a `TypedEvent` under
/// `gts.cf.core.events.event.v1~cf.bss.pricing.<name>.v1~` from `bss-pricing`, and the bound
/// producer prepares each one at bind — a broker that lacks any one of them fails the boot
/// rather than failing the first business transaction that announces it.
#[tokio::test]
async fn the_bound_producer_prepares_every_pricing_event_type_at_bind() {
    let listed: std::collections::BTreeSet<String> = EVENT_TYPES
        .iter()
        .map(|(n, _, _)| (*n).to_owned())
        .collect();
    assert_eq!(
        typed_events_in_src(),
        listed,
        "the census lists every TypedEvent"
    );
    for (name, type_id, subject) in EVENT_TYPES {
        let short = type_id
            .strip_prefix("gts.cf.core.events.event.v1~cf.bss.pricing.")
            .and_then(|rest| rest.strip_suffix(".v1~"))
            .unwrap_or_else(|| panic!("{name}: {type_id} is not a pricing event type"));
        assert!(
            !short.is_empty() && short.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{name}: {type_id}"
        );
        // A subject type names a kind of entity: a GTS type id, which the broker refuses without
        // its trailing `~`.
        assert!(
            subject.starts_with("gts.cf.core.events.subject.v1~cf.bss.pricing.")
                && subject.ends_with(".v1~"),
            "{name}: {subject}"
        );
    }
    bind_with(&EVENT_TYPES)
        .await
        .expect("a broker that knows every pricing event type binds");
    for (index, (name, type_id, _)) in EVENT_TYPES.iter().enumerate() {
        let mut known = EVENT_TYPES.to_vec();
        known.remove(index);
        let error = bind_with(&known)
            .await
            .err()
            .unwrap_or_else(|| panic!("{name} is not prepared at bind: the boot succeeded"));
        assert!(
            format!("{error:#}").contains(type_id),
            "{name}: the boot names the missing type: {error:#}"
        );
    }
}
