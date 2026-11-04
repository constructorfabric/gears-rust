#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Single-instance SQLite startup seeding through real outbox admission (T31).
//! Multi-instance races run on server databases in `seeding_backends_test`.

use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ConnectionTrait, Database, Statement};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit_db::{DBProvider, DbError};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::metrics::{
    AdmissionMetrics, DeliveryOutcome, PassLabels, RefusalStage, TerminalStatus,
};
use types_registry::domain::registry_service::{
    BatchGetItem, EntityKey, EntityLookup, MAX_BATCH_GET_KEYS, RegistryService,
};
use types_registry::domain::seeding::{SeedError, SeedSet, seed};
use types_registry::domain::selection::FieldSelection;
use types_registry::infra::outbox::OutboxDispatch;
use types_registry_sdk::ReconcileStop;

mod common;

const BUDGET: Duration = Duration::from_secs(30);

const AM_BASE: &str = "gts.cf.core.am.tenant_type.v1~";
const CUSTOMER: &str = "gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~";
const PLUGIN_SPEC: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.core.seed_test.plugin.v1~";
const PERMISSION: &str = "gts.cf.toolkit.authz.permission.v1~cf.core.seed_test.read.v1";

/// One registry instance with its own running outbox pipeline.
struct Registry {
    service: Arc<RegistryService>,
    outbox: toolkit_db::outbox::OutboxHandle,
}

async fn registry(db: &Arc<DBProvider<DbError>>, config: TypesRegistryConfig) -> Registry {
    registry_with(db, config, common::metrics()).await
}

async fn registry_with(
    db: &Arc<DBProvider<DbError>>,
    config: TypesRegistryConfig,
    metrics: Arc<dyn AdmissionMetrics>,
) -> Registry {
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        common::stores(),
        RegistrationPolicy::default(),
        config,
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        metrics,
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .expect("start the admission outbox");
    Registry { service, outbox }
}

fn dsn(dir: &common::TestDir) -> String {
    format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("seed.db").display()
    )
}

async fn database(dir: &common::TestDir) -> Arc<DBProvider<DbError>> {
    common::provider_for_with_outbox(&dsn(dir), 8).await
}

/// Every durable registration operation's item count, read straight from the
/// tables: what the seeder actually submitted, request by request.
async fn submitted_batches(dir: &common::TestDir) -> Vec<i64> {
    let path = dir.path().join("seed.db");
    let db = Database::connect(format!("sqlite://{}?mode=ro", path.display()))
        .await
        .expect("open the seed database");
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT operation_id, COUNT(*) FROM types_registry__operation_item \
             GROUP BY operation_id"
                .to_owned(),
        ))
        .await
        .expect("read operation items");
    rows.iter()
        .map(|row| row.try_get_by_index::<i64>(1).expect("item count"))
        .collect()
}

/// AM-style tenant-type base with the entity-reference keywords used by its inventory.
fn am_base() -> Value {
    json!({
        "$id": format!("gts://{AM_BASE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": "Base tenant type schema for Account Management.",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": {"allOf": [{"type": "string", "format": "gts-instance-id", "x-gts-instance-ref": "gts.*"}]},
            "payload": {"type": "object"}
        },
        "required": ["payload"],
        "x-gts-traits-schema": {
            "type": "object",
            "properties": {
                "allowed_parent_types": {
                    "type": "array",
                    "items": {"type": "string", "x-gts-type-ref": AM_BASE},
                    "default": []
                },
                "idp_provisioning": {"type": "boolean", "default": false}
            },
            "additionalProperties": false
        },
        "x-gts-traits": {"allowed_parent_types": [], "idp_provisioning": false},
        "x-gts-abstract": true
    })
}

