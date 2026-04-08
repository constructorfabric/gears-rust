#![allow(clippy::unwrap_used, clippy::expect_used)]

//! GTS type system validation tests for Resource Group module (metadata approach).
//!
//! Validates the ADR-001 type architecture using the in-memory types-registry:
//! - Base RG contract with x-gts-traits-schema, additionalProperties:false, metadata placeholder
//! - Chained RG types with single $ref + inline metadata properties + x-gts-traits
//! - Entity schemas registered for reference (not $ref'd from chained types)
//! - Instance validation: base fields + metadata fields validated at GTS level
//! - Topology trait constraints (`can_be_root`, `allowed_parents`, `allowed_memberships`)

mod common;

use common::create_service;
use serde_json::json;
use types_registry_sdk::ListQuery;

// =============================================================================
// Helpers: schema factories (from ADR-001, metadata approach)
// =============================================================================

/// RG base contract — gts.x.core.rg.type.v1~
/// Closed model (additionalProperties:false) with metadata placeholder.
fn rg_base_contract() -> serde_json::Value {
    json!({
        "$id": "gts://gts.x.core.rg.type.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Resource Group Type",
        "type": "object",
        "x-gts-traits-schema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "can_be_root": {
                    "type": "boolean",
                    "default": false
                },
                "allowed_parents": {
                    "type": "array",
                    "items": { "type": "string", "x-gts-ref": "gts.x.core.rg.type.v1~" },
                    "default": []
                },
                "allowed_memberships": {
                    "type": "array",
                    "items": { "type": "string", "x-gts-ref": "gts.*" },
                    "default": []
                }
            }
        },
        "x-gts-traits": {
            "can_be_root": false,
            "allowed_parents": [],
            "allowed_memberships": []
        },
        "required": ["id", "name"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "name": { "type": "string", "minLength": 1, "maxLength": 255 },
            "parent_id": { "type": ["string", "null"], "format": "uuid" },
            "tenant_id": { "type": "string", "format": "uuid", "readOnly": true },
            "depth": { "type": "integer", "readOnly": true },
            "metadata": {
                "type": "object",
                "description": "Type-specific fields. Schema overridden by each chained RG type."
            }
        }
    })
}

/// Tenant entity schema — gts.y.core.tn.tenant.v1~ (registered for reference, not $ref'd)
fn tenant_entity_schema() -> serde_json::Value {
    json!({
        "$id": "gts://gts.y.core.tn.tenant.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Tenant",
        "type": "object",
        "required": ["id", "name"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "name": { "type": "string", "minLength": 1, "maxLength": 255 },
            "custom_domain": { "type": "string", "format": "hostname" },
            "barrier": { "type": "boolean", "default": false }
        }
    })
}

/// Department entity schema — gts.w.core.org.department.v1~ (registered for reference)
fn department_entity_schema() -> serde_json::Value {
    json!({
        "$id": "gts://gts.w.core.org.department.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Department",
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "short_description": { "type": "string", "maxLength": 500 },
            "category": { "type": "string", "maxLength": 100 }
        }
    })
}

/// Branch entity schema — gts.x.core.rg.branch.v1~ (registered for reference)
fn branch_entity_schema() -> serde_json::Value {
    json!({
        "$id": "gts://gts.x.core.rg.branch.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Branch",
        "type": "object",
        "required": ["id", "name"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "name": { "type": "string", "minLength": 1, "maxLength": 255 },
            "location": { "type": "string" }
        }
    })
}

/// User resource schema — gts.z.core.idp.user.v1~
fn user_resource_schema() -> serde_json::Value {
    json!({
        "$id": "gts://gts.z.core.idp.user.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "User",
        "type": "object",
        "required": ["id", "email", "display_name"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "email": { "type": "string", "format": "email" },
            "display_name": { "type": "string", "minLength": 1, "maxLength": 255 },
            "avatar_url": { "type": "string", "format": "uri" }
        }
    })
}

/// Course resource schema — gts.z.core.lms.course.v1~
fn course_resource_schema() -> serde_json::Value {
    json!({
        "$id": "gts://gts.z.core.lms.course.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Course",
        "type": "object",
        "required": ["id", "title"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "title": { "type": "string", "minLength": 1, "maxLength": 255 }
        }
    })
}

