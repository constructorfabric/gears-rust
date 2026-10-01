//! `OData` schema of the bundle listing: standard toolkit cursor paging
//! ordered by `id` (the tiebreaker), with no custom filter fields.

pub use generated::{BundleQuery, BundleQueryFilterField as BundleFilterField};

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
}
