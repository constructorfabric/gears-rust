// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-persistence:p1
//! Infrastructure storage layer - database persistence and `OData` mapping.

pub mod entity;
pub mod group_repo;
mod gts_type_dict;
pub mod membership_repo;
pub mod migrations;
mod odata_filter;
pub mod odata_mapper;

// -- Foreign key constraint names --
//
// `PostgreSQL` includes the constraint name in a foreign-key-violation error
// message; `SQLite` does not (it says only "FOREIGN KEY constraint failed").
// `GroupRepository::insert` and `MembershipRepository::insert` match on these
// names to tell which referenced row is missing, rather than reporting a
// generic database error for every FK violation. Kept in one place so the
// names cannot drift out of sync with the DDL in
// `migrations/m20260306_000001_initial.rs` (and the `SQLite` rebuild in
// `m20261111_000001_gts_type_dictionary.rs`), which name each constraint.

/// `resource_group.parent_id` -> `resource_group.id`, `ON DELETE RESTRICT`.
pub(crate) const FK_RESOURCE_GROUP_PARENT: &str = "fk_resource_group_parent";

/// `resource_group_membership.group_id` -> `resource_group.id`, `ON DELETE RESTRICT`.
pub(crate) const FK_RGM_GROUP_ID: &str = "fk_rgm_group_id";
