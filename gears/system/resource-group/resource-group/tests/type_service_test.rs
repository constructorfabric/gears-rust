// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-type-mgmt:p1
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! Phase 2 tests: Type management CRUD, metadata_schema internal storage logic,
//! and GTS path <-> ID resolution.
//!
//! Covers TC-TYP-01..16, TC-META-01..11, TC-GTS-01..15.
//! Overlapping TC-META/TC-GTS cases are implemented once with a comment noting both IDs.

mod common;

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use resource_group::domain::error::DomainError;
use resource_group::domain::repo::TypeRepositoryTrait;
use resource_group::domain::type_service::TypeService;
use resource_group::infra::storage::entity::{
    gts_type::{self, Entity as GtsTypeEntity},
    gts_type_allowed_membership::{self, Entity as AllowedMembershipEntity},
    gts_type_allowed_parent::{self, Entity as AllowedParentEntity},
};
use resource_group::infra::storage::type_repo::TypeRepository;
use resource_group_sdk::{CreateTypeRequest, UpdateTypeRequest};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use toolkit_db::secure::{SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;

/// Generate a unique GTS type code with the given suffix.
fn type_code(suffix: &str) -> String {
    format!(
        "gts.cf.core.rg.type.v1~x.test.{}{}.v1~",
        suffix,
        Uuid::now_v7().as_simple()
    )
}

/// System-level scope for direct DB assertions (no tenant filtering).
fn system_scope() -> AccessScope {
    AccessScope::allow_all()
}

// =========================================================================
// Type CRUD tests (TC-TYP-01..16)
// =========================================================================

/// TC-TYP-01: Create type with valid allowed_parent_types.
/// Child type created; allowed_parent_types contains parent code;
/// junction rows COUNT = len(allowed_parent_types).
#[tokio::test]
async fn type_create_with_valid_parents() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    // Create parent type first
    let parent = common::create_root_type(&type_svc, "parent").await;

    // Create child type with parent in allowed_parent_types
    let child_code = type_code("child");
    let child = type_svc
        .create_type(CreateTypeRequest {
            code: child_code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![parent.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create child type");

    assert_eq!(child.allowed_parent_types, vec![parent.code.clone()]);
    assert!(!child.can_be_root);

    // DB assertion: junction rows count matches
    let conn = db.conn().expect("get conn");
    let scope = system_scope();

    let child_model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&child_code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query child type")
        .expect("child type exists");

    let junction_rows = AllowedParentEntity::find()
        .filter(gts_type_allowed_parent::Column::TypeId.eq(child_model.id))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query junction");

    assert_eq!(junction_rows.len(), 1, "Expected 1 junction row");

    // Verify parent_type_id is resolved from GTS path to SMALLINT
    let parent_model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&parent.code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query parent")
        .expect("parent type exists");
    assert_eq!(junction_rows[0].parent_type_id, parent_model.id);
}

/// TC-TYP-02: Create type with non-existent allowed_parent_types -> Validation error.
#[tokio::test]
async fn type_create_nonexistent_parents() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("orphan");
    let nonexistent_parent = type_code("ghost");
    let err = type_svc
        .create_type(CreateTypeRequest {
            code,
            can_be_root: false,
            allowed_parent_types: vec![nonexistent_parent],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "Expected Validation error: {err:?}"
    );
    assert!(
        err.to_string().contains("not found"),
        "Error should mention 'not found': {err:?}"
    );
}

/// TC-TYP-03: Create type with non-existent allowed_membership_types -> error.
#[tokio::test]
async fn type_create_nonexistent_memberships() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("membfail");
    let nonexistent_membership = type_code("nomemb");
    let err = type_svc
        .create_type(CreateTypeRequest {
            code,
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![nonexistent_membership],
            metadata_schema: None,
        })
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "Expected Validation error for missing membership: {err:?}"
    );
}

/// TC-TYP-04: Placement invariant: can_be_root=false, allowed_parent_types=[] -> Validation error.
#[tokio::test]
async fn type_create_placement_invariant_violation() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("noplacement");
    let err = type_svc
        .create_type(CreateTypeRequest {
            code,
            can_be_root: false,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "Expected Validation error: {err:?}"
    );
    assert!(
        err.to_string().contains("root placement or"),
        "Error should mention placement invariant: {err:?}"
    );
}

/// TC-TYP-05: Update type happy path -- new allowed_parent_types replace old ones.
/// DB assertion: old junction rows deleted, new rows match new list.
#[tokio::test]
async fn type_update_replaces_parents() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let parent_a = common::create_root_type(&type_svc, "pa").await;
    let parent_b = common::create_root_type(&type_svc, "pb").await;

    // Create child with parent_a
    let child_code = type_code("updchild");
    type_svc
        .create_type(CreateTypeRequest {
            code: child_code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![parent_a.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create child");

    // Update child to have parent_b instead of parent_a
    let updated = type_svc
        .update_type(
            &child_code,
            UpdateTypeRequest {
                can_be_root: false,
                allowed_parent_types: vec![parent_b.code.clone()],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect("update child");

    assert_eq!(updated.allowed_parent_types, vec![parent_b.code.clone()]);

    // DB assertion: junction rows now only contain parent_b
    let conn = db.conn().expect("get conn");
    let scope = system_scope();

    let child_model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&child_code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");

    let junction_rows = AllowedParentEntity::find()
        .filter(gts_type_allowed_parent::Column::TypeId.eq(child_model.id))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query junction");

    assert_eq!(
        junction_rows.len(),
        1,
        "Expected exactly 1 junction row after update"
    );

    let parent_b_model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&parent_b.code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");
    assert_eq!(junction_rows[0].parent_type_id, parent_b_model.id);
}

/// TC-TYP-06: Update type -- remove allowed_parent in use by groups.
/// -> AllowedParentTypesViolation with violating group names.
#[tokio::test]
async fn type_update_remove_parent_in_use() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // Create parent and child types
    let parent_type = common::create_root_type(&type_svc, "usedpar").await;
    let child_type =
        common::create_child_type(&type_svc, "usedchild", &[&parent_type.code], &[]).await;

    // Create a parent group and a child group under it
    let parent_group =
        common::create_root_group(&group_svc, &ctx, &parent_type.code, "ParentGrp", tenant_id)
            .await;
    common::create_child_group(
        &group_svc,
        &ctx,
        &child_type.code,
        parent_group.id,
        "ChildGrp",
        tenant_id,
    )
    .await;

    // Try to remove parent_type from child_type's allowed_parent_types
    let err = type_svc
        .update_type(
            &child_type.code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::AllowedParentTypesViolation { .. }),
        "Expected AllowedParentTypesViolation: {err:?}"
    );
    assert!(
        err.to_string().contains("ChildGrp"),
        "Error should mention violating group name: {err:?}"
    );
}

/// TC-TYP-07: Update type -- set can_be_root=false with root groups existing.
/// -> AllowedParentTypesViolation with root group names.
#[tokio::test]
async fn type_update_disable_root_with_root_groups() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // Create another type for parents
    let alt_parent_type = common::create_root_type(&type_svc, "altpar").await;

    // Create root type and a root group
    let root_type = common::create_root_type(&type_svc, "roottp").await;
    common::create_root_group(&group_svc, &ctx, &root_type.code, "RootGrp", tenant_id).await;

    // Try to set can_be_root=false (must provide a parent to satisfy placement invariant)
    let err = type_svc
        .update_type(
            &root_type.code,
            UpdateTypeRequest {
                can_be_root: false,
                allowed_parent_types: vec![alt_parent_type.code.clone()],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::AllowedParentTypesViolation { .. }),
        "Expected AllowedParentTypesViolation: {err:?}"
    );
    assert!(
        err.to_string().contains("RootGrp"),
        "Error should mention root group name: {err:?}"
    );
}

/// TC-TYP-08: Update type -- not found.
#[tokio::test]
async fn type_update_not_found() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("notfound");
    let err = type_svc
        .update_type(
            &code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::TypeNotFound { .. }),
        "Expected TypeNotFound: {err:?}"
    );
}

