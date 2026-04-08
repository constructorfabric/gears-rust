//! REST DTOs for resource-group type and group management.

use resource_group_sdk::models::{
    CreateGroupRequest, CreateTypeRequest, PatchGroupRequest, ResourceGroup,
    ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth, UpdateGroupRequest,
    UpdateTypeRequest, option_option,
};
use uuid::Uuid;

/// REST DTO for GTS type representation.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct TypeDto {
    /// GTS type path
    pub code: String,
    /// Whether groups of this type can be root nodes
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types
    pub allowed_parents: Vec<String>,
    /// GTS type paths of allowed membership resource types
    pub allowed_memberships: Vec<String>,
    /// Optional JSON Schema for instance metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<serde_json::Value>,
}

/// REST DTO for creating a new GTS type.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct CreateTypeDto {
    /// GTS type path. Must have prefix `gts.x.system.rg.type.v1~`.
    pub code: String,
    /// Whether groups of this type can be root nodes.
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types.
    #[serde(default)]
    pub allowed_parents: Vec<String>,
    /// GTS type paths of allowed membership resource types.
    #[serde(default)]
    pub allowed_memberships: Vec<String>,
    /// Optional JSON Schema for instance metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<serde_json::Value>,
}

/// REST DTO for updating a GTS type (full replacement via PUT).
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct UpdateTypeDto {
    /// Whether groups of this type can be root nodes.
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types.
    #[serde(default)]
    pub allowed_parents: Vec<String>,
    /// GTS type paths of allowed membership resource types.
    #[serde(default)]
    pub allowed_memberships: Vec<String>,
    /// Optional JSON Schema for instance metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<serde_json::Value>,
}

// -- Conversions --

impl From<ResourceGroupType> for TypeDto {
    fn from(t: ResourceGroupType) -> Self {
        Self {
            code: t.code,
            can_be_root: t.can_be_root,
            allowed_parents: t.allowed_parents,
            allowed_memberships: t.allowed_memberships,
            metadata_schema: t.metadata_schema,
        }
    }
}

impl From<CreateTypeDto> for CreateTypeRequest {
    fn from(dto: CreateTypeDto) -> Self {
        Self {
            code: dto.code,
            can_be_root: dto.can_be_root,
            allowed_parents: dto.allowed_parents,
            allowed_memberships: dto.allowed_memberships,
            metadata_schema: dto.metadata_schema,
        }
    }
}

impl From<UpdateTypeDto> for UpdateTypeRequest {
    fn from(dto: UpdateTypeDto) -> Self {
        Self {
            can_be_root: dto.can_be_root,
            allowed_parents: dto.allowed_parents,
            allowed_memberships: dto.allowed_memberships,
            metadata_schema: dto.metadata_schema,
        }
    }
}

// -- Group DTOs --

/// REST DTO for hierarchy context in group responses.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct HierarchyDto {
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Tenant scope.
    pub tenant_id: Uuid,
}

/// REST DTO for hierarchy context with depth in group responses.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct HierarchyWithDepthDto {
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
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
#[modkit_macros::api_dto(request, response)]
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
#[modkit_macros::api_dto(request, response)]
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
#[modkit_macros::api_dto(request)]
pub struct CreateGroupDto {
    /// GTS chained type path. Must have prefix `gts.x.system.rg.type.v1~`.
    #[serde(rename = "type")]
    pub type_path: String,
    /// Display name (1..255 characters).
    pub name: String,
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// REST DTO for updating a resource group (full replacement via PUT).
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct UpdateGroupDto {
    /// GTS chained type path. Must have prefix `gts.x.system.rg.type.v1~`.
    #[serde(rename = "type")]
    pub type_path: String,
    /// Display name (1..255 characters).
    pub name: String,
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// REST DTO for patching a resource group (partial update via PATCH).
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
#[allow(clippy::option_option)]
pub struct PatchGroupDto {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "option_option::deserialize")]
    pub parent_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "option_option::deserialize")]
    pub metadata: Option<Option<serde_json::Value>>,
}

impl From<PatchGroupDto> for PatchGroupRequest {
    fn from(dto: PatchGroupDto) -> Self {
        Self {
            name: dto.name,
            parent_id: dto.parent_id,
            metadata: dto.metadata,
        }
    }
}

// -- Group conversions --

impl From<ResourceGroup> for GroupDto {
    fn from(g: ResourceGroup) -> Self {
        Self {
            id: g.id,
            type_path: g.type_path,
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
            type_path: g.type_path,
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
            type_path: dto.type_path,
            name: dto.name,
            parent_id: dto.parent_id,
            metadata: dto.metadata,
        }
    }
}

impl From<UpdateGroupDto> for UpdateGroupRequest {
    fn from(dto: UpdateGroupDto) -> Self {
        Self {
            type_path: dto.type_path,
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
#[modkit_macros::api_dto(response)]
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