/// Tenant as RG type — single $ref + inline metadata
fn tenant_rg_type() -> serde_json::Value {
    json!({
        "$id": "gts://gts.x.core.rg.type.v1~y.core.tn.tenant.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [
            { "$ref": "gts://gts.x.core.rg.type.v1~" },
            {
                "properties": {
                    "metadata": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "custom_domain": { "type": "string", "format": "hostname" },
                            "barrier": { "type": "boolean", "default": false }
                        }
                    }
                },
                "x-gts-traits": {
                    "can_be_root": true,
                    "allowed_parents": ["gts.x.core.rg.type.v1~y.core.tn.tenant.v1~"],
                    "allowed_memberships": ["gts.z.core.idp.user.v1~"]
                }
            }
        ]
    })
}

/// Department as RG type — single $ref + inline metadata
fn department_rg_type() -> serde_json::Value {
    json!({
        "$id": "gts://gts.x.core.rg.type.v1~w.core.org.department.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [
            { "$ref": "gts://gts.x.core.rg.type.v1~" },
            {
                "properties": {
                    "metadata": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "category": { "type": "string", "maxLength": 100 },
                            "short_description": { "type": "string", "maxLength": 500 }
                        }
                    }
                },
                "x-gts-traits": {
                    "can_be_root": false,
                    "allowed_parents": ["gts.x.core.rg.type.v1~y.core.tn.tenant.v1~"],
                    "allowed_memberships": ["gts.z.core.idp.user.v1~"]
                }
            }
        ]
    })
}

/// Branch as RG type — single $ref + inline metadata
fn branch_rg_type() -> serde_json::Value {
    json!({
        "$id": "gts://gts.x.core.rg.type.v1~x.core.rg.branch.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [
            { "$ref": "gts://gts.x.core.rg.type.v1~" },
            {
                "properties": {
                    "metadata": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "location": { "type": "string" }
                        }
                    }
                },
                "x-gts-traits": {
                    "can_be_root": false,
                    "allowed_parents": ["gts.x.core.rg.type.v1~w.core.org.department.v1~"],
                    "allowed_memberships": [
                        "gts.z.core.idp.user.v1~",
                        "gts.z.core.lms.course.v1~"
                    ]
                }
            }
        ]
    })
}

/// Registers all schemas and chained RG types, switches to ready.
fn setup_rg_type_system() -> std::sync::Arc<types_registry::domain::service::TypesRegistryService> {
    let service = create_service();

    // Phase 1: base schemas (entity schemas registered for reference, not $ref'd)
    let base_schemas = vec![
        rg_base_contract(),
        tenant_entity_schema(),
        department_entity_schema(),
        branch_entity_schema(),
        user_resource_schema(),
        course_resource_schema(),
    ];
    let results = service.register(base_schemas);
    for (i, r) in results.iter().enumerate() {
        assert!(r.is_ok(), "Base schema {i} registration failed: {r:?}");
    }

    // Phase 2: chained RG types (single $ref to base contract + inline metadata)
    let chained_types = vec![tenant_rg_type(), department_rg_type(), branch_rg_type()];
    let results = service.register(chained_types);
    for (i, r) in results.iter().enumerate() {
        assert!(r.is_ok(), "Chained type {i} registration failed: {r:?}");
    }

    // Phase 3: validate & activate
    service
        .switch_to_ready()
        .expect("switch_to_ready failed - schemas invalid");

    service
}

// =============================================================================
// 1. Schema Registration & Traits
// =============================================================================

#[tokio::test]
async fn test_all_rg_schemas_register_and_validate() {
    let service = setup_rg_type_system();
    let all_types = service
        .list(&ListQuery::default().with_is_type(true))
        .unwrap();
    assert_eq!(
        all_types.len(),
        9,
        "Expected 9 type schemas (6 base + 3 chained)"
    );
}

#[tokio::test]
async fn test_base_rg_contract_has_metadata_property() {
    let service = setup_rg_type_system();
    let rg_base = service.get("gts.x.core.rg.type.v1~").unwrap();
    assert!(rg_base.is_type());
    assert_eq!(rg_base.vendor(), Some("x"));

    let content = &rg_base.content;
    assert!(content.get("x-gts-traits-schema").is_some());
    assert!(content["properties"]["metadata"].is_object());
}

