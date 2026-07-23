// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-29 by Constructor Tech
//! REST DTOs for resource-group type and group management.

use resource_group_sdk::models::{
    CreateGroupRequest, CreateTypeRequest, ResourceGroup, ResourceGroupMembership,
    ResourceGroupType, ResourceGroupWithDepth, UpdateGroupRequest, UpdateTypeRequest,
};
use uuid::Uuid;

/// REST DTO for GTS type representation.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct TypeDto {
    /// GTS type path
    pub code: String,
    /// Whether groups of this type can be root nodes
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types
    pub allowed_parent_types: Vec<String>,
    /// GTS type paths of allowed membership resource types
    pub allowed_membership_types: Vec<String>,
    /// Optional JSON Schema for instance metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<serde_json::Value>,
}

/// REST DTO for creating a new GTS type.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateTypeDto {
    /// GTS type path. Must have prefix `gts.cf.core.rg.type.v1~`.
    ///
    /// Whether the type creates a new tenant scope is derived from the code:
    /// any path starting with the tenant RG type prefix is a tenant type.
    pub code: String,
    /// Whether groups of this type can be root nodes.
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types.
    #[serde(default)]
    pub allowed_parent_types: Vec<String>,
    /// GTS type paths of allowed membership resource types.
    #[serde(default)]
    pub allowed_membership_types: Vec<String>,
    /// Optional JSON Schema for instance metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<serde_json::Value>,
}

/// REST DTO for updating a GTS type (full replacement via PUT).
///
/// Every replaceable field is **required** so an omitted field cannot be
/// confused with "preserve previous value". Nullable fields
/// (`metadata_schema`) must be sent explicitly as `null` to clear them.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateTypeDto {
    /// Whether groups of this type can be root nodes.
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types.
    pub allowed_parent_types: Vec<String>,
    /// GTS type paths of allowed membership resource types.
    pub allowed_membership_types: Vec<String>,
    /// JSON Schema for instance metadata (`null` to clear).
    #[schema(required)]
    pub metadata_schema: Option<serde_json::Value>,
}

// -- Conversions --

impl From<ResourceGroupType> for TypeDto {
    fn from(t: ResourceGroupType) -> Self {
        Self {
            code: t.code,
            can_be_root: t.can_be_root,
            allowed_parent_types: t.allowed_parent_types,
            allowed_membership_types: t.allowed_membership_types,
            metadata_schema: t.metadata_schema,
        }
    }
}

impl From<CreateTypeDto> for CreateTypeRequest {
    fn from(dto: CreateTypeDto) -> Self {
        Self {
            code: dto.code,
            can_be_root: dto.can_be_root,
            allowed_parent_types: dto.allowed_parent_types,
            allowed_membership_types: dto.allowed_membership_types,
            metadata_schema: dto.metadata_schema,
        }
    }
}

impl From<UpdateTypeDto> for UpdateTypeRequest {
    fn from(dto: UpdateTypeDto) -> Self {
        Self {
            can_be_root: dto.can_be_root,
            allowed_parent_types: dto.allowed_parent_types,
            allowed_membership_types: dto.allowed_membership_types,
            metadata_schema: dto.metadata_schema,
        }
    }
}

// -- Group DTOs --

/// REST DTO for hierarchy context in group responses.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct HierarchyDto {
    /// Parent group ID (null for root groups).
    #[schema(required)]
    pub parent_id: Option<Uuid>,
    /// Tenant scope.
    pub tenant_id: Uuid,
}

/// REST DTO for hierarchy context with depth in group responses.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct HierarchyWithDepthDto {
    /// Parent group ID (null for root groups).
    #[schema(required)]
    pub parent_id: Option<Uuid>,
    /// Tenant scope.
    pub tenant_id: Uuid,
    /// Relative distance from reference group.
    pub depth: i32,
}