/// TC-TYP-09: Delete type with existing groups -> ConflictActiveReferences.
#[tokio::test]
async fn type_delete_with_active_groups() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));
    let group_svc = common::make_group_service(db.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let rt = common::create_root_type(&type_svc, "delwg").await;
    common::create_root_group(&group_svc, &ctx, &rt.code, "BusyGrp", tenant_id).await;

    let err = type_svc
        .delete_type(&rt.code)
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::ConflictActiveReferences { .. }),
        "Expected ConflictActiveReferences: {err:?}"
    );
}

/// TC-TYP-10: Update type -- placement invariant on new values.
#[tokio::test]
async fn type_update_placement_invariant_violation() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let rt = common::create_root_type(&type_svc, "updinv").await;

    let err = type_svc
        .update_type(
            &rt.code,
            UpdateTypeRequest {
                can_be_root: false,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "Expected Validation error: {err:?}"
    );
    assert!(
        err.to_string().contains("root placement or"),
        "Error should mention placement invariant: {err:?}"
    );
}

/// TC-TYP-11: Create type with self-reference in allowed_parent_types -> error (not found
/// because the type doesn't exist yet at resolve time).
#[tokio::test]
async fn type_create_self_reference_parent() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("selfref");
    let err = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![code],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect_err("should fail");

    // Self-reference fails because the type doesn't exist yet when resolving
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "Expected error for self-reference: {err:?}"
    );
}

/// TC-TYP-12: Create type with invalid format in allowed_parent_types -> Validation.
#[tokio::test]
async fn type_create_invalid_parent_format() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("badfmt");
    let err = type_svc
        .create_type(CreateTypeRequest {
            code,
            can_be_root: false,
            allowed_parent_types: vec!["invalid-no-prefix".to_owned()],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "Expected Validation error: {err:?}"
    );
    assert!(
        err.to_string().contains("prefix"),
        "Error should mention prefix: {err:?}"
    );
}