/// `config/e2e-local.yaml`'s customer: its platform-root parent is only an `x-gts-type-ref`.
fn customer(description: &str) -> Value {
    json!({
        "$id": format!("gts://{CUSTOMER}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [{"$ref": format!("gts://{AM_BASE}")}],
        "description": description,
        "type": "object",
        "x-gts-traits": {
            "allowed_parent_types": [
                "gts.cf.core.am.tenant_type.v1~cf.core.am.platform.v1~",
                CUSTOMER
            ],
            "idp_provisioning": false
        }
    })
}

fn plugin_spec() -> Value {
    json!({
        "$id": format!("gts://{PLUGIN_SPEC}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [{"$ref": "gts://gts.cf.toolkit.plugins.plugin.v1~"}]
    })
}

fn permission() -> Value {
    json!({
        "id": PERMISSION,
        "resource_type": "gts.cf.core.seed_test.thing.v1~",
        "action": "read",
        "display_name": "Read things"
    })
}

/// The real `toolkit-gts` inventory linked into this binary, plus documents of a
/// second crate depending on it, plus the configured customer.
fn seeds(customer_description: &str) -> SeedSet {
    let mut documents = toolkit_gts::all_inventory_type_schemas().expect("inventory schemas");
    documents.extend(toolkit_gts::all_inventory_instances().expect("inventory instances"));
    assert!(
        documents
            .iter()
            .any(|d| d["$id"] == "gts://gts.cf.toolkit.plugins.plugin.v1~"),
        "the toolkit-gts plugin base must be linked into this test binary"
    );
    documents.extend([am_base(), plugin_spec(), permission()]);
    documents.push(customer(customer_description));
    SeedSet::collect(documents, &TypesRegistryConfig::default().entity_id_fields)
        .expect("a valid seed set")
}

/// `gts_id -> (resource_version, content)` of every key, through the database read.
async fn stored(service: &RegistryService, ids: &[&str]) -> Vec<Option<(i64, Value)>> {
    let items: Vec<BatchGetItem> = ids
        .iter()
        .map(|id| BatchGetItem::from(EntityKey::GtsId((*id).to_owned())))
        .collect();
    let answers = service
        .batch_get(common::caller(), &items, FieldSelection::full())
        .await
        .expect("read seeds");
    ids.iter()
        .map(|id| {
            let lookup = common::answer_for(&answers, &EntityKey::GtsId((*id).to_owned()));
            match lookup {
                EntityLookup::Found { record, .. } => Some((
                    record.origin.expect("managed").resource_version,
                    common::doc(record.content.as_deref()).expect("content"),
                )),
                EntityLookup::NotFound => None,
                EntityLookup::Unchanged { .. } => panic!("unconditional read answered unchanged"),
            }
        })
        .collect()
}

const ALL: [&str; 6] = [
    "gts.cf.toolkit.plugins.plugin.v1~",
    "gts.cf.toolkit.authz.permission.v1~",
    AM_BASE,
    CUSTOMER,
    PLUGIN_SPEC,
    PERMISSION,
];

#[tokio::test]
async fn cold_start_admits_inventory_and_configured_entities() {
    let dir = common::TestDir::new("tr-seed-cold");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let set = seeds("Customer tenant under the platform root (E2E).");

    let summary = seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect("a cold start seeds every document");

    assert_eq!(summary.documents, set.len());
    assert!(!summary.up_to_date);
    let read = stored(&r.service, &ALL).await;
    for (id, row) in ALL.iter().zip(&read) {
        assert_eq!(
            row.as_ref().map(|(rv, _)| *rv),
            Some(1),
            "{id} admitted once"
        );
    }
    assert_eq!(
        read[3].as_ref().unwrap().1,
        customer("Customer tenant under the platform root (E2E).")
    );
}

/// A budget too large for the clock is refused up front, naming the budget, instead of
/// becoming an already expired deadline; nothing is submitted.
#[tokio::test]
async fn an_unschedulable_budget_is_refused_before_anything_is_submitted() {
    let dir = common::TestDir::new("tr-seed-budget");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;

    let error = seed(
        &r.service,
        &seeds("first"),
        Duration::MAX,
        &CancellationToken::new(),
    )
    .await
    .expect_err("an unschedulable budget");

    assert!(
        matches!(error, SeedError::BudgetTooLarge { budget } if budget == Duration::MAX),
        "{error}"
    );
    assert!(submitted_batches(&dir).await.is_empty());
}

#[tokio::test]
async fn restart_with_equal_seeds_allocates_no_revision() {
    let dir = common::TestDir::new("tr-seed-restart");
    let db = database(&dir).await;
    let set = seeds("first");
    {
        let r = registry(&db, TypesRegistryConfig::default()).await;
        seed(&r.service, &set, BUDGET, &CancellationToken::new())
            .await
            .expect("first boot");
        r.outbox.stop().await;
    }
    drop(db);
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;

    let summary = seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect("second boot");

    assert!(summary.up_to_date, "nothing to submit");
    let read = stored(&r.service, &ALL).await;
    assert!(
        read.iter()
            .all(|row| row.as_ref().map(|(rv, _)| *rv) == Some(1))
    );
}

#[tokio::test]
async fn permitted_configured_change_revises_through_cas() {
    let dir = common::TestDir::new("tr-seed-update");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    seed(
        &r.service,
        &seeds("first"),
        BUDGET,
        &CancellationToken::new(),
    )
    .await
    .expect("first boot");

    seed(
        &r.service,
        &seeds("second"),
        BUDGET,
        &CancellationToken::new(),
    )
    .await
    .expect("a permitted change converges");

    let read = stored(&r.service, &[CUSTOMER, AM_BASE]).await;
    assert_eq!(read[0], Some((2, customer("second"))));
    assert_eq!(
        read[1].as_ref().map(|(rv, _)| *rv),
        Some(1),
        "equal seeds keep their revision"
    );
}

#[tokio::test]
async fn a_missing_derivation_target_is_refused_by_identifier() {
    let dir = common::TestDir::new("tr-seed-missing");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    // The customer without its AM base: a real derivation/`$ref` dependency.
    let set = SeedSet::collect(
        [customer("orphan")],
        &TypesRegistryConfig::default().entity_id_fields,
    )
    .unwrap();

    let started = std::time::Instant::now();
    let error = seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect_err("a missing base must fail the seed");
    assert!(
        started.elapsed() < BUDGET / 3,
        "it does not wait out the budget"
    );

    // Pending on its dependency, and nothing else can store it: the call stalls at
    // once rather than waiting out the budget.
    let SeedError::Unsettled { stop, failures } = &error else {
        panic!("expected an unsettled seed, got {error}");
    };
    assert_eq!(*stop, ReconcileStop::Stalled, "{error}");
    assert_eq!(failures.len(), 1, "{error}");
    assert_eq!(failures[0].gts_id, CUSTOMER);
    assert_eq!(failures[0].reason, "dependency_not_found", "{error}");
    assert_eq!(stored(&r.service, &[CUSTOMER]).await, [None]);
}

#[tokio::test]
async fn an_invalid_document_is_rejected_by_identifier() {
    let dir = common::TestDir::new("tr-seed-rejected");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let invalid = "gts.cf.core.seed_test.invalid.v1~";
    let mut documents = vec![
        json!({
            "$id": format!("gts://{invalid}"),
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object"
        }),
        am_base(),
    ];
    documents.push(customer("valid"));
    let set =
        SeedSet::collect(documents, &TypesRegistryConfig::default().entity_id_fields).unwrap();

    let error = seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect_err("a rejected seed fails the whole seed");

    // Its synchronous refusal stops the call: its batch mates stay unjudged.
    let SeedError::Rejected(failures) = &error else {
        panic!("expected a rejection, got {error}");
    };
    let rejected: Vec<&str> = failures
        .iter()
        .filter(|f| f.reason == "rejected")
        .map(|f| f.gts_id.as_str())
        .collect();
    assert_eq!(rejected, [invalid], "{error}");
    assert!(
        failures
            .iter()
            .all(|f| f.gts_id == invalid || f.reason == "skipped"),
        "{error}"
    );
    assert_eq!(stored(&r.service, &[invalid]).await, [None]);
}

#[tokio::test]
async fn a_set_larger_than_the_batch_limit_is_admitted_in_bounded_batches() {
    let dir = common::TestDir::new("tr-seed-oversized");
    let db = database(&dir).await;
    let mut config = TypesRegistryConfig::default();
    config.limits.batch_candidates = 2;
    let r = registry(&db, config).await;
    let set = seeds("first");

    seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect("the whole set, two candidates at a time");

    let batches = submitted_batches(&dir).await;
    assert!(batches.iter().all(|items| *items <= 2), "{batches:?}");
    assert!(batches.len() * 2 >= set.len(), "{batches:?}");
    for (id, row) in ALL.iter().zip(stored(&r.service, &ALL).await) {
        assert_eq!(row.map(|(rv, _)| rv), Some(1), "{id} admitted once");
    }
}

/// One base Type Schema and `count` instances of it, by identifier.
fn instances(count: usize) -> Vec<(String, Value)> {
    let base = "gts.cf.core.seed_test.many.v1~";
    let mut documents = vec![(
        base.to_owned(),
        json!({
            "$id": format!("gts://{base}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {"id": {"type": "string"}, "n": {"type": "integer"}}
        }),
    )];
    documents.extend((0..count).map(|n| {
        let id = format!("{base}cf.core.seed_test.i{n:03}.v1");
        let document = json!({"id": id, "n": n});
        (id, document)
    }));
    documents
}

/// [`stored`] for any number of keys, in pages the batch read admits.
async fn stored_paged(service: &RegistryService, ids: &[&str]) -> Vec<Option<(i64, Value)>> {
    let mut rows = Vec::with_capacity(ids.len());
    for page in ids.chunks(MAX_BATCH_GET_KEYS) {
        rows.extend(stored(service, page).await);
    }
    rows
}

#[tokio::test]
async fn more_seeds_than_the_default_limit_go_out_in_several_requests() {
    let dir = common::TestDir::new("tr-seed-many");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let documents = instances(150);
    let set = SeedSet::collect(
        documents.iter().map(|(_, document)| document.clone()),
        &TypesRegistryConfig::default().entity_id_fields,
    )
    .unwrap();
    let limit = TypesRegistryConfig::default().limits.batch_candidates;
    assert!(set.len() > limit, "the set must exceed the default limit");

    seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect("a cold start over the limit");

    let batches = submitted_batches(&dir).await;
    assert!(batches.len() >= 2, "{batches:?}");
    assert!(
        batches
            .iter()
            .all(|items| usize::try_from(*items).is_ok_and(|n| n <= limit)),
        "{batches:?}"
    );
    let ids: Vec<&str> = documents.iter().map(|(id, _)| id.as_str()).collect();
    let read = stored_paged(&r.service, &ids).await;
    for ((id, document), row) in documents.iter().zip(&read) {
        assert_eq!(row.as_ref(), Some(&(1, document.clone())), "{id}");
    }

    let restart = seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect("an equal restart");
    assert!(restart.up_to_date);
    assert_eq!(
        submitted_batches(&dir).await.len(),
        batches.len(),
        "no new operation"
    );
}

/// `gts.cf.core.seed_test.chain_a.v1~` refers to `…chain_b…`, and so on to the last
/// link: identifier order, which is submission order, is the reverse of dependency order.
fn chain(links: u8) -> (Vec<String>, SeedSet) {
    let ids: Vec<String> = (0..links)
        .map(|n| format!("gts.cf.core.seed_test.chain_{}.v1~", char::from(b'a' + n)))
        .collect();
    let mut documents: Vec<Value> = ids
        .iter()
        .enumerate()
        .map(|(n, id)| {
            let mut schema = json!({
                "$id": format!("gts://{id}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {"id": {"type": "string"}}
            });
            if let Some(next) = ids.get(n + 1) {
                schema["properties"]["next"] = json!({"$ref": format!("gts://{next}")});
            }
            schema
        })
        .collect();
    // An instance of the head: it waits for the whole chain.
    let instance = format!("{}cf.core.seed_test.head.v1", ids[0]);
    documents.push(json!({"id": instance}));
    let mut all = ids;
    all.push(instance);
    let set =
        SeedSet::collect(documents, &TypesRegistryConfig::default().entity_id_fields).unwrap();
    (all, set)
}

#[tokio::test]
async fn a_dependency_chain_against_submission_order_converges_one_candidate_at_a_time() {
    let dir = common::TestDir::new("tr-seed-chain");
    let db = database(&dir).await;
    let mut config = TypesRegistryConfig::default();
    config.limits.batch_candidates = 1;
    let r = registry(&db, config).await;
    // Seven links: more passes than the SDK's default five.
    let (ids, set) = chain(7);

    seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect("every link converges");

    let batches = submitted_batches(&dir).await;
    assert!(batches.iter().all(|items| *items == 1), "{batches:?}");
    assert!(batches.len() > ids.len(), "dependents waited: {batches:?}");
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let read = stored(&r.service, &ids).await;
    assert!(
        read.iter()
            .all(|row| row.as_ref().map(|(rv, _)| *rv) == Some(1)),
        "{read:?}"
    );
}

#[tokio::test]
async fn an_expired_budget_fails_naming_the_undecided_seeds() {
    let dir = common::TestDir::new("tr-seed-deadline");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let set = seeds("first");

    let error = seed(&r.service, &set, Duration::ZERO, &CancellationToken::new())
        .await
        .expect_err("no budget, no seed");

    let SeedError::Deadline { undecided, .. } = &error else {
        panic!("expected a deadline, got {error}");
    };
    assert!(undecided.iter().any(|f| f.gts_id == CUSTOMER), "{error}");
}

#[tokio::test]
async fn cancellation_stops_seeding() {
    let dir = common::TestDir::new("tr-seed-cancel");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = seed(&r.service, &seeds("first"), BUDGET, &cancel)
        .await
        .expect_err("a cancelled startup does not seed");

    assert!(matches!(error, SeedError::Cancelled), "{error}");
}

/// Cancels `token` the first time the admission worker terminalizes a candidate: the
/// moment a seed's submission has been accepted and is being admitted.
#[derive(Debug)]
struct CancelOnAdmission(CancellationToken);

impl AdmissionMetrics for CancelOnAdmission {
    fn unchanged_probe(&self, _hit: bool) {}

    fn candidate_terminalized(&self, _status: TerminalStatus, _labels: PassLabels) {
        self.0.cancel();
    }

    fn refused(&self, _stage: RefusalStage, _reason: &'static str, _labels: PassLabels) {}

    fn compat_verdict(
        &self,
        _verdict: gts::CompatibilityVerdict,
        _forced: bool,
        _labels: PassLabels,
    ) {
    }

    fn revalidation_retried(
        &self,
        _drift: &types_registry::domain::admission::vector::VectorDrift,
    ) {
    }

    fn observe_activation_write_set(&self, _refreshed: usize, _labels: PassLabels) {}

    fn observe_operation_duration(&self, _elapsed: Duration) {}

    fn admission_delivery(&self, _outcome: DeliveryOutcome) {}
}

/// Cancelled while its submission is being admitted, seeding fails as cancelled — never
/// as a success or a deadline — after it had submitted.
#[tokio::test]
async fn a_cancellation_during_admission_stops_seeding() {
    let dir = common::TestDir::new("tr-seed-cancel-in-flight");
    let db = database(&dir).await;
    let cancel = CancellationToken::new();
    let r = registry_with(
        &db,
        TypesRegistryConfig::default(),
        Arc::new(CancelOnAdmission(cancel.clone())),
    )
    .await;

    let error = seed(&r.service, &seeds("first"), BUDGET, &cancel)
        .await
        .expect_err("cancelled during admission");

    assert!(matches!(error, SeedError::Cancelled), "{error}");
    assert!(
        !submitted_batches(&dir).await.is_empty(),
        "seeding had begun"
    );
}

#[tokio::test]
async fn a_change_only_in_a_signed_zero_is_revised() {
    let dir = common::TestDir::new("tr-seed-signed-zero");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let id = "gts.cf.core.seed_test.zero.v1~";
    let with_default = |default: Value| {
        SeedSet::collect(
            [json!({
                "$id": format!("gts://{id}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {"v": {"type": "number", "default": default}}
            })],
            &TypesRegistryConfig::default().entity_id_fields,
        )
        .unwrap()
    };
    seed(
        &r.service,
        &with_default(json!(0.0)),
        BUDGET,
        &CancellationToken::new(),
    )
    .await
    .expect("first boot");

    let summary = seed(
        &r.service,
        &with_default(json!(-0.0)),
        BUDGET,
        &CancellationToken::new(),
    )
    .await
    .expect("the change converges");

    assert!(
        !summary.up_to_date,
        "-0.0 is a change, not an equal restart"
    );
    let (rv, content) = stored(&r.service, &[id]).await.remove(0).expect("stored");
    assert_eq!(rv, 2);
    assert_eq!(content["properties"]["v"]["default"].to_string(), "-0.0");
}

#[tokio::test]
async fn an_empty_set_still_honours_the_budget_and_cancellation() {
    let dir = common::TestDir::new("tr-seed-empty");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let empty = SeedSet::default();

    let expired = seed(
        &r.service,
        &empty,
        Duration::ZERO,
        &CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(expired, Err(SeedError::Deadline { .. })),
        "{expired:?}"
    );

    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = seed(&r.service, &empty, BUDGET, &cancel).await;
    assert!(
        matches!(cancelled, Err(SeedError::Cancelled)),
        "{cancelled:?}"
    );
}
