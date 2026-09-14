// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-persistence:p1
//! Infrastructure storage layer - database persistence and `OData` mapping.

pub mod entity;
pub mod group_repo;
pub mod membership_repo;
pub mod migrations;
pub mod odata_mapper;
pub mod type_repo;

// -- Foreign key constraint names --
//
// `PostgreSQL` includes the constraint name in a foreign-key-violation error
// message; `SQLite` does not (it says only "FOREIGN KEY constraint failed").
// `GroupRepository::insert` and `MembershipRepository::insert` match on these
// names to tell which referenced row is missing, rather than reporting a
// generic database error for every FK violation. Kept in one place so the
// three names cannot drift out of sync with the DDL in
// `migrations/m20260306_000001_initial.rs`, which names each constraint
// again in both its Postgres and `SQLite` branches.

/// `resource_group.parent_id` -> `resource_group.id`, `ON DELETE RESTRICT`.
pub(crate) const FK_RESOURCE_GROUP_PARENT: &str = "fk_resource_group_parent";

/// `resource_group.gts_type_id` -> `gts_type.id`, `ON DELETE RESTRICT`.
///
/// `GroupRepository::insert` deliberately does not match on this one -- see
/// the comment on `map_insert_error` for why -- so outside of tests nothing
/// currently reads it; `#[cfg(test)]` keeps it from being reported as dead
/// code while it is still named here as the third leg of this table's FK
/// picture, alongside the other two.
#[cfg(test)]
pub(crate) const FK_RG_GTS_TYPE: &str = "fk_rg_gts_type";

/// `resource_group_membership.group_id` -> `resource_group.id`, `ON DELETE RESTRICT`.
pub(crate) const FK_RGM_GROUP_ID: &str = "fk_rgm_group_id";
