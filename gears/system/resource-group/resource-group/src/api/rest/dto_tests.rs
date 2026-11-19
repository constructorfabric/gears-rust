// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-rest-api:p2
use super::*;
use toolkit_gts::gts_id;
use uuid::Uuid;

// TC-DTO-03: ResourceGroup -> GroupDto
#[test]
fn dto_group_from_resource_group() {
    let parent_id = Uuid::now_v7();
    let tenant_id = Uuid::now_v7();
    let group = ResourceGroup {
        id: Uuid::now_v7(),
        code: gts_id!("cf.system.rg.type.v1~").to_owned(),
        name: "My Group".to_owned(),
        hierarchy: resource_group_sdk::models::GroupHierarchy {
            parent_id: Some(parent_id),
            tenant_id,
        },
        metadata: Some(serde_json::json!({"k": "v"})),
    };
    let dto: GroupDto = group.clone().into();
    assert_eq!(dto.id, group.id);
    assert_eq!(dto.type_path, gts_id!("cf.system.rg.type.v1~"));
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
        code: gts_id!("cf.system.rg.type.v1~").to_owned(),
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

// TC-DTO-05: Deserialize {"type": gts_id!(".."), "name": "X"} into CreateGroupDto
#[test]
fn dto_create_group_deserialize_type_key() {
    let json = serde_json::json!({
        "type": gts_id!("cf.system.rg.type.v1~x.test.dto.mytype.v1~"),
        "name": "X"
    })
    .to_string();
    let dto: CreateGroupDto = serde_json::from_str(&json).unwrap();
    assert_eq!(
        dto.type_path,
        gts_id!("cf.system.rg.type.v1~x.test.dto.mytype.v1~")
    );
    assert_eq!(dto.name, "X");
    assert!(dto.parent_id.is_none());
    assert!(dto.id.is_none());
}

// TC-DTO-05b: Caller-supplied `id` is deserialized and passed through to the SDK request
#[test]
fn dto_create_group_id_passthrough() {
    let id = Uuid::now_v7();
    let json = serde_json::json!({
        "id": id,
        "type": gts_id!("cf.system.rg.type.v1~x.test.dto.mytype.v1~"),
        "name": "X"
    })
    .to_string();
    let dto: CreateGroupDto = serde_json::from_str(&json).unwrap();
    assert_eq!(dto.id, Some(id));

    let req: CreateGroupRequest = dto.into();
    assert_eq!(req.id, Some(id));
}

// TC-DTO-05c: Omitted `id` maps to `None` in the SDK request (server generates it)
#[test]
fn dto_create_group_no_id_maps_to_none() {
    let dto = CreateGroupDto {
        id: None,
        type_path: gts_id!("cf.system.rg.type.v1~x.test.dto.mytype.v1~").to_owned(),
        name: "X".to_owned(),
        parent_id: None,
        tenant_id: None,
        metadata: None,
    };
    let req: CreateGroupRequest = dto.into();
    assert!(req.id.is_none());
}

// TC-DTO-07: MembershipDto serialization has no tenant_id key
#[test]
fn dto_membership_serialize_no_tenant_id() {
    let membership = ResourceGroupMembership {
        group_id: Uuid::now_v7(),
        resource_type: gts_id!("cf.system.rg.type.v1~").to_owned(),
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
