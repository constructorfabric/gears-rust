//! `DbRouteRepo`: `RouteRepository` on the `oagw_route*` tables.
//!
//! `get_by_id` and `list` read the route rows plus one batched query per child
//! table (HTTP match, methods, gRPC match, tags, plugins): 6 queries, no joins,
//! so a route is never dropped for a missing optional child.
//! `find_matching_in_tenants` loads the candidates that allow the request's
//! method in one join (routes ⋈ HTTP match ⋈ methods), selecting only the
//! columns route selection reads; the shared `route_matching::select_route`
//! picks the winner, and one more join loads the winner in full with its
//! path, methods and plugins (its tags only when requested): 2 queries (3 with
//! tags) at any hierarchy depth. The loaded winner is re-checked against the
//! request; a request that raced a write selects once more (2 more queries).
//! `list_proxy_routes` loads every enabled HTTP route of several upstreams in
//! full with that same join, without the winner filter: 1 query.
//! `get_by_id` and `list` read in one read-only snapshot transaction;
//! `find_matching_in_tenants` and `list_proxy_routes` run in none (ADR-0018
//! *Transactions*).
//! Creates and updates run in one READ COMMITTED transaction, retried on
//! contention, and replace child rows by delete-then-insert. An enabled HTTP
//! route first locks its upstream row (`upstream_lock`) and runs the overlap
//! query inside the transaction, so two overlapping writes on one upstream
//! serialize and the second sees the first. Deletes are one statement.
//! The registry reconcile's cross-tenant read lives in `registry_row_db_repo`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use async_trait::async_trait;
use sea_orm::ActiveValue::NotSet;
use sea_orm::{
    ColumnTrait, DbBackend, EntityTrait, FromQueryResult, IdenStatic, Iterable, JoinType, Order,
    QueryFilter, QuerySelect, RelationTrait, Select,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, ScopeError, SecureDeleteExt, SecureEntityExt, SecureUpdateExt,
    secure_insert, secure_insert_many,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use super::entity::{
    route, route_grpc_match, route_http_match, route_method, route_plugin, route_tag,
};
use super::error::{internal, invalid_stored, not_found, route_create, route_update};
use super::load::{Parent, by_parent, children, max_in_list, snapshot, update_one};
use super::mapper::{
    CandidateRow, MATCH_TYPE_HTTP, RouteChildRows, RouteChildren, http_candidate, method_text,
    parse_managed_by, parse_stored_method, route_from_rows, route_rows, sorted_tags,
};
#[cfg(all(test, feature = "integration"))]
use super::upstream_lock::{Hold, HoldBeforeCommit};
use super::upstream_lock::{lock_upstream, read_committed_for};
use crate::domain::model::{HttpMethod, ListQuery, Route};
use crate::domain::repo::{RepositoryError, RouteOverlap, RouteRepository, RowKey, Tags};
use crate::domain::route_matching::{parse_method, select_route};

const ENTITY: &str = "route";

/// Column prefix of the plugin binding in [`FullRouteRow`].
const PLUGIN_PREFIX: &str = "plugin_";

/// A route joined with its path, one of its methods and one of its plugin
/// bindings (`None`: no bindings).
#[derive(FromQueryResult)]
struct FullRouteRow {
    #[sea_orm(nested)]
    route: route::Model,
    path_prefix: String,
    method: String,
    #[sea_orm(nested(prefix = "plugin_"))]
    plugin: Option<route_plugin::Model>,
}

/// A stored route that overlaps a route being written, with one shared method.
#[derive(FromQueryResult)]
struct OverlapRow {
    id: Uuid,
    method: String,
}

/// Database-backed route repository.
pub(crate) struct DbRouteRepo {
    db: DBProvider<RepositoryError>,
    /// Holds a create's or update's transaction open before its commit.
    #[cfg(all(test, feature = "integration"))]
    hold: Hold,
}

impl DbRouteRepo {
    /// Create a repository over the given database provider.
    pub(crate) fn new(db: DBProvider<RepositoryError>) -> Self {
        Self {
            db,
            #[cfg(all(test, feature = "integration"))]
            hold: Hold::default(),
        }
    }

    /// Hold the next create or update transaction open after its last
    /// statement: it signals `locked` and commits once `release` fires.
    #[cfg(all(test, feature = "integration"))]
    pub(crate) fn hold_next_write(&self, hold: HoldBeforeCommit) {
        self.hold.set(hold);
    }
}

/// A failure inside a route write transaction. A failed statement keeps its
/// driver error until the retry helper has seen it, so contention (`SQLite`'s
/// busy writer, a deadlock) is retried; [`WriteTxError::into_repo`] then reads
/// it as the statement's repository error.
#[derive(Debug)]
enum WriteTxError {
    /// A statement failed.
    Statement(ScopeError, Statement),
    /// The transaction itself failed (begin, commit).
    Db(DbError),
    /// Decided inside the transaction: an overlap, a missing row.
    Repo(RepositoryError),
}

/// Which statement of a route write failed, for its error mapping.
#[derive(Debug, Clone, Copy)]
enum Statement {
    InsertRoute,
    UpdateRoute,
    Other,
}

impl From<DbError> for WriteTxError {
    fn from(e: DbError) -> Self {
        Self::Db(e)
    }
}

impl From<ScopeError> for WriteTxError {
    fn from(e: ScopeError) -> Self {
        Self::Statement(e, Statement::Other)
    }
}

impl WriteTxError {
    fn into_repo(self, route: &Route) -> RepositoryError {
        match self {
            Self::Statement(e, Statement::InsertRoute) => route_create(e, route),
            Self::Statement(e, Statement::UpdateRoute) => route_update(e, route),
            Self::Statement(e, Statement::Other) => internal(e),
            Self::Db(e) => e.into(),
            Self::Repo(e) => e,
        }
    }
}

/// The driver error of a [`WriteTxError`], for contention classification.
fn write_tx_db_err(e: &WriteTxError) -> Option<&sea_orm::DbErr> {
    match e {
        WriteTxError::Statement(ScopeError::Db(inner), _)
        | WriteTxError::Db(DbError::Sea(inner)) => Some(inner),
        _ => None,
    }
}

/// The single row of a one-to-one child, if present.
fn first<M>(rows: Option<Vec<M>>) -> Option<M> {
    rows.and_then(|rows| rows.into_iter().next())
}

/// Load the five child tables of `rows` (one query each) and build the
/// aggregates, in the order of `rows`.
pub(super) async fn assemble(
    runner: &impl DBRunner,
    rows: Vec<route::Model>,
) -> Result<Vec<Route>, RepositoryError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let parents: Vec<Parent> = rows.iter().map(|r| (r.id, r.tenant_id)).collect();
    let mut http = by_parent(
        children::<route_http_match::Entity>(runner, route_http_match::Column::RouteId, &parents)
            .await?,
        |m| m.route_id,
    );
    let mut methods = by_parent(
        children::<route_method::Entity>(runner, route_method::Column::RouteId, &parents).await?,
        |m| m.route_id,
    );
    let mut grpc = by_parent(
        children::<route_grpc_match::Entity>(runner, route_grpc_match::Column::RouteId, &parents)
            .await?,
        |m| m.route_id,
    );
    let mut tags = by_parent(
        children::<route_tag::Entity>(runner, route_tag::Column::RouteId, &parents).await?,
        |t| t.route_id,
    );
    let mut plugins = by_parent(
        children::<route_plugin::Entity>(runner, route_plugin::Column::RouteId, &parents).await?,
        |p| p.route_id,
    );
    rows.into_iter()
        .map(|row| {
            let id = row.id;
            route_from_rows(
                row,
                RouteChildren {
                    http_match: first(http.remove(&id)),
                    methods: methods.remove(&id).unwrap_or_default(),
                    grpc_match: first(grpc.remove(&id)),
                    tags: tags.remove(&id).unwrap_or_default(),
                    plugins: plugins.remove(&id).unwrap_or_default(),
                },
            )
        })
        .collect()
}