/// TC-TYP-13: Delete nonexistent type -> TypeNotFound.
#[tokio::test]
async fn type_delete_not_found() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("delnf");
    let err = type_svc.delete_type(&code).await.expect_err("should fail");

    assert!(
        matches!(err, DomainError::TypeNotFound { .. }),
        "Expected TypeNotFound: {err:?}"
    );
}

/// TC-TYP-14: Create type with metadata_schema -> returned type has matching metadata_schema.
#[tokio::test]
async fn type_create_with_metadata_schema() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("withmeta");
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        }
    });

    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema.clone()),
        })
        .await
        .expect("create type with schema");

    assert_eq!(rg_type.metadata_schema, Some(schema));
}

/// TC-TYP-15: Update type replaces allowed_membership_types [A, B] -> [B, C].
/// DB assertion: gts_type_allowed_membership contains only new entries.
#[tokio::test]
async fn type_update_replaces_memberships() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let ma = common::create_root_type(&type_svc, "memba").await;
    let mb = common::create_root_type(&type_svc, "membb").await;
    let mc = common::create_root_type(&type_svc, "membc").await;

    let code = type_code("membupd");
    type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![ma.code.clone(), mb.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create type");

    // Update memberships to [B, C]
    let updated = type_svc
        .update_type(
            &code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![mb.code.clone(), mc.code.clone()],
                metadata_schema: None,
            },
        )
        .await
        .expect("update type");

    let mut actual_memberships = updated.allowed_membership_types.clone();
    actual_memberships.sort();
    let mut expected = vec![mb.code.clone(), mc.code.clone()];
    expected.sort();
    assert_eq!(actual_memberships, expected);

    // DB assertion: junction table contains only B and C
    let conn = db.conn().expect("get conn");
    let scope = system_scope();

    let type_model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");

    let membership_rows = AllowedMembershipEntity::find()
        .filter(gts_type_allowed_membership::Column::TypeId.eq(type_model.id))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("query");

    assert_eq!(
        membership_rows.len(),
        2,
        "Expected exactly 2 membership junction rows"
    );
}

/// TC-TYP-16: Update type -- hierarchy check skips deleted parent type -> no error.
/// If the previously allowed parent type was deleted, removing it from allowed_parent_types
/// should succeed because there can be no groups using a deleted type.
#[tokio::test]
async fn type_update_hierarchy_check_skips_deleted_parent() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let parent = common::create_root_type(&type_svc, "delpar").await;
    let child_code = type_code("skipchild");
    type_svc
        .create_type(CreateTypeRequest {
            code: child_code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![parent.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create child");

    // Delete the parent type (no groups use it)
    type_svc
        .delete_type(&parent.code)
        .await
        .expect("delete parent");

    // Now update child to remove the (deleted) parent -- should succeed
    // because resolve_id returns None for deleted parent, so no violation check occurs.
    // We must provide can_be_root=true since we are removing the only parent.
    let updated = type_svc
        .update_type(
            &child_code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: None,
            },
        )
        .await
        .expect("update should succeed");

    assert!(updated.allowed_parent_types.is_empty());
    assert!(updated.can_be_root);
}

// =========================================================================
// Metadata schema internal logic tests (TC-META-01..11 / TC-GTS-10..15)
// =========================================================================

/// TC-META-01 / TC-GTS-12: Type with metadata_schema Object round-trip.
/// Returned schema matches input. DB stored JSONB has `__can_be_root`.
/// API response has NO `__can_be_root`.
#[tokio::test]
async fn meta_object_schema_roundtrip() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("metaobj");
    let schema = json!({
        "type": "object",
        "properties": {
            "label": { "type": "string" }
        }
    });

    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema.clone()),
        })
        .await
        .expect("create type");

    // Service layer returns clean schema (no __ keys)
    assert_eq!(rg_type.metadata_schema, Some(schema));

    // DB stored JSONB has __can_be_root
    let conn = db.conn().expect("get conn");
    let scope = system_scope();

    let model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");

    let stored = model.metadata_schema.expect("stored schema present");
    assert_eq!(
        stored.get("__can_be_root"),
        Some(&json!(true)),
        "DB should contain __can_be_root"
    );
    // User keys preserved in DB
    assert!(stored.get("type").is_some(), "User 'type' key in DB");
    assert!(
        stored.get("properties").is_some(),
        "User 'properties' key in DB"
    );

    // API response (loaded via service) has no __can_be_root
    let loaded = type_svc.get_type(&code).await.expect("get type");
    if let Some(ref ms) = loaded.metadata_schema {
        assert!(
            ms.get("__can_be_root").is_none(),
            "API response should not contain __can_be_root"
        );
    }
}

