#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Real local platform API and database/outbox admission (SPEC §10.1, D15, D19).

use std::sync::Arc;

use gts::{GtsId, GtsIdPattern, GtsInstanceId};
use serde_json::{Value, json};
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use toolkit_security::PlatformSecurityContext;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::key::EntityKey as DomainKey;
use types_registry::domain::local_client::LocalClient;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{EntityLookup as DomainLookup, RegistryService};
use types_registry::domain::selection::FieldSelection as DomainSelection;
use types_registry::infra::outbox::OutboxDispatch;
use types_registry_sdk::{
    AdmissionFailure, BatchGetEntitiesRequest, BatchGetItem, CandidateStatus,
    DeleteEntitiesRequest, DeleteItem, DeletionOutcome, EntityField, EntityFilter, EntityKey,
    EntityKind, EntityLookup, FieldSelection, IdempotencyKey, LifecycleStatus, ListEntitiesRequest,
    Operation, OperationStatus, Origin, PageRequest, PlatformTypesRegistryApi, Projection,
    Provenance, PublisherContext, ReadOptions, RegisterEntitiesRequest, RegisterItem,
    RegistrationOutcome,
};

mod common;

const CF_TYPE: &str = gts_id!("cf.core.example.type.v1~");
const CF_OTHER: &str = gts_id!("cf.core.example.other.v1~");
const CF_INSTANCE: &str = gts_id!("cf.core.example.type.v1~cf.core.example.first.v1");

struct Harness {
    client: LocalClient,
    service: Arc<RegistryService>,
    db: Arc<DBProvider<DbError>>,
    dispatch: Arc<OutboxDispatch>,
    _outbox: Box<dyn std::any::Any + Send>,
    _dir: Arc<common::TestDir>,
}

async fn harness() -> Harness {
    harness_with(TypesRegistryConfig::default()).await
}

async fn harness_with(config: TypesRegistryConfig) -> Harness {
    let dir = Arc::new(common::TestDir::new("tr-local-client"));
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("local.db").display()
    );
    let db = common::provider_for_with_outbox(&dsn, 8).await;
    serve_with(db, dir, config).await
}

/// A healthy service with a running outbox over `db`.
async fn serve_with(
    db: Arc<DBProvider<DbError>>,
    dir: Arc<common::TestDir>,
    config: TypesRegistryConfig,
) -> Harness {
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        common::stores(),
        RegistrationPolicy::default(),
        config,
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        common::metrics(),
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .expect("start the admission outbox");
    Harness {
        client: LocalClient::new(Arc::clone(&service)),
        service,
        db,
        dispatch,
        _outbox: Box::new(outbox),
        _dir: dir,
    }
}

/// Accept through h’s running outbox, but fail operation read-back.
fn failing_read_back(h: &Harness) -> LocalClient {
    LocalClient::new(Arc::new(RegistryService::new(
        h.db.db(),
        common::TestStores::failing_operation_read(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::clone(&h.dispatch) as Arc<dyn OperationDispatch>,
        common::metrics(),
    )))
}

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("valid identifier")
}

fn instance_id(s: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(s).expect("an Instance identifier")
}

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "local-client-test".to_owned(),
        version: "0.1.0".parse().expect("version"),
    }
}

fn schema(gts_id: &str) -> serde_json::Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn register(items: Vec<RegisterItem>) -> RegisterEntitiesRequest {
    RegisterEntitiesRequest {
        items,
        dry_run: false,
        publisher: publisher(),
    }
}

fn create(gts_id: &str, content: serde_json::Value) -> RegisterItem {
    RegisterItem {
        gts_id: id(gts_id),
        content,
        expected_resource_version: None,
        skip_compatibility_check: false,
    }
}

async fn completed(client: &LocalClient, operation_id: uuid::Uuid) -> Operation {
    common::await_delivery("operation completes", || async {
        let operation = client
            .get_operation(&ctx(), operation_id)
            .await
            .expect("operation reads");
        (operation.status() == OperationStatus::Completed).then_some(operation)
    })
    .await
}

fn items_of(operation: Operation) -> Vec<types_registry_sdk::RegistrationItemResult> {
    match operation {
        Operation::Registration(op) => op.items,
        Operation::Deletion(_) => panic!("expected a registration"),
    }
}

async fn register_and_complete(
    h: &Harness,
    key: &str,
    request: RegisterEntitiesRequest,
) -> Operation {
    let submitted = h
        .client
        .register_entities(&ctx(), IdempotencyKey::new(key).unwrap(), request)
        .await
        .expect("registration is accepted");
    completed(&h.client, submitted.operation_id).await
}

fn batch(keys: Vec<BatchGetItem>, projection: Projection) -> BatchGetEntitiesRequest {
    BatchGetEntitiesRequest {
        items: keys,
        projection,
        fresh: false,
    }
}

