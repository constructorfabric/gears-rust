// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-rest-odata:p1
//! `OData` filter field definitions for group hierarchy queries.
//!
//! Hierarchy `$filter` fields: `hierarchy/depth` (eq, ne, gt, ge, lt, le),
//! `type` (eq, ne, in).

use modkit_odata::filter::{FieldKind, FilterField};

/// Filter field enum for group hierarchy queries.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum HierarchyFilterField {
    /// Filter by relative depth from reference group.
    HierarchyDepth,
    /// Filter by GTS type path.
    Type,
}

impl FilterField for HierarchyFilterField {
    const FIELDS: &'static [Self] = &[Self::HierarchyDepth, Self::Type];

    fn name(&self) -> &'static str {
        match self {
            Self::HierarchyDepth => "hierarchy/depth",
            Self::Type => "type",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::HierarchyDepth | Self::Type => FieldKind::I64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TC-ODATA-04: HierarchyFilterField names + kinds
    #[test]
    fn hierarchy_filter_field_names_and_kinds() {
        assert_eq!(
            HierarchyFilterField::HierarchyDepth.name(),
            "hierarchy/depth"
        );
        assert_eq!(HierarchyFilterField::HierarchyDepth.kind(), FieldKind::I64);

        assert_eq!(HierarchyFilterField::Type.name(), "type");
        assert_eq!(HierarchyFilterField::Type.kind(), FieldKind::I64);

        assert_eq!(HierarchyFilterField::FIELDS.len(), 2);
    }
}
