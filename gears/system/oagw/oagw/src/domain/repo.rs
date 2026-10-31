use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::domain::model::{HttpMethod, ListQuery, Route, Upstream};
use async_trait::async_trait;
use toolkit_macros::domain_model;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by repository operations. `Clone`, so the requests
/// waiting on one proxy-cache load all receive its error (ADR-0020).
#[domain_model]
#[derive(Debug, Clone, thiserror::Error)]
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
    /// A route create or update would overlap the stored route `0.id` (the
    /// rule of [`RouteRepository::create`]); nothing was written.
    #[error("route overlaps route {} on method {:?}", .0.id, .0.method)]
    RouteOverlap(RouteOverlap),
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

/// A stored route that a route being written would overlap: it serves
/// `method` on the same tenant, upstream, path and priority, and is enabled.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteOverlap {
    /// The stored route's ID.
    pub id: Uuid,
    /// One method both routes allow.
    pub method: HttpMethod,
}

impl RouteOverlap {
    /// The overlap a write of route `writing` reports, of every (stored
    /// route, shared method) that overlaps it: the lowest route ID,
    /// then the first shared method in canonical order (the conflict a scan
    /// of the upstream's routes in ID order finds). The route itself is not
    /// an overlap.
    #[must_use]
    pub fn reported(
        overlaps: impl IntoIterator<Item = RouteOverlap>,
        writing: Uuid,
    ) -> Option<RouteOverlap> {
        overlaps
            .into_iter()
            .filter(|o| o.id != writing)
            .min_by_key(|o| (o.id, o.method))
    }
}

/// What [`RegistryRowRepository::delete_upstream_if_unused`] did.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// The upstream had no routes and is gone.
    Deleted,
    /// Routes still use the upstream; nothing was deleted.
    InUse,
    /// No such upstream in the tenant.
    NotFound,
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
    ///
    /// An enabled HTTP route must not overlap a stored route: an enabled
    /// HTTP route of the same tenant and upstream with a byte-equal path, an
    /// equal priority and a shared method. The check runs in the same atomic
    /// step as the insert: an overlap returns
    /// [`RepositoryError::RouteOverlap`] ([`RouteOverlap::reported`]), and
    /// of two concurrent overlapping writes only one succeeds. A database
    /// locks the upstream row for the check and answers an upstream missing
    /// from the tenant with NotFound for `upstream`; the in-memory repository
    /// does not check the upstream (the Control Plane reads it first).
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

    /// The enabled HTTP routes of each upstream in `upstreams`, in full but
    /// without tags: what the proxy configuration cache stores per upstream
    /// (ADR-0020). One entry per distinct key, in the order the keys first
    /// appear, with its routes in ID order; the entry is empty when the
    /// key's tenant has no such upstream or the upstream has no enabled HTTP
    /// route. A route without a method can never match and is left out.
    async fn list_proxy_routes(
        &self,
        upstreams: &[RowKey],
    ) -> Result<Vec<(RowKey, Vec<Route>)>, RepositoryError>;

    /// Update an existing route, matched on `(id, tenant_id)`. Returns NotFound
    /// otherwise. The stored `managed_by` is never changed and is returned as
    /// stored. `upstream_id` is written as given: the Control Plane passes the
    /// stored one, except when the registry reconcile moves a registry route
    /// to another upstream. A database rejects an upstream missing from the
    /// tenant with NotFound for `upstream`; the in-memory repository does not
    /// check it. An enabled HTTP route is checked for overlaps on the upstream
    /// it is written to (the target of a move), leaving itself out,
    /// atomically with the write, as in [`Self::create`].
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
}

/// The proxy path's configuration reads, without tags (ADR-0020). On a
/// database with the proxy configuration cache enabled they are served from
/// the per-replica cache; the uncached path reads the repositories.
#[async_trait]
pub trait ProxyConfigReader: Send + Sync {
    /// Every upstream with `alias` owned by a tenant in `chain`, including
    /// disabled ones, in unspecified order; empty when there is none. The
    /// `alias` must already be normalized, as for
    /// [`UpstreamRepository::list_by_alias_for_tenants`].
    async fn upstreams_by_alias(
        &self,
        alias: &str,
        chain: &[Uuid],
    ) -> Result<Arc<[Upstream]>, RepositoryError>;

    /// The enabled HTTP routes of each upstream, in full, as
    /// [`RouteRepository::list_proxy_routes`] returns them: one entry per key,
    /// in the order of `upstreams`.
    async fn routes_of(&self, upstreams: &[RowKey]) -> Result<Vec<Arc<[Route]>>, RepositoryError>;
}

/// The registry reconcile's own access to registry-managed rows: reads
/// across every tenant, and the delete of a dropped registry upstream
/// (ADR-0018 *Provisioning from the Types Registry*). Kept off [`UpstreamRepository`] and
/// [`RouteRepository`], which the Control Plane behind the Management API
/// and the SDK holds, so no request path can read across tenants or delete
/// through it; only the gear's registry provisioner holds it.
#[async_trait]
pub trait RegistryRowRepository: Send + Sync {
    /// Every registry-managed upstream in full, across all tenants, in key
    /// order.
    async fn list_upstreams(&self) -> Result<Vec<Upstream>, RepositoryError>;

