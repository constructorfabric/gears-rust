//! # toolkit-db-macros
//!
//! Procedural macros for the `toolkit-db` secure ORM layer.
//!
//! ## `#[derive(Scopable)]`
//!
//! Automatically implements `ScopableEntity` for a `SeaORM` entity based on attributes.
//!
//! **IMPORTANT**: All four scope dimensions must be explicitly specified. No implicit defaults.
//!
//! ### Example
//!
//! ```ignore
//! use sea_orm::entity::prelude::*;
//! use toolkit_db::secure::Scopable;
//!
//! #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
//! #[sea_orm(table_name = "users")]
//! #[secure(
//!     tenant_col = "tenant_id",
//!     resource_col = "id",
//!     no_owner,
//!     no_type
//! )]
//! pub struct Model {
//!     #[sea_orm(primary_key)]
//!     pub id: Uuid,
//!     pub tenant_id: Uuid,
//!     pub email: String,
//! }
//! ```
//!
//! ### Attributes
//!
//! Each scope dimension requires exactly one declaration:
//! - **Tenant**: `tenant_col = "column_name"` OR `no_tenant`
//! - **Resource**: `resource_col = "column_name"` OR `no_resource`
//! - **Owner**: `owner_col = "column_name"` OR `no_owner`
//! - **Type**: `type_col = "column_name"` OR `no_type`
//! - **Unrestricted**: `unrestricted` (forbids all other attributes)
//! - **Custom PEP property**: `pep_prop(property_name = "column_name")` (repeatable)
//!
//! ## Note on `OData` Macros
//!
//! OData-related derives like `ODataFilterable` have been moved to `toolkit-odata-macros`.
//! Use that crate for `OData` protocol macros.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod scopable;

/// Derive macro for implementing `ScopableEntity`.
///
/// Place this on your `SeaORM` Model struct along with `#[secure(...)]` attributes.
///
/// # Attributes
///
/// **All four scope dimensions must be explicitly specified:**
///
/// - `tenant_col = "column_name"` OR `no_tenant` - Tenant isolation column
/// - `resource_col = "column_name"` OR `no_resource` - Primary resource ID column
/// - `owner_col = "column_name"` OR `no_owner` - Owner-based filtering column
/// - `type_col = "column_name"` OR `no_type` - Type-based filtering column
/// - `unrestricted` - Mark as global entity (forbids all other attributes)
/// - `pep_prop(property_name = "column_name")` - Custom PEP property mapping (repeatable)
///
/// The macro generates the `SCOPE_PROPERTIES` table from those attributes,
/// under the well-known property names:
/// - `tenant_col` → `"owner_tenant_id"` (`pep_properties::OWNER_TENANT_ID`)
/// - `resource_col` → `"id"` (`pep_properties::RESOURCE_ID`)
/// - `owner_col` → `"owner_id"` (`pep_properties::OWNER_ID`)
/// - Each `pep_prop(name = "col")` → `"name"`
///
/// `type_col` gets no entry: no property name addresses it, so no scope can.
/// It is the one dimension the macro still emits as a method.
///
/// `resolve_property()`, `scope_columns()`, `tenant_col()`, `resource_col()`
/// and `owner_col()` are **not** generated. They are provided by the
/// `ScopeProperties` trait, which reads them all back out of that one table --
/// see its documentation for what a manual implementation writes.
///
/// # Example
///
/// ```ignore
/// #[derive(DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "users")]
/// #[secure(
///     tenant_col = "tenant_id",
///     resource_col = "id",
///     no_owner,
///     no_type
/// )]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: Uuid,
///     pub tenant_id: Uuid,
///     pub email: String,
/// }
/// ```
///
/// # Custom PEP Properties
///
/// Use `pep_prop(...)` to add custom authorization property mappings beyond the
/// standard dimension columns:
///
/// ```ignore
/// #[derive(DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "resources")]
/// #[secure(
///     tenant_col = "tenant_id",
///     resource_col = "id",
///     no_owner,
///     no_type,
///     pep_prop(department_id = "department_id"),
/// )]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: Uuid,
///     pub tenant_id: Uuid,
///     pub department_id: Uuid,
/// }
/// ```
///
/// # Global Entities
///
/// For entities that are not tenant-scoped (global lookup tables, system config, etc.),
/// use the `unrestricted` flag:
///
/// ```ignore
/// #[derive(DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "system_config")]
/// #[secure(unrestricted)]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: Uuid,
///     pub key: String,
///     pub value: String,
/// }
/// ```
#[proc_macro_derive(Scopable, attributes(secure))]
pub fn derive_scopable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    scopable::expand_derive_scopable(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