/// TC-META-02: Type `metadata_schema` with a non-Object (array) shape.
/// Behavior asserted: the schema fails JSON-Schema validation up-front, so
/// the create call returns a `Validation`-class error mentioning
/// "not a valid JSON Schema". (No round-trip ever happens — the schema is
/// rejected at the API boundary; the legacy "wrap/unwrap" comment was stale.)
#[tokio::test]
async fn meta_non_object_array_roundtrip() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("metaarr");
    let schema = json!(["string", "number"]);

    let result = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema.clone()),
        })
        .await;

    // Array is not valid JSON Schema (must be object or boolean) -> rejected
    assert!(result.is_err(), "Array schema should be rejected");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("not a valid JSON Schema"),
        "Error should mention invalid JSON Schema"
    );
}

/// TC-META-03: Type `metadata_schema` with a non-Object (string) shape.
/// Behavior asserted: same as TC-META-02 — early validation rejection with
/// "not a valid JSON Schema". The previous "wrap and lose data" comment was
/// stale; nothing is wrapped, the schema is rejected before storage.
#[tokio::test]
async fn meta_non_object_string_roundtrip() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("metastr");
    let schema = json!("just a string");

    let result = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema),
        })
        .await;

    // String is not valid JSON Schema (must be object or boolean) -> rejected
    assert!(result.is_err(), "String schema should be rejected");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("not a valid JSON Schema"),
        "Error should mention invalid JSON Schema"
    );
}

/// TC-META-04: Type `metadata_schema` with a non-Object (number) shape.
/// Behavior asserted: same as TC-META-02/03 — early validation rejection
/// with "not a valid JSON Schema". Together with TC-META-11
/// (`meta_any_object_schema_accepted`) the suite documents the actual
/// contract: `metadata_schema` must be a JSON Schema, which means an
/// **object** (or boolean) at the root; non-object roots are rejected,
/// while *any* object — even with arbitrary user-defined keys — is
/// accepted without keyword-level validation.
#[tokio::test]
async fn meta_non_object_number_roundtrip() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("metanum");
    let schema = json!(42);

    let result = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema),
        })
        .await;

    // Number is not valid JSON Schema (must be object or boolean) -> rejected
    assert!(result.is_err(), "Number schema should be rejected");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("not a valid JSON Schema"),
        "Error should mention invalid JSON Schema"
    );
}

/// TC-META-05: User sends __can_be_root in metadata_schema.
/// System's can_be_root wins; user's __can_be_root is overwritten.
#[tokio::test]
async fn meta_user_can_be_root_overwritten() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("metacbr");
    // User sends __can_be_root=false but the request says can_be_root=true
    let schema = json!({
        "__can_be_root": false,
        "user_field": "hello"
    });

    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema),
        })
        .await
        .expect("create type");

    // Service layer derives can_be_root from stored __can_be_root which should be true
    assert!(rg_type.can_be_root, "System's can_be_root=true should win");

    // Returned metadata_schema should not contain __can_be_root
    let ms = rg_type.metadata_schema.expect("has schema");
    assert!(
        ms.get("__can_be_root").is_none(),
        "API should not expose __can_be_root"
    );
    assert_eq!(ms.get("user_field"), Some(&json!("hello")));

    // Verify DB stored the system value (true), not user value (false)
    let conn = db.conn().expect("get conn");
    let scope = system_scope();

    let model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");
    let stored = model.metadata_schema.expect("stored");
    assert_eq!(
        stored.get("__can_be_root"),
        Some(&json!(true)),
        "DB should store system can_be_root=true, overwriting user's false"
    );
}

/// TC-META-06 / TC-GTS-13: User sends __other_internal key.
/// Stripped on read; returned metadata has only non-__ keys.
#[tokio::test]
async fn meta_internal_keys_stripped() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("metaint");
    let schema = json!({
        "__custom_internal": "should be stripped",
        "visible_field": 42
    });

    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema),
        })
        .await
        .expect("create type");

    let ms = rg_type.metadata_schema.expect("has schema");
    assert!(
        ms.get("__custom_internal").is_none(),
        "__ prefixed keys should be stripped"
    );
    assert_eq!(ms.get("visible_field"), Some(&json!(42)));
}

/// TC-META-07 / TC-GTS-14: Single underscore key _myField preserved.
#[tokio::test]
async fn meta_single_underscore_key_preserved() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("metasingle");
    let schema = json!({
        "_myField": "should survive"
    });

    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema),
        })
        .await
        .expect("create type");

    let ms = rg_type.metadata_schema.expect("has schema");
    assert_eq!(
        ms.get("_myField"),
        Some(&json!("should survive")),
        "Single underscore keys should be preserved"
    );
}

/// TC-META-08 / TC-GTS-15: metadata_schema=None -> stored with __can_be_root.
/// DB has {"__can_be_root": true/false}; API returns None.
#[tokio::test]
async fn meta_none_stored_with_can_be_root() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("metanone");
    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create type");

    // API returns None (only internal keys, stripped to empty -> None)
    assert_eq!(rg_type.metadata_schema, None);

    // DB stores {"__can_be_root": true}
    let conn = db.conn().expect("get conn");
    let scope = system_scope();

    let model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");
    let stored = model.metadata_schema.expect("stored schema present");
    assert_eq!(
        stored,
        json!({"__can_be_root": true}),
        "DB should store __can_be_root even with None schema"
    );
}