/// Insert the child rows of one route.
async fn insert_children(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    rows: RouteChildRows,
) -> Result<(), ScopeError> {
    if let Some(http_match) = rows.http_match {
        secure_insert::<route_http_match::Entity>(http_match, scope, tx).await?;
    }
    if let Some(grpc_match) = rows.grpc_match {
        secure_insert::<route_grpc_match::Entity>(grpc_match, scope, tx).await?;
    }
    secure_insert_many::<route_method::Entity>(rows.methods, scope, tx).await?;
    secure_insert_many::<route_tag::Entity>(rows.tags, scope, tx).await?;
    secure_insert_many::<route_plugin::Entity>(rows.plugins, scope, tx).await?;
    Ok(())
}

/// Delete the child rows of one route from each listed child table.
macro_rules! delete_children {
    ($tx:expr, $scope:expr, $route_id:expr, $($entity:ident),+ $(,)?) => {
        $(
            $entity::Entity::delete_many()
                .filter($entity::Column::RouteId.eq($route_id))
                .secure()
                .scope_with($scope)
                .exec($tx)
                .await?;
        )+
    };
}

/// The request a proxy route lookup serves.
struct Request<'a> {
    tenant_chain: &'a [Uuid],
    upstream_ids: &'a [Uuid],
    method: HttpMethod,
    path: &'a str,
}

/// The outcome of one selection: the candidate query, then the winner query.
#[derive(Debug)]
enum Attempt {
    /// No candidate matches the request.
    NoMatch,
    /// The winner as the second query loaded it, without tags; `None` when it
    /// was deleted, disabled or moved to another upstream in between.
    Winner(Option<Box<Route>>),
}

/// Run `attempt` and keep its winner only if the winner, as loaded, still
/// matches the request. The candidate and winner queries are separate
/// statements, so a write committed between them can change, remove or move
/// the winner; such a request is selected once more, and a second miss is no
/// match (ADR-0018 *Transactions*). `None`: no match.
async fn resolve_verified<F, Fut>(
    request: &Request<'_>,
    mut attempt: F,
) -> Result<Option<Route>, RepositoryError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Attempt, RepositoryError>>,
{
    for _ in 0..2 {
        match attempt().await? {
            Attempt::NoMatch => return Ok(None),
            Attempt::Winner(Some(route)) if still_matches(&route, request) => {
                return Ok(Some(*route));
            }
            Attempt::Winner(_) => {}
        }
    }
    Ok(None)
}

/// Whether a loaded winner matches the request by the selection rule itself.
fn still_matches(route: &Route, request: &Request<'_>) -> bool {
    select_route(
        [route],
        request.tenant_chain,
        request.upstream_ids,
        request.method,
        request.path,
    )
    .is_some()
}

