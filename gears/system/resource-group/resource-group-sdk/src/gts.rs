// Created: 2026-04-16 by Constructor Tech
//! GTS schema definitions for the Resource Group type system.

use toolkit_gts::gts_id;
use toolkit_gts::local::gts_type_schema;

/// Type-level binding to the membership schema used by new groups.
#[derive(schemars::JsonSchema, serde::Serialize, toolkit_gts::GtsTraitsSchema)]
pub struct ResourceGroupTypeTraitsV1 {
    /// GTS ID of a concrete membership schema.
    #[schemars(extend("x-gts-type-ref" = GROUP_MEMBERSHIP_TYPE_ID))]
    pub membership_type: String,
}

/// Abstract GTS base type for Resource Group types: `gts.cf.core.rg.group.v1~`.
///
/// The persisted types registry is the only catalogue of RG types. Owners
/// publish derived types (`gts.cf.core.rg.group.v1~<vendor>...~`) before
/// application `init`; resource-group publishes this base.
///
/// Concrete types fix the `membership_type` trait with a string `const`.
/// Narrowing `parent_type`, `metadata` and membership `resource_type` is
/// optional. RG enforces the published schema and does not check that it
/// narrows anything. The bases allow root
/// placement, any RG parent type and any active concrete member type.
///
/// "Is this type a tenant?" is **not** a trait — it is derived from the type
/// code: any type whose GTS chain starts with one of the configured tenant
/// type prefixes (default: [`TENANT_GROUP_TYPE_ID`]) is a tenant type.
///
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.rg.group.v1~"),
    description = "Resource group placement, membership schema and metadata",
    properties = "id,metadata,parent_type",
    traits_schema = inline(ResourceGroupTypeTraitsV1),
    base = true,
    gts_abstract = true
)]
#[schemars(extend("required" = ["parent_type"]))]
pub struct ResourceGroupTypeV1<M> {
    /// GTS type path (schema identifier).
    pub id: gts::GtsInstanceId,
    /// The group's type-specific metadata; constrained by derived types.
    pub metadata: M,
    /// Actual parent's GTS type, or null when the group is a root.
    #[schemars(extend("x-gts-type-ref" = GROUP_TYPE_ID))]
    pub parent_type: Option<String>,
}

/// Resource type checked against the schema bound to the target group.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.rg.group_membership.v1~"),
    description = "Allowed resource types for group membership",
    properties = "gts_type",
    base = true,
    gts_abstract = true
)]
pub struct ResourceGroupMembershipV1 {
    /// GTS type of the candidate member resource.
    #[serde(rename = "resource_type")]
    #[schemars(with = "String", extend("x-gts-type-ref" = "gts.*"))]
    pub gts_type: gts::GtsTypeId,
}

/// GTS type ID of the base group schema, used for group authorization and errors.
///
/// Lands in `CanonicalError::{NotFound,AlreadyExists}.ctx.resource_type`
/// for every group-attributable error and backs the impl crate's
/// `#[resource_error("…")]` REST marker. The macro literal there cannot
/// reference this const (proc-macros can't resolve consts), so the
/// round-trip tests in [`crate::error`] assert the two stay equal.
///
/// Match it against the resource-scoped projection variants
/// ([`crate::ResourceGroupError::NotFound`] /
/// [`crate::ResourceGroupError::AlreadyExists`]).
pub const GROUP_TYPE_ID: &str = <ResourceGroupTypeV1<()> as gts::GtsSchema>::TYPE_ID;

/// GTS type ID of the base membership schema, used for membership authorization.
pub const GROUP_MEMBERSHIP_TYPE_ID: &str = <ResourceGroupMembershipV1 as gts::GtsSchema>::TYPE_ID;

/// GTS type ID of the default tenant group.
///
/// Any RG type whose code **starts with** a configured tenant-type path is
/// considered a tenant type — creating a group of such a type starts a new
/// tenant scope (`tenant_id = group.id`). Non-tenant types inherit
/// `tenant_id` from their parent. There is no explicit `is_tenant` boolean on
/// the type record; the prefix is the single source of truth.
///
/// This constant is the default of the resource-group gear's
/// `tenant_type_paths` and the rg-tr-plugin's `tenant_types` config; a
/// deployment with its own vendor overrides both lists (e.g.
/// `gts.cf.core.rg.group.v1~acme.core._.tenant.v1~`).
///
/// Owners publish the tenant RG type through Types Registry and allow a
/// `null` parent type where root tenants are supported.
pub const TENANT_GROUP_TYPE_ID: &str = gts_id!("cf.core.rg.group.v1~cf.core.rg.tenant.v1~");

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