/// TC-META-09 / TC-GTS-10: can_be_root derived from stored __can_be_root.
/// true -> true, false -> false.
#[tokio::test]
async fn meta_can_be_root_derived_from_stored_key() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    // Test can_be_root=true
    let root_type = common::create_root_type(&type_svc, "cbrtrue").await;
    assert!(
        root_type.can_be_root,
        "Root type should have can_be_root=true"
    );

    // Test can_be_root=false (with allowed_parent_types)
    let child_type =
        common::create_child_type(&type_svc, "cbrfalse", &[&root_type.code], &[]).await;
    assert!(
        !child_type.can_be_root,
        "Child type should have can_be_root=false"
    );

    // Verify via DB: __can_be_root values
    let conn = db.conn().expect("get conn");
    let scope = system_scope();

    let root_model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&root_type.code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");
    let root_stored = root_model.metadata_schema.expect("stored");
    assert_eq!(root_stored.get("__can_be_root"), Some(&json!(true)));

    let child_model = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&child_type.code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");
    let child_stored = child_model.metadata_schema.expect("stored");
    assert_eq!(child_stored.get("__can_be_root"), Some(&json!(false)));
}

/// TC-META-10 / TC-GTS-11: can_be_root fallback when __can_be_root missing.
/// Uses allowed_parent_types.is_empty() as fallback.
/// Requires direct DB insert to create a type row without __can_be_root in JSONB.
#[tokio::test]
async fn meta_can_be_root_fallback_no_stored_key() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("nocbrkey");

    // Insert directly into DB without __can_be_root using secure insert
    let conn = db.conn().expect("get conn");
    let scope = system_scope();

    let model = gts_type::ActiveModel {
        schema_id: Set(code.clone()),
        metadata_schema: Set(Some(json!({"user_field": "test"}))),
        ..Default::default()
    };
    secure_insert::<GtsTypeEntity>(model, &scope, &conn)
        .await
        .expect("direct insert");

    // Load via service -- no allowed_parent_types so fallback = true
    let loaded = type_svc.get_type(&code).await.expect("get type");
    assert!(
        loaded.can_be_root,
        "Fallback: no __can_be_root + no parents -> can_be_root=true"
    );

    // Now create a type with parents and no __can_be_root
    let parent_code = type_code("fallbackpar");
    let parent_model = gts_type::ActiveModel {
        schema_id: Set(parent_code.clone()),
        metadata_schema: Set(Some(json!({"__can_be_root": true}))),
        ..Default::default()
    };
    secure_insert::<GtsTypeEntity>(parent_model, &scope, &conn)
        .await
        .expect("insert parent");

    let child_code = type_code("fallbackchild");
    let child_model = gts_type::ActiveModel {
        schema_id: Set(child_code.clone()),
        metadata_schema: Set(Some(json!({"only_user": true}))),
        ..Default::default()
    };
    secure_insert::<GtsTypeEntity>(child_model, &scope, &conn)
        .await
        .expect("insert child");

    // Manually insert allowed_parent junction row
    let parent_row = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&parent_code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");

    let child_row = GtsTypeEntity::find()
        .filter(gts_type::Column::SchemaId.eq(&child_code))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query")
        .expect("exists");

    let junction = gts_type_allowed_parent::ActiveModel {
        type_id: Set(child_row.id),
        parent_type_id: Set(parent_row.id),
    };
    secure_insert::<AllowedParentEntity>(junction, &scope, &conn)
        .await
        .expect("insert junction");

    // Load via service -- has allowed_parent_types so fallback = false
    let loaded_child = type_svc.get_type(&child_code).await.expect("get type");
    assert!(
        !loaded_child.can_be_root,
        "Fallback: no __can_be_root + has parents -> can_be_root=false"
    );
}

/// TC-META-11: `metadata_schema` keyword-validation — any **object**
/// (regardless of which JSON-Schema keywords it does or does not use) is
/// accepted at the API boundary. The root-shape constraint is still
/// enforced (see TC-META-02/03/04 for non-object rejection); this test
/// covers the complementary "no keyword whitelist" guarantee.
#[tokio::test]
async fn meta_any_object_schema_accepted() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    // The root is an object — required by the schema shape rule. Inside,
    // arbitrary keys are tolerated (no keyword-level validation).
    let code = type_code("anyjson");
    let schema = json!({
        "banana": true,
        "count": 999,
        "nested": { "deep": [1, 2, 3] }
    });

    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code,
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema.clone()),
        })
        .await
        .expect("any object schema should be accepted");

    assert_eq!(rg_type.metadata_schema, Some(schema));
}

// =========================================================================
// GTS Resolution tests (TC-GTS-01..09)
// =========================================================================

