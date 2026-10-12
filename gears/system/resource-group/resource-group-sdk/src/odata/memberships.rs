// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-rest-odata:p1
//! `OData` filter field definitions for membership entities.
//!
//! Membership list `$filter` fields: `group_id` (eq, ne, in), `resource_type` (eq, ne, in),
//! `resource_id` (eq, ne, in).
//! RG accepts UUID literals with or without single quotes for `group_id`.
//! `resource_type` uses a quoted, registered GTS type path; invalid filter values
//! return Invalid Argument (HTTP 400). Every referenced type must be registered,
//! including values in `ne`, `in`, and negated predicates. An unknown type rejects
//! the entire filter, even when an `in` list also contains registered types.

use toolkit_odata::filter::{FieldKind, FilterField};

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