/// One selection: the candidates that allow the method, the winner by
/// `select_route`, then the winner in full with its path, methods and plugins.
async fn select_and_load(
    conn: &impl DBRunner,
    request: &Request<'_>,
) -> Result<Attempt, RepositoryError> {
    let Request {
        tenant_chain,
        upstream_ids,
        method,
        path,
    } = *request;

    // One row per route that allows `method` (the methods' primary key is
    // `(route_id, method)`), with only the columns selection reads. Inner
    // joins on the composite foreign keys, whose tenant keeps every child
    // in its scoped route's tenant: a route without an HTTP match cannot
    // match.
    let candidates: Vec<CandidateRow> = route::Entity::find()
        .filter(route::Column::UpstreamId.is_in(upstream_ids.iter().copied()))
        .filter(route::Column::Enabled.eq(true))
        .filter(route::Column::MatchType.eq(MATCH_TYPE_HTTP))
        .filter(route_method::Column::Method.eq(method_text(method)))
        .secure()
        .scope_with(&AccessScope::for_tenants(tenant_chain.to_vec()))
        .project_all(conn, |q| {
            q.select_only()
                .column(route::Column::Id)
                .column(route::Column::TenantId)
                .column(route::Column::UpstreamId)
                .column(route::Column::Priority)
                .column(route_http_match::Column::PathPrefix)
                .join_rev(JoinType::InnerJoin, route_http_match::Relation::Route.def())
                .join_rev(JoinType::InnerJoin, route_method::Relation::Route.def())
                .into_model::<CandidateRow>()
        })
        .await
        .map_err(internal)?;
    let views: Vec<Route> = candidates
        .into_iter()
        .map(|c| http_candidate(c, method))
        .collect();
    let Some(winner) = select_route(&views, tenant_chain, upstream_ids, method, path) else {
        return Ok(Attempt::NoMatch);
    };
    let (winner_id, winner_tenant) = (winner.id, winner.tenant_id);

    // The winner in full. Its path and upstream are read here, not taken
    // from the candidate query, so the caller checks the route as stored now.
    let winner = full_http_routes(
        conn,
        route::Entity::find()
            .filter(route::Column::Id.eq(winner_id))
            .filter(route::Column::UpstreamId.is_in(upstream_ids.iter().copied())),
        AccessScope::for_tenant(winner_tenant),
    )
    .await?
    .pop();
    Ok(Attempt::Winner(winner.map(Box::new)))
}

/// The enabled HTTP routes `select` filters, in full with their path,
/// methods and plugins but without tags, in ID order: one query of one row
/// per (route, method, plugin binding), inner-joined to the HTTP match and
/// the methods and left-joined to the plugins on the composite foreign keys,
/// whose tenant keeps every child in its scoped route's tenant.
async fn full_http_routes(
    conn: &impl DBRunner,
    select: Select<route::Entity>,
    scope: AccessScope,
) -> Result<Vec<Route>, RepositoryError> {
    let rows: Vec<FullRouteRow> = select
        .filter(route::Column::Enabled.eq(true))
        .filter(route::Column::MatchType.eq(MATCH_TYPE_HTTP))
        .secure()
        .scope_with(&scope)
        .project_all(conn, |q| {
            route_plugin::Column::iter()
                .fold(
                    q.join_rev(JoinType::InnerJoin, route_http_match::Relation::Route.def())
                        .column_as(route_http_match::Column::PathPrefix, "path_prefix")
                        .join_rev(JoinType::InnerJoin, route_method::Relation::Route.def())
                        .column_as(route_method::Column::Method, "method")
                        .join_rev(JoinType::LeftJoin, route_plugin::Relation::Route.def()),
                    |q, col| q.column_as(col, format!("{PLUGIN_PREFIX}{}", col.as_str())),
                )
                .into_model::<FullRouteRow>()
        })
        .await
        .map_err(internal)?;

    /// One route's rows, folded.
    struct Parts {
        row: route::Model,
        path_prefix: String,
        methods: Vec<route_method::Model>,
        plugins: BTreeMap<i32, route_plugin::Model>,
    }
    let mut routes: BTreeMap<Uuid, Parts> = BTreeMap::new();
    for FullRouteRow {
        route,
        path_prefix,
        method,
        plugin,
    } in rows
    {
        let parts = routes.entry(route.id).or_insert_with(|| Parts {
            row: route,
            path_prefix,
            methods: Vec::new(),
            plugins: BTreeMap::new(),
        });
        if !parts.methods.iter().any(|m| m.method == method) {
            parts.methods.push(route_method::Model {
                route_id: parts.row.id,
                tenant_id: parts.row.tenant_id,
                method,
            });
        }
        if let Some(plugin) = plugin {
            parts.plugins.entry(plugin.position).or_insert(plugin);
        }
    }
    routes
        .into_values()
        .map(|parts| {
            let http_match = route_http_match::Model {
                route_id: parts.row.id,
                tenant_id: parts.row.tenant_id,
                path_prefix: parts.path_prefix,
            };
            route_from_rows(
                parts.row,
                RouteChildren {
                    http_match: Some(http_match),
                    methods: parts.methods,
                    grpc_match: None,
                    tags: Vec::new(),
                    plugins: parts.plugins.into_values().collect(),
                },
            )
        })
        .collect()
}

/// `routes` (in ID order) under the key of their upstream: one entry per
/// distinct key, in the order the keys first appear. Tenant and upstream are
/// filtered separately, so a route whose `(tenant, upstream)` pair is not
/// one of the keys is dropped here.
fn group_by_upstream(keys: &[RowKey], routes: Vec<Route>) -> Vec<(RowKey, Vec<Route>)> {
    let mut grouped: HashMap<RowKey, Vec<Route>> = HashMap::new();
    for route in routes {
        let key = RowKey {
            tenant_id: route.tenant_id,
            id: route.upstream_id,
        };
        grouped.entry(key).or_default().push(route);
    }
    let mut seen = HashSet::new();
    keys.iter()
        .filter(|key| seen.insert(**key))
        .map(|key| (*key, grouped.remove(key).unwrap_or_default()))
        .collect()
}

