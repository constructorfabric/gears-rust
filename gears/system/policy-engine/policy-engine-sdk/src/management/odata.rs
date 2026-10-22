//! `OData` schemas of the listings: standard toolkit cursor paging. Bundles
//! are ordered by `id` (the tiebreaker), with no custom filter fields; a
//! bundle's versions by `ordinal`, newest first.

pub use generated::{
    BundleQuery, BundleQueryFilterField as BundleFilterField, VersionQuery,
    VersionQueryFilterField as VersionFilterField,
};

// The `ODataFilterable` expansion generates undocumented public enums; the
// allowance is scoped to this module so the crate-wide `missing_docs` denial
// still holds everywhere else.
#[allow(missing_docs)]
mod generated {
    use toolkit_odata_macros::ODataFilterable;
    use uuid::Uuid;

    /// Paging schema of a bundle listing (never constructed; it only drives
    /// the derive).
    #[derive(ODataFilterable)]
    pub struct BundleQuery {
        /// Bundle identity.
        #[odata(filter(kind = "Uuid"))]
        pub id: Uuid,
    }

    /// Paging schema of a version listing (never constructed; it only
    /// drives the derive).
    #[derive(ODataFilterable)]
    pub struct VersionQuery {
        /// Version ordinal within its bundle.
        #[odata(filter(kind = "I64"))]
        pub ordinal: i64,
    }
}
