// Created: 2026-08-13 by Constructor Tech
//! Declared `OData` query surfaces.
//!
//! A field is filterable only if it appears here. That is the whole mechanism
//! behind the rejection rule: the derive generates an enum with one variant per
//! declared field, so `$filter=secret_column eq 'x'` has no variant to parse
//! into and is refused before it reaches a query — never silently dropped.
//!
//! Silent dropping is the dangerous failure. An administrator asking for the
//! categories in one domain, whose filter was ignored, would be shown **every**
//! category believing the list was filtered.

use toolkit_odata_macros::ODataFilterable;

/// The filterable surface of a category.
///
/// Deliberately narrow. `description` and `icon` are presentation, and
/// `sort_order` is an ordering weight rather than something to select on; none
/// of them answers a question an administrator asks of a list. A field can be
/// added later without breaking a caller, whereas removing one cannot.
#[derive(ODataFilterable)]
pub struct CategoryQuery {
    /// The stable slug — an exact-match lookup by the key a setting embeds.
    #[odata(filter(kind = "String"))]
    pub key: String,

    /// Display name, which later search builds on through the trigram index.
    #[odata(filter(kind = "String"))]
    pub name: String,

    /// The domain a category belongs to, and the filter the administrative
    /// listing is built around.
    #[odata(filter(kind = "String"))]
    pub domain_affinity: String,
}

/// The generated filter-field enum for categories.
pub use CategoryQueryFilterField as CategoryFilterField;

/// The filterable surface of a setting declaration.
///
/// Narrow for the same reason as [`CategoryQuery`], and shaped by the questions
/// DESIGN.md records an administrator asking of this list: which category
/// (§4.3), which administrative domain (§4.3), and which `mode` -- standard mode
/// excludes advanced-only declarations (§4.2 *Mode*), so filtering on it is not
/// a convenience but part of how the console renders at all.
///
/// `status` is here because retirement is a soft delete: retired rows stay in
/// the table and an administrator auditing a category needs to ask for them.
/// `owner_module` answers "what did this gear contribute", which is the question
/// behind every reconcile.
///
/// Deliberately absent: `default_value` and `description` are search, not
/// filter, and reach the caller through the trigram indexes instead;
/// `scope_class`, `data_classification` and the boolean flags are rendering
/// concerns that no listing is organised around.
#[derive(ODataFilterable)]
pub struct DeclarationQuery {
    /// The full setting key -- an exact-match lookup by the key a consumer holds.
    #[odata(filter(kind = "String"))]
    pub key: String,

    /// Owning category, the primary axis an administrative listing is grouped by.
    #[odata(filter(kind = "Uuid"))]
    pub category_id: uuid::Uuid,

    /// The administrative domain, filtered on for the same reason categories are.
    #[odata(filter(kind = "String"))]
    pub domain_affinity: String,

    /// `standard` or `advanced`.
    #[odata(filter(kind = "String"))]
    pub mode: String,

    /// `active` or `retired`.
    #[odata(filter(kind = "String"))]
    pub status: String,

    /// The contributing module, for module-contributed declarations.
    #[odata(filter(kind = "String"))]
    pub owner_module: String,
}

/// The generated filter-field enum for declarations.
pub use DeclarationQueryFilterField as DeclarationFilterField;