#[tokio::test]
async fn a_submit_returns_the_operation_as_read_with_its_real_items() {
    let h = harness().await;

    let submitted = h
        .client
        .register_entities(
            &ctx(),
            IdempotencyKey::new("k-read").unwrap(),
            register(vec![create(CF_TYPE, schema(CF_TYPE))]),
        )
        .await
        .expect("accepted");

    assert_eq!(
        submitted.items.len(),
        1,
        "never an empty receipt-built operation"
    );
    assert_eq!(submitted.items[0].gts_id, id(CF_TYPE));

    let items = items_of(completed(&h.client, submitted.operation_id).await);
    assert!(
        matches!(
            items[0].outcome,
            RegistrationOutcome::Succeeded {
                resource_version: 1
            }
        ),
        "{:?}",
        items[0].outcome
    );
}

#[tokio::test]
async fn a_terminal_replay_returns_the_operation_read_back_with_its_items() {
    let h = harness().await;
    let first = register_and_complete(
        &h,
        "k-replay",
        register(vec![create(CF_TYPE, schema(CF_TYPE))]),
    )
    .await;

    let replay = h
        .client
        .register_entities(
            &ctx(),
            IdempotencyKey::new("k-replay").unwrap(),
            register(vec![create(CF_TYPE, schema(CF_TYPE))]),
        )
        .await
        .expect("the same key and request replays");

    assert_eq!(replay.operation_id, first.operation_id());
    assert_eq!(replay.status, OperationStatus::Completed);
    assert_eq!(replay.items.len(), 1);
    assert_eq!(replay.items[0].outcome.status(), CandidateStatus::Succeeded);
}

#[tokio::test]
async fn an_instance_document_without_an_id_registers_under_the_item_identifier() {
    let h = harness().await;
    register_and_complete(
        &h,
        "k-type",
        register(vec![create(CF_TYPE, schema(CF_TYPE))]),
    )
    .await;

    let items = items_of(
        register_and_complete(
            &h,
            "k-inst",
            register(vec![create(CF_INSTANCE, json!({ "name": "first" }))]),
        )
        .await,
    );
    assert_eq!(
        items[0].outcome.status(),
        CandidateStatus::Succeeded,
        "{:?}",
        items[0].outcome
    );

    let lookups = h
        .client
        .batch_get_entities(
            &ctx(),
            batch(
                vec![EntityKey::from(id(CF_INSTANCE)).into()],
                Projection::Select(FieldSelection::with(&[EntityField::Content])),
            ),
        )
        .await
        .expect("reads");
    let EntityLookup::Found {
        entity: snapshot, ..
    } = &lookups.0[&EntityKey::from(id(CF_INSTANCE))]
    else {
        panic!("the Instance is found");
    };
    assert_eq!(snapshot.kind, EntityKind::Instance);
    assert_eq!(snapshot.content, Some(json!({ "name": "first" })));
}

#[tokio::test]
async fn a_type_schema_whose_id_differs_from_the_item_identifier_is_refused() {
    let h = harness().await;

    let refused = h
        .client
        .register_entities(
            &ctx(),
            IdempotencyKey::new("k-mismatch").unwrap(),
            register(vec![create(CF_TYPE, schema(CF_OTHER))]),
        )
        .await;

    assert!(
        matches!(
            refused,
            Err(toolkit_canonical_errors::CanonicalError::InvalidArgument { .. })
        ),
        "{refused:?}"
    );
}

/// The typed request reaches acceptance: without deployment permission the refusal
/// names the SDK's `skip_compatibility_check` field.
#[tokio::test]
async fn a_typed_skip_request_is_refused_naming_its_field() {
    let h = harness().await;
    let mut item = create(CF_TYPE, schema(CF_TYPE));
    item.skip_compatibility_check = true;

    let refused = h
        .client
        .register_entities(
            &ctx(),
            IdempotencyKey::new("k-skip").unwrap(),
            register(vec![item]),
        )
        .await;

    let Err(toolkit_canonical_errors::CanonicalError::InvalidArgument {
        ctx: toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations },
        ..
    }) = &refused
    else {
        panic!("a field refusal: {refused:?}");
    };
    assert_eq!(
        field_violations
            .iter()
            .map(|v| v.field.as_str())
            .collect::<Vec<_>>(),
        [types_registry_sdk::field::SKIP_COMPATIBILITY_CHECK_FIELD]
    );
}

/// Where the deployment permits it, a typed skip request on a later minor is admitted
/// and its Type Schema reads back `provenance.compatibility_check_skipped = true`.
#[tokio::test]
async fn a_typed_skip_request_is_recorded_and_read_back() {
    let h = harness_with(TypesRegistryConfig {
        allow_compatibility_force: true,
        ..TypesRegistryConfig::default()
    })
    .await;
    let first = gts_id!("cf.core.example.minor.v1.0~");
    let second = gts_id!("cf.core.example.minor.v1.1~");
    register_and_complete(&h, "k-first", register(vec![create(first, schema(first))])).await;
    let mut item = create(second, schema(second));
    item.skip_compatibility_check = true;
    let items = items_of(register_and_complete(&h, "k-second", register(vec![item])).await);
    assert_eq!(
        items[0].outcome.status(),
        CandidateStatus::Succeeded,
        "{:?}",
        items[0].outcome
    );

    let projection = Projection::Select(FieldSelection::with(&[EntityField::Provenance]));
    let lookups = h
        .client
        .batch_get_entities(
            &ctx(),
            batch(
                vec![
                    EntityKey::from(id(first)).into(),
                    EntityKey::from(id(second)).into(),
                ],
                projection,
            ),
        )
        .await
        .expect("reads");
    let skipped = |key: &str| match &lookups.0[&EntityKey::from(id(key))] {
        EntityLookup::Found { entity, .. } => {
            entity.provenance.map(|p| p.compatibility_check_skipped)
        }
        other => panic!("{key} is found: {other:?}"),
    };
    assert_eq!(
        skipped(first),
        Some(false),
        "the first minor has nothing to skip"
    );
    assert_eq!(skipped(second), Some(true));
}

