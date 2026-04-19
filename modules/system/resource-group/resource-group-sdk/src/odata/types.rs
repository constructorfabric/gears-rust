// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-rest-odata:p1
//! `OData` filter field definitions for GTS type resources.

use modkit_odata_macros::ODataFilterable;

/// Type filterable fields schema.
///
/// This struct defines which fields can be used in `OData` `$filter` queries
/// for GTS type resources. The field names match the wire format.
#[derive(ODataFilterable)]
pub struct TypeQuery {
    #[odata(filter(kind = "String"))]
    pub code: String,
}

/// Type alias for the generated filter field enum.
pub use TypeQueryFilterField as TypeFilterField;