    /// Every registry-managed route in full, across all tenants, in key
    /// order.
    async fn list_routes(&self) -> Result<Vec<Route>, RepositoryError>;

    /// Delete the tenant's registry-managed upstream `id` only if no route
    /// uses it, as one step against concurrent route creates: on a database
    /// the upstream row is locked, the routes checked and the row deleted in
    /// one transaction, so a route insert either committed first (`InUse`)
    /// or fails its foreign key. In memory the routes are checked first; a
    /// route create already past its upstream check can still land after the
    /// delete, as for [`RouteRepository::delete_by_upstream`]. An upstream
    /// the Management API created answers `NotFound` and is left alone.
    async fn delete_upstream_if_unused(
        &self,
        tenant_id: Uuid,
        id: Uuid,
    ) -> Result<DeleteOutcome, RepositoryError>;
}

/// What the registry reconcile gate reads from the registry-state row.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryState {
    /// Highest deployment generation that claimed the registry.
    pub generation: Option<u64>,
    /// Hex SHA-256 of the registry content of the last clean reconcile.
    pub content_hash: Option<String>,
}

/// The installation-wide registry-state row that gates the registry
/// reconcile on a database: generation, content hash and lease
/// (ADR-0018 *Provisioning from the Types Registry*). Lease times come from the store's own
/// clock (the database clock), never the caller's. Every write is one
/// conditional statement.
#[async_trait]
pub trait RegistryStateStore: Send + Sync {
    /// The stored generation and content hash.
    async fn read(&self) -> Result<RegistryState, RepositoryError>;

    /// Raise the stored generation to `generation` if it is lower or unset.
    /// `true` when it changed.
    async fn claim(&self, generation: u64) -> Result<bool, RepositoryError>;

    /// Take the lease for `owner` until `ttl` from now when it is free,
    /// expired or already `owner`'s, and, with a `generation`, only when the
    /// stored one is not newer, claiming it in the same statement. Taking it
    /// clears the recorded content hash: until the run finishes clean, the
    /// rows match no content. `true` when taken.
    async fn try_acquire(
        &self,
        owner: Uuid,
        ttl: Duration,
        generation: Option<u64>,
    ) -> Result<bool, RepositoryError>;

    /// Extend `owner`'s lease to `ttl` from now. `false` when `owner` no
    /// longer holds it.
    async fn renew(&self, owner: Uuid, ttl: Duration) -> Result<bool, RepositoryError>;

    /// Release `owner`'s lease, recording `content_hash` (and when) if given.
    /// `false`, and nothing recorded, when `owner` no longer holds it.
    async fn finish(
        &self,
        owner: Uuid,
        content_hash: Option<&str>,
    ) -> Result<bool, RepositoryError>;
}

// ---------------------------------------------------------------------------
// Waits after a write
// ---------------------------------------------------------------------------

/// A repository may wait after a write until the write's effect has reached
/// other replicas (the proxy configuration change log, ADR-0020 *Keeping the
/// Cache Fresh*), so a caller that writes and then calls another replica
/// sees it there. Writes with no such caller, such as a registry reconcile
/// run, can run in [`one_wait_at_the_end`] instead.
type WaitAfterWrite = futures_util::future::BoxFuture<'static, ()>;

tokio::task_local! {
    /// The wait of the latest write in the enclosing [`one_wait_at_the_end`].
    static DEFERRED_WAIT: Arc<parking_lot::Mutex<Option<WaitAfterWrite>>>;
}

/// Run `work`, letting each repository write it makes return without its
/// wait after the write, then wait once, for the latest write's. That covers
/// every earlier write: the publisher acks appends in the order it was handed
/// the changes.
///
/// The scope is task-local: a write `work` spawns onto another task waits
/// as usual.
pub(crate) async fn one_wait_at_the_end<F: Future>(work: F) -> F::Output {
    let deferred = Arc::new(parking_lot::Mutex::new(None));
    let output = DEFERRED_WAIT.scope(Arc::clone(&deferred), work).await;
    let wait = deferred.lock().take();
    if let Some(wait) = wait {
        wait.await;
    }
    output
}

/// Hand a write's `wait` to the enclosing [`one_wait_at_the_end`], replacing
/// the previous write's. Outside of one, `wait` comes back for the caller to
/// await.
///
/// # Errors
///
/// `wait`, back, when no [`one_wait_at_the_end`] encloses the call.
#[cfg_attr(
    not(feature = "cluster"),
    expect(
        dead_code,
        reason = "only the proxy configuration cache's cluster change log waits after a write"
    )
)]
pub(crate) fn defer_wait(wait: WaitAfterWrite) -> Result<(), WaitAfterWrite> {
    let mut wait = Some(wait);
    let _ = DEFERRED_WAIT.try_with(|deferred| *deferred.lock() = wait.take());
    wait.map_or(Ok(()), Err)
}