/// One row per (stored route, shared method) of the overlap rule (see
/// `RouteRepository::create`). Path equality is byte-exact on every backend
/// (ADR-0009 *Appendix B*).
async fn overlap_rows(
    runner: &impl DBRunner,
    tenant_id: Uuid,
    upstream_id: Uuid,
    path: &str,
    priority: i32,
    methods: &[HttpMethod],
) -> Result<Vec<OverlapRow>, ScopeError> {
    route::Entity::find()
        .filter(route::Column::UpstreamId.eq(upstream_id))
        .filter(route::Column::Enabled.eq(true))
        .filter(route::Column::MatchType.eq(MATCH_TYPE_HTTP))
        .filter(route::Column::Priority.eq(priority))
        .filter(route_http_match::Column::PathPrefix.eq(path))
        .filter(route_method::Column::Method.is_in(methods.iter().map(|m| method_text(*m))))
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant_id))
        .project_all(runner, |q| {
            q.select_only()
                .column(route::Column::Id)
                .column(route_method::Column::Method)
                .join_rev(JoinType::InnerJoin, route_http_match::Relation::Route.def())
                .join_rev(JoinType::InnerJoin, route_method::Relation::Route.def())
                .into_model::<OverlapRow>()
        })
        .await
}

fn overlaps_from(rows: Vec<OverlapRow>) -> Result<Vec<RouteOverlap>, RepositoryError> {
    rows.into_iter()
        .map(|row| {
            Ok(RouteOverlap {
                id: row.id,
                method: parse_stored_method(&row.method)
                    .map_err(|e| invalid_stored(ENTITY, row.id, e))?,
            })
        })
        .collect()
}

/// The first statements of a route write: lock the upstream the route is
/// written to (`lock_upstream`), then apply the overlap rule; a writer
/// behind the lock reads what the one before it committed. Only an enabled
/// HTTP route can overlap, so any other neither locks nor checks.
async fn lock_and_check_overlap(
    tx: &DbTx<'_>,
    backend: DbBackend,
    route: &Route,
) -> Result<(), WriteTxError> {
    let Some(http) = route.match_rules.http.as_ref().filter(|_| route.enabled) else {
        return Ok(());
    };
    if http.methods.is_empty() {
        return Ok(());
    }
    if !lock_upstream(tx, backend, route.tenant_id, route.upstream_id, None).await? {
        return Err(WriteTxError::Repo(not_found("upstream", route.upstream_id)));
    }
    let rows = overlap_rows(
        tx,
        route.tenant_id,
        route.upstream_id,
        &http.path,
        route.priority,
        &http.methods,
    )
    .await?;
    let overlaps = overlaps_from(rows).map_err(WriteTxError::Repo)?;
    match RouteOverlap::reported(overlaps, route.id) {
        Some(overlap) => Err(WriteTxError::Repo(RepositoryError::RouteOverlap(overlap))),
        None => Ok(()),
    }
}

#[async_trait]
impl RouteRepository for DbRouteRepo {
    async fn create(&self, route: Route) -> Result<Route, RepositoryError> {
        let now = OffsetDateTime::now_utc();
        let db = self.db.db();
        let backend = db.backend();
        let stored = route.clone();
        #[cfg(all(test, feature = "integration"))]
        let hold = self.hold.clone();
        db.transaction_with_retry(read_committed_for(backend), write_tx_db_err, move |tx| {
            let stored = stored.clone();
            #[cfg(all(test, feature = "integration"))]
            let hold = hold.clone();
            Box::pin(async move {
                lock_and_check_overlap(tx, backend, &stored).await?;
                let rows = route_rows(&stored, now).map_err(WriteTxError::Repo)?;
                let scope = AccessScope::for_tenant(stored.tenant_id);
                secure_insert::<route::Entity>(rows.row, &scope, tx)
                    .await
                    .map_err(|e| WriteTxError::Statement(e, Statement::InsertRoute))?;
                insert_children(tx, &scope, rows.children).await?;
                #[cfg(all(test, feature = "integration"))]
                hold.wait().await;
                Ok(())
            })
        })
        .await
        .map_err(|e| e.into_repo(&route))?;
        Ok(route)
    }

    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Route, RepositoryError> {
        self.db
            .transaction_with_config(snapshot(), move |tx| {
                Box::pin(async move {
                    let row = route::Entity::find()
                        .filter(route::Column::Id.eq(id))
                        .secure()
                        .scope_with(&AccessScope::for_tenant(tenant_id))
                        .one(tx)
                        .await
                        .map_err(internal)?
                        .ok_or_else(|| not_found(ENTITY, id))?;
                    assemble(tx, vec![row])
                        .await?
                        .pop()
                        .ok_or_else(|| not_found(ENTITY, id))
                })
            })
            .await
    }

