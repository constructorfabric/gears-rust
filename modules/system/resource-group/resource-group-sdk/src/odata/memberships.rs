// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-rest-odata:p1
//! `OData` filter field definitions for membership entities.
//!
//! Membership list `$filter` fields: `group_id` (eq, ne, in), `resource_type` (eq, ne, in),
//! `resource_id` (eq, ne, in).

use modkit_odata::filter::{FieldKind, FilterField};

/// Filter field enum for membership list queries.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum MembershipFilterField {
    /// Filter by group ID.
    GroupId,
    /// Filter by resource type (GTS type path).
    ResourceType,
    /// Filter by resource ID.
    ResourceId,
}

impl FilterField for MembershipFilterField {
    const FIELDS: &'static [Self] = &[Self::GroupId, Self::ResourceType, Self::ResourceId];

    fn name(&self) -> &'static str {
        match self {
            Self::GroupId => "group_id",
            Self::ResourceType => "resource_type",
            Self::ResourceId => "resource_id",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::GroupId => FieldKind::Uuid,
            Self::ResourceType => FieldKind::I64,
            Self::ResourceId => FieldKind::String,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TC-ODATA-05: MembershipFilterField names + kinds
    #[test]
    fn membership_filter_field_names_and_kinds() {
        assert_eq!(MembershipFilterField::GroupId.name(), "group_id");
        assert_eq!(MembershipFilterField::GroupId.kind(), FieldKind::Uuid);

        assert_eq!(MembershipFilterField::ResourceType.name(), "resource_type");
        assert_eq!(MembershipFilterField::ResourceType.kind(), FieldKind::I64);

        assert_eq!(MembershipFilterField::ResourceId.name(), "resource_id");
        assert_eq!(MembershipFilterField::ResourceId.kind(), FieldKind::String);

        assert_eq!(MembershipFilterField::FIELDS.len(), 3);
    }
}
