#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use types_registry::domain::selection::FieldSelection;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::outbox::MessageResult;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use uuid::Uuid;

use types_registry::api::rest::dto::OperationDto;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::{Candidate, SubmitRequest};
use types_registry::domain::enums::{OperationItemStatus, OperationKind, OperationStatus};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{EntityKey, RegistryService};
use types_registry::infra::outbox::AdmissionHandler;
use types_registry::infra::storage::entity::operation_item;

mod common;

const NOW: OffsetDateTime = datetime!(2026-09-16 12:00:00 UTC);
const BASE: &str = gts_id!("cf.core.missingdeps.base.v1~");
const DERIVED: &str = gts_id!("cf.core.missingdeps.base.v1~cf.core.missingdeps.derived.v1~");
const INSTANCE: &str = gts_id!("cf.core.missingdeps.base.v1~cf.core.missingdeps.example.v1");
const REFERENCING: &str = gts_id!("cf.core.missingdeps.referencing.v1~");
const INDEPENDENT: &str = gts_id!("cf.core.missingdeps.independent.v1~");

fn registry(db: &Arc<DBProvider<DbError>>) -> Arc<RegistryService> {
    Arc::new(RegistryService::new(
        db.db(),
        common::stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        common::no_dispatch(),
        common::metrics(),
    ))
}

fn schema(id: &str) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn candidate(id: &str, content: Value) -> Candidate {
    Candidate {
        gts_id: id.to_owned(),
        content: Some(content),
        expected_resource_version: None,
        force: false,
    }
}

async fn first_delivery(registry: &Arc<RegistryService>, candidates: Vec<Candidate>) -> Uuid {
    first_delivery_with_mode(registry, candidates, false).await
}

async fn first_delivery_with_mode(
    registry: &Arc<RegistryService>,
    candidates: Vec<Candidate>,
    dry_run: bool,
) -> Uuid {
    let receipt = registry
        .submit(
            &SubmitRequest {
                idempotency_key: Some("missing-dependency".to_owned()),
                kind: OperationKind::Registration,
                dry_run,
                candidates,
            },
            NOW,
        )
        .await
        .expect("accept the structurally valid submission");
    assert_eq!(receipt.status, OperationStatus::Pending);

    let handler = AdmissionHandler::new(Arc::clone(registry), 8);
    let result = handler
        .admit_payload(receipt.operation_id.to_string().as_bytes(), 0)
        .await;
    assert!(
        matches!(result, MessageResult::Ok),
        "a missing dependency must ack on the first delivery, without retry or dead-letter: {result:?}",
    );
    receipt.operation_id
}