    async fn list(
        &self,
        tenant_id: Uuid,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, RepositoryError> {
        let (skip, top) = (u64::from(query.skip), u64::from(query.top));
        self.db
            .transaction_with_config(snapshot(), move |tx| {
                Box::pin(async move {
                    let mut select = route::Entity::find();
                    if let Some(upstream_id) = upstream_id {
                        select = select.filter(route::Column::UpstreamId.eq(upstream_id));
                    }
                    let rows = select
                        .secure()
                        .scope_with(&AccessScope::for_tenant(tenant_id))
                        .order_by(route::Column::Id, Order::Asc)
                        .offset(skip)
                        .limit(top)
                        .all(tx)
                        .await
                        .map_err(internal)?;
                    assemble(tx, rows).await
                })
            })
            .await
    }

    async fn find_matching_in_tenants(
        &self,
        tenant_chain: &[Uuid],
        upstream_ids: &[Uuid],
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<Route, RepositoryError> {
        let no_match = || not_found(ENTITY, Uuid::nil());
        let Some(method) = parse_method(method) else {
            return Err(no_match());
        };
        if tenant_chain.is_empty() || upstream_ids.is_empty() {
            return Err(no_match());
        }
        let conn = self.db.conn()?;
        let request = Request {
            tenant_chain,
            upstream_ids,
            method,
            path,
        };
        let route = resolve_verified(&request, || select_and_load(&conn, &request))
            .await?
            .ok_or_else(no_match)?;
        let tags = match tags {
            Tags::Load => {
                let winner: [Parent; 1] = [(route.id, route.tenant_id)];
                let rows =
                    children::<route_tag::Entity>(&conn, route_tag::Column::RouteId, &winner)
                        .await?;
                sorted_tags(rows.into_iter().map(|t| t.tag))
            }
            Tags::Skip => Vec::new(),
        };
        Ok(Route { tags, ..route })
    }

    async fn list_proxy_routes(
        &self,
        upstreams: &[RowKey],
    ) -> Result<Vec<(RowKey, Vec<Route>)>, RepositoryError> {
        if upstreams.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.conn()?;
        let mut routes = Vec::new();
        // Each chunk binds at most one tenant per key plus the upstream IDs;
        // only keys past the backend's bind limit take a second query.
        for chunk in upstreams.chunks(max_in_list(&conn, 2).div_euclid(2).max(1)) {
            let tenants: BTreeSet<Uuid> = chunk.iter().map(|k| k.tenant_id).collect();
            routes.extend(
                full_http_routes(
                    &conn,
                    route::Entity::find()
                        .filter(route::Column::UpstreamId.is_in(chunk.iter().map(|k| k.id))),
                    AccessScope::for_tenants(tenants.into_iter().collect()),
                )
                .await?,
            );
        }
        Ok(group_by_upstream(upstreams, routes))
    }

    async fn update(&self, route: Route) -> Result<Route, RepositoryError> {
        let now = OffsetDateTime::now_utc();
        let db = self.db.db();
        let backend = db.backend();
        let stored = route.clone();
        #[cfg(all(test, feature = "integration"))]
        let hold = self.hold.clone();
        let (upstream_id, managed_by) = db
            .transaction_with_retry(read_committed_for(backend), write_tx_db_err, move |tx| {
                let stored = stored.clone();
                #[cfg(all(test, feature = "integration"))]
                let hold = hold.clone();
                Box::pin(async move {
                    // A move checks, and locks, the upstream it moves to.
                    lock_and_check_overlap(tx, backend, &stored).await?;
                    let rows = route_rows(&stored, now).map_err(WriteTxError::Repo)?;
                    let scope = AccessScope::for_tenant(stored.tenant_id);
                    // The key, the owner and `created_at` stay as stored; the
                    // upstream is written (a registry move), and the composite
                    // foreign key rejects one missing from the tenant.
                    let mut row = rows.row;
                    row.id = NotSet;
                    row.tenant_id = NotSet;
                    row.managed_by = NotSet;
                    row.created_at = NotSet;
                    let update = route::Entity::update_many()
                        .set(row)
                        .filter(route::Column::Id.eq(stored.id))
                        .secure()
                        .scope_with(&scope);
                    let updated =
                        update_one(tx, backend, update, route::Column::Id, stored.id, &scope)
                            .await
                            .map_err(|e| WriteTxError::Statement(e, Statement::UpdateRoute))?;
                    let Some(updated) = updated else {
                        return Err(WriteTxError::Repo(not_found(ENTITY, stored.id)));
                    };

                    delete_children!(
                        tx,
                        &scope,
                        stored.id,
                        route_http_match,
                        route_method,
                        route_grpc_match,
                        route_tag,
                        route_plugin,
                    );
                    insert_children(tx, &scope, rows.children).await?;
                    let managed_by = parse_managed_by(&updated.managed_by)
                        .map_err(|e| WriteTxError::Repo(invalid_stored(ENTITY, stored.id, e)))?;
                    #[cfg(all(test, feature = "integration"))]
                    hold.wait().await;
                    Ok((updated.upstream_id, managed_by))
                })
            })
            .await
            .map_err(|e| e.into_repo(&route))?;
        Ok(Route {
            upstream_id,
            managed_by,
            ..route
        })
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        let conn = self.db.conn()?;
        // Match, method, tag and plugin rows go by cascade.
        let deleted = route::Entity::delete_many()
            .filter(route::Column::Id.eq(id))
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant_id))
            .exec(&conn)
            .await
            .map_err(internal)?;
        if deleted.rows_affected == 0 {
            return Err(not_found(ENTITY, id));
        }
        Ok(())
    }

    /// No-op: the composite route→upstream foreign key cascades, so
    /// [`UpstreamRepository::delete`](crate::domain::repo::UpstreamRepository::delete)
    /// already removed the routes in the same statement
    /// (`migration_tests::case_upstream_delete_cascades`).
    async fn delete_by_upstream(
        &self,
        _tenant_id: Uuid,
        _upstream_id: Uuid,
    ) -> Result<(), RepositoryError> {
        Ok(())
    }
}

