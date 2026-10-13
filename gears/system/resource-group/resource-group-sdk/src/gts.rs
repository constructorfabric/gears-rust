// Created: 2026-04-16 by Constructor Tech
//! GTS schema definitions for the Resource Group type system.

use schemars::JsonSchema;
use toolkit_gts::{GtsTraitsSchema, gts_id, gts_type_schema};

// RG types are GTS Type Schemas derived from `gts.cf.core.rg.type.v1~`.
//
// Topology lives in `x-gts-traits` (`can_be_root`, `allowed_parent_types`,
// `allowed_membership_types`); the JSON schema of a group's metadata lives at
// `properties.metadata` of the derived type schema. "Is this type a tenant?"
// is **not** a trait: any type whose GTS chain starts with
// [`TENANT_RG_TYPE_PATH`] is a tenant type.

// A single `allowed_parent_types` entry: a GTS id of an RG type. The
// `x-gts-type-ref` prefix makes the GTS store check that each value is derived
// from the RG base type. `transparent` keeps the schema a bare string.
#[allow(dead_code)]
#[derive(serde::Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-type-ref" = gts_id!("cf.core.rg.type.v1~")))]
pub struct RgParentTypeRef(pub String);

// A single `allowed_membership_types` entry: a GTS id of any resource type.
#[allow(dead_code)]
#[derive(serde::Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-type-ref" = "gts.*"))]
pub struct RgMemberTypeRef(pub String);

// Placement traits of `gts.cf.core.rg.type.v1~` (emitted as the base's
// `x-gts-traits-schema`). Plain `//` on purpose: a `///` doc on the struct
// would surface as a `description` on the emitted traits schema.
#[allow(clippy::doc_markdown)]
#[derive(Default, JsonSchema, serde::Serialize, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceGroupTypeTraits {
    /// Whether groups of this type can be root nodes (no parent).
    #[serde(default)]
    pub can_be_root: bool,
    /// GTS ids of RG types allowed as parent. A type listing its own id allows same-type nesting.
    #[serde(default)]
    pub allowed_parent_types: Vec<RgParentTypeRef>,
    /// GTS ids of resource types allowed as members of groups of this type.
    #[serde(default)]
    pub allowed_membership_types: Vec<RgMemberTypeRef>,
}

/// GTS base type schema for Resource Group types (abstract).
///
/// `id` is inert (the `gts-macros` base-struct contract requires it); the
/// generic `metadata` field is the container where a derived type describes
/// the metadata of its groups (`properties.metadata`). It is not required.
///
/// Placement rules are carried by `x-gts-traits` (see
/// [`ResourceGroupTypeTraits`]). The base declares generic defaults because
/// the GTS trait validator rejects a traits schema without values in the
/// chain, even for an abstract type.
///
/// # Schema ID
///
/// ```text
/// gts.cf.core.rg.type.v1~
/// ```
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.rg.type.v1~"),
    description = "Resource Group base type. Derived types describe group metadata in `metadata` and placement rules in x-gts-traits.",
    properties = "id,metadata",
    traits_schema = inline(ResourceGroupTypeTraits),
    traits = serde_json::json!({
        "can_be_root": false,
        "allowed_parent_types": [],
        "allowed_membership_types": []
    }),
    gts_abstract = true
)]
#[schemars(extend("required" = []))]
pub struct ResourceGroupTypeV1<M> {
    /// Inert: present only to satisfy the gts-macros base-struct contract. Not required.
    pub id: gts::GtsInstanceId,
    /// Group metadata container; derived types declare its fields.
    pub metadata: M,
}

/// Lifecycle status stored in tenant group metadata. Wire tokens are the
/// lowercase values read by the tenant-resolver RG plugin.
// Variants stay undocumented: variant `///` docs would make schemars emit a
// `oneOf` of `const` subschemas instead of a flat `enum`.
#[allow(dead_code)]
#[derive(JsonSchema, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TenantGroupStatus {
    Active,
    Suspended,
    Deleted,
}

/// Tenant RG type (`gts.cf.core.rg.type.v1~cf.core._.tenant.v1~`).
///
/// A root-capable type whose only allowed parent is itself (nested tenants
/// live under tenants). Metadata fields are read by the tenant-resolver RG
/// plugin.
#[gts_type_schema(
    dir_path = "schemas",
    base = ResourceGroupTypeV1,
    type_id = gts_id!("cf.core.rg.type.v1~cf.core._.tenant.v1~"),
    description = "Tenant resource group type",
    properties = "status,self_managed,custom_domain",
    traits = serde_json::json!({
        "can_be_root": true,
        "allowed_parent_types": [TENANT_RG_TYPE_PATH],
        "allowed_membership_types": []
    })
)]
pub struct TenantResourceGroupTypeV1 {
    /// Tenant lifecycle status. Absent means active.
    pub status: Option<TenantGroupStatus>,
    /// Whether the tenant is self-managed (a barrier for ancestor visibility).
    pub self_managed: Option<bool>,
    /// Custom domain of the tenant.
    pub custom_domain: Option<String>,
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

/// GTS type path for the tenant resource-group type.
///
/// Any RG type whose code **starts with** this path is considered a tenant
/// type — creating a group of such a type starts a new tenant scope
/// (`tenant_id = group.id`). Non-tenant types inherit `tenant_id` from their
/// parent. The prefix is the single source of truth: the type carries no
/// `is_tenant` trait, and the group row's `is_tenant` flag is derived from it
/// at creation.
///
/// The tenant RG type itself is declared in this module with
/// `can_be_root: true` so root tenants are valid placements.
pub const TENANT_RG_TYPE_PATH: &str = gts_id!("cf.core.rg.type.v1~cf.core._.tenant.v1~");

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