/// TC-GTS-01: resolve_id for existing type -> Some(id) where id is i16.
#[tokio::test]
async fn gts_resolve_id_existing() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let rt = common::create_root_type(&type_svc, "resid").await;

    let conn = db.conn().expect("get conn");
    let id = TypeRepository
        .resolve_id(&conn, &rt.code)
        .await
        .expect("resolve_id");
    assert!(
        id.is_some(),
        "resolve_id should return Some for existing type"
    );
    let id_val = id.unwrap();
    // Verify the id is a positive SMALLINT
    assert!(id_val > 0, "Type id should be positive");
}

/// TC-GTS-02: resolve_id for nonexistent path -> None.
#[tokio::test]
async fn gts_resolve_id_nonexistent() {
    let db = common::test_db().await;

    let conn = db.conn().expect("get conn");
    let code = type_code("noexist");
    let id = TypeRepository
        .resolve_id(&conn, &code)
        .await
        .expect("resolve_id");
    assert!(
        id.is_none(),
        "resolve_id should return None for nonexistent type"
    );
}

/// TC-GTS-03: resolve_ids batch -- all found -> Ok(vec![id1, id2, id3]).
#[tokio::test]
async fn gts_resolve_ids_all_found() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let t1 = common::create_root_type(&type_svc, "batch1").await;
    let t2 = common::create_root_type(&type_svc, "batch2").await;
    let t3 = common::create_root_type(&type_svc, "batch3").await;

    let conn = db.conn().expect("get conn");
    let codes = vec![t1.code.clone(), t2.code.clone(), t3.code.clone()];
    let ids = TypeRepository
        .resolve_ids(&conn, &codes)
        .await
        .expect("resolve_ids");

    assert_eq!(ids.len(), 3, "Should resolve all 3 types");
    // All IDs should be distinct
    let mut unique = ids;
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 3, "All IDs should be distinct");
}

/// TC-GTS-04: resolve_ids batch -- some missing -> Err(Validation("Referenced types not found: ...")).
#[tokio::test]
async fn gts_resolve_ids_some_missing() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let t1 = common::create_root_type(&type_svc, "partfound").await;
    let missing_code = type_code("missing");

    let conn = db.conn().expect("get conn");
    let codes = vec![t1.code.clone(), missing_code.clone()];
    let err = TypeRepository
        .resolve_ids(&conn, &codes)
        .await
        .expect_err("should fail");

    assert!(
        matches!(err, DomainError::Validation { .. }),
        "Expected Validation error: {err:?}"
    );
    assert!(
        err.to_string().contains("not found"),
        "Error should mention 'not found': {err:?}"
    );
}

/// TC-GTS-05: resolve_ids -- multiple missing -> error lists both.
#[tokio::test]
async fn gts_resolve_ids_multiple_missing() {
    let db = common::test_db().await;

    let missing1 = type_code("miss1");
    let missing2 = type_code("miss2");

    let conn = db.conn().expect("get conn");
    let codes = vec![missing1.clone(), missing2.clone()];
    let err = TypeRepository
        .resolve_ids(&conn, &codes)
        .await
        .expect_err("should fail");

    let msg = err.to_string();
    assert!(
        msg.contains("not found"),
        "Error should mention 'not found': {msg}"
    );
    // Both missing codes should be mentioned
    assert!(
        msg.contains("miss1") && msg.contains("miss2"),
        "Error should list both missing codes: {msg}"
    );
}

/// TC-GTS-06: resolve_ids empty list -> Ok(vec![]).
#[tokio::test]
async fn gts_resolve_ids_empty_list() {
    let db = common::test_db().await;

    let conn = db.conn().expect("get conn");
    let codes: Vec<String> = vec![];
    let ids = TypeRepository
        .resolve_ids(&conn, &codes)
        .await
        .expect("resolve_ids");
    assert!(ids.is_empty(), "Empty input should return empty result");
}

/// TC-GTS-07: Full roundtrip: create -> resolve_id -> load_full_type_by_id -> path matches.
#[tokio::test]
async fn gts_full_roundtrip_create_resolve_load() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let rt = common::create_root_type(&type_svc, "roundtrip").await;

    let conn = db.conn().expect("get conn");

    // resolve_id
    let id = TypeRepository
        .resolve_id(&conn, &rt.code)
        .await
        .expect("resolve_id")
        .expect("type exists");

    // load_full_type_by_id
    let loaded = TypeRepository
        .load_full_type_by_id(&conn, id)
        .await
        .expect("load_full_type_by_id");

    assert_eq!(loaded.code, rt.code, "Path should match after roundtrip");
    assert_eq!(loaded.can_be_root, rt.can_be_root);
}

/// TC-GTS-08: load_allowed_parent_types junction -> IDs -> paths. Returns parent code.
#[tokio::test]
async fn gts_load_allowed_parent_types_returns_paths() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let parent = common::create_root_type(&type_svc, "gtspar").await;
    let child = common::create_child_type(&type_svc, "gtschild", &[&parent.code], &[]).await;

    assert_eq!(
        child.allowed_parent_types,
        vec![parent.code.clone()],
        "allowed_parent_types should contain the parent's GTS path"
    );

    // Also verify via direct get_type (which goes through load_full_type)
    let loaded = type_svc.get_type(&child.code).await.expect("get type");
    assert_eq!(loaded.allowed_parent_types, vec![parent.code]);
}

