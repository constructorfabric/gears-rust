// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-traits:p1
use async_trait::async_trait;
use resource_group_sdk::models::{
    ResourceGroup, ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth,
};
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::storage::entity::{
    gts_type, resource_group as rg_entity, resource_group_membership as membership_entity,
};

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait GroupRepositoryTrait: Send + Sync + 'static {
    // -- Read operations --

    async fn find_by_id<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<ResourceGroup>, DomainError>;

    async fn find_model_by_id<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<rg_entity::Model>, DomainError>;

    /// Return the id of *any* existing root group (`parent_id` IS NULL) whose
    /// `gts_type.schema_id` starts with the given prefix, or `None` when no
    /// such root exists. Used to enforce tenant-root uniqueness
    /// (`cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`).
    ///
    /// Expected to be called inside a `SERIALIZABLE` transaction so the
    /// uniqueness check is race-free against concurrent root creations.
    async fn find_root_id_with_type_prefix<C: DBRunner>(
        &self,
        db: &C,
        type_prefix: &str,
    ) -> Result<Option<Uuid>, DomainError>;

    async fn list_groups<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, DomainError>;

    async fn get_descendants<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError>;

    async fn get_ancestors<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, DomainError>;

    // -- Write operations --

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: i16,
        name: &str,
        metadata: Option<&serde_json::Value>,
        tenant_id: Uuid,
    ) -> Result<rg_entity::Model, DomainError>;

    /// Apply the update and return nothing.
    ///
    /// Deliberately not the updated row: `update_many` reports a row count,
    /// so returning the model meant reading it straight back, and both
    /// callers threw that model away and read again for themselves. Every
    /// value the row now holds was supplied by the caller, so a caller that
    /// wants the updated entity can assemble it without asking (RG-08).
    async fn update<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: i16,
        name: &str,
        metadata: Option<&serde_json::Value>,
    ) -> Result<(), DomainError>;

    async fn delete_by_id<C: DBRunner>(&self, db: &C, id: Uuid) -> Result<(), DomainError>;

    // -- Closure table operations --

    async fn insert_closure_self_row<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError>;

    /// Returns the number of closure rows written, as the database counted
    /// them: the statement is an `INSERT ... SELECT`, so the rows never reach
    /// this process and nothing here can derive the figure.
    async fn insert_ancestor_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        child_id: Uuid,
        parent_id: Uuid,
    ) -> Result<u64, DomainError>;

    /// Delete every group in `group_ids` in one statement per bind-parameter
    /// chunk, not one per group (RG-10).
    async fn delete_by_id_many<C: DBRunner>(&self, db: &C, ids: &[Uuid])
    -> Result<(), DomainError>;

    /// Delete all memberships for every group in `group_ids` in one statement
    /// per bind-parameter chunk, not one per group (RG-10).
    async fn delete_memberships_many<C: DBRunner>(
        &self,
        db: &C,
        group_ids: &[Uuid],
    ) -> Result<(), DomainError>;

    /// Delete all closure rows (both as ancestor and as descendant) for
    /// every group in `group_ids`, in 2 statements per bind-parameter chunk
    /// rather than 2 per group (RG-10) — safe since the whole batch is
    /// deleted together with no ordering to preserve.
    async fn delete_all_closure_rows_many<C: DBRunner>(
        &self,
        db: &C,
        group_ids: &[Uuid],
    ) -> Result<(), DomainError>;

    /// Every descendant of `group_id` (the self-row excluded) with its depth
    /// relative to `group_id`, so callers needing that depth (RG-05's depth
    /// check, RG-10's depth-level batching) don't re-query for it.
    async fn get_descendant_ids_with_depth<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<Vec<(Uuid, i32)>, DomainError>;

    async fn get_depth<C: DBRunner>(&self, db: &C, group_id: Uuid) -> Result<i32, DomainError>;

    async fn count_children<C: DBRunner>(
        &self,
        db: &C,
        parent_id: Uuid,
    ) -> Result<u64, DomainError>;

    async fn is_descendant<C: DBRunner>(
        &self,
        db: &C,
        potential_ancestor: Uuid,
        potential_descendant: Uuid,
    ) -> Result<bool, DomainError>;

    async fn delete_ancestor_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        keep_self: bool,
    ) -> Result<(), DomainError>;

    async fn delete_all_closure_rows<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<(), DomainError>;

    /// Returns the number of closure rows written; see
    /// [`Self::insert_ancestor_closure_rows`] for why the caller cannot
    /// compute it.
    async fn rebuild_subtree_closure<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        new_parent_id: Option<Uuid>,
    ) -> Result<u64, DomainError>;

    async fn has_memberships<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<bool, DomainError>;

    async fn resolve_type_paths_batch<C: DBRunner>(
        &self,
        db: &C,
        type_ids: &[i16],
    ) -> Result<std::collections::HashMap<i16, String>, DomainError>;
}

