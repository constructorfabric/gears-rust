#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The local tenant API (SPEC §10.1, D17): the platform's reads under a tenant context.
//!
//! Every answer is compared with the platform answer to the same request on the same
//! client, so lookups, validators, cursors and refusals are shown to be one implementation.

use std::sync::Arc;

use gts::{GtsId, GtsIdPattern, GtsTypeId};
use serde_json::json;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::local_client::LocalClient;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::RegistryService;
use types_registry::infra::outbox::OutboxDispatch;
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetItem, EntityField, EntityFilter, EntityKey, EntityKind,
    EntityLookup, FieldSelection, IdempotencyKey, ListEntitiesRequest, OperationStatus,
    PageRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, Projection,
    PublisherContext, RegisterEntitiesRequest, RegisterItem, TypesRegistryApi, TypesRegistryApiExt,
    Validator,
};

mod common;

const CF_TYPE: &str = gts_id!("cf.core.example.type.v1~");
const CF_OTHER: &str = gts_id!("cf.core.example.other.v1~");
const CF_INSTANCE: &str = gts_id!("cf.core.example.type.v1~cf.core.example.first.v1");

struct Harness {
    client: LocalClient,
    _outbox: Box<dyn std::any::Any + Send>,
    _dir: common::TestDir,
}

async fn harness() -> Harness {
    let dir = common::TestDir::new("tr-tenant-local-client");
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("local.db").display()
    );
    let db = common::provider_for_with_outbox(&dsn, 8).await;
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
        client: LocalClient::new(service),
        _outbox: Box::new((outbox, db)),
        _dir: dir,
    }
}

fn platform() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn tenant() -> SecurityContext {
    SecurityContext::anonymous()
}

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("valid identifier")
}

fn type_id(s: &str) -> GtsTypeId {
    GtsTypeId::try_new(s).expect("a Type Schema identifier")
}

fn schema(gts_id: &str) -> serde_json::Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn create(gts_id: &str, content: serde_json::Value) -> RegisterItem {
    RegisterItem {
        gts_id: id(gts_id),
        content,
        expected_resource_version: None,
        force: false,
    }
}

/// Registers `items` through the platform contract — a tenant cannot — and waits.
async fn seed(h: &Harness, items: Vec<RegisterItem>) {
    let submitted = PlatformTypesRegistryApi::register_entities(
        &h.client,
        &platform(),
        IdempotencyKey::new("seed").unwrap(),
        RegisterEntitiesRequest {
            items,
            dry_run: false,
            publisher: PublisherContext {
                name: "tenant-local-client-test".to_owned(),
                version: "0.1.0".parse().expect("version"),
            },
        },
    )
    .await
    .expect("registration is accepted");
    common::await_delivery("operation completes", || async {
        let operation = h
            .client
            .get_operation(&platform(), submitted.operation_id)
            .await
            .expect("operation reads");
        (operation.status() == OperationStatus::Completed).then_some(())
    })
    .await;
}

fn batch(items: Vec<BatchGetItem>, projection: Projection) -> BatchGetEntitiesRequest {
    BatchGetEntitiesRequest {
        items,
        projection,
        fresh: false,
    }
}

/// A refusal as a caller sees it on the wire.
fn wire(error: CanonicalError) -> serde_json::Value {
    serde_json::to_value(Problem::from(error)).expect("serializes")
}

#[tokio::test]
async fn an_exact_read_through_the_tenant_helper_is_the_platform_read() {
    let h = harness().await;
    seed(
        &h,
        vec![
            create(CF_TYPE, schema(CF_TYPE)),
            create(CF_INSTANCE, json!({ "name": "first" })),
        ],
    )
    .await;
    let projection = Projection::Select(FieldSelection::with(&[EntityField::Content]));

    let schema_read = TypesRegistryApiExt::get_type_schema(
        &h.client,
        &tenant(),
        &type_id(CF_TYPE),
        projection.clone(),
    )
    .await
    .expect("present");
    assert_eq!(schema_read.content, Some(schema(CF_TYPE)));
    assert_eq!(
        schema_read,
        PlatformTypesRegistryApiExt::get_type_schema(
            &h.client,
            &platform(),
            &type_id(CF_TYPE),
            projection.clone()
        )
        .await
        .expect("present")
    );

    let instance_read = TypesRegistryApiExt::get_instance_by_uuid(
        &h.client,
        &tenant(),
        id(CF_INSTANCE).to_uuid(),
        projection.clone(),
    )
    .await
    .expect("present");
    assert_eq!(instance_read.content, Some(json!({ "name": "first" })));
    assert_eq!(
        instance_read,
        PlatformTypesRegistryApiExt::get_instance_by_uuid(
            &h.client,
            &platform(),
            id(CF_INSTANCE).to_uuid(),
            projection
        )
        .await
        .expect("present")
    );
}

