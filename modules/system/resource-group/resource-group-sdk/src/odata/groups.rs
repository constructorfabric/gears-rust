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
            // Type is SMALLINT in DB; filter values are resolved from string to
            // integer at the persistence boundary before OData processing.
            Self::Type => FieldKind::I64,
            Self::Name => FieldKind::String,
            Self::HierarchyParentId | Self::Id => FieldKind::Uuid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TC-ODATA-01: GroupFilterField names
    #[test]
    fn group_filter_field_names_correct() {
        assert_eq!(GroupFilterField::Type.name(), "type");
        assert_eq!(
            GroupFilterField::HierarchyParentId.name(),
            "hierarchy/parent_id"
        );
        assert_eq!(GroupFilterField::Id.name(), "id");
        assert_eq!(GroupFilterField::Name.name(), "name");
    }

    // TC-ODATA-02: GroupFilterField kinds
    #[test]
    fn group_filter_field_kinds_correct() {
        assert_eq!(GroupFilterField::Type.kind(), FieldKind::I64);
        assert_eq!(GroupFilterField::HierarchyParentId.kind(), FieldKind::Uuid);
        assert_eq!(GroupFilterField::Id.kind(), FieldKind::Uuid);
        assert_eq!(GroupFilterField::Name.kind(), FieldKind::String);
    }

    // TC-ODATA-03: FIELDS completeness
    #[test]
    fn group_filter_field_completeness() {
        assert_eq!(GroupFilterField::FIELDS.len(), 4);
    }
}
