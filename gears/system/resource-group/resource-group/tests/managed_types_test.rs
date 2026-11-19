#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Resource Group consumption of managed schemas from the registry SDK.
// @cpt-dod:cpt-cf-resource-group-dod-testing-managed-types:p1

use std::sync::Arc;

use serde_json::{Value, json};
use toolkit_gts::{gts_id, gts_uri};
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use types_registry_sdk::{
    CompilePayloadValidatorError, EntityField, FieldSelection, GtsCompilePayloadValidatorError,
    GtsTypeId, PlatformTypesRegistryApiExt, Projection, ReadOptions, TypeSchema,
};

mod common;

const MEMBER_V1: &str = gts_id!("cf.core.rg.group_membership.v1~");
const TEAM_SCHEMA: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.team.v1~");
const TEAM_MEMBER: &str = gts_id!("cf.core.rg.group_membership.v1~cf.test.rg.team_member.v1~");
const BASE: &str = gts_id!("cf.core.rg.type.v1~");
const USER: &str = gts_id!("cf.test.rg.user.v1~");
const TENANT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~");
const DEPT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~");

fn registry() -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    for (id, schema) in resource_group_sdk::gts_declarations() {
        registry.seed(&id, serde_json::from_str(&schema).expect("declared JSON"));
    }
    registry
}

fn user_type() -> Value {
    json!({
        "$id": gts_uri!("cf.test.rg.user.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "id": { "type": "string" } }
    })
}

/// A tenant RG type: self-nesting, users as members, and metadata described
/// through a local `$defs` reference.
fn tenant_type() -> Value {
    json!({
        "$id": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-traits": {"membership_type": TEAM_MEMBER},
        "x-gts-traits-schema": {"properties": {"membership_type": {"const": TEAM_MEMBER}}},
        "$defs": {
            "tenant_meta": {
                "type": "object",
                "required": ["tier"],
                "properties": { "tier": { "type": "string" } }
            }
        },
        "allOf": [
            { "$ref": gts_uri!("cf.core.rg.type.v1~") },
            { "properties": {
                "parent_type": {"type": ["string", "null"], "enum": [null, TENANT]},
                "metadata": { "$ref": "#/$defs/tenant_meta" }
            } }
        ]
    })
}

/// Derived from the tenant type: inherits its metadata constraints and adds one.
fn dept_type() -> Value {
    json!({
        "$id": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [
            { "$ref": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~") },
            { "properties": { "metadata": {
                "type": "object",
                "properties": { "code": { "type": "string", "maxLength": 3 } }
            } } }
        ]
    })
}

async fn read(registry: &MockTypesRegistry, type_id: &str) -> TypeSchema {
    registry
        .get_type_schema(
            &PlatformSecurityContext::outbound_marker(),
            &GtsTypeId::new(type_id),
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::ResolvedSchema,
            ]))),
        )
        .await
        .expect("seeded type")
}

fn registry_with_derived_types() -> MockTypesRegistry {
    let registry = registry();
    registry.seed(USER, user_type());
    registry.seed(TENANT, tenant_type());
    registry.seed(DEPT, dept_type());
    registry
}