async fn assert_missing_dependency(
    db: &Arc<DBProvider<DbError>>,
    registry: &RegistryService,
    operation_id: Uuid,
    candidate_id: &str,
    dependency_id: &str,
    dependency_kind: &str,
) {
    let operation = registry
        .operation(operation_id)
        .await
        .expect("read operation")
        .expect("operation exists");
    assert_eq!(operation.status, OperationStatus::Completed);
    let item = operation
        .items
        .iter()
        .find(|item| item.gts_id == candidate_id)
        .expect("candidate has a result");
    assert_eq!(item.status, OperationItemStatus::Failed);
    assert_eq!(item.resource_version, None);

    let conn = db.conn().expect("connection");
    let stored = operation_item::Entity::find()
        .filter(operation_item::Column::OperationId.eq(operation_id))
        .filter(operation_item::Column::GtsId.eq(candidate_id))
        .secure()
        .scope_with(&common::allow_all())
        .one(&conn)
        .await
        .expect("read stored item directly")
        .expect("stored item exists");
    assert!(
        stored.request_payload.is_none(),
        "terminal items discard submitted content"
    );
    let error: Value =
        serde_json::from_str(stored.error_payload.as_deref().expect("failure is durable"))
            .expect("stored failure is JSON");
    assert_eq!(error["reason"], "dependency_not_found");
    assert_eq!(error["dependency_id"], dependency_id);
    assert_eq!(error["dependency_kind"], dependency_kind);
    assert!(
        error["message"]
            .as_str()
            .expect("human explanation")
            .contains(dependency_id),
        "the client explanation identifies the missing dependency: {error}",
    );

    let wire = serde_json::to_value(OperationDto::from(operation)).expect("serialize polling DTO");
    let wire_item = wire["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|item| item["gts_id"] == candidate_id)
        .expect("candidate is exposed to the client");
    assert_eq!(
        wire_item["error"], error,
        "polling preserves the stored diagnostic fields"
    );
    assert!(
        registry
            .entity(
                &EntityKey::GtsId(candidate_id.to_owned()),
                FieldSelection::full()
            )
            .await
            .expect("read refused candidate")
            .is_none(),
        "a refused candidate must not become a registered entity",
    );
}

#[tokio::test]
async fn missing_conforming_type_fails_on_the_first_delivery_with_client_diagnostics() {
    let db = common::test_db().await;
    let registry = registry(&db);
    let operation_id =
        first_delivery(&registry, vec![candidate(INSTANCE, json!({ "name": "x" }))]).await;

    assert_missing_dependency(
        &db,
        &registry,
        operation_id,
        INSTANCE,
        BASE,
        "conforming_type",
    )
    .await;
}

#[tokio::test]
async fn missing_base_fails_on_the_first_delivery_with_client_diagnostics() {
    let db = common::test_db().await;
    let registry = registry(&db);
    let mut derived = schema(DERIVED);
    derived["allOf"] = json!([{ "$ref": format!("gts://{BASE}") }]);
    let operation_id = first_delivery(&registry, vec![candidate(DERIVED, derived)]).await;

    assert_missing_dependency(&db, &registry, operation_id, DERIVED, BASE, "base").await;
}

#[tokio::test]
async fn missing_schema_reference_fails_on_the_first_delivery_with_client_diagnostics() {
    let db = common::test_db().await;
    let registry = registry(&db);
    let mut referencing = schema(REFERENCING);
    referencing["properties"]["other"] = json!({ "$ref": format!("gts://{BASE}") });
    let operation_id = first_delivery(&registry, vec![candidate(REFERENCING, referencing)]).await;

    assert_missing_dependency(&db, &registry, operation_id, REFERENCING, BASE, "ref").await;
}

#[tokio::test]
async fn a_missing_dependency_does_not_prevent_an_independent_candidate_from_completing() {
    let db = common::test_db().await;
    let registry = registry(&db);
    let operation_id = first_delivery(
        &registry,
        vec![
            candidate(INSTANCE, json!({ "name": "x" })),
            candidate(INDEPENDENT, schema(INDEPENDENT)),
        ],
    )
    .await;

    assert_missing_dependency(
        &db,
        &registry,
        operation_id,
        INSTANCE,
        BASE,
        "conforming_type",
    )
    .await;
    let operation = registry
        .operation(operation_id)
        .await
        .expect("read")
        .expect("operation");
    let independent = operation
        .items
        .iter()
        .find(|item| item.gts_id == INDEPENDENT)
        .expect("independent result");
    assert_eq!(independent.status, OperationItemStatus::Succeeded);
    assert_eq!(independent.resource_version, Some(1));
    assert!(independent.error.is_none());
    assert!(
        registry
            .entity(
                &EntityKey::GtsId(INDEPENDENT.to_owned()),
                FieldSelection::full()
            )
            .await
            .expect("read admitted entity")
            .is_some()
    );
}

#[tokio::test]
async fn dry_run_missing_dependencies_keep_the_same_diagnostics_without_entity_writes() {
    let mut derived = schema(DERIVED);
    derived["allOf"] = json!([{ "$ref": format!("gts://{BASE}") }]);
    let mut referencing = schema(REFERENCING);
    referencing["properties"]["other"] = json!({ "$ref": format!("gts://{BASE}") });
    for (candidate_id, content, dependency_kind) in [
        (INSTANCE, json!({ "name": "x" }), "conforming_type"),
        (DERIVED, derived, "base"),
        (REFERENCING, referencing, "ref"),
    ] {
        let db = common::test_db().await;
        let registry = registry(&db);
        let operation_id = first_delivery_with_mode(
            &registry,
            vec![
                candidate(candidate_id, content),
                candidate(INDEPENDENT, schema(INDEPENDENT)),
            ],
            true,
        )
        .await;

        assert_missing_dependency(
            &db,
            &registry,
            operation_id,
            candidate_id,
            BASE,
            dependency_kind,
        )
        .await;
        let operation = registry
            .operation(operation_id)
            .await
            .expect("read dry-run operation")
            .expect("operation exists");
        assert!(operation.dry_run);
        let independent = operation
            .items
            .iter()
            .find(|item| item.gts_id == INDEPENDENT)
            .expect("independent candidate has a result");
        assert_eq!(independent.status, OperationItemStatus::Succeeded);
        assert_eq!(independent.resource_version, None);
        assert!(
            registry
                .entity(
                    &EntityKey::GtsId(INDEPENDENT.to_owned()),
                    FieldSelection::full()
                )
                .await
                .expect("read independently validated entity")
                .is_none(),
            "even a successful dry-run candidate must not be persisted",
        );
    }
}

#[tokio::test]
async fn an_instance_before_its_conforming_type_in_the_same_batch_succeeds() {
    let db = common::test_db().await;
    let registry = registry(&db);
    let operation_id = first_delivery(
        &registry,
        vec![
            candidate(INSTANCE, json!({ "name": "x" })),
            candidate(BASE, schema(BASE)),
        ],
    )
    .await;

    let operation = registry
        .operation(operation_id)
        .await
        .expect("read operation")
        .expect("operation exists");
    assert_eq!(operation.status, OperationStatus::Completed);
    assert_eq!(operation.items.len(), 2);
    for id in [INSTANCE, BASE] {
        let item = operation
            .items
            .iter()
            .find(|item| item.gts_id == id)
            .expect("candidate has a result");
        assert_eq!(
            item.status,
            OperationItemStatus::Succeeded,
            "{id}: {item:?}"
        );
        assert_eq!(item.resource_version, Some(1));
        assert!(item.error.is_none());
        assert!(
            registry
                .entity(&EntityKey::GtsId(id.to_owned()), FieldSelection::full())
                .await
                .expect("read registered entity")
                .is_some(),
            "the batch must register {id} regardless of input order",
        );
    }
}
