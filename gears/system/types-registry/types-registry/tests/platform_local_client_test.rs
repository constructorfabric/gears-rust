#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Real local platform API and database/outbox admission (SPEC §10.1, D15, D19).

use std::sync::Arc;

use gts::{GtsId, GtsIdPattern};
use serde_json::json;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use toolkit_security::PlatformSecurityContext;
use types_registry::api::local_client::PlatformLocalClient;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::key::EntityKey as DomainKey;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{EntityLookup as DomainLookup, RegistryService};
use types_registry::domain::selection::FieldSelection as DomainSelection;
use types_registry::infra::outbox::OutboxDispatch;
use types_registry_sdk::{
    AdmissionFailure, BatchGetEntitiesRequest, BatchGetItem, CandidateStatus,
    DeleteEntitiesRequest, DeleteItem, EntityField, EntityFilter, EntityKey, EntityKind,
    EntityLookup, FieldSelection, IdempotencyKey, LifecycleStatus, ListEntitiesRequest, Operation,
    OperationStatus, Origin, PageRequest, PlatformTypesRegistryApi, Projection, PublisherContext,
    RegisterEntitiesRequest, RegisterItem,
};

mod common;

const CF_TYPE: &str = gts_id!("cf.core.example.type.v1~");
const CF_OTHER: &str = gts_id!("cf.core.example.other.v1~");
const CF_INSTANCE: &str = gts_id!("cf.core.example.type.v1~cf.core.example.first.v1");

struct Harness {
    client: PlatformLocalClient,
    service: Arc<RegistryService>,
    db: Arc<DBProvider<DbError>>,
    dispatch: Arc<OutboxDispatch>,
    _outbox: Box<dyn std::any::Any + Send>,
    _dir: Arc<common::TestDir>,
}

async fn harness() -> Harness {
    let dir = Arc::new(common::TestDir::new("tr-local-client"));
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("local.db").display()
    );
    let db = common::provider_for_with_outbox(&dsn, 8).await;
    serve(db, dir).await
}

/// A healthy service with a running outbox over `db`.
async fn serve(db: Arc<DBProvider<DbError>>, dir: Arc<common::TestDir>) -> Harness {
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        common::stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        common::metrics(),
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .expect("start the admission outbox");
    Harness {
        client: PlatformLocalClient::new(Arc::clone(&service)),
        service,
        db,
        dispatch,
        _outbox: Box::new(outbox),
        _dir: dir,
    }
}

/// Accept through h’s running outbox, but fail operation read-back.
fn failing_read_back(h: &Harness) -> PlatformLocalClient {
    PlatformLocalClient::new(Arc::new(RegistryService::new(
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
        force: false,
    }
}

async fn completed(client: &PlatformLocalClient, operation_id: uuid::Uuid) -> Operation {
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
    assert_eq!(items[0].status, CandidateStatus::Succeeded);
    assert_eq!(items[0].resource_version, Some(1));
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
    assert_eq!(replay.items[0].status, CandidateStatus::Succeeded);
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
        items[0].status,
        CandidateStatus::Succeeded,
        "{:?}",
        items[0].error
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
    let EntityLookup::Found { snapshot, .. } = &lookups.0[&EntityKey::from(id(CF_INSTANCE))] else {
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
    let EntityLookup::Found { snapshot, etag } = &first.0[&key] else {
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

    // T22d's value, framed exactly as REST frames it.
    let selection = DomainSelection::parse(&["content", "origin"]).expect("selection");
    let DomainLookup::Found { etag: domain, .. } = h
        .service
        .lookup(&DomainKey::GtsId(CF_TYPE.to_owned()), selection, None)
        .await
        .expect("domain read")
    else {
        panic!("found");
    };
    assert_eq!(
        etag.as_bytes(),
        format!("\"{}\"", domain.encode()).as_bytes()
    );

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
    assert_eq!(stale.items[0].status, CandidateStatus::Failed);
    let failure = AdmissionFailure::from_canonical(stale.items[0].error.as_ref().expect("error"))
        .expect("decodes");
    assert_eq!(failure.reason, "precondition_failed");

    let ok = h
        .client
        .delete_entities(&ctx(), IdempotencyKey::new("k-del").unwrap(), delete(1))
        .await
        .expect("accepted");
    let Operation::Deletion(done) = completed(&h.client, ok.operation_id).await else {
        panic!("deletion");
    };
    assert_eq!(done.items[0].status, CandidateStatus::Succeeded);
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
    let EntityLookup::Found { snapshot, .. } = &lookups.0[&EntityKey::from(id(CF_TYPE))] else {
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
            .operation(operation_id)
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
    assert_eq!(items[0].status, CandidateStatus::Succeeded);
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
async fn register_and_await_completes_through_the_local_client_and_the_outbox() {
    use types_registry_sdk::PlatformTypesRegistryApiExt;

    let h = harness().await;
    let api: Arc<dyn PlatformTypesRegistryApi> =
        Arc::new(PlatformLocalClient::new(Arc::clone(&h.service)));

    let operation = api
        .register_and_await(
            &ctx(),
            IdempotencyKey::new("k-await").unwrap(),
            register(vec![create(CF_TYPE, schema(CF_TYPE))]),
            std::time::Duration::from_secs(10),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("completes");

    assert_eq!(operation.status, OperationStatus::Completed);
    assert_eq!(operation.items[0].status, CandidateStatus::Succeeded);
    let snapshot = api
        .get_type_schema(&ctx(), CF_TYPE, Projection::Default)
        .await
        .expect("reads back");
    assert_eq!(snapshot.kind, EntityKind::TypeSchema);
}

#[tokio::test]
async fn reconciliation_through_the_local_client_admits_then_reports_up_to_date() {
    use types_registry_sdk::{Outcome, ReconcileOptions, Reconciliation, reconcile};

    let h = harness().await;
    let desired = vec![
        (CF_TYPE.to_owned(), schema(CF_TYPE)),
        (CF_INSTANCE.to_owned(), json!({ "name": "first" })),
    ];
    let cancel = tokio_util::sync::CancellationToken::new();

    let first = reconcile(
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
        outcomes.values().all(|o| matches!(o, Outcome::Admitted)),
        "{outcomes:?}"
    );

    let second = reconcile(
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
async fn a_full_read_carries_every_materialized_document_and_the_provenance() {
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
    let EntityLookup::Found { snapshot, .. } = &read.0[&key] else {
        panic!("found");
    };

    let DomainLookup::Found { record, .. } = h
        .service
        .lookup(
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
    let provenance = snapshot.provenance.as_ref().expect("selected");
    let domain = record.provenance.as_ref().expect("stored");
    assert_eq!(provenance.gts_spec_version, domain.gts_spec_version);
    assert_eq!(provenance.gts_impl_version, domain.gts_impl_version);
    assert_eq!(provenance.compat_forced, domain.compat_forced);
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