#[tokio::test]
async fn test_chained_tenant_has_inline_metadata_schema() {
    let service = setup_rg_type_system();
    let tenant_rg = service
        .get("gts.x.core.rg.type.v1~y.core.tn.tenant.v1~")
        .unwrap();
    assert!(tenant_rg.is_type());
    assert_eq!(tenant_rg.segments.len(), 2);

    let content = &tenant_rg.content;
    let all_of = content["allOf"].as_array().unwrap();
    let override_block = all_of
        .iter()
        .find(|item| item.get("x-gts-traits").is_some())
        .unwrap();
    let traits = &override_block["x-gts-traits"];
    assert_eq!(traits["can_be_root"], json!(true));

    // Verify inline metadata properties exist
    let meta_props = &override_block["properties"]["metadata"]["properties"];
    assert!(meta_props["custom_domain"].is_object());
    assert!(meta_props["barrier"].is_object());
}

#[tokio::test]
async fn test_chained_department_cannot_be_root() {
    let service = setup_rg_type_system();
    let dept_rg = service
        .get("gts.x.core.rg.type.v1~w.core.org.department.v1~")
        .unwrap();
    let all_of = dept_rg.content["allOf"].as_array().unwrap();
    let traits_block = all_of
        .iter()
        .find(|i| i.get("x-gts-traits").is_some())
        .unwrap();
    assert_eq!(traits_block["x-gts-traits"]["can_be_root"], json!(false));
    assert_eq!(
        traits_block["x-gts-traits"]["allowed_parents"],
        json!(["gts.x.core.rg.type.v1~y.core.tn.tenant.v1~"])
    );
}

#[tokio::test]
async fn test_branch_allows_users_and_courses_as_members() {
    let service = setup_rg_type_system();
    let branch_rg = service
        .get("gts.x.core.rg.type.v1~x.core.rg.branch.v1~")
        .unwrap();
    let all_of = branch_rg.content["allOf"].as_array().unwrap();
    let traits_block = all_of
        .iter()
        .find(|i| i.get("x-gts-traits").is_some())
        .unwrap();
    let memberships = traits_block["x-gts-traits"]["allowed_memberships"]
        .as_array()
        .unwrap();
    assert_eq!(memberships.len(), 2);
    assert!(memberships.contains(&json!("gts.z.core.idp.user.v1~")));
    assert!(memberships.contains(&json!("gts.z.core.lms.course.v1~")));
}

// =============================================================================
// 2. Multi-vendor Type Isolation
// =============================================================================

#[tokio::test]
async fn test_vendor_isolation_across_rg_types() {
    let service = setup_rg_type_system();
    assert!(
        service
            .list(&ListQuery::default().with_vendor("x"))
            .unwrap()
            .len()
            >= 3
    );
    assert!(
        !service
            .list(&ListQuery::default().with_vendor("y"))
            .unwrap()
            .is_empty()
    );
    assert!(
        !service
            .list(&ListQuery::default().with_vendor("w"))
            .unwrap()
            .is_empty()
    );
    assert!(
        service
            .list(&ListQuery::default().with_vendor("z"))
            .unwrap()
            .len()
            >= 2
    );
}

// =============================================================================
// 3. Valid Instances (metadata approach)
// =============================================================================

#[tokio::test]
async fn test_valid_tenant_root_no_metadata() {
    let service = setup_rg_type_system();
    let t1 = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.t1.v1",
        "name": "T1",
        "parent_id": null,
        "tenant_id": "11111111-1111-1111-1111-111111111111",
        "depth": 0
    });
    let results = service.register(vec![t1]);
    assert!(
        results[0].is_ok(),
        "Root tenant without metadata: {:?}",
        results[0]
    );
}

#[tokio::test]
async fn test_valid_tenant_with_metadata_custom_domain() {
    let service = setup_rg_type_system();
    let t9 = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.t9.v1",
        "name": "T9",
        "parent_id": null,
        "tenant_id": "99999999-9999-9999-9999-999999999999",
        "depth": 0,
        "metadata": { "custom_domain": "t9.example.com" }
    });
    let results = service.register(vec![t9]);
    assert!(
        results[0].is_ok(),
        "Tenant with metadata.custom_domain: {:?}",
        results[0]
    );
}

#[tokio::test]
async fn test_valid_tenant_with_metadata_barrier() {
    let service = setup_rg_type_system();
    let t7 = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.t7.v1",
        "name": "T7",
        "parent_id": "11111111-1111-1111-1111-111111111111",
        "tenant_id": "77777777-7777-7777-7777-777777777777",
        "depth": 1,
        "metadata": { "barrier": true }
    });
    let results = service.register(vec![t7]);
    assert!(
        results[0].is_ok(),
        "Tenant with metadata.barrier: {:?}",
        results[0]
    );
}