#[async_trait]
pub trait TypeRepositoryTrait: Send + Sync + 'static {
    async fn find_by_code<C: DBRunner>(
        &self,
        db: &C,
        code: &str,
    ) -> Result<Option<ResourceGroupType>, DomainError>;

    /// Same lookup as `find_by_code`, but also returns the surrogate
    /// SMALLINT id, letting a caller that needs both avoid a separate
    /// `resolve_id` call for the same code (RG-11).
    /// The row and the type built from it, in one query.
    ///
    /// Returns the `gts_type` row itself, not just its surrogate id: the
    /// update path needs `created_at` to assemble its answer without reading
    /// the row back after the write, and this query already held it.
    async fn find_by_code_with_id<C: DBRunner>(
        &self,
        db: &C,
        code: &str,
    ) -> Result<Option<(gts_type::Model, ResourceGroupType)>, DomainError>;

    async fn load_full_type_by_id<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<ResourceGroupType, DomainError>;

    async fn load_full_type<C: DBRunner>(
        &self,
        db: &C,
        type_model: &gts_type::Model,
    ) -> Result<ResourceGroupType, DomainError>;

    async fn resolve_id<C: DBRunner>(&self, db: &C, code: &str)
    -> Result<Option<i16>, DomainError>;

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        schema_id: &str,
        metadata_schema: Option<&serde_json::Value>,
    ) -> Result<gts_type::Model, DomainError>;

    async fn insert_allowed_parent_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        parent_ids: &[i16],
    ) -> Result<(), DomainError>;

    async fn insert_allowed_membership_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        membership_ids: &[i16],
    ) -> Result<(), DomainError>;

    async fn delete_allowed_parent_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<(), DomainError>;

    async fn delete_allowed_membership_types<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<(), DomainError>;

    /// Update `metadata_schema` and `updated_at` on one `gts_type` row.
    ///
    /// Returns the `updated_at` it wrote rather than the row: every other
    /// column is either the key it was addressed by or immutable, so the
    /// caller can assemble the new row from what it already holds. Reading
    /// it back cost a second `gts_type` SELECT per update (RG-08).
    async fn update_type<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
        metadata_schema: Option<&serde_json::Value>,
    ) -> Result<time::OffsetDateTime, DomainError>;

    async fn delete_by_id<C: DBRunner>(&self, db: &C, type_id: i16) -> Result<(), DomainError>;

    async fn count_groups_of_type<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<u64, DomainError>;

    /// Batch replacement for a removed-allowed-parent-types sweep: resolves
    /// every candidate parent-type path to its surrogate id and finds every
    /// group of `child_type_id` whose direct parent is of one of those
    /// types, in a small constant number of queries regardless of how many
    /// paths are checked (was one `resolve_id` + one single-parent
    /// violation lookup per candidate -- slope 2.0, N+1 audit finding (b)
    /// -- see `check_hierarchy_safety`).
    ///
    /// A `parent_code` that doesn't currently resolve to any `gts_type` row
    /// is silently skipped (mirrors the pre-batch code's per-item `if let
    /// Some(parent_id) = ...` guard) -- reachable when the parent type was
    /// deleted out from under a still-referencing junction row (junction
    /// FKs declare `ON DELETE CASCADE`, but that cascade only fires when
    /// the connection actually enforces FK constraints; the test suite's
    /// `SQLite` connections don't turn that pragma on, see
    /// `type_update_hierarchy_check_skips_deleted_parent`). A type that no
    /// longer exists has no groups referencing it, so there's nothing to
    /// check either way. Returns `(parent_code, group_id, group_name)`
    /// triples so callers can attribute each violation back to the specific
    /// removed-parent path that caused it.
    async fn find_groups_violating_removed_parents<C: DBRunner>(
        &self,
        db: &C,
        child_type_id: i16,
        parent_codes: &[String],
    ) -> Result<Vec<(String, Uuid, String)>, DomainError>;

    async fn find_root_groups_of_type<C: DBRunner>(
        &self,
        db: &C,
        type_id: i16,
    ) -> Result<Vec<(Uuid, String)>, DomainError>;

    async fn list_types<C: DBRunner>(
        &self,
        db: &C,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, DomainError>;

    async fn resolve_ids<C: DBRunner>(
        &self,
        db: &C,
        codes: &[String],
    ) -> Result<Vec<i16>, DomainError>;
}

#[async_trait]
pub trait MembershipRepositoryTrait: Send + Sync + 'static {
    async fn list_memberships<C: DBRunner>(
        &self,
        db: &C,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, DomainError>;

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<membership_entity::Model, DomainError>;

    async fn delete<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<u64, DomainError>;

    async fn find_by_composite_key<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<Option<membership_entity::Model>, DomainError>;

    async fn get_existing_membership_tenant_ids<C: DBRunner>(
        &self,
        db: &C,
        gts_type_id: i16,
        resource_id: &str,
    ) -> Result<Vec<Uuid>, DomainError>;
}