#[tokio::test]
async fn a_batch_read_is_found_then_unchanged_under_the_same_select() {
    let h = harness().await;
    seed(&h, vec![create(CF_TYPE, schema(CF_TYPE))]).await;
    let projection = Projection::Select(FieldSelection::with(&[
        EntityField::Content,
        EntityField::Origin,
    ]));
    let key = EntityKey::from(id(CF_TYPE));
    let absent = EntityKey::from(id(CF_OTHER));
    let first_request = batch(
        vec![key.clone().into(), absent.clone().into()],
        projection.clone(),
    );

    let first = TypesRegistryApi::batch_get_entities(&h.client, &tenant(), first_request.clone())
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
    assert_eq!(first.0[&absent], EntityLookup::NotFound);
    assert_eq!(
        first,
        PlatformTypesRegistryApi::batch_get_entities(&h.client, &platform(), first_request)
            .await
            .expect("reads"),
        "the same lookups, validator bytes included"
    );

    let conditional = batch(
        vec![BatchGetItem {
            key: key.clone(),
            if_none_match: Some(etag.clone()),
        }],
        projection,
    );
    let again = TypesRegistryApi::batch_get_entities(&h.client, &tenant(), conditional.clone())
        .await
        .expect("reads");
    assert_eq!(
        again.0[&key],
        EntityLookup::Unchanged { etag: etag.clone() }
    );
    assert_eq!(
        again,
        PlatformTypesRegistryApi::batch_get_entities(&h.client, &platform(), conditional)
            .await
            .expect("reads")
    );
}

#[tokio::test]
async fn an_unusable_condition_reads_in_full_and_an_oversized_one_is_refused() {
    let h = harness().await;
    seed(&h, vec![create(CF_TYPE, schema(CF_TYPE))]).await;
    let key = EntityKey::from(id(CF_TYPE));
    let conditional = |validator: Vec<u8>| {
        batch(
            vec![BatchGetItem {
                key: key.clone(),
                if_none_match: Some(Validator::from_bytes(validator)),
            }],
            Projection::Default,
        )
    };

    // Not UTF-8, so not a token this registry issued: the condition is unusable (DESIGN §3.3).
    let unusable = conditional(vec![0xff, 0xfe]);
    let tenant_read = TypesRegistryApi::batch_get_entities(&h.client, &tenant(), unusable.clone())
        .await
        .expect("an unusable condition is not a refusal");
    assert!(matches!(tenant_read.0[&key], EntityLookup::Found { .. }));
    assert_eq!(
        tenant_read,
        PlatformTypesRegistryApi::batch_get_entities(&h.client, &platform(), unusable)
            .await
            .expect("reads")
    );

    // The bound holds whatever the bytes are.
    let oversized = conditional(vec![0xff; 2048]);
    let tenant_refusal =
        TypesRegistryApi::batch_get_entities(&h.client, &tenant(), oversized.clone())
            .await
            .expect_err("over the validator bound");
    let platform_refusal =
        PlatformTypesRegistryApi::batch_get_entities(&h.client, &platform(), oversized)
            .await
            .expect_err("over the validator bound");
    assert!(matches!(
        tenant_refusal,
        CanonicalError::InvalidArgument { .. }
    ));
    assert_eq!(wire(tenant_refusal), wire(platform_refusal));
}

#[tokio::test]
async fn discovery_pages_with_the_platform_cursor_and_refuses_a_rebound_one() {
    let h = harness().await;
    seed(
        &h,
        vec![
            create(CF_TYPE, schema(CF_TYPE)),
            create(CF_OTHER, schema(CF_OTHER)),
        ],
    )
    .await;
    let filter = EntityFilter {
        pattern: Some(GtsIdPattern::try_new("gts.cf.core.example.*").expect("pattern")),
        kind: Some(EntityKind::TypeSchema),
        ..EntityFilter::default()
    };
    let page = |cursor| ListEntitiesRequest {
        filter: filter.clone(),
        projection: Projection::Default,
        page: PageRequest {
            limit: Some(1),
            cursor,
        },
    };

    let first = TypesRegistryApi::list_entities(&h.client, &tenant(), page(None))
        .await
        .expect("first page");
    assert_eq!(first.items.len(), 1);
    assert_eq!(
        first,
        PlatformTypesRegistryApi::list_entities(&h.client, &platform(), page(None))
            .await
            .expect("first page"),
        "the same page and the same cursor bytes"
    );
    let cursor = first.next.expect("a second page exists");

    let second = TypesRegistryApi::list_entities(&h.client, &tenant(), page(Some(cursor.clone())))
        .await
        .expect("second page");
    assert_eq!(second.items.len(), 1);
    assert_ne!(second.items[0].gts_id, first.items[0].gts_id);
    assert_eq!(second.next, None);
    assert_eq!(
        second,
        PlatformTypesRegistryApi::list_entities(&h.client, &platform(), page(Some(cursor.clone())))
            .await
            .expect("second page")
    );

    let rebound = ListEntitiesRequest {
        filter: EntityFilter {
            kind: Some(EntityKind::Instance),
            ..filter.clone()
        },
        ..page(Some(cursor))
    };
    let tenant_refusal = TypesRegistryApi::list_entities(&h.client, &tenant(), rebound.clone())
        .await
        .expect_err("a cursor replayed under another filter");
    let platform_refusal = PlatformTypesRegistryApi::list_entities(&h.client, &platform(), rebound)
        .await
        .expect_err("a cursor replayed under another filter");
    assert_eq!(wire(tenant_refusal), wire(platform_refusal));
}