/// TC-GTS-09: load_allowed_membership_types junction -> IDs -> paths. Returns membership code.
#[tokio::test]
async fn gts_load_allowed_membership_types_returns_paths() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let membership_type = common::create_root_type(&type_svc, "gtsmemb").await;
    let code = type_code("gtswithmemb");
    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![membership_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create type");

    assert_eq!(
        rg_type.allowed_membership_types,
        vec![membership_type.code.clone()],
        "allowed_membership_types should contain the membership type's GTS path"
    );

    // Verify via get_type
    let loaded = type_svc.get_type(&code).await.expect("get type");
    assert_eq!(loaded.allowed_membership_types, vec![membership_type.code]);
}

// =========================================================================
// Security/Attack Tests for Type metadata_schema (TC-META-ATK-01..07, 10, 11)
// =========================================================================

/// TC-META-ATK-01: Overwrite __can_be_root via metadata_schema -- system field wins.
#[tokio::test]
async fn security_metadata_schema_cannot_overwrite_can_be_root() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    // Create a parent type first so we can set can_be_root=false
    let parent = common::create_root_type(&type_svc, "atk01par").await;

    let code = type_code("atk01");
    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: false,
            allowed_parent_types: vec![parent.code.clone()],
            allowed_membership_types: vec![],
            metadata_schema: Some(json!({
                "__can_be_root": true,
                "custom_field": "value"
            })),
        })
        .await
        .expect("create type with __can_be_root in metadata");

    // System field should remain false regardless of metadata
    assert!(
        !rg_type.can_be_root,
        "System can_be_root must not be overridden by metadata_schema"
    );
}

/// TC-META-ATK-02: __can_be_root non-boolean value -- no panic, fallback works.
#[tokio::test]
async fn security_metadata_schema_can_be_root_non_boolean() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("atk02");
    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(json!({
                "__can_be_root": "not-a-bool",
                "__allowed_parent_types": [1, 2, 3]
            })),
        })
        .await
        .expect("create type with non-bool __ keys");

    assert!(rg_type.can_be_root, "System can_be_root must remain true");
}

/// TC-META-ATK-03: Multiple __ keys -- no accumulation, only non-__ keys returned.
#[tokio::test]
async fn security_metadata_schema_double_underscore_keys_filtered() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("atk03");
    let schema = json!({
        "__internal": "hidden",
        "__secret": 42,
        "visible": true
    });

    type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema),
        })
        .await
        .expect("create type with __ keys");

    // Retrieval must strip all `__*` keys — both the caller-supplied ones
    // (`__internal`, `__secret`) and the gear-internal ones added during
    // storage (`__can_be_root`). Only non-underscore keys should remain.
    // A slip in the filter (e.g. `starts_with('_')` vs `starts_with("__")`,
    // or dropping the filter entirely) would let caller data leak back out,
    // so the assertions below check each case.
    let loaded = type_svc.get_type(&code).await.expect("get type");
    assert!(loaded.can_be_root, "System fields unaffected by __ keys");

    let schema = loaded
        .metadata_schema
        .expect("non-underscore keys must survive; schema must not be None");
    let obj = schema
        .as_object()
        .expect("schema must be a JSON object after filtering");

    assert!(
        obj.get("__internal").is_none(),
        "caller-supplied __internal must not leak back, got: {obj:?}",
    );
    assert!(
        obj.get("__secret").is_none(),
        "caller-supplied __secret must not leak back, got: {obj:?}",
    );
    assert!(
        obj.get("__can_be_root").is_none(),
        "gear-internal __can_be_root must not leak back, got: {obj:?}",
    );
    assert_eq!(
        obj.get("visible"),
        Some(&json!(true)),
        "non-underscore key must survive the filter",
    );
    assert_eq!(
        obj.len(),
        1,
        "only the single non-__ key should remain, got: {obj:?}",
    );
}

/// TC-META-ATK-04: Huge metadata_schema (1MB). Document behavior -- no panic.
#[tokio::test]
async fn security_metadata_schema_huge_payload() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("atk04");
    let big_value = "A".repeat(1_000_000);
    let schema = json!({"huge": big_value});

    let result = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema),
        })
        .await;

    match result {
        Ok(rg_type) => {
            // If accepted, verify roundtrip
            let loaded = type_svc.get_type(&code).await.expect("get type");
            let schema = loaded.metadata_schema.unwrap();
            assert_eq!(
                schema["huge"].as_str().unwrap().len(),
                1_000_000,
                "1MB schema should roundtrip"
            );
            assert!(rg_type.can_be_root);
        }
        // Deterministic deny classes are acceptable: validation rejects
        // oversize payloads up-front, and the storage layer may reject
        // through the DB (e.g. SQLite TEXT/JSON limits). Any other error
        // class indicates a regression.
        Err(DomainError::Validation { .. } | DomainError::Database(_)) => {}
        Err(e) => panic!("unexpected error class for large metadata schema: {e:?}"),
    }
}

