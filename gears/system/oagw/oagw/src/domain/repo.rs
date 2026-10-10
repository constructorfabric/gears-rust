use crate::domain::model::{ListQuery, Route, Upstream};
use async_trait::async_trait;
use toolkit_macros::domain_model;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by repository operations.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    /// The requested entity does not exist (for the tenant).
    #[error("{entity} not found: {id}")]
    NotFound {
        /// Kind of entity looked up (for example `upstream` or `route`).
        entity: &'static str,
        /// ID that was not found.
        id: Uuid,
    },
    /// A uniqueness constraint was violated.
    #[error("{entity} conflict on {resource}: {detail}")]
    Conflict {
        /// Kind of entity involved (for example `upstream` or `route`).
        entity: &'static str,
        /// Name of the conflicting resource (for example the alias).
        resource: String,
        /// Human-readable description of the conflict.
        detail: String,
    },
    /// Unexpected storage failure.
    #[error("internal: {0}")]
    Internal(String),
}

/// The tenant and ID of a stored upstream or route.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowKey {
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// Row ID.
    pub id: Uuid,
}

/// Whether a read loads tags. Tags are discovery metadata the proxy never
/// reads, so proxy resolution skips them; a skipped read returns no tags.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tags {
    /// Load the tags.
    Load,
    /// Skip the tags; the read returns none.
    Skip,
}

// ---------------------------------------------------------------------------
// Repository traits
// ---------------------------------------------------------------------------

/// Repository trait for upstream persistence.
///
/// Callers must pass tags canonical (de-duplicated and sorted, as the Control
/// Plane does); every backend returns them in that order. Plugins keep their
/// position order. `managed_by` is written on create and never changed.
#[async_trait]
pub trait UpstreamRepository: Send + Sync {
    /// Insert a new upstream. Returns Conflict if the ID already exists or the
    /// alias is taken for the tenant.
    async fn create(&self, upstream: Upstream) -> Result<Upstream, RepositoryError>;

    /// Get an upstream by id, scoped to a tenant.
    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Upstream, RepositoryError>;

    /// List upstreams for a tenant, ordered by id ascending, with pagination.
    async fn list(
        &self,
        tenant_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, RepositoryError>;

    /// Update an existing upstream. Preserves id, tenant_id and managed_by,
    /// and returns the stored values of all three.
    async fn update(&self, upstream: Upstream) -> Result<Upstream, RepositoryError>;

    /// Delete an upstream. Returns NotFound if it does not exist. Its routes:
    /// see [`RouteRepository::delete_by_upstream`].
    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError>;

    /// List upstreams with the given alias restricted to a set of tenant IDs.
    /// Used for budget validation where only descendants of a particular
    /// ancestor are relevant.
    ///
    /// The provided `alias` must already be normalized (lowercase) as
    /// implementations perform case-sensitive exact-string matching.
    /// Result order is unspecified; an empty `tenant_ids` returns no rows.
    async fn list_by_alias_for_tenants(
        &self,
        alias: &str,
        tenant_ids: &std::collections::HashSet<Uuid>,
        tags: Tags,
    ) -> Result<Vec<Upstream>, RepositoryError>;

    /// Keys of every registry-managed upstream, across all tenants, in key
    /// order. Only the startup registry reconcile calls it.
    async fn list_registry_keys(&self) -> Result<Vec<RowKey>, RepositoryError>;
}

/// Repository trait for route persistence.
///
/// Callers must pass tags and HTTP methods canonical (de-duplicated and in
/// canonical order, as the Control Plane does); every backend returns them in
/// that order. Plugins keep their position order. `managed_by` is written on
/// create and never changed.
#[async_trait]
pub trait RouteRepository: Send + Sync {
    /// Insert a new route. Returns Conflict if the ID already exists.
    async fn create(&self, route: Route) -> Result<Route, RepositoryError>;

    /// Get a route by id, scoped to a tenant.
    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Route, RepositoryError>;

    /// List routes for a tenant, ordered by id ascending, with pagination and
    /// an optional upstream filter.
    async fn list(
        &self,
        tenant_id: Uuid,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, RepositoryError>;

    /// Find the best enabled HTTP route for `method` and `path` among routes
    /// attached to any of `upstream_ids` and owned by a tenant in `tenant_chain`.
    ///
    /// `upstream_ids` is ordered by preference: the selected upstream first,
    /// then ancestor upstreams closest-first. Selection follows
    /// [`crate::domain::route_matching::select_route`]. Unknown methods never match.
    async fn find_matching_in_tenants(
        &self,
        tenant_chain: &[Uuid],
        upstream_ids: &[Uuid],
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<Route, RepositoryError>;

    /// Update an existing route, matched on `(id, tenant_id)`. Returns NotFound
    /// otherwise. The stored `upstream_id` and `managed_by` are never changed
    /// and are returned as stored.
    async fn update(&self, route: Route) -> Result<Route, RepositoryError>;

    /// Delete a route.
    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError>;

    /// Delete the tenant's routes left on an upstream that
    /// [`UpstreamRepository::delete`] has just removed; call it only after
    /// that delete. On the database it is a no-op: the upstream delete
    /// cascaded, and the foreign key rejects any later route on the upstream.
    /// In memory it removes the routes present now; a route create that
    /// passed its upstream check before the delete can still land afterwards
    /// and stay orphaned.
    async fn delete_by_upstream(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
    ) -> Result<(), RepositoryError>;

    /// Keys of every registry-managed route, across all tenants, in key
    /// order. Only the startup registry reconcile calls it.
    async fn list_registry_keys(&self) -> Result<Vec<RowKey>, RepositoryError>;
}
