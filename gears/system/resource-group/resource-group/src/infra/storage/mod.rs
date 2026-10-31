// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-persistence:p1
//! Infrastructure storage layer - database persistence and `OData` mapping.

pub mod entity;
pub mod group_repo;
pub mod membership_repo;
pub mod migrations;
pub(crate) mod odata_filter;
pub mod odata_mapper;

// -- Constraint names --
//
// `PostgreSQL` includes the constraint name in a foreign-key- or
// unique-violation error message; `SQLite` does not (it says only
// "FOREIGN KEY constraint failed", or names the columns of a unique
// violation). `GroupRepository::insert` and `MembershipRepository::insert`
// match on these names to tell which referenced row is missing / which rule
// was broken, rather than reporting a generic database error. Kept in one
// place so the names cannot drift out of sync with the DDL in
// `migrations/m20260306_000001_initial.rs` and
// `migrations/m20261012_000001_types_in_types_registry.rs`.

/// `resource_group.parent_id` -> `resource_group.id`, `ON DELETE RESTRICT`.
pub(crate) const FK_RESOURCE_GROUP_PARENT: &str = "fk_resource_group_parent";

/// `resource_group_membership.group_id` -> `resource_group.id`, `ON DELETE RESTRICT`.
pub(crate) const FK_RGM_GROUP_ID: &str = "fk_rgm_group_id";

/// Partial unique index allowing at most one tenant root
/// (`parent_id IS NULL AND is_tenant`).
pub(crate) const UQ_RG_TENANT_ROOT: &str = "uq_rg_tenant_root";