#[tokio::test]
async fn a_read_carries_the_domain_validator_byte_for_byte_and_answers_unchanged() {
    let h = harness().await;
    register_and_complete(&h, "k-v", register(vec![create(CF_TYPE, schema(CF_TYPE))])).await;
    let projection = Projection::Select(FieldSelection::with(&[
        EntityField::Content,
        EntityField::Origin,
    ]));

    let key = EntityKey::from(id(CF_TYPE));
    let first = h
        .client
        .batch_get_entities(&ctx(), batch(vec![key.clone().into()], projection.clone()))
        .await
        .expect("reads");
    let EntityLookup::Found {
        entity: snapshot,
        etag,
    } = &first.0[&key]
    else {
        panic!("found");
    };
    assert_eq!(snapshot.content, Some(schema(CF_TYPE)));
    assert!(matches!(
        snapshot.origin,
        Some(Origin::Managed {
            resource_version: 1,
            ..
        })
    ));

    // T22d's token itself: the RFC 9110 quotes are REST's representation, not the validator.
    let selection = DomainSelection::parse(&["content", "origin"]).expect("selection");
    let DomainLookup::Found { etag: domain, .. } = h
        .service
        .lookup(
            common::caller(),
            &DomainKey::GtsId(CF_TYPE.to_owned()),
            selection,
            None,
        )
        .await
        .expect("domain read")
    else {
        panic!("found");
    };
    assert_eq!(etag.as_bytes(), domain.encode().as_bytes());

    let again = h
        .client
        .batch_get_entities(
            &ctx(),
            batch(
                vec![BatchGetItem {
                    key: key.clone(),
                    if_none_match: Some(etag.clone()),
                }],
                projection,
            ),
        )
        .await
        .expect("reads");
    assert_eq!(
        again.0[&key],
        EntityLookup::Unchanged { etag: etag.clone() }
    );
}

#[tokio::test]
async fn a_validator_this_registry_never_issued_is_answered_in_full() {
    let h = harness().await;
    register_and_complete(&h, "k-n", register(vec![create(CF_TYPE, schema(CF_TYPE))])).await;
    let key = EntityKey::from(id(CF_TYPE));

    let lookups = h
        .client
        .batch_get_entities(
            &ctx(),
            batch(
                vec![BatchGetItem {
                    key: key.clone(),
                    if_none_match: Some(types_registry_sdk::Validator::from_bytes(
                        b"\"not-a-token\"".to_vec(),
                    )),
                }],
                Projection::Default,
            ),
        )
        .await
        .expect("an unknown token is not a refusal");

    assert!(matches!(lookups.0[&key], EntityLookup::Found { .. }));
}

#[tokio::test]
async fn an_absent_key_is_not_found_and_answers_under_the_key_as_asked() {
    let h = harness().await;
    let by_uuid = EntityKey::GtsUuid(id(CF_TYPE).to_uuid());

    let lookups = h
        .client
        .batch_get_entities(
            &ctx(),
            batch(
                vec![EntityKey::from(id(CF_TYPE)).into(), by_uuid.clone().into()],
                Projection::Default,
            ),
        )
        .await
        .expect("reads");

    assert_eq!(lookups.0.len(), 2);
    assert_eq!(lookups.0[&by_uuid], EntityLookup::NotFound);
}

#[tokio::test]
async fn discovery_pages_with_a_cursor_bound_to_its_query() {
    let h = harness().await;
    register_and_complete(
        &h,
        "k-two",
        register(vec![
            create(CF_TYPE, schema(CF_TYPE)),
            create(CF_OTHER, schema(CF_OTHER)),
        ]),
    )
    .await;
    let filter = EntityFilter {
        pattern: Some(GtsIdPattern::try_new("gts.cf.core.example.*").expect("pattern")),
        kind: Some(EntityKind::TypeSchema),
        ..EntityFilter::default()
    };
    let first = h
        .client
        .list_entities(
            &ctx(),
            ListEntitiesRequest {
                filter: filter.clone(),
                projection: Projection::Default,
                page: PageRequest {
                    limit: Some(1),
                    cursor: None,
                },
            },
        )
        .await
        .expect("first page");
    assert_eq!(first.items.len(), 1);
    let cursor = first.next.expect("a second page exists");

    let second = h
        .client
        .list_entities(
            &ctx(),
            ListEntitiesRequest {
                filter: filter.clone(),
                projection: Projection::Default,
                page: PageRequest {
                    limit: Some(1),
                    cursor: Some(cursor.clone()),
                },
            },
        )
        .await
        .expect("second page");
    assert_eq!(second.items.len(), 1);
    assert_ne!(second.items[0].gts_id, first.items[0].gts_id);
    assert_eq!(second.next, None);

    let rebound = h
        .client
        .list_entities(
            &ctx(),
            ListEntitiesRequest {
                filter: EntityFilter {
                    kind: Some(EntityKind::Instance),
                    ..filter
                },
                projection: Projection::Default,
                page: PageRequest {
                    limit: Some(1),
                    cursor: Some(cursor),
                },
            },
        )
        .await;
    assert!(
        rebound.is_err(),
        "a cursor replayed under another filter is refused"
    );
}

