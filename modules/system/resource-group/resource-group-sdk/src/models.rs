// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1
//! SDK model types for the resource-group module.
//!
//! These types form the public contract between the resource-group module
//! and its consumers. They are transport-agnostic and use string-based
//! GTS type paths (no surrogate SMALLINT IDs).

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// -- GtsTypePath value object --

// @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-1
/// Maximum length of a GTS type path.
const GTS_TYPE_PATH_MAX_LEN: usize = 1024;

/// Validated GTS type path value object.
///
/// A GTS type path follows the pattern `gts.<segment>~(<segment>~)*` where
/// each segment consists of lowercase alphanumeric characters, underscores,
/// and dots. Examples: `gts.cf.core.rg.type.v1~`, `gts.cf.core.rg.type.v1~cf.core._.tenant.v1~`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GtsTypePath(String);

impl GtsTypePath {
    /// Create a new `GtsTypePath` from a raw string, applying validation.
    ///
    /// # Errors
    /// Returns an error if the string is empty, exceeds 1024 characters,
    /// or does not match the GTS type path format.
    pub fn new(raw: impl Into<String>) -> Result<Self, String> {
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-2
        let raw = raw.into();
        let s = raw.trim().to_lowercase();
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-2

        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-3
        if s.is_empty() {
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-3a
            return Err("GTS type path must not be empty".to_owned());
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-3a
        }
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-3

        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-5
        if s.len() > GTS_TYPE_PATH_MAX_LEN {
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-5a
            return Err("GTS type path exceeds maximum length".to_owned());
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-5a
        }
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-5

        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-4
        // Validate format using the canonical gts crate parser.
        // Each tilde-separated segment must be a valid GTS ID with 5+ tokens
        // (vendor.package.namespace.type.vMAJOR).
        if gts::GtsID::new(&s).is_err() {
            // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-4a
            return Err("Invalid GTS type path format".to_owned());
            // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-4a
        }
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-4

        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-6
        // @cpt-begin:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-7
        Ok(Self(s))
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-7
        // @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-6
    }

    /// Return the inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GtsTypePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<GtsTypePath> for String {
    fn from(p: GtsTypePath) -> Self {
        p.0
    }
}

impl TryFrom<String> for GtsTypePath {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl AsRef<str> for GtsTypePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
// @cpt-end:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1:inst-gts-val-1

// -- Type --

/// A GTS resource group type definition.
///
/// Matches the REST `Type` schema. All references use string GTS type paths;
/// surrogate SMALLINT IDs are internal to the persistence layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroupType {
    /// GTS type path (e.g. `gts.cf.core.rg.type.v1~cf.core._.tenant.v1~`)
    pub code: String,
    /// Whether groups of this type can be root nodes (no parent).
    pub can_be_root: bool,
    /// GTS type paths of types allowed as parents.
    pub allowed_parent_types: Vec<String>,
    /// GTS type paths of resource types allowed as members.
    pub allowed_membership_types: Vec<String>,
    /// Optional JSON Schema for the metadata object of instances of this type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_schema: Option<serde_json::Value>,
}

/// Request body for creating a new GTS type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTypeRequest {
    /// GTS type path. Must have prefix `gts.cf.core.rg.type.v1~`.
    ///
    /// Whether this creates a new tenant scope is derived from the code: any
    /// type whose path starts with [`TENANT_RG_TYPE_PATH`](crate::TENANT_RG_TYPE_PATH)
    /// is a tenant type (`tenant_id = group.id` for its instances).
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

/// Request body for updating an existing GTS type (full replacement via PUT).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTypeRequest {
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

// -- Group --

/// Hierarchy context for a resource group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupHierarchy {
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Tenant scope.
    pub tenant_id: Uuid,
}

/// Hierarchy context for a resource group with depth information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupHierarchyWithDepth {
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Tenant scope.
    pub tenant_id: Uuid,
    /// Relative distance from reference group.
    pub depth: i32,
}

/// A resource group entity.
///
/// Group responses do NOT include `created_at`/`updated_at` (per DESIGN).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroup {
    /// Group identifier.
    pub id: Uuid,
    /// GTS chained type code (e.g. `gts.cf.core.rg.type.v1~cf.core._.tenant.v1~`).
    #[serde(rename = "type")]
    pub code: String,
    /// Display name.
    pub name: String,
    /// Hierarchy context.
    pub hierarchy: GroupHierarchy,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// A resource group entity with depth information (for hierarchy queries).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroupWithDepth {
    /// Group identifier.
    pub id: Uuid,
    /// GTS chained type code (e.g. `gts.cf.core.rg.type.v1~cf.core._.tenant.v1~`).
    #[serde(rename = "type")]
    pub code: String,
    /// Display name.
    pub name: String,
    /// Hierarchy context with depth.
    pub hierarchy: GroupHierarchyWithDepth,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Request body for creating a new resource group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGroupRequest {
    /// Optional caller-supplied ID (used by seeding for stable identity).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// GTS chained type code. Must have prefix `gts.cf.core.rg.type.v1~`.
    #[serde(rename = "type")]
    pub code: String,
    /// Display name (1..255 characters).
    pub name: String,
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Request body for updating a resource group (full replacement via PUT).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateGroupRequest {
    /// GTS chained type code. Must have prefix `gts.cf.core.rg.type.v1~`.
    #[serde(rename = "type")]
    pub code: String,
    /// Display name (1..255 characters).
    pub name: String,
    /// Parent group ID (null for root groups).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

// -- Membership --

/// A membership link between a resource and a group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroupMembership {
    /// Group this resource belongs to.
    pub group_id: Uuid,
    /// GTS type path of the resource.
    pub resource_type: String,
    /// External resource identifier.
    pub resource_id: String,
}

// @cpt-dod:cpt-cf-resource-group-dod-testing-sdk-models:p1
#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;

// @cpt-end:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-full
