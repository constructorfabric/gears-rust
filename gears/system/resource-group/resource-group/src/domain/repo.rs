// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-traits:p1
use async_trait::async_trait;
use resource_group_sdk::models::{ResourceGroup, ResourceGroupMembership, ResourceGroupWithDepth};
use toolkit_db::secure::{DBRunner, DbTx};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Stored resource types selected for removal of a membership.
#[derive(Debug, Clone, Copy)]
#[domain_model]
pub enum MemberTypes<'a> {
    Exact(&'a str),
    Family(&'a types_registry_sdk::GtsTypeId),
}

/// Storage key of a GTS type in RG's local identity dictionary (`gts_type`).
///
/// Pass this key from [`GroupRepositoryTrait::ensure_type_key`] to the
/// insert in the same transaction. Types Registry owns admission and
/// lifecycle for the corresponding GTS identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[domain_model]
pub struct GtsTypeKey(pub(crate) i16);

/// A stored group with its type key resolved to the GTS type identifier.
#[derive(Clone, Debug, PartialEq)]
#[domain_model]
pub struct GroupRecord {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    /// The group's GTS type identifier (immutable).
    pub code: String,
    pub name: String,
    pub metadata: Option<serde_json::Value>,
    pub tenant_id: Uuid,
}

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

    /// Find the stored group by ID.
    ///
    /// Reads under the system scope, with no caller scope applied. Never use
    /// it as an authorization gate, use `find_model_by_id_scoped` for that.
    async fn find_model_by_id<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<GroupRecord>, DomainError>;

    /// The tenant that owns a group, read under the system scope.
    ///
    /// Fills the PDP request of a single-group operation; never use it as an
    /// authorization gate.
    async fn find_tenant_id<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
    ) -> Result<Option<Uuid>, DomainError>;

    /// Find the stored group by ID, visible under `scope`.
    async fn find_model_by_id_scoped<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<GroupRecord>, DomainError>;

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
    ) -> Result<Option<GroupRecord>, DomainError>;

    /// Return the id of *any* existing root group (`parent_id` IS NULL) whose
    /// GTS type identifier starts with any of the given prefixes, or `None`
    /// when no such root exists (or `type_prefixes` is empty). Used to enforce tenant-root uniqueness
    /// (`cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`).
    ///
    /// Expected to be called inside a `SERIALIZABLE` transaction so the
    /// uniqueness check is race-free against concurrent root creations.
    async fn find_root_id_with_type_prefixes<C: DBRunner>(
        &self,
        db: &C,
        type_prefixes: &[String],
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

    /// The dictionary key for the GTS type `code`, created on first use.
    ///
    /// Call inside the write transaction after registry admission and other
    /// checks. The key row rolls back with the referencing write, but PostgreSQL
    /// identity values are not reclaimed. Concurrent first uses share a key,
    /// with a typed serialization error triggering retry under `SERIALIZABLE`.
    /// Reads, filters and deletes only look up keys.
    async fn ensure_type_key<C: DBRunner>(
        &self,
        db: &C,
        code: &str,
    ) -> Result<GtsTypeKey, DomainError>;

    async fn insert<C: DBRunner>(
        &self,
        db: &C,
        id: Uuid,
        parent_id: Option<Uuid>,
        type_key: GtsTypeKey,
        name: &str,
        metadata: Option<&serde_json::Value>,
        tenant_id: Uuid,
    ) -> Result<(), DomainError>;

    /// Apply the update and return the number of rows it touched.
    ///
    /// Deliberately not the updated row: `update_many` reports a row count,
    /// so returning the model meant reading it straight back, and both
    /// callers threw that model away and read again for themselves. Every
    /// value the row now holds was supplied by the caller, so a caller that
    /// wants the updated entity can assemble it without asking (RG-08).
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
    /// List memberships. `scope` is the caller's scope and is applied through
    /// the owning group; an unconstrained scope means no tenant filtering.
    async fn list_memberships<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, DomainError>;

    /// Insert the membership and its type key in the same transaction. Call
    /// after validation because PostgreSQL identity values survive rollback.
    /// Duplicate errors use the GTS type identifier.
    async fn insert(
        &self,
        tx: &DbTx<'_>,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<(), DomainError>;

    /// Delete matching memberships. `scope` is the caller's scope and is applied
    /// through the owning group; an unconstrained scope means no tenant
    /// filtering. Stored types are matched through the dictionary without
    /// creating a key or consulting the registry: an unknown type deletes
    /// nothing.
    async fn delete<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        group_id: Uuid,
        types: MemberTypes<'_>,
        resource_id: &str,
    ) -> Result<u64, DomainError>;

    /// Whether `(resource_type, resource_id)` already has a membership owned by
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
    ///
    /// `resource_type` is matched through the identity dictionary in the same
    /// statement, without creating a key: a type with no key has no
    /// memberships anywhere.
    async fn has_membership_in_other_tenant<C: DBRunner>(
        &self,
        db: &C,
        resource_type: &str,
        resource_id: &str,
        tenant_id: Uuid,
    ) -> Result<bool, DomainError>;
}