#[tokio::test]
async fn deletion_round_trips_and_a_failed_item_carries_its_reason() {
    let h = harness().await;
    register_and_complete(
        &h,
        "k-del-setup",
        register(vec![create(CF_TYPE, schema(CF_TYPE))]),
    )
    .await;
    let delete = |version| DeleteEntitiesRequest {
        items: vec![DeleteItem {
            key: EntityKey::from(id(CF_TYPE)),
            expected_resource_version: version,
        }],
        dry_run: false,
        publisher: publisher(),
    };

    let stale = h
        .client
        .delete_entities(
            &ctx(),
            IdempotencyKey::new("k-del-stale").unwrap(),
            delete(7),
        )
        .await
        .expect("accepted");
    let Operation::Deletion(stale) = completed(&h.client, stale.operation_id).await else {
        panic!("deletion");
    };
    assert_eq!(stale.items[0].outcome.status(), CandidateStatus::Failed);
    let failure = AdmissionFailure::from_canonical(stale.items[0].outcome.error().expect("error"))
        .expect("decodes");
    assert_eq!(
        failure.reason,
        types_registry_sdk::item_failure::AdmissionFailureReason::PreconditionFailed
    );

    let ok = h
        .client
        .delete_entities(&ctx(), IdempotencyKey::new("k-del").unwrap(), delete(1))
        .await
        .expect("accepted");
    let Operation::Deletion(done) = completed(&h.client, ok.operation_id).await else {
        panic!("deletion");
    };
    assert!(
        matches!(done.items[0].outcome, DeletionOutcome::Succeeded { .. }),
        "{:?}",
        done.items[0].outcome
    );
    assert_eq!(done.items[0].entity_key, EntityKey::from(id(CF_TYPE)));

    let lookups = h
        .client
        .batch_get_entities(
            &ctx(),
            batch(
                vec![EntityKey::from(id(CF_TYPE)).into()],
                Projection::Default,
            ),
        )
        .await
        .expect("reads");
    let EntityLookup::Found {
        entity: snapshot, ..
    } = &lookups.0[&EntityKey::from(id(CF_TYPE))]
    else {
        panic!("a tombstone stays exact-readable");
    };
    assert_eq!(snapshot.lifecycle_status, LifecycleStatus::Deleted);
}

#[tokio::test]
async fn an_unknown_operation_is_not_found() {
    let h = harness().await;
    let missing = h.client.get_operation(&ctx(), uuid::Uuid::nil()).await;
    assert!(matches!(
        missing,
        Err(toolkit_canonical_errors::CanonicalError::NotFound { .. })
    ));
}

fn aborted_operation(error: &toolkit_canonical_errors::CanonicalError) -> uuid::Uuid {
    assert!(
        matches!(
            error,
            toolkit_canonical_errors::CanonicalError::Aborted { .. }
        ),
        "{error:?}"
    );
    error
        .resource_name()
        .and_then(|name| uuid::Uuid::parse_str(name).ok())
        .expect("the error names the accepted operation")
}

#[tokio::test]
async fn a_failed_read_back_names_the_accepted_operation_and_the_same_key_recovers_it() {
    let h = harness().await;
    let request = || register(vec![create(CF_TYPE, schema(CF_TYPE))]);

    let lost = failing_read_back(&h)
        .register_entities(&ctx(), IdempotencyKey::new("k-lost").unwrap(), request())
        .await
        .expect_err("the read back fails");
    let operation_id = aborted_operation(&lost);
    assert!(
        h.service
            .operation(common::caller(), operation_id)
            .await
            .expect("reads")
            .is_some(),
        "the named operation is the one that was persisted"
    );

    let recovered = h
        .client
        .register_entities(&ctx(), IdempotencyKey::new("k-lost").unwrap(), request())
        .await
        .expect("the same key replays");
    assert_eq!(recovered.operation_id, operation_id);
    assert_eq!(recovered.items.len(), 1);
    let items = items_of(completed(&h.client, operation_id).await);
    assert_eq!(items[0].outcome.status(), CandidateStatus::Succeeded);
}