#[tokio::test]
async fn the_abstract_group_base_constrains_parent_types_of_concrete_groups() {
    let registry = registry();
    let concrete = json!({
        "$id": format!("gts://{TEAM_SCHEMA}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-traits": {"membership_type": TEAM_MEMBER},
        "x-gts-traits-schema": {"properties": {"membership_type": {"const": TEAM_MEMBER}}},
        "allOf": [{"$ref": format!("gts://{BASE}")}]
    });
    registry.seed(TEAM_SCHEMA, concrete);
    assert!(matches!(
        read(&registry, BASE).await.compile_payload_validator(),
        Err(CompilePayloadValidatorError::Upstream(
            GtsCompilePayloadValidatorError::Abstract(_)
        ))
    ));

    let validator = read(&registry, TEAM_SCHEMA)
        .await
        .compile_payload_validator()
        .expect("concrete group inherits the base parent type constraint");

    for parent_type in [Value::Null, json!(TENANT), json!(DEPT)] {
        validator
            .validate(&json!({"parent_type": parent_type}))
            .expect("root or a type in the resource-group family");
    }
    validator
        .validate(&json!({}))
        .expect_err("parent_type is required");
    validator
        .validate(&json!({"parent_type": USER}))
        .expect_err("a type outside the resource-group family cannot be a parent");
}

#[tokio::test]
async fn the_group_payload_requires_parent_type_and_excludes_the_binding() {
    let registry = registry_with_derived_types();
    let validator = read(&registry, TENANT)
        .await
        .compile_payload_validator()
        .expect("tenant schema");
    let mut root = json!({
        "parent_type": null, "metadata": {"tier": "gold"}
    });
    validator
        .validate(&root)
        .expect("null is the required root placement value");
    root.as_object_mut()
        .expect("group document")
        .remove("parent_type");
    validator
        .validate(&root)
        .expect_err("parent_type is required");
    root["parent_type"] = json!(TENANT);
    validator
        .validate(&root)
        .expect("nests under the same type");
    root["membership_type"] = json!(TEAM_MEMBER);
    validator
        .validate(&root)
        .expect_err("the binding belongs to the type's traits");
}

#[tokio::test]
async fn the_sdk_validator_applies_inherited_metadata_through_a_nested_ref() {
    let registry = registry_with_derived_types();
    let validator = read(&registry, DEPT)
        .await
        .compile_payload_validator()
        .expect("compiles from the managed materialization");

    let payload = |metadata| {
        json!({
            "parent_type": null, "metadata": metadata
        })
    };
    validator
        .validate(&payload(json!({ "tier": "gold", "code": "ab" })))
        .expect("conforms to the tenant and dept constraints");
    for invalid in [
        json!({"code": "ab"}),
        json!({"tier": 7}),
        json!({"tier": "gold", "code": "abcd"}),
        json!("not an object"),
    ] {
        validator
            .validate(&payload(invalid))
            .expect_err("metadata constraints apply");
    }
}

#[tokio::test]
async fn existing_and_new_groups_resolve_the_current_membership_schema() {
    use resource_group::domain::error::DomainError;
    use resource_group::domain::repo::GroupRepositoryTrait;
    use resource_group::infra::storage::group_repo::GroupRepository;
    use resource_group::infra::types_registry::RegistryTypeCatalog;
    use resource_group_sdk::CreateGroupRequest;

    let registry = Arc::new(registry());
    registry.seed(USER, user_type());
    registry.seed(
        TEAM_MEMBER,
        json!({
            "$id": format!("gts://{TEAM_MEMBER}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [
                {"$ref": format!("gts://{MEMBER_V1}")},
                {"properties": {"resource_type": {"type": "string", "const": USER}}}
            ]
        }),
    );
    let mut team_schema = json!({
        "$id": format!("gts://{TEAM_SCHEMA}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-traits": {"membership_type": TEAM_MEMBER},
        "x-gts-traits-schema": {"properties": {"membership_type": {"const": TEAM_MEMBER}}},
        "allOf": [{"$ref": format!("gts://{BASE}")}]
    });
    registry.seed(TEAM_SCHEMA, team_schema.clone());

    let db = common::test_db().await;
    let catalog = Arc::new(RegistryTypeCatalog::new(registry.clone()));
    let groups = common::make_group_service_with(db.clone(), catalog.clone());
    let members = common::make_membership_service_with(db.clone(), catalog);
    let tenant = uuid::Uuid::new_v4();
    let ctx = common::make_ctx(tenant);
    let imported_id = uuid::Uuid::new_v4();
    {
        let conn = db.conn().expect("RG database");
        let type_key = GroupRepository
            .ensure_type_key(&conn, TEAM_SCHEMA)
            .await
            .expect("imported group type");
        GroupRepository
            .insert(&conn, imported_id, None, type_key, "Imported", None, tenant)
            .await
            .expect("imported group");
    }
    members
        .add_membership(&ctx, imported_id, USER, "imported-user")
        .await
        .expect("the existing row uses the published membership schema");
    assert!(matches!(
        members
            .add_membership(&ctx, imported_id, TEAM_SCHEMA, "not-a-user")
            .await,
        Err(DomainError::Validation { .. })
    ));
    let team = groups
        .create_group(
            &ctx,
            CreateGroupRequest::new(TEAM_SCHEMA, "Platform"),
            tenant,
        )
        .await
        .expect("team root");
    let replacement = gts_id!("cf.core.rg.group_membership.v1~cf.test.rg.groups_only.v1~");
    registry.seed(
        replacement,
        json!({
            "$id": format!("gts://{replacement}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [
                {"$ref": format!("gts://{MEMBER_V1}")},
                {"properties": {"resource_type": {"const": TEAM_SCHEMA}}}
            ]
        }),
    );
    // Inject a different registry snapshot to exercise rule resolution independently of admission.
    team_schema["x-gts-traits"]["membership_type"] = json!(replacement);
    team_schema["x-gts-traits-schema"]["properties"]["membership_type"]["const"] =
        json!(replacement);
    registry.seed(TEAM_SCHEMA, team_schema);
    let new_team = groups
        .create_group(&ctx, CreateGroupRequest::new(TEAM_SCHEMA, "New"), tenant)
        .await
        .expect("new group uses the replacement binding");
    for group_id in [imported_id, team.id, new_team.id] {
        assert!(matches!(
            members
                .add_membership(&ctx, group_id, USER, "user-43")
                .await,
            Err(DomainError::Validation { .. })
        ));
        members
            .add_membership(&ctx, group_id, TEAM_SCHEMA, "team-43")
            .await
            .expect("every group uses the current rule for its type");
    }
}
