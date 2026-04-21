// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-rest-api:p2
use super::*;
use uuid::Uuid;

// TC-DTO-01: ResourceGroupType -> TypeDto
#[test]
fn dto_type_from_resource_group_type() {
    let rgt = ResourceGroupType {
        code: "gts.x.system.rg.type.v1~x.test.dto.mytype.v1~".to_owned(),
        can_be_root: true,
        allowed_parent_types: vec!["gts.parent~".to_owned()],
        allowed_membership_types: vec!["gts.member~".to_owned()],
        metadata_schema: Some(serde_json::json!({"type": "object"})),
    };
    let dto: TypeDto = rgt.into();
    assert_eq!(dto.code, "gts.x.system.rg.type.v1~x.test.dto.mytype.v1~");
    assert!(dto.can_be_root);
    assert_eq!(dto.allowed_parent_types, vec!["gts.parent~"]);
    assert_eq!(dto.allowed_membership_types, vec!["gts.member~"]);
    assert!(dto.metadata_schema.is_some());
}

// TC-DTO-02: CreateTypeDto -> CreateTypeRequest
#[test]
fn dto_create_type_to_request() {
    let dto = CreateTypeDto {
        code: "gts.x.system.rg.type.v1~x.test.dto.mytype.v1~".to_owned(),
        can_be_root: false,
        allowed_parent_types: vec!["gts.parent~".to_owned()],
        allowed_membership_types: vec![],
        metadata_schema: None,
    };
    let req: CreateTypeRequest = dto.into();
    assert_eq!(req.code, "gts.x.system.rg.type.v1~x.test.dto.mytype.v1~");
    assert!(!req.can_be_root);
    assert_eq!(req.allowed_parent_types, vec!["gts.parent~"]);
    assert!(req.allowed_membership_types.is_empty());
    assert!(req.metadata_schema.is_none());
}

// TC-DTO-03: ResourceGroup -> GroupDto
#[test]
fn dto_group_from_resource_group() {
    let parent_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    let group = ResourceGroup {
        id: Uuid::now_v7(),
        code: "gts.x.system.rg.type.v1~".to_owned(),
        name: "My Group".to_owned(),
        hierarchy: resource_group_sdk::models::GroupHierarchy {
            parent_id: Some(parent_id),
            tenant_id,
        },
        metadata: Some(serde_json::json!({"k": "v"})),
    };
    let dto: GroupDto = group.clone().into();
    assert_eq!(dto.id, group.id);
    assert_eq!(dto.type_path, "gts.x.system.rg.type.v1~");
    assert_eq!(dto.name, "My Group");
    assert_eq!(dto.hierarchy.parent_id, Some(parent_id));
    assert_eq!(dto.hierarchy.tenant_id, tenant_id);
    assert!(dto.metadata.is_some());
}

// TC-DTO-04: ResourceGroupWithDepth -> GroupWithDepthDto
#[test]
fn dto_group_with_depth_from_resource_group() {
    let tenant_id = Uuid::now_v7();
    let gwd = ResourceGroupWithDepth {
        id: Uuid::now_v7(),
        code: "gts.x.system.rg.type.v1~".to_owned(),
        name: "Depth Group".to_owned(),
        hierarchy: resource_group_sdk::models::GroupHierarchyWithDepth {
            parent_id: None,
            tenant_id,
            depth: 3,
        },
        metadata: None,
    };
    let dto: GroupWithDepthDto = gwd.into();
    assert_eq!(dto.name, "Depth Group");
    assert_eq!(dto.hierarchy.depth, 3);
    assert!(dto.hierarchy.parent_id.is_none());
    assert_eq!(dto.hierarchy.tenant_id, tenant_id);
}

// TC-DTO-05: Deserialize {"type": "gts...", "name": "X"} into CreateGroupDto
#[test]
fn dto_create_group_deserialize_type_key() {
    let json = r#"{"type": "gts.x.system.rg.type.v1~x.test.dto.mytype.v1~", "name": "X"}"#;
    let dto: CreateGroupDto = serde_json::from_str(json).unwrap();
    assert_eq!(
        dto.type_path,
        "gts.x.system.rg.type.v1~x.test.dto.mytype.v1~"
    );
    assert_eq!(dto.name, "X");
    assert!(dto.parent_id.is_none());
}

// TC-DTO-06: Deserialize with no vectors -> defaults to empty
#[test]
fn dto_create_type_deserialize_missing_vectors_default_empty() {
    let json = r#"{"code": "gts.x.system.rg.type.v1~x.test.dto.mytype.v1~", "can_be_root": true}"#;
    let dto: CreateTypeDto = serde_json::from_str(json).unwrap();
    assert!(dto.allowed_parent_types.is_empty());
    assert!(dto.allowed_membership_types.is_empty());
}

// TC-DTO-07: MembershipDto serialization has no tenant_id key
#[test]
fn dto_membership_serialize_no_tenant_id() {
    let membership = ResourceGroupMembership {
        group_id: Uuid::now_v7(),
        resource_type: "gts.x.system.rg.type.v1~".to_owned(),
        resource_id: "res-001".to_owned(),
    };
    let dto: MembershipDto = membership.into();
    let json = serde_json::to_value(&dto).unwrap();
    assert!(
        json.get("tenant_id").is_none(),
        "MembershipDto should not contain tenant_id, got: {json}"
    );
    assert!(json.get("group_id").is_some());
    assert!(json.get("resource_type").is_some());
    assert!(json.get("resource_id").is_some());
}