#[tokio::test]
async fn a_failed_deletion_read_back_is_recovered_by_the_same_key() {
    let h = harness().await;
    register_and_complete(
        &h,
        "k-setup",
        register(vec![create(CF_TYPE, schema(CF_TYPE))]),
    )
    .await;
    let request = || DeleteEntitiesRequest {
        items: vec![DeleteItem {
            key: EntityKey::from(id(CF_TYPE)),
            expected_resource_version: 1,
        }],
        dry_run: false,
        publisher: publisher(),
    };

    let lost = failing_read_back(&h)
        .delete_entities(
            &ctx(),
            IdempotencyKey::new("k-del-lost").unwrap(),
            request(),
        )
        .await
        .expect_err("the read back fails");
    let operation_id = aborted_operation(&lost);

    let recovered = h
        .client
        .delete_entities(
            &ctx(),
            IdempotencyKey::new("k-del-lost").unwrap(),
            request(),
        )
        .await
        .expect("the same key replays");
    assert_eq!(recovered.operation_id, operation_id);
    assert_eq!(recovered.items.len(), 1);
}

#[tokio::test]
async fn a_policy_refusal_stops_reconciliation_with_unjudged_entities_skipped() {
    use toolkit_canonical_errors::CanonicalError;
    use types_registry_sdk::{
        PlatformTypesRegistryApiExt, ReconcileOptions, ReconcileOutcome, ReconcilePendingCause,
        ReconcileReport, ReconcileStop, TypesRegistryError,
    };

    const DENIED: &str = "gts.acme.closed.example.type.v1~";
    let h = harness().await;
    let api: &dyn PlatformTypesRegistryApi = &h.client;
    let error: CanonicalError = api
        .reconcile_entities_and_await(
            &ctx(),
            &publisher(),
            &[
                (id(CF_TYPE), schema(CF_TYPE)),
                (id(DENIED), schema(DENIED)),
                (id(CF_OTHER), schema(CF_OTHER)),
            ],
            &ReconcileOptions::default(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect_err("vendor denied by deployment policy");
    let report = ReconcileReport::from_canonical(&error).expect("report");
    assert_eq!(report.stop, ReconcileStop::InvalidInput);
    let ReconcileOutcome::Rejected(error) = &report.outcomes[&id(DENIED)] else {
        panic!("named policy refusal: {report:?}");
    };
    assert!(matches!(
        TypesRegistryError::from(error.clone()),
        TypesRegistryError::PolicyRefused { .. }
    ));
    for key in [CF_TYPE, CF_OTHER] {
        assert!(matches!(
            report.outcomes[&id(key)],
            ReconcileOutcome::Pending(ReconcilePendingCause::Skipped)
        ));
    }
}

#[tokio::test]
async fn incomplete_reconciliation_exposes_a_canonical_report_through_the_local_client() {
    use toolkit_canonical_errors::{CanonicalError, Problem};
    use types_registry_sdk::{
        PlatformTypesRegistryApiExt, ReconcileOptions, ReconcileOutcome, ReconcilePendingCause,
        ReconcileReport, ReconcileStop, Reconciliation,
    };

    let h = harness().await;
    let api: &dyn PlatformTypesRegistryApi = &h.client;
    let mut options = ReconcileOptions::default();
    options.batch_size = std::num::NonZeroUsize::MIN;
    let result: Result<Reconciliation, CanonicalError> = api
        .reconcile_entities_and_await(
            &ctx(),
            &publisher(),
            &[
                (id(CF_OTHER), schema(CF_OTHER)),
                (id(CF_TYPE), json!({ "type": "object" })),
                (id(CF_INSTANCE), json!({ "name": "not submitted" })),
            ],
            &options,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
    let error = result.expect_err("missing schema identity stops submission");
    let transported = CanonicalError::try_from(Problem::from_error(&error).unwrap()).unwrap();
    let report = ReconcileReport::from_canonical(&transported).expect("canonical report");
    assert_eq!(report.stop, ReconcileStop::InvalidInput);
    assert_eq!(report.outcomes.len(), 3);
    assert!(matches!(
        report.outcomes[&id(CF_OTHER)],
        ReconcileOutcome::Admitted
    ));
    let ReconcileOutcome::Rejected(CanonicalError::InvalidArgument { resource_name, .. }) =
        &report.outcomes[&id(CF_TYPE)]
    else {
        panic!("expected the original validation error: {report:?}");
    };
    assert_eq!(resource_name.as_deref(), Some(CF_TYPE));
    assert!(matches!(
        report.outcomes[&id(CF_INSTANCE)],
        ReconcileOutcome::Pending(ReconcilePendingCause::Skipped)
    ));
}

#[tokio::test]
async fn reconcile_entities_and_await_creates_then_updates_drift_through_a_dyn_client() {
    use types_registry_sdk::{
        PlatformTypesRegistryApiExt, ReconcileOptions, ReconcileOutcome, Reconciliation,
    };

    let h = harness().await;
    let api: Arc<dyn PlatformTypesRegistryApi> = Arc::new(LocalClient::new(Arc::clone(&h.service)));
    let cancel = tokio_util::sync::CancellationToken::new();
    let reconcile = |desired: Vec<(GtsId, serde_json::Value)>| {
        let api = Arc::clone(&api);
        let cancel = cancel.clone();
        async move {
            api.reconcile_entities_and_await(
                &ctx(),
                &publisher(),
                &desired,
                &ReconcileOptions::default(),
                &cancel,
            )
            .await
            .expect("reconciles")
        }
    };
    let admitted = |run: Reconciliation| {
        let Reconciliation::Reconciled(outcomes) = run else {
            panic!("work was submitted: {run:?}");
        };
        assert!(
            outcomes
                .values()
                .all(|o| matches!(o, ReconcileOutcome::Admitted)),
            "{outcomes:?}"
        );
    };

    admitted(
        reconcile(vec![
            (id(CF_TYPE), schema(CF_TYPE)),
            (id(CF_INSTANCE), json!({ "name": "first" })),
        ])
        .await,
    );

    // The same identifier with other content is an update, not a conflict.
    admitted(reconcile(vec![(id(CF_INSTANCE), json!({ "name": "second" }))]).await);
    let instance = api
        .get_instance(
            &ctx(),
            &instance_id(CF_INSTANCE),
            ReadOptions {
                projection: Projection::Select(FieldSelection::with(&[EntityField::Content])),
                ..ReadOptions::default()
            },
        )
        .await
        .expect("reads back");
    assert_eq!(instance.content, Some(json!({ "name": "second" })));
}

#[tokio::test]
async fn reconciliation_through_the_local_client_admits_then_reports_up_to_date() {
    use types_registry_sdk::{
        PlatformTypesRegistryApiExt, ReconcileOptions, ReconcileOutcome, Reconciliation,
    };

    let h = harness().await;
    let desired = vec![
        (id(CF_TYPE), schema(CF_TYPE)),
        (id(CF_INSTANCE), json!({ "name": "first" })),
    ];
    let cancel = tokio_util::sync::CancellationToken::new();

    let first = PlatformTypesRegistryApiExt::reconcile_entities_and_await(
        &h.client,
        &ctx(),
        &publisher(),
        &desired,
        &ReconcileOptions::default(),
        &cancel,
    )
    .await
    .expect("reconciles");
    let Reconciliation::Reconciled(outcomes) = first else {
        panic!("the first run submits");
    };
    assert!(
        outcomes
            .values()
            .all(|o| matches!(o, ReconcileOutcome::Admitted)),
        "{outcomes:?}"
    );

    let second = PlatformTypesRegistryApiExt::reconcile_entities_and_await(
        &h.client,
        &ctx(),
        &publisher(),
        &desired,
        &ReconcileOptions::default(),
        &cancel,
    )
    .await
    .expect("reconciles");
    assert!(matches!(second, Reconciliation::UpToDate), "{second:?}");
}

#[tokio::test]
async fn a_full_read_carries_every_materialized_document_and_the_waiver_flag() {
    let h = harness().await;
    register_and_complete(
        &h,
        "k-full",
        register(vec![create(CF_TYPE, schema(CF_TYPE))]),
    )
    .await;

    let key = EntityKey::from(id(CF_TYPE));
    let read = h
        .client
        .batch_get_entities(
            &ctx(),
            batch(
                vec![key.clone().into()],
                Projection::Select(FieldSelection::full()),
            ),
        )
        .await
        .expect("reads");
    let EntityLookup::Found {
        entity: snapshot, ..
    } = &read.0[&key]
    else {
        panic!("found");
    };

    let DomainLookup::Found { record, .. } = h
        .service
        .lookup(
            common::caller(),
            &DomainKey::GtsId(CF_TYPE.to_owned()),
            DomainSelection::full(),
            None,
        )
        .await
        .expect("domain read")
    else {
        panic!("found");
    };
    let json = |raw: &Option<Box<serde_json::value::RawValue>>| {
        raw.as_ref()
            .map(|raw| serde_json::from_str::<serde_json::Value>(raw.get()).expect("JSON"))
    };
    assert!(snapshot.resolved_schema.is_some(), "a Type Schema has one");
    assert_eq!(snapshot.resolved_schema, json(&record.resolved_schema));
    assert_eq!(snapshot.effective_traits, json(&record.effective_traits));
    assert_eq!(
        snapshot.effective_traits_schema,
        json(&record.effective_traits_schema)
    );
    assert_eq!(
        snapshot.provenance,
        Some(Provenance {
            compatibility_check_skipped: false
        }),
        "selected"
    );
    assert_eq!(
        snapshot.provenance.map(|p| p.compatibility_check_skipped),
        record.provenance.map(|p| p.compatibility_check_skipped)
    );
}

#[tokio::test]
async fn discovery_honours_a_chain_depth() {
    let h = harness().await;
    register_and_complete(
        &h,
        "k-depth",
        register(vec![create(CF_TYPE, schema(CF_TYPE))]),
    )
    .await;
    register_and_complete(
        &h,
        "k-depth-i",
        register(vec![create(CF_INSTANCE, json!({ "name": "first" }))]),
    )
    .await;
    let list = |depth: u8| ListEntitiesRequest {
        filter: EntityFilter {
            pattern: Some(GtsIdPattern::try_new("gts.cf.core.example.*").expect("pattern")),
            max_chain_depth: Some(std::num::NonZeroU8::new(depth).expect("non-zero")),
            ..EntityFilter::default()
        },
        projection: Projection::Default,
        page: PageRequest::default(),
    };

    let shallow = h
        .client
        .list_entities(&ctx(), list(1))
        .await
        .expect("lists");
    let deep = h
        .client
        .list_entities(&ctx(), list(2))
        .await
        .expect("lists");

    let ids = |page: &types_registry_sdk::ListEntitiesResponse| {
        page.items
            .iter()
            .map(|s| s.gts_id.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&shallow),
        [CF_TYPE],
        "one segment: the Type Schema only"
    );
    assert!(
        ids(&deep).contains(&CF_INSTANCE.to_owned()),
        "{:?}",
        ids(&deep)
    );
}

/// The checks the SDK's fake also passes: the two cannot drift apart unnoticed.
#[tokio::test]
async fn the_local_client_conforms_to_the_contract() {
    let h = harness().await;
    types_registry_sdk::testing_platform::conformance::run(&h.client).await;
}

/// Events as a JSON-logging production subscriber at INFO writes them, spans included.
mod captured {
    use std::io;
    use std::sync::Arc;

    use parking_lot::Mutex;
    use serde_json::Value;
    use tracing::subscriber::DefaultGuard;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    pub struct Log(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Log {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Log {
        type Writer = Self;

        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }

    /// Keeps a second dispatcher registered for the whole binary. With only one, tracing-core
    /// computes a callsite's interest from the registering thread's default, so a test
    /// without a capture can cache `never` for a callsite another test's capture needs.
    fn keep_interest_shared() {
        static SHARED: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
        SHARED.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    }

    impl Log {
        pub fn install() -> (Self, DefaultGuard) {
            keep_interest_shared();
            let log = Self::default();
            let subscriber = tracing_subscriber::fmt()
                .json()
                .with_current_span(true)
                .with_span_list(true)
                .with_max_level(tracing::Level::INFO)
                .with_writer(log.clone())
                .finish();
            (log, tracing::subscriber::set_default(subscriber))
        }

        pub fn lines(&self) -> Vec<Value> {
            String::from_utf8_lossy(&self.0.lock())
                .lines()
                .map(|line| serde_json::from_str(line).expect("a JSON log line"))
                .collect()
        }

        /// The lines whose `log_kind` field is `name`.
        pub fn named(&self, name: &str) -> Vec<Value> {
            self.lines()
                .into_iter()
                .filter(|line| line["fields"]["log_kind"] == name)
                .collect()
        }
    }

    /// The fields of the innermost span named `name` around `line`.
    pub fn span<'a>(line: &'a Value, name: &str) -> &'a Value {
        line["spans"]
            .as_array()
            .and_then(|spans| spans.iter().rev().find(|s| s["name"] == name))
            .unwrap_or_else(|| panic!("no {name} span around {line}"))
    }
}

#[tokio::test]
async fn an_accepted_and_a_replayed_submission_name_one_operation_and_its_key() {
    let h = harness().await;
    let (log, _guard) = captured::Log::install();
    let key = uuid::Uuid::new_v4().to_string();
    let request = || register(vec![create(CF_TYPE, schema(CF_TYPE))]);

    let first = h
        .client
        .register_entities(&ctx(), IdempotencyKey::new(key.clone()).unwrap(), request())
        .await
        .expect("accepted");
    // The acknowledgement was lost: the client retries under the same key.
    let replay = h
        .client
        .register_entities(&ctx(), IdempotencyKey::new(key.clone()).unwrap(), request())
        .await
        .expect("replayed");

    assert_eq!(replay.operation_id, first.operation_id);
    let accepted = log.named("operation_accepted");
    assert_eq!(accepted.len(), 2, "{accepted:#?}");
    for (line, replayed) in accepted.iter().zip([false, true]) {
        let fields = &line["fields"];
        assert_eq!(fields["idempotency_key"], key.as_str());
        assert_eq!(fields["operation_id"], first.operation_id.to_string());
        assert_eq!(fields["replayed"], replayed);
        assert_eq!(fields["kind"], "registration");
    }
}

#[tokio::test]
async fn reconciliation_through_the_local_client_is_traced_from_call_to_operation() {
    use types_registry_sdk::{PlatformTypesRegistryApiExt, ReconcileOptions};

    let h = harness().await;
    let (log, _guard) = captured::Log::install();
    h.client
        .reconcile_entities_and_await(
            &ctx(),
            &publisher(),
            &[(id(CF_TYPE), schema(CF_TYPE))],
            &ReconcileOptions::default(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("reconciles");

    let [accepted] = <[_; 1]>::try_from(log.named("operation_accepted")).expect("one");
    let [batch] = <[_; 1]>::try_from(log.named("batch_accepted")).expect("one");
    let [finished] = <[_; 1]>::try_from(log.named("reconcile_finished")).expect("one");

    // The registry's acceptance runs in the caller's task, inside the call's spans.
    let reconcile_id = &captured::span(&finished, "reconcile")["reconcile_id"];
    assert_eq!(
        &captured::span(&accepted, "reconcile")["reconcile_id"],
        reconcile_id
    );
    let key = &captured::span(&batch, "reconcile_batch")["idempotency_key"];
    assert_eq!(&accepted["fields"]["idempotency_key"], key);
    assert_eq!(
        &captured::span(&accepted, "reconcile_batch")["idempotency_key"],
        key
    );
    assert_eq!(
        accepted["fields"]["operation_id"],
        batch["fields"]["operation_id"]
    );
    assert_eq!(finished["fields"]["stop"], "completed");
    assert_eq!(finished["fields"]["admitted"], 1);
    assert_eq!(finished["level"], "INFO");
    let text = log
        .lines()
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    assert!(
        !text.contains("\"properties\""),
        "no document content is logged"
    );
}

/// The local client, except that a registration's answer never comes back: the registry
/// accepts it, says so on `accepted`, and the client never learns it.
struct LostAcknowledgement {
    client: LocalClient,
    accepted: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl PlatformTypesRegistryApi for LostAcknowledgement {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<
        types_registry_sdk::BatchGetEntitiesResponse,
        toolkit_canonical_errors::CanonicalError,
    > {
        self.client.batch_get_entities(ctx, request).await
    }

    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, toolkit_canonical_errors::CanonicalError>
    {
        self.client.list_entities(ctx, request).await
    }

    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<types_registry_sdk::RegistrationOperation, toolkit_canonical_errors::CanonicalError>
    {
        self.client
            .register_entities(ctx, key, request)
            .await
            .expect("the registry accepts it");
        self.accepted.notify_one();
        std::future::pending().await
    }

    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<types_registry_sdk::DeletionOperation, toolkit_canonical_errors::CanonicalError>
    {
        self.client.delete_entities(ctx, key, request).await
    }

    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: uuid::Uuid,
    ) -> Result<Operation, toolkit_canonical_errors::CanonicalError> {
        self.client.get_operation(ctx, operation_id).await
    }
}

/// A remote client's lost acknowledgement, investigated from the logs alone: the client's
/// INFO events of one call name the key, and the registry's acceptance event with that key
/// names the operation — joined by the key, not by span nesting.
#[tokio::test]
async fn a_lost_acknowledgement_is_bridged_from_the_calls_logs_to_its_operation() {
    use types_registry_sdk::{PlatformTypesRegistryApiExt, ReconcileOptions};

    let h = harness().await;
    let api = LostAcknowledgement {
        client: LocalClient::new(Arc::clone(&h.service)),
        accepted: tokio::sync::Notify::new(),
    };
    let (log, _guard) = captured::Log::install();
    let cancel = tokio_util::sync::CancellationToken::new();
    let (ctx, publisher, options) = (ctx(), publisher(), ReconcileOptions::default());
    let desired = [(id(CF_TYPE), schema(CF_TYPE))];
    // The caller gives up only once the registry has accepted: no budget races the call,
    // and a call that ends first fails the test at once.
    let call = api.reconcile_entities_and_await(&ctx, &publisher, &desired, &options, &cancel);
    tokio::pin!(call);
    let result = tokio::select! {
        result = &mut call => panic!("the call ended before the registry accepted: {result:?}"),
        () = api.accepted.notified() => {
            cancel.cancel();
            call.await
        }
    };
    assert!(
        matches!(
            result,
            Err(toolkit_canonical_errors::CanonicalError::Cancelled { .. })
        ),
        "the caller cancelled: {result:?}"
    );

    let [finished] = <[_; 1]>::try_from(log.named("reconcile_finished")).expect("one");
    assert_eq!(finished["fields"]["stop"], "cancelled");
    assert!(
        log.named("batch_accepted").is_empty(),
        "no acknowledgement arrived"
    );
    let reconcile_id = &captured::span(&finished, "reconcile")["reconcile_id"];

    // The client's side: its INFO events of this call, and the key they name.
    let keys: Vec<Value> = log
        .named("batch_submitted")
        .iter()
        .filter(|line| &captured::span(line, "reconcile")["reconcile_id"] == reconcile_id)
        .map(|line| captured::span(line, "reconcile_batch")["idempotency_key"].clone())
        .collect();
    let [key] = <[_; 1]>::try_from(keys).expect("one submitted payload");

    // The registry's side, by key alone.
    let accepted: Vec<Value> = log
        .named("operation_accepted")
        .into_iter()
        .filter(|line| line["fields"]["idempotency_key"] == key)
        .collect();
    let [accepted] = <[_; 1]>::try_from(accepted).expect("the registry accepted it once");
    let operation_id: uuid::Uuid = accepted["fields"]["operation_id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .expect("an operation id");
    h.client
        .get_operation(&ctx, operation_id)
        .await
        .expect("the operation the logs lead to exists");
}
