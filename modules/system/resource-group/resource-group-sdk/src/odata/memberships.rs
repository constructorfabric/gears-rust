// Created: 2026-04-16 by Constructor Tech
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
            Self::ResourceType | Self::ResourceId => FieldKind::String,
        }
    }
}

#[cfg(test)]
#[path = "memberships_tests.rs"]
mod memberships_tests;