#[tokio::test]
async fn test_valid_department_with_metadata() {
    let service = setup_rg_type_system();
    let d2 = json!({
        "id": "gts.x.core.rg.type.v1~w.core.org.department.v1~x.core._.d2.v1",
        "name": "D2",
        "parent_id": "11111111-1111-1111-1111-111111111111",
        "tenant_id": "11111111-1111-1111-1111-111111111111",
        "depth": 1,
        "metadata": { "category": "finance", "short_description": "Mega Department" }
    });
    let results = service.register(vec![d2]);
    assert!(
        results[0].is_ok(),
        "Department with metadata: {:?}",
        results[0]
    );
}

#[tokio::test]
async fn test_valid_branch_with_metadata() {
    let service = setup_rg_type_system();
    let b3 = json!({
        "id": "gts.x.core.rg.type.v1~x.core.rg.branch.v1~x.core._.b3.v1",
        "name": "B3",
        "parent_id": "22222222-2222-2222-2222-222222222222",
        "tenant_id": "11111111-1111-1111-1111-111111111111",
        "depth": 2,
        "metadata": { "location": "Building A, Floor 3" }
    });
    let results = service.register(vec![b3]);
    assert!(
        results[0].is_ok(),
        "Branch with metadata.location: {:?}",
        results[0]
    );
}

#[tokio::test]
async fn test_valid_user_instance() {
    let service = setup_rg_type_system();
    let user = json!({
        "id": "gts.z.core.idp.user.v1~z.core._.idp_user1.v1",
        "email": "alice@example.com",
        "display_name": "Alice"
    });
    let results = service.register(vec![user]);
    assert!(results[0].is_ok(), "Valid user: {:?}", results[0]);
}

#[tokio::test]
async fn test_valid_course_instance() {
    let service = setup_rg_type_system();
    let course = json!({
        "id": "gts.z.core.lms.course.v1~z.core._.lms_course1.v1",
        "title": "Introduction to GTS"
    });
    let results = service.register(vec![course]);
    assert!(results[0].is_ok(), "Valid course: {:?}", results[0]);
}

// =============================================================================
// 4. Invalid Instances: base field violations
// =============================================================================

#[tokio::test]
async fn test_tenant_missing_required_name() {
    let service = setup_rg_type_system();
    let bad = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.bad1.v1",
        "parent_id": null,
        "tenant_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
        "depth": 0
    });
    assert!(service.register(vec![bad])[0].is_err(), "Missing name");
}

#[tokio::test]
async fn test_tenant_name_too_long() {
    let service = setup_rg_type_system();
    let bad = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.bad2.v1",
        "name": "X".repeat(256),
        "parent_id": null,
        "tenant_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
        "depth": 0
    });
    assert!(service.register(vec![bad])[0].is_err(), "Name > 255");
}

#[tokio::test]
async fn test_tenant_empty_name() {
    let service = setup_rg_type_system();
    let bad = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.bad3.v1",
        "name": "",
        "parent_id": null,
        "tenant_id": "cccccccc-cccc-cccc-cccc-cccccccccccc",
        "depth": 0
    });
    assert!(service.register(vec![bad])[0].is_err(), "Empty name");
}

#[tokio::test]
async fn test_user_missing_required_email() {
    let service = setup_rg_type_system();
    let bad = json!({
        "id": "gts.z.core.idp.user.v1~z.core._.bad_user1.v1",
        "display_name": "Bob"
    });
    assert!(service.register(vec![bad])[0].is_err(), "Missing email");
}

#[tokio::test]
async fn test_course_missing_required_title() {
    let service = setup_rg_type_system();
    let bad = json!({
        "id": "gts.z.core.lms.course.v1~z.core._.bad_course.v1"
    });
    assert!(service.register(vec![bad])[0].is_err(), "Missing title");
}

// =============================================================================
// 5. Invalid Instances: metadata field violations (GTS-level validation)
// =============================================================================

#[tokio::test]
async fn test_metadata_department_category_too_long() {
    let service = setup_rg_type_system();
    let bad = json!({
        "id": "gts.x.core.rg.type.v1~w.core.org.department.v1~x.core._.bad_cat.v1",
        "name": "Bad Dept",
        "parent_id": "11111111-1111-1111-1111-111111111111",
        "tenant_id": "11111111-1111-1111-1111-111111111111",
        "depth": 1,
        "metadata": { "category": "X".repeat(101) }
    });
    assert!(
        service.register(vec![bad])[0].is_err(),
        "category > 100 chars rejected at GTS level"
    );
}