/// REST DTO for resource group representation.
///
/// Group responses do NOT include `created_at`/`updated_at` (per DESIGN).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct GroupDto {
    /// Group identifier.
    pub id: Uuid,
    /// GTS chained type path.
    #[serde(rename = "type")]
    pub type_path: String,
    /// Display name.
    pub name: String,
    /// Hierarchy context.
    pub hierarchy: HierarchyDto,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// REST DTO for resource group with depth (hierarchy queries).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct GroupWithDepthDto {
    /// Group identifier.
    pub id: Uuid,
    /// GTS chained type path.
    #[serde(rename = "type")]
    pub type_path: String,
    /// Display name.
    pub name: String,
    /// Hierarchy context with depth.
    pub hierarchy: HierarchyWithDepthDto,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// REST DTO for creating a new resource group.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateGroupDto {
    /// Optional caller-supplied ID. If omitted, the server generates a UUID.
    ///
    /// For tenant-typed groups the ID becomes the group's tenant scope
    /// (`tenant_id == group.id`), so a platform sync can align the RG tenant
    /// scope with an existing tenant identifier by supplying it here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// GTS chained type path. Must have prefix `gts.cf.core.rg.type.v1~`.
    #[serde(rename = "type")]
    pub type_path: String,
    /// Display name (1..255 characters).
    pub name: String,
    /// Parent group ID (null for root groups).
    pub parent_id: Option<Uuid>,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// REST DTO for updating a resource group (full replacement via PUT).
///
/// **The group's GTS type is immutable after creation.** The payload
/// deliberately does not carry a `type` field — to change a group's type,
/// delete the existing group and create a new one. See the SDK
/// `UpdateGroupRequest` doc for the full rationale.
///
/// Every replaceable field is **required** so an omitted field cannot be
/// confused with "preserve previous value". Nullable fields (`parent_id`,
/// `metadata`) must be sent explicitly as `null` to clear them — for
/// example, moving a group to root requires `"parent_id": null`, not an
/// omitted key.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateGroupDto {
    /// Display name (1..255 characters).
    pub name: String,
    /// Parent group ID (`null` for root groups).
    #[schema(required)]
    pub parent_id: Option<Uuid>,
    /// Type-specific metadata (`null` to clear).
    #[schema(required)]
    pub metadata: Option<serde_json::Value>,
}

// -- Group conversions --

impl From<ResourceGroup> for GroupDto {
    fn from(g: ResourceGroup) -> Self {
        Self {
            id: g.id,
            type_path: g.code,
            name: g.name,
            hierarchy: HierarchyDto {
                parent_id: g.hierarchy.parent_id,
                tenant_id: g.hierarchy.tenant_id,
            },
            metadata: g.metadata,
        }
    }
}

impl From<ResourceGroupWithDepth> for GroupWithDepthDto {
    fn from(g: ResourceGroupWithDepth) -> Self {
        Self {
            id: g.id,
            type_path: g.code,
            name: g.name,
            hierarchy: HierarchyWithDepthDto {
                parent_id: g.hierarchy.parent_id,
                tenant_id: g.hierarchy.tenant_id,
                depth: g.hierarchy.depth,
            },
            metadata: g.metadata,
        }
    }
}

impl From<CreateGroupDto> for CreateGroupRequest {
    fn from(dto: CreateGroupDto) -> Self {
        Self {
            id: dto.id,
            code: dto.type_path,
            name: dto.name,
            parent_id: dto.parent_id,
            metadata: dto.metadata,
        }
    }
}

impl From<UpdateGroupDto> for UpdateGroupRequest {
    fn from(dto: UpdateGroupDto) -> Self {
        Self {
            name: dto.name,
            parent_id: dto.parent_id,
            metadata: dto.metadata,
        }
    }
}

// -- Membership DTOs --

/// REST DTO for membership representation.
///
/// Membership responses do NOT include `tenant_id` (derived from group).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct MembershipDto {
    /// Group identifier.
    pub group_id: Uuid,
    /// GTS type path of the resource type.
    pub resource_type: String,
    /// Resource identifier.
    pub resource_id: String,
}

// -- Membership conversions --

impl From<ResourceGroupMembership> for MembershipDto {
    fn from(m: ResourceGroupMembership) -> Self {
        Self {
            group_id: m.group_id,
            resource_type: m.resource_type,
            resource_id: m.resource_id,
        }
    }
}

// @cpt-dod:cpt-cf-resource-group-dod-testing-odata-dto:p1

#[cfg(test)]
#[path = "dto_tests.rs"]
mod tests;
