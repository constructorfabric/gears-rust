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

/// The browse surface over effective values: `GET /settings-service/v1/settings`.
///
/// Written by hand rather than derived because `needs_review` is a boolean and
/// the derive knows only strings and UUIDs. The three fields are the whole
/// vocabulary: `category_id` and `key` select declarations — `key in (…)` is
/// the bulk read by key set — and `needs_review eq true` switches the listing
/// to the flagged override rows in the caller's subtree. `tenant` is resolution
/// context and deliberately not a field here: it is a query parameter, so a
/// filter on it is refused as unknown rather than silently narrowing a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingFilterField {
    /// The full setting key.
    Key,
    /// The owning category.
    CategoryId,
    /// Whether an override at a scope is flagged for review.
    NeedsReview,
}

impl toolkit_odata::filter::FilterField for SettingFilterField {
    const FIELDS: &'static [Self] = &[Self::Key, Self::CategoryId, Self::NeedsReview];

    fn name(&self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::CategoryId => "category_id",
            Self::NeedsReview => "needs_review",
        }
    }

    fn kind(&self) -> toolkit_odata::filter::FieldKind {
        match self {
            Self::Key => toolkit_odata::filter::FieldKind::String,
            Self::CategoryId => toolkit_odata::filter::FieldKind::Uuid,
            Self::NeedsReview => toolkit_odata::filter::FieldKind::Bool,
        }
    }
}