/// The overlap check against concurrent writes on `PostgreSQL`, `MySQL` and
/// `SQLite`, with separate connections: the second overlapping write waits on
/// the upstream lock (on `SQLite`, the database's write lock), then sees the
/// first and is rejected.
#[cfg(all(test, feature = "integration"))]
mod race_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::oneshot;
    use toolkit_db::Db;

    use super::*;
    use crate::domain::gts_helpers::HTTP_PROTOCOL_ID;
    use crate::domain::model::{
        Endpoint, HttpMatch, ManagedBy, MatchRules, PathSuffixMode, Scheme, Server, Upstream,
    };
    use crate::domain::repo::UpstreamRepository;
    use crate::infra::storage::testing;
    use crate::infra::storage::upstream_db_repo::DbUpstreamRepo;

    fn upstream(tenant_id: Uuid, alias: &str) -> Upstream {
        Upstream {
            id: Uuid::new_v4(),
            tenant_id,
            alias: alias.into(),
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "api.example.com".into(),
                    port: 443,
                }],
            },
            protocol: HTTP_PROTOCOL_ID.into(),
            enabled: true,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            managed_by: ManagedBy::Api,
        }
    }

    /// An enabled `GET /v1` route on `upstream_id`.
    fn route(tenant_id: Uuid, upstream_id: Uuid) -> Route {
        Route {
            id: Uuid::new_v4(),
            tenant_id,
            upstream_id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Get],
                    path: "/v1".into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
            managed_by: ManagedBy::Api,
        }
    }

    /// Long enough for a blocked statement to show it is blocked.
    const BLOCKED: Duration = Duration::from_millis(300);

    /// Start `first` through `held` and hold its transaction open after its
    /// insert; returns the write and the release.
    async fn hold_create(
        held: &Arc<DbRouteRepo>,
        first: Route,
    ) -> (
        tokio::task::JoinHandle<Result<Route, RepositoryError>>,
        oneshot::Sender<()>,
    ) {
        let (locked_tx, locked_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        held.hold_next_write(HoldBeforeCommit {
            locked: locked_tx,
            release: release_rx,
        });
        let write = tokio::spawn({
            let held = held.clone();
            async move { held.create(first).await }
        });
        locked_rx.await.expect("the first write ran its statements");
        (write, release_tx)
    }

    /// ADR-0018: PERS-RACE-003.
    async fn a_second_overlapping_create_waits_and_is_rejected(db: Db) {
        let db = DBProvider::<RepositoryError>::new(db);
        let upstreams = DbUpstreamRepo::new(db.clone());
        let held = Arc::new(DbRouteRepo::new(db.clone()));
        let other = DbRouteRepo::new(db);
        let tenant = Uuid::new_v4();
        let up = upstreams.create(upstream(tenant, "api")).await.unwrap();
        let first = route(tenant, up.id);
        let (write, release) = hold_create(&held, first.clone()).await;

        let second = tokio::spawn(async move { other.create(route(tenant, up.id)).await });
        tokio::time::sleep(BLOCKED).await;
        assert!(
            !second.is_finished(),
            "the second write must wait on the lock"
        );

        release.send(()).unwrap();
        write.await.unwrap().unwrap();
        let result = second.await.unwrap();
        assert!(
            matches!(result, Err(RepositoryError::RouteOverlap(o)) if o.id == first.id),
            "got {result:?}"
        );
    }

    /// A move locks and checks the upstream it moves to.
    ///
    /// ADR-0018: PERS-RACE-004.
    async fn a_move_waits_on_the_target_and_is_rejected(db: Db) {
        let db = DBProvider::<RepositoryError>::new(db);
        let upstreams = DbUpstreamRepo::new(db.clone());
        let held = Arc::new(DbRouteRepo::new(db.clone()));
        let other = DbRouteRepo::new(db);
        let tenant = Uuid::new_v4();
        let from = upstreams.create(upstream(tenant, "from")).await.unwrap();
        let to = upstreams.create(upstream(tenant, "to")).await.unwrap();
        let moving = other.create(route(tenant, from.id)).await.unwrap();
        let first = route(tenant, to.id);
        let (write, release) = hold_create(&held, first.clone()).await;

        let second = tokio::spawn(async move {
            other
                .update(Route {
                    upstream_id: to.id,
                    ..moving
                })
                .await
        });
        tokio::time::sleep(BLOCKED).await;
        assert!(
            !second.is_finished(),
            "the move must wait on the target's lock"
        );

        release.send(()).unwrap();
        write.await.unwrap().unwrap();
        let result = second.await.unwrap();
        assert!(
            matches!(result, Err(RepositoryError::RouteOverlap(o)) if o.id == first.id),
            "got {result:?}"
        );
    }

    /// Many overlapping creates at once: exactly one is written.
    ///
    /// ADR-0018: PERS-RACE-005.
    async fn concurrent_overlapping_creates_write_one(db: Db) {
        let db = DBProvider::<RepositoryError>::new(db);
        let upstreams = DbUpstreamRepo::new(db.clone());
        let routes = Arc::new(DbRouteRepo::new(db));
        let tenant = Uuid::new_v4();
        let up = upstreams.create(upstream(tenant, "api")).await.unwrap();
        let writes: Vec<_> = (0..8)
            .map(|_| {
                let routes = routes.clone();
                tokio::spawn(async move { routes.create(route(tenant, up.id)).await })
            })
            .collect();
        let mut written = 0;
        for write in writes {
            match write.await.unwrap() {
                Ok(_) => written += 1,
                Err(RepositoryError::RouteOverlap(_)) => {}
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(written, 1);
    }

    #[tokio::test]
    async fn races_on_postgres() {
        let server = testing::postgres_server().await;
        a_second_overlapping_create_waits_and_is_rejected(server.fresh().await).await;
        a_move_waits_on_the_target_and_is_rejected(server.fresh().await).await;
        concurrent_overlapping_creates_write_one(server.fresh().await).await;
    }

    #[tokio::test]
    async fn races_on_mysql() {
        let server = testing::mysql_server().await;
        a_second_overlapping_create_waits_and_is_rejected(server.fresh().await).await;
        a_move_waits_on_the_target_and_is_rejected(server.fresh().await).await;
        concurrent_overlapping_creates_write_one(server.fresh().await).await;
    }

    /// On `MySQL` an upstream update and a route write lock the upstream's
    /// records in one order (see `upstream_lock::lock_upstream`), so they
    /// wait instead of deadlocking.
    ///
    /// ADR-0018: PERS-RACE-006.
    #[tokio::test]
    async fn a_route_write_and_an_upstream_update_do_not_deadlock_on_mysql() {
        use sea_orm::sea_query::Expr;
        use toolkit_db::secure::secure_insert;

        use super::super::entity::{upstream, upstream_tag};

        let server = testing::mysql_server().await;
        let db = DBProvider::<RepositoryError>::new(server.fresh().await);
        let upstreams = DbUpstreamRepo::new(db.clone());
        let routes = DbRouteRepo::new(db.clone());
        let tenant = Uuid::new_v4();
        let up = upstreams.create(upstream(tenant, "api")).await.unwrap();
        let deadlocks = server.mysql_deadlocks().await;

        let (updated_tx, updated_rx) = oneshot::channel();
        let (go_tx, go_rx) = oneshot::channel::<()>();
        let update = tokio::spawn(async move {
            db.transaction(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::for_tenant(tenant);
                    upstream::Entity::update_many()
                        .secure()
                        .col_expr(upstream::Column::Alias, Expr::value("renamed"))
                        .filter(sea_orm::Condition::all().add(upstream::Column::Id.eq(up.id)))
                        .scope_with(&scope)
                        .exec(tx)
                        .await
                        .map_err(internal)?;
                    let _ = updated_tx.send(());
                    let _ = go_rx.await;
                    let tag = upstream_tag::ActiveModel {
                        upstream_id: sea_orm::ActiveValue::Set(up.id),
                        tenant_id: sea_orm::ActiveValue::Set(tenant),
                        tag: sea_orm::ActiveValue::Set("t".into()),
                    };
                    secure_insert::<upstream_tag::Entity>(tag, &scope, tx)
                        .await
                        .map_err(internal)?;
                    Ok(())
                })
            })
            .await
        });
        updated_rx.await.expect("the upstream row is updated");

        let write = tokio::spawn(async move { routes.create(route(tenant, up.id)).await });
        tokio::time::sleep(BLOCKED).await;
        assert!(
            !write.is_finished(),
            "the route write must wait on the upstream update"
        );
        go_tx.send(()).unwrap();
        update.await.unwrap().expect("the upstream update commits");
        write.await.unwrap().expect("the route write follows it");
        assert_eq!(server.mysql_deadlocks().await, deadlocks, "no deadlock");
    }

    /// `SQLite` has no row lock: the lock is a write, so a writer waits for
    /// the database's write lock (the busy timeout) instead of failing.
    /// A file, so the pool's connections lock as separate processes would.
    #[tokio::test]
    async fn races_on_sqlite() {
        for case in 0..3 {
            let file = testing::SqliteFile::new();
            let db = file.open().await;
            match case {
                0 => a_second_overlapping_create_waits_and_is_rejected(db).await,
                1 => a_move_waits_on_the_target_and_is_rejected(db).await,
                _ => concurrent_overlapping_creates_write_one(db).await,
            }
        }
    }
}

