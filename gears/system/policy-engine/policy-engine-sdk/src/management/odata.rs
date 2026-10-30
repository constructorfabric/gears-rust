//! `OData` schemas of the listings: standard toolkit cursor paging. Bundles
//! are ordered by `id` (the tiebreaker), filterable by name and owning
//! tenant; a bundle's
//! versions by `ordinal`, newest first; assignments by `id`, filterable by
//! their bundle and target tenant.

pub use generated::{
    AssignmentQuery, AssignmentQueryFilterField as AssignmentFilterField, BundleQuery,
    BundleQueryFilterField as BundleFilterField, VersionQuery,
    VersionQueryFilterField as VersionFilterField,
};

// The `ODataFilterable` expansion generates undocumented public enums; the
// allowance is scoped to this module so the crate-wide `missing_docs` denial
// still holds everywhere else.
#[allow(missing_docs)]
mod generated {
    use toolkit_odata_macros::ODataFilterable;
    use uuid::Uuid;

    /// Filter and paging schema of a bundle listing (never constructed; it
    /// only drives the derive).
    #[derive(ODataFilterable)]
    pub struct BundleQuery {
        /// Bundle identity (the tiebreaker).
        #[odata(filter(kind = "Uuid"))]
        pub id: Uuid,
        /// Bundle name, unique within its owning tenant.
        #[odata(filter(kind = "String"))]
        pub name: String,
        /// Owning tenant; with `name`, it names exactly one bundle.
        #[odata(filter(kind = "Uuid"))]
        pub owner_tenant_id: Uuid,
    }

    /// Paging schema of a version listing (never constructed; it only
    /// drives the derive).
    #[derive(ODataFilterable)]
    pub struct VersionQuery {
        /// Version ordinal within its bundle.
        #[odata(filter(kind = "I64"))]
        pub ordinal: i64,
    }

    /// Filter and paging schema of an assignment listing (never
    /// constructed; it only drives the derive).
    #[derive(ODataFilterable)]
    pub struct AssignmentQuery {
        /// Assignment identity (the tiebreaker).
        #[odata(filter(kind = "Uuid"))]
        pub id: Uuid,
        /// Assigned bundle.
        #[odata(filter(kind = "Uuid"))]
        pub bundle_id: Uuid,
        /// Target tenant.
        #[odata(filter(kind = "Uuid"))]
        pub tenant_id: Uuid,
    }
}
