//! GTS schema definitions for the Resource Group type system.

use gts_macros::struct_to_gts_schema;

/// GTS base type schema for Resource Group types.
///
/// Defines the `x-gts-traits-schema` contract: `can_be_root`, `is_tenant`,
/// `allowed_parent_types`, `allowed_membership_types`.
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
    properties = "id,can_be_root,is_tenant,allowed_parent_types,allowed_membership_types"
)]
pub struct ResourceGroupTypeV1 {
    /// GTS type path (schema identifier).
    pub id: gts::GtsInstanceId,
    /// Whether groups of this type can be root nodes (no parent). Default `false`.
    pub can_be_root: bool,
    /// Whether instances create their own tenant scope (`tenant_id = group.id`). Default `false`.
    pub is_tenant: bool,
    /// GTS type paths of allowed parent types.
    pub allowed_parent_types: Vec<String>,
    /// GTS type paths of allowed membership resource types.
    pub allowed_membership_types: Vec<String>,
}