// Database-only behavior; the shared contract is in `conformance_tests`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::gts_helpers::HTTP_PROTOCOL_ID;
    use crate::domain::model::{
        Endpoint, GrpcMatch, HttpMatch, HttpMethod, ManagedBy, MatchRules, PathSuffixMode, Scheme,
        Server, Upstream,
    };
    use crate::domain::repo::UpstreamRepository;
    use crate::infra::storage::testing::shared_sqlite;
    use crate::infra::storage::upstream_db_repo::DbUpstreamRepo;

    fn upstream(tenant_id: Uuid) -> Upstream {
        Upstream {
            id: Uuid::new_v4(),
            tenant_id,
            alias: "api".into(),
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "api.example.com".into(),
                    port: 443,
                }],
            },
            protocol: HTTP_PROTOCOL_ID.into(),
            enabled: true,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            managed_by: ManagedBy::Api,
        }
    }

    fn route(tenant_id: Uuid, upstream_id: Uuid) -> Route {
        Route {
            id: Uuid::new_v4(),
            tenant_id,
            upstream_id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Get],
                    path: "/v1".into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec!["t".into()],
            priority: 0,
            enabled: true,
            managed_by: ManagedBy::Api,
        }
    }

    /// `route` (an enabled HTTP route, which locks its upstream), and with a
    /// new ID the same route disabled and as a gRPC route, which take no
    /// lock: only the foreign key checks their upstream.
    fn unlocked_variants(route: Route) -> [Route; 3] {
        let disabled = Route {
            id: Uuid::new_v4(),
            enabled: false,
            ..route.clone()
        };
        let grpc = Route {
            id: Uuid::new_v4(),
            match_rules: MatchRules {
                http: None,
                grpc: Some(GrpcMatch {
                    service: "example.chat.v1.Chat".into(),
                    method: "Complete".into(),
                }),
            },
            ..route.clone()
        };
        [route, disabled, grpc]
    }

    /// Runs [`resolve_verified`] over scripted attempts and returns the route
    /// it accepts and how many attempts it made.
    async fn resolve_scripted(
        request: &Request<'_>,
        script: Vec<Attempt>,
    ) -> (Option<Route>, usize) {
        let mut script = script.into_iter();
        let mut calls = 0;
        let resolved = resolve_verified(request, || {
            calls += 1;
            std::future::ready(Ok(script.next().expect("attempt beyond the script")))
        })
        .await
        .unwrap();
        (resolved, calls)
    }

    /// A winner as loaded after a write committed between the two queries.
    fn raced_winners(tenant: Uuid, upstream: Uuid) -> [(&'static str, Option<Box<Route>>); 3] {
        let mut method_dropped = route(tenant, upstream);
        if let Some(http) = method_dropped.match_rules.http.as_mut() {
            http.methods = vec![HttpMethod::Post];
        }
        [
            ("method dropped", Some(Box::new(method_dropped))),
            ("winner missing", None),
            (
                "winner on another upstream",
                Some(Box::new(route(tenant, Uuid::new_v4()))),
            ),
        ]
    }

    /// A winner that no longer matches is selected once more, and the
    /// second selection's winner is served.
    ///
    /// ADR-0018: PERS-PROXY-001.
    #[tokio::test]
    async fn raced_winner_is_selected_again() {
        let (tenant, upstream) = (Uuid::new_v4(), Uuid::new_v4());
        let request = Request {
            tenant_chain: &[tenant],
            upstream_ids: &[upstream],
            method: HttpMethod::Get,
            path: "/v1/x",
        };
        for (case, raced) in raced_winners(tenant, upstream) {
            let current = route(tenant, upstream);
            let (resolved, calls) = resolve_scripted(
                &request,
                vec![
                    Attempt::Winner(raced),
                    Attempt::Winner(Some(Box::new(current.clone()))),
                ],
            )
            .await;
            assert_eq!(resolved.map(|r| r.id), Some(current.id), "{case}");
            assert_eq!(calls, 2, "{case}");
        }
    }

    /// A second miss is no match: the lookup retries once, not until it wins.
    #[tokio::test]
    async fn raced_winner_twice_is_no_match() {
        let (tenant, upstream) = (Uuid::new_v4(), Uuid::new_v4());
        let request = Request {
            tenant_chain: &[tenant],
            upstream_ids: &[upstream],
            method: HttpMethod::Get,
            path: "/v1/x",
        };
        for (case, raced) in raced_winners(tenant, upstream) {
            let (resolved, calls) = resolve_scripted(
                &request,
                vec![Attempt::Winner(raced.clone()), Attempt::Winner(raced)],
            )
            .await;
            assert!(resolved.is_none(), "{case}");
            assert_eq!(calls, 2, "{case}");
        }
    }

    /// The normal path is one selection: a matching winner is served as
    /// loaded, and no candidate is no match without a retry.
    #[tokio::test]
    async fn matching_winner_or_no_candidate_selects_once() {
        let (tenant, upstream) = (Uuid::new_v4(), Uuid::new_v4());
        let request = Request {
            tenant_chain: &[tenant],
            upstream_ids: &[upstream],
            method: HttpMethod::Get,
            path: "/v1/x",
        };
        let current = route(tenant, upstream);
        let (resolved, calls) = resolve_scripted(
            &request,
            vec![Attempt::Winner(Some(Box::new(current.clone())))],
        )
        .await;
        assert_eq!((resolved.map(|r| r.id), calls), (Some(current.id), 1));

        let (resolved, calls) = resolve_scripted(&request, vec![Attempt::NoMatch]).await;
        assert_eq!((resolved.map(|r| r.id), calls), (None, 1));
    }

    /// A route on a missing upstream or on another tenant's upstream is
    /// rejected, by the upstream lock (an enabled HTTP route) or by the
    /// composite foreign key `(tenant_id, upstream_id)` (any other); both
    /// read as the upstream not being found, and nothing is stored.
    #[tokio::test]
    async fn route_on_missing_or_foreign_upstream_is_upstream_not_found() {
        let db = DBProvider::<RepositoryError>::new(shared_sqlite().await);
        let upstreams = DbUpstreamRepo::new(db.clone());
        let routes = DbRouteRepo::new(db);
        let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
        let foreign = upstreams.create(upstream(other)).await.unwrap();

        for upstream_id in [Uuid::new_v4(), foreign.id] {
            for new_route in unlocked_variants(route(tenant, upstream_id)) {
                let result = routes.create(new_route.clone()).await;
                assert!(
                    matches!(
                        result,
                        Err(RepositoryError::NotFound { entity: "upstream", id }) if id == upstream_id
                    ),
                    "{new_route:?}: got {result:?}"
                );
            }
        }
        assert!(
            routes
                .list(tenant, None, &ListQuery::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A move to a missing upstream or another tenant's is rejected the same
    /// way, and the route stays on its upstream.
    ///
    /// ADR-0018: PERS-STORE-004.
    #[tokio::test]
    async fn move_to_missing_or_foreign_upstream_is_upstream_not_found() {
        let db = DBProvider::<RepositoryError>::new(shared_sqlite().await);
        let upstreams = DbUpstreamRepo::new(db.clone());
        let routes = DbRouteRepo::new(db);
        let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
        let home = upstreams.create(upstream(tenant)).await.unwrap();
        let foreign = upstreams.create(upstream(other)).await.unwrap();
        for variant in unlocked_variants(route(tenant, home.id)) {
            let stored = routes.create(variant).await.unwrap();
            for upstream_id in [Uuid::new_v4(), foreign.id] {
                let result = routes
                    .update(Route {
                        upstream_id,
                        ..stored.clone()
                    })
                    .await;
                assert!(
                    matches!(
                        result,
                        Err(RepositoryError::NotFound { entity: "upstream", id }) if id == upstream_id
                    ),
                    "{stored:?}: got {result:?}"
                );
            }
            assert_eq!(routes.get_by_id(tenant, stored.id).await.unwrap(), stored);
        }
    }
}
