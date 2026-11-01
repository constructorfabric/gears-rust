// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-traits:p1
use std::collections::HashMap;

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::storage::entity::{
    resource_group as rg_entity, resource_group_membership as membership_entity,
};

/// One row of a hierarchy read: the group entity plus its depth relative to
/// the reference group (`>= 0` for descendants, `<= 0` for ancestors).
#[derive(Debug, Clone)]
pub struct HierarchyRow {
    pub model: rg_entity::Model,
    pub depth: i32,
}

/// Type restriction of a hierarchy read, expressed in Registry References.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyTypeFilter {
    /// No `type` restriction in the query.
    None,
    /// Only groups of this type.
    Eq(Uuid),
    /// The query named a type that the Types Registry does not know: no group
    /// can match, the page is empty.
    NoMatch,
}

impl HierarchyTypeFilter {
    /// Whether a group of type `gts_type_id` passes the filter.
    #[must_use]
    pub fn matches(self, gts_type_id: Uuid) -> bool {
        match self {
            Self::None => true,
            Self::Eq(wanted) => wanted == gts_type_id,
            Self::NoMatch => false,
        }
    }
}

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait GroupRepositoryTrait: Send + Sync + 'static {
    // -- Read operations --

    /// Read a group row within the caller's scope (`SecureORM` tenant filter).
    async fn find_scoped_model<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<rg_entity::Model>, DomainError>;

    async fn find_model_by_id<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<rg_entity::Model>, DomainError>;

    /// Same read, taking a row lock on the group for the rest of the
    /// transaction.
    ///
    /// For the paths that decide something from rows *related* to this group
    /// -- its children, its memberships -- and then write based on that
    /// decision. Serializing those writers on the group row is what keeps the
    /// decision true, and is why such a path does not need SERIALIZABLE.
    ///
    /// Backends without row locks (`SQLite`) ignore the clause; they are
    /// serializable regardless, so the guarantee is unchanged.
    async fn find_model_by_id_for_update<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<rg_entity::Model>, DomainError>;

    /// Return the id of *any* existing tenant root group
    /// (`parent_id IS NULL AND is_tenant`), or `None` when there is none.
    /// Used to enforce tenant-root uniqueness
    /// (`cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`); the
    /// `uq_rg_tenant_root` unique index is the backstop against races.
    async fn find_tenant_root_id<C: DBRunner>(&self, db: &C) -> Result<Option<Uuid>, DomainError>;

    /// `resolved` maps every type path used in the `type` filter to its
    /// Registry Reference.
    async fn list_groups<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        query: &ODataQuery,
        resolved: &HashMap<String, Uuid>,
    ) -> Result<Page<rg_entity::Model>, DomainError>;

    async fn get_descendants<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
        type_filter: HierarchyTypeFilter,
    ) -> Result<Page<HierarchyRow>, DomainError>;

    async fn get_ancestors<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        query: &ODataQuery,
        type_filter: HierarchyTypeFilter,
    ) -> Result<Page<HierarchyRow>, DomainError>;

    // -- Write operations --

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        gts_type_id: Uuid,
        is_tenant: bool,
        name: &str,
        metadata: Option<&serde_json::Value>,
        tenant_id: Uuid,
    ) -> Result<rg_entity::Model, DomainError>;

    /// Apply the update and return the number of rows it touched.
    ///
    /// Deliberately not the updated row: `update_many` reports a row count,
    /// so returning the model meant reading it straight back, and both
    /// callers threw that model away and read again for themselves. Every
    /// value the row now holds was supplied by the caller, so a caller that
    /// wants the updated entity can assemble it without asking (RG-08).
    ///
    /// The group's type is immutable, so the update carries no type.
    ///
    /// `rows_affected` is `0` when `id` doesn't match any row; what to do
    /// about that is the caller's call, not this method's -- both current
    /// callers turn it into not-found.
    async fn update<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        name: &str,
        metadata: Option<&serde_json::Value>,
    ) -> Result<u64, DomainError>;

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

    /// Deepest descendant of `group_id` relative to it, or `0` when it has
    /// none. A single `MAX(depth)` aggregate over the closure table, for
    /// callers -- the move path's depth-limit check -- that need only the
    /// scalar; see [`Self::get_descendant_ids_with_depth`] for callers
    /// (force delete) that need the row set itself.
    async fn get_max_descendant_depth<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
    ) -> Result<i32, DomainError>;

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
}

#[async_trait]
pub trait MembershipRepositoryTrait: Send + Sync + 'static {
    async fn list_memberships<C: DBRunner>(
        &self,
        db: &C,
        query: &ODataQuery,
        resolved: &HashMap<String, Uuid>,
    ) -> Result<Page<membership_entity::Model>, DomainError>;

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: Uuid,
        resource_id: &str,
    ) -> Result<membership_entity::Model, DomainError>;

    async fn delete<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: Uuid,
        resource_id: &str,
    ) -> Result<u64, DomainError>;

    async fn find_by_composite_key<C: DBRunner>(
        &self,
        db: &C,
        group_id: Uuid,
        gts_type_id: Uuid,
        resource_id: &str,
    ) -> Result<Option<membership_entity::Model>, DomainError>;

    /// Whether `(gts_type_id, resource_id)` already has a membership owned by
    /// a tenant other than `tenant_id`.
    ///
    /// An existence check, not a count and not a fetch: one statement,
    /// `LIMIT 1`, no rows carried back into the process. The question the
    /// caller asks is "is this resource already someone else's", and the
    /// first conflicting row answers it -- a resource in a hundred groups
    /// costs the same as one in two.
    ///
    /// The inner subquery is deliberately unscoped and this method builds
    /// `system_scope()` itself: it is an integrity read, and it must see
    /// every membership of the pair regardless of the caller's view.
    /// `resource_group_membership` declares no scope columns, so a
    /// constrained `AccessScope` would not merely narrow this read -- every
    /// constraint fails to resolve a column and the whole condition compiles
    /// to `WHERE false` (`cond.rs`, `build_constraint_condition`), which
    /// would report every resource compatible with every tenant.
    ///
    /// This read and the membership insert it gates must share one
    /// `SERIALIZABLE` transaction. It is a predicate read followed by a
    /// write into that same predicate: two first memberships from different
    /// tenants each see no conflict and both commit otherwise (RG-01). At
    /// `SERIALIZABLE` that is the write skew SSI cancels, and the retry
    /// already wrapping the transaction re-runs it against the winner.
    async fn has_membership_in_other_tenant<C: DBRunner>(
        &self,
        db: &C,
        gts_type_id: Uuid,
        resource_id: &str,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError>;
}
