// Created: 2026-04-16 by Constructor Tech
//! GTS schema definitions for the Resource Group type system.

use toolkit_gts::gts_id;
use toolkit_gts::local::gts_type_schema;

/// Abstract GTS base type for Resource Group types: `gts.cf.core.rg.type.v1~`.
///
/// The persisted types registry is the only catalogue of RG types. Owners
/// publish derived types (`gts.cf.core.rg.type.v1~<vendor>...~`) before
/// application `init`; resource-group publishes this base.
///
/// A concrete type must fix `membership_type` with a string `const` for new
/// groups; it may narrow `parent_type` and metadata. The owner does the
/// narrowing: resource-group enforces the schema as published and does not
/// check that it narrows anything. Left as declared here, `parent_type`
/// admits `null` (a root) and any RG type, and a membership schema's
/// `resource_type` admits any active GTS type.
///
/// "Is this type a tenant?" is **not** a trait — it is derived from the type
/// code: any type whose GTS chain starts with one of the configured tenant
/// type prefixes (default: [`TENANT_RG_TYPE_PATH`]) is a tenant type.
///
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.rg.type.v1~"),
    description = "Resource group placement, membership schema and metadata",
    properties = "id,metadata,parent_type,membership_type",
    base = true,
    gts_abstract = true
)]
#[schemars(extend("required" = ["parent_type", "membership_type"]))]
pub struct ResourceGroupTypeV1<M> {
    /// GTS type path (schema identifier).
    pub id: gts::GtsInstanceId,
    /// The group's type-specific metadata; constrained by derived types.
    pub metadata: M,
    /// Actual parent's GTS type, or null when the group is a root.
    #[schemars(extend("x-gts-type-ref" = TYPE_RESOURCE_TYPE))]
    pub parent_type: Option<String>,
    /// Exact membership schema identifier, bound at group creation.
    #[schemars(extend("x-gts-type-ref" = MEMBERSHIP_TYPE_V1))]
    pub membership_type: String,
}

/// Base of schemas constraining the type of a group member.
pub const MEMBERSHIP_TYPE_V1: &str = gts_id!("cf.core.rg.membership.v1~");

/// Resource type checked against the schema bound to the target group.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.rg.membership.v1~"),
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

/// Canonical GTS resource type for a resource **group** as a resource.
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
// `rg` namespace, NOT the crate-name-derived `resource_group`: this
// string is the PEP-evaluated resource type for every group CRUD gate,
// and every deployed role grant is written against the documented
// `gts.cf.core.rg.*` family (siblings: `rg.group_membership.v1~`,
// `rg.type.v1~`). The gts-rust v0.11.0 migration mechanically swapped
// the literal for a crate-derived id and silently renamed the type —
// which 403'd every existing grant (tenant members could no longer
// create groups, AM's ownership probe on tenant delete failed closed).
pub const GROUP_RESOURCE_TYPE: &str = gts_id!("cf.core.rg.group.v1~");

/// Canonical GTS resource type for a resource-group membership link.
pub const GROUP_MEMBERSHIP_RESOURCE_TYPE: &str = gts_id!("cf.core.rg.group_membership.v1~");

/// GTS identifier of the RG base type (`gts.cf.core.rg.type.v1~`): the
/// prefix every RG type identifier starts with.
pub const TYPE_RESOURCE_TYPE: &str = gts_id!("cf.core.rg.type.v1~");

/// Default GTS type path for the tenant resource-group type.
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
/// `gts.cf.core.rg.type.v1~acme.core._.tenant.v1~`).
///
/// Owners publish the tenant RG type through Types Registry and allow a
/// `null` parent type where root tenants are supported.
pub const TENANT_RG_TYPE_PATH: &str = gts_id!("cf.core.rg.type.v1~cf.core._.tenant.v1~");

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