/// TC-META-ATK-05: Deeply nested metadata_schema (100+ levels) -- no panic.
#[tokio::test]
async fn security_metadata_schema_deep_nesting() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("atk05");

    // Build 100-level nested JSON
    let mut nested: serde_json::Value = json!("leaf");
    for _ in 0..100 {
        nested = json!({"child": nested});
    }

    let result = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(nested),
        })
        .await;

    // Should not panic regardless of outcome; tightened to accept only the
    // deterministic deny classes. A deep-nesting payload may legitimately
    // be rejected by validation (depth/size limits) or by the DB layer.
    match result {
        Ok(_) => {
            let loaded = type_svc.get_type(&code).await.expect("get type");
            assert!(loaded.metadata_schema.is_some());
        }
        Err(DomainError::Validation { .. } | DomainError::Database(_)) => {}
        Err(e) => panic!("unexpected error class for deeply nested metadata: {e:?}"),
    }
}

/// TC-META-ATK-06: Special JSON values (null, true) in metadata_schema -- handled gracefully.
#[tokio::test]
async fn security_metadata_schema_special_values() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    // Test with null value
    let code1 = type_code("atk06a");
    let t1 = type_svc
        .create_type(CreateTypeRequest {
            code: code1.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(json!(null)),
        })
        .await;
    // null schema is accepted or rejected with a deterministic deny class.
    if let Err(e) = t1 {
        assert!(
            matches!(e, DomainError::Validation { .. } | DomainError::Database(_)),
            "unexpected error class for null metadata_schema: {e:?}"
        );
    }

    // Test with bare true
    let code2 = type_code("atk06b");
    let t2 = type_svc
        .create_type(CreateTypeRequest {
            code: code2.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(json!(true)),
        })
        .await;
    if let Err(e) = t2 {
        assert!(
            matches!(e, DomainError::Validation { .. } | DomainError::Database(_)),
            "unexpected error class for `true` metadata_schema: {e:?}"
        );
    }
}

/// TC-META-ATK-07: SQL column name keys in metadata -- no collision.
#[tokio::test]
async fn security_metadata_schema_sql_column_names() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("atk07");
    let schema = json!({
        "code": "fake-code",
        "can_be_root": "fake-bool",
        "gts_type_id": 999,
        "id": "fake-id",
        "SELECT": "* FROM gts_type"
    });

    let rg_type = type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(schema),
        })
        .await
        .expect("create type with SQL column name keys");

    // System fields must be unaffected
    assert!(rg_type.can_be_root);
    assert_eq!(rg_type.code, code);

    let loaded = type_svc.get_type(&code).await.expect("get type");
    assert_eq!(
        loaded.code, code,
        "code field must not be overwritten by metadata"
    );
}

/// TC-META-ATK-10: Update metadata_schema -- old keys do not leak. Full replacement.
#[tokio::test]
async fn security_metadata_schema_update_full_replacement() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("atk10");
    type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(json!({"old_key": "old_value", "shared": 1})),
        })
        .await
        .expect("create type");

    // Update with completely different schema
    let updated = type_svc
        .update_type(
            &code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: Some(json!({"new_key": "new_value"})),
            },
        )
        .await
        .expect("update type");

    let schema = updated.metadata_schema.unwrap();
    assert!(
        schema.get("old_key").is_none(),
        "Old key should not leak after full replacement: {schema}"
    );
    assert!(
        schema.get("shared").is_none(),
        "Shared key should not persist after full replacement: {schema}"
    );
    assert_eq!(schema["new_key"], "new_value");
}

/// TC-META-ATK-11: Concurrent updates do not merge -- last write wins.
#[tokio::test]
async fn security_metadata_schema_last_write_wins() {
    let db = common::test_db().await;
    let type_svc = TypeService::new(db.clone(), Arc::new(TypeRepository));

    let code = type_code("atk11");
    type_svc
        .create_type(CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: Some(json!({"version": 1})),
        })
        .await
        .expect("create type");

    // Simulate sequential updates (no real concurrency needed to verify no-merge)
    type_svc
        .update_type(
            &code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: Some(json!({"version": 2, "extra": "a"})),
            },
        )
        .await
        .expect("update v2");

    type_svc
        .update_type(
            &code,
            UpdateTypeRequest {
                can_be_root: true,
                allowed_parent_types: vec![],
                allowed_membership_types: vec![],
                metadata_schema: Some(json!({"version": 3})),
            },
        )
        .await
        .expect("update v3");

    let loaded = type_svc.get_type(&code).await.expect("get type");
    let schema = loaded.metadata_schema.unwrap();
    assert_eq!(schema["version"], 3);
    assert!(
        schema.get("extra").is_none(),
        "Previous update keys must not merge: {schema}"
    );
}
