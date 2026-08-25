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
