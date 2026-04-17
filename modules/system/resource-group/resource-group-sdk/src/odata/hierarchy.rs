// Created: 2026-04-16 by Constructor Tech
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
            Self::HierarchyDepth => FieldKind::I64,
            // Type is a GTS type path string in the public API; the persistence
            // layer resolves string paths to SMALLINT IDs after OData validation.
            Self::Type => FieldKind::String,
        }
    }
}

#[cfg(test)]
#[path = "hierarchy_tests.rs"]
mod hierarchy_tests;