#[tokio::test]
async fn test_metadata_department_short_description_too_long() {
    let service = setup_rg_type_system();
    let bad = json!({
        "id": "gts.x.core.rg.type.v1~w.core.org.department.v1~x.core._.bad_desc.v1",
        "name": "Bad Dept",
        "parent_id": "11111111-1111-1111-1111-111111111111",
        "tenant_id": "11111111-1111-1111-1111-111111111111",
        "depth": 1,
        "metadata": { "short_description": "X".repeat(501) }
    });
    assert!(
        service.register(vec![bad])[0].is_err(),
        "short_description > 500 chars rejected"
    );
}

#[tokio::test]
async fn test_metadata_tenant_barrier_wrong_type() {
    let service = setup_rg_type_system();
    let bad = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.bad_barrier.v1",
        "name": "Bad Barrier",
        "parent_id": null,
        "tenant_id": "dddddddd-dddd-dddd-dddd-dddddddddddd",
        "depth": 0,
        "metadata": { "barrier": "yes" }
    });
    assert!(
        service.register(vec![bad])[0].is_err(),
        "barrier='yes' (string) rejected"
    );
}

#[tokio::test]
async fn test_metadata_tenant_unknown_field_rejected() {
    let service = setup_rg_type_system();
    let bad = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.bad_unknown.v1",
        "name": "Unknown Field Tenant",
        "parent_id": null,
        "tenant_id": "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee",
        "depth": 0,
        "metadata": { "foo": "bar" }
    });
    assert!(
        service.register(vec![bad])[0].is_err(),
        "Unknown metadata field rejected by additionalProperties:false"
    );
}

#[tokio::test]
async fn test_top_level_custom_field_passes_gts_but_app_layer_rejects() {
    // NOTE: Base contract is open model (no additionalProperties:false) because
    // GTS OP#12 compatibility check rejects derived schemas that loosen it.
    // Top-level field isolation is enforced at the application layer (RG module),
    // not at GTS level. GTS only validates metadata sub-object.
    let service = setup_rg_type_system();
    let instance = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.flat_ok.v1",
        "name": "Flat Field Tenant",
        "parent_id": null,
        "tenant_id": "ffffffff-ffff-ffff-ffff-ffffffffffff",
        "depth": 0,
        "barrier": true
    });
    // GTS accepts (open model), RG module would strip/reject at app layer
    assert!(
        service.register(vec![instance])[0].is_ok(),
        "GTS accepts top-level extra fields (open model); app layer enforces isolation"
    );
}

// =============================================================================
// 6. GTS ID Format Validation
// =============================================================================

#[tokio::test]
async fn test_invalid_gts_id_no_prefix() {
    let service = setup_rg_type_system();
    let bad = json!({ "$id": "x.core.rg.type.v1~", "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" });
    assert!(service.register(vec![bad])[0].is_err());
}

#[tokio::test]
async fn test_invalid_gts_id_uppercase() {
    let service = setup_rg_type_system();
    let bad = json!({ "$id": "gts://gts.X.Core.Rg.Type.V1~", "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" });
    assert!(service.register(vec![bad])[0].is_err());
}

#[tokio::test]
async fn test_invalid_gts_id_missing_version() {
    let service = setup_rg_type_system();
    let bad = json!({ "$id": "gts://gts.x.core.rg.type~", "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" });
    assert!(service.register(vec![bad])[0].is_err());
}

// =============================================================================
// 7. Chaining: broken $ref
// =============================================================================

#[tokio::test]
async fn test_chained_type_with_broken_ref_fails_on_ready() {
    let service = create_service();
    let orphan = json!({
        "$id": "gts://gts.x.core.rg.type.v1~q.nonexistent._.foo.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [
            { "$ref": "gts://gts.x.core.rg.type.v1~" },
            { "x-gts-traits": { "can_be_root": true, "allowed_parents": [], "allowed_memberships": [] } }
        ]
    });
    assert!(
        service.register(vec![orphan])[0].is_ok(),
        "Config mode accepts"
    );
    assert!(
        service.switch_to_ready().is_err(),
        "Missing $ref target rejects on ready"
    );
}

// =============================================================================
// 8. Full Hierarchy Batch (metadata approach)
// =============================================================================

