// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-rest-odata:p1
//! `OData` filter field definitions for resource group entities.
//!
//! Group list `$filter` fields: `type` (eq, ne, in), `hierarchy/parent_id` (eq, ne, in),
//! `id` (eq, ne, in), `name` (eq, ne, in).
//!
//! The `hierarchy/parent_id` field uses `OData` nested path syntax; since the
//! `ODataFilterable` derive macro does not support slash-separated names,
//! the `FilterField` trait is implemented manually.

use modkit_odata::filter::{FieldKind, FilterField};

/// Filter field enum for group list queries.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum GroupFilterField {
    /// Filter by GTS type path.
    Type,
    /// Filter by parent group ID (direct parent only).
    HierarchyParentId,
    /// Filter by group ID.
    Id,
    /// Filter by group name.
    Name,
}

impl FilterField for GroupFilterField {
    const FIELDS: &'static [Self] = &[Self::Type, Self::HierarchyParentId, Self::Id, Self::Name];

    fn name(&self) -> &'static str {
        match self {
            Self::Type => "type",
            Self::HierarchyParentId => "hierarchy/parent_id",
            Self::Id => "id",
            Self::Name => "name",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            // Type is a GTS type path string in the public API; the persistence
            // layer resolves string paths to SMALLINT IDs after OData validation.
            Self::Type | Self::Name => FieldKind::String,
            Self::HierarchyParentId | Self::Id => FieldKind::Uuid,
        }
    }
}

#[cfg(test)]
#[path = "groups_tests.rs"]
mod groups_tests;
