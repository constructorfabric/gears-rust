// Created: 2026-04-16 by Constructor Tech
//! GTS schema definitions for the Resource Group type system.

use gts_macros::struct_to_gts_schema;

/// GTS base type schema for Resource Group types.
///
/// Defines the `x-gts-traits-schema` contract: `can_be_root`,
/// `allowed_parent_types`, `allowed_membership_types`.
///
/// "Is this type a tenant?" is **not** a trait — it is derived from the type
/// code: any type whose GTS chain starts with [`TENANT_RG_TYPE_PATH`] is a
/// tenant type.
///
/// All chained RG types (tenant, department, branch, etc.) inherit from this
/// base contract via `allOf` + `$ref`.
///
/// # TODO: replace manual DTOs when `gts-macros` supports `x-gts-traits-schema`
///
/// Currently `gts-macros` (`struct_to_gts_schema`) does not generate
/// `x-gts-traits-schema` in the output JSON Schema. Once it does, this struct
/// should replace:
/// - `models::ResourceGroupType` (response DTO)
/// - `models::CreateTypeRequest` (create request DTO)
/// - `models::UpdateTypeRequest` (update request DTO)
///
/// Blockers: `gts-macros` needs camelCase serde support, `x-gts-traits-schema`
/// generation, `metadata_schema` field support, and `Clone`/`Debug`/`Default`
/// derives.
///
/// # Schema ID
///
/// ```text
/// gts.cf.core.rg.type.v1~
/// ```
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = true,
    schema_id = "gts.cf.core.rg.type.v1~",
    description = "Resource Group base type — defines placement and tenant scope traits",
    properties = "id,can_be_root,allowed_parent_types,allowed_membership_types"
)]
pub struct ResourceGroupTypeV1 {
    /// GTS type path (schema identifier).
    pub id: gts::GtsInstanceId,
    /// Whether groups of this type can be root nodes (no parent). Default `false`.
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types.
    pub allowed_parent_types: Vec<String>,
    /// GTS type paths of allowed membership resource types.
    pub allowed_membership_types: Vec<String>,
}

/// GTS type path for the tenant resource-group type.
///
/// Any RG type whose code **starts with** this path is considered a tenant
/// type — creating a group of such a type starts a new tenant scope
/// (`tenant_id = group.id`). Non-tenant types inherit `tenant_id` from their
/// parent. There is no explicit `is_tenant` boolean on the type record; the
/// prefix is the single source of truth.
///
/// The tenant RG type itself is seeded externally (via API/config) with
/// `can_be_root: true` so root tenants are valid placements.
pub const TENANT_RG_TYPE_PATH: &str = "gts.cf.core.rg.type.v1~cf.core._.tenant.v1~";