#[tokio::test]
async fn test_full_hierarchy_batch() {
    let service = setup_rg_type_system();

    let instances = vec![
        json!({
            "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.t1.v1",
            "name": "T1", "parent_id": null,
            "tenant_id": "11111111-1111-1111-1111-111111111111", "depth": 0
        }),
        json!({
            "id": "gts.x.core.rg.type.v1~w.core.org.department.v1~x.core._.d2.v1",
            "name": "D2", "parent_id": "11111111-1111-1111-1111-111111111111",
            "tenant_id": "11111111-1111-1111-1111-111111111111", "depth": 1,
            "metadata": { "category": "finance", "short_description": "Mega Department" }
        }),
        json!({
            "id": "gts.x.core.rg.type.v1~x.core.rg.branch.v1~x.core._.b3.v1",
            "name": "B3", "parent_id": "22222222-2222-2222-2222-222222222222",
            "tenant_id": "11111111-1111-1111-1111-111111111111", "depth": 2,
            "metadata": { "location": "Building A, Floor 3" }
        }),
        json!({
            "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.t7.v1",
            "name": "T7", "parent_id": "11111111-1111-1111-1111-111111111111",
            "tenant_id": "77777777-7777-7777-7777-777777777777", "depth": 1,
            "metadata": { "barrier": true }
        }),
        json!({
            "id": "gts.x.core.rg.type.v1~w.core.org.department.v1~x.core._.d8.v1",
            "name": "D8", "parent_id": "77777777-7777-7777-7777-777777777777",
            "tenant_id": "77777777-7777-7777-7777-777777777777", "depth": 2,
            "metadata": { "category": "hr" }
        }),
        json!({
            "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.t9.v1",
            "name": "T9", "parent_id": null,
            "tenant_id": "99999999-9999-9999-9999-999999999999", "depth": 0,
            "metadata": { "custom_domain": "t9.example.com" }
        }),
    ];

    let results = service.register(instances);
    for (i, r) in results.iter().enumerate() {
        assert!(r.is_ok(), "Hierarchy instance {i} failed: {r:?}");
    }

    let all = service.list(&ListQuery::default()).unwrap();
    assert_eq!(all.len(), 15, "9 types + 6 instances");

    let instances_only = service
        .list(&ListQuery::default().with_is_type(false))
        .unwrap();
    assert_eq!(instances_only.len(), 6);
}

// =============================================================================
// 9. Deferred Validation
// =============================================================================

#[tokio::test]
async fn test_config_mode_accepts_invalid_then_ready_rejects() {
    let service = create_service();

    let schemas = vec![
        rg_base_contract(),
        tenant_entity_schema(),
        user_resource_schema(),
        tenant_rg_type(),
    ];
    for r in &service.register(schemas) {
        assert!(r.is_ok());
    }

    // Invalid tenant: missing name (accepted in config mode)
    let bad = json!({
        "id": "gts.x.core.rg.type.v1~y.core.tn.tenant.v1~x.core._.bad_cfg.v1",
        "parent_id": null, "tenant_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "depth": 0
    });
    assert!(
        service.register(vec![bad])[0].is_ok(),
        "Config mode defers validation"
    );

    let ready_result = service.switch_to_ready();
    assert!(ready_result.is_err(), "Ready rejects invalid instance");
    let err = ready_result.unwrap_err();
    let errors = err.validation_errors().unwrap();
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| {
        e.gts_id
            .contains("gts.x.core.rg.type.v1~y.core.tn.tenant.v1~")
    }));
}

// =============================================================================
// 10. Wildcard & Query
// =============================================================================

#[tokio::test]
async fn test_wildcard_query_all_rg_chained_types() {
    let service = setup_rg_type_system();
    let results = service
        .list(&ListQuery::default().with_pattern("gts.x.core.rg.type.v1~*"))
        .unwrap();
    assert!(results.len() >= 3);
}

#[tokio::test]
async fn test_query_by_namespace_rg() {
    let service = setup_rg_type_system();
    let results = service
        .list(&ListQuery::default().with_namespace("rg"))
        .unwrap();
    assert!(!results.is_empty());
}

// =============================================================================
// 11. Idempotent Registration
// =============================================================================

#[tokio::test]
async fn test_idempotent_schema_registration() {
    let service = setup_rg_type_system();
    assert!(service.register(vec![tenant_entity_schema()])[0].is_ok());
}

#[tokio::test]
async fn test_modified_schema_reregistration_fails() {
    let service = setup_rg_type_system();
    let modified = json!({
        "$id": "gts://gts.y.core.tn.tenant.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Tenant MODIFIED", "type": "object",
        "required": ["id", "name"],
        "properties": { "id": { "type": "string" }, "name": { "type": "string" } }
    });
    assert!(service.register(vec![modified])[0].is_err());
}
