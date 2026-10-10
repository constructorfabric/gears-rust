//! `DbRouteRepo`: `RouteRepository` on the `oagw_route*` tables.
//!
//! `get_by_id` and `list` read the route rows plus one batched query per child
//! table (HTTP match, methods, gRPC match, tags, plugins): 6 queries, no joins,
//! so a route is never dropped for a missing optional child.
//! `find_matching_in_tenants` loads the candidates that allow the request's
//! method in one join (routes ⋈ HTTP match ⋈ methods), selecting only the
//! columns route selection reads; the shared `route_matching::select_route`
//! picks the winner, and one more join loads the winner in full with its
//! methods and plugins (its tags only when requested): 2 queries (3 with
//! tags) at any hierarchy depth.
//! `get_by_id` and `list` read in one read-only snapshot transaction;
//! `find_matching_in_tenants` runs in none (ADR-0018 *Transactions*).
//! Creates and updates run in one transaction and replace child rows by
//! delete-then-insert; deletes are one statement.
//! `list_registry_keys` is the one read across all tenants: the startup
//! registry reconcile owns rows in any tenant (ADR-0018 *Tenant Scope*).

use std::collections::BTreeMap;

use async_trait::async_trait;
use sea_orm::ActiveValue::NotSet;
use sea_orm::{
    ColumnTrait, EntityTrait, FromQueryResult, IdenStatic, Iterable, JoinType, Order, QueryFilter,
    QuerySelect, RelationTrait,
};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
    secure_insert_many,
};
use uuid::Uuid;

use super::entity::{
    route, route_grpc_match, route_http_match, route_method, route_plugin, route_tag,
};
use super::error::{internal, invalid_stored, not_found, route_create};
use super::load::{Parent, by_parent, children, snapshot, update_one};
use super::mapper::{
    CandidateRow, MATCH_TYPE_HTTP, RouteChildRows, RouteChildren, http_candidate, managed_by_text,
    method_text, parse_managed_by, route_from_rows, route_rows,
};
use crate::domain::model::{ListQuery, ManagedBy, Route};
use crate::domain::repo::{RepositoryError, RouteRepository, RowKey, Tags};
use crate::domain::route_matching::{parse_method, select_route};

const ENTITY: &str = "route";

/// Column prefix of the plugin binding in [`WinnerRow`].
const PLUGIN_PREFIX: &str = "plugin_";

/// The winning route joined with one of its methods and one of its plugin
/// bindings (`None`: no bindings).
#[derive(FromQueryResult)]
struct WinnerRow {
    #[sea_orm(nested)]
    route: route::Model,
    method: String,
    #[sea_orm(nested(prefix = "plugin_"))]
    plugin: Option<route_plugin::Model>,
}

/// Database-backed route repository.
pub(crate) struct DbRouteRepo {
    db: DBProvider<RepositoryError>,
}

impl DbRouteRepo {
    /// Create a repository over the given database provider.
    pub(crate) fn new(db: DBProvider<RepositoryError>) -> Self {
        Self { db }
    }
}

/// The single row of a one-to-one child, if present.
fn first<M>(rows: Option<Vec<M>>) -> Option<M> {
    rows.and_then(|rows| rows.into_iter().next())
}

/// Load the five child tables of `rows` (one query each) and build the
/// aggregates, in the order of `rows`.
async fn assemble(
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
) -> Result<(), RepositoryError> {
    if let Some(http_match) = rows.http_match {
        secure_insert::<route_http_match::Entity>(http_match, scope, tx)
            .await
            .map_err(internal)?;
    }
    if let Some(grpc_match) = rows.grpc_match {
        secure_insert::<route_grpc_match::Entity>(grpc_match, scope, tx)
            .await
            .map_err(internal)?;
    }
    secure_insert_many::<route_method::Entity>(rows.methods, scope, tx)
        .await
        .map_err(internal)?;
    secure_insert_many::<route_tag::Entity>(rows.tags, scope, tx)
        .await
        .map_err(internal)?;
    secure_insert_many::<route_plugin::Entity>(rows.plugins, scope, tx)
        .await
        .map_err(internal)?;
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
                .await
                .map_err(internal)?;
        )+
    };
}

#[async_trait]
impl RouteRepository for DbRouteRepo {
    async fn create(&self, route: Route) -> Result<Route, RepositoryError> {
        let rows = route_rows(&route, OffsetDateTime::now_utc())?;
        let stored = route.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::for_tenant(stored.tenant_id);
                    secure_insert::<route::Entity>(rows.row, &scope, tx)
                        .await
                        .map_err(|e| route_create(e, &stored))?;
                    insert_children(tx, &scope, rows.children).await
                })
            })
            .await?;
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
            .project_all(&conn, |q| {
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
        let winner =
            select_route(&views, tenant_chain, upstream_ids, method, path).ok_or_else(no_match)?;
        let (winner_id, winner_tenant) = (winner.id, winner.tenant_id);
        let Some(path_prefix) = winner.match_rules.http.as_ref().map(|h| h.path.clone()) else {
            return Err(no_match());
        };

        // The winner in full: one row per (method, plugin binding), joined on
        // the composite foreign keys like the candidates.
        let rows: Vec<WinnerRow> = route::Entity::find()
            .filter(route::Column::Id.eq(winner_id))
            .filter(route::Column::Enabled.eq(true))
            .filter(route::Column::MatchType.eq(MATCH_TYPE_HTTP))
            .secure()
            .scope_with(&AccessScope::for_tenant(winner_tenant))
            .project_all(&conn, |q| {
                route_plugin::Column::iter()
                    .fold(
                        q.join_rev(JoinType::InnerJoin, route_method::Relation::Route.def())
                            .column_as(route_method::Column::Method, "method")
                            .join_rev(JoinType::LeftJoin, route_plugin::Relation::Route.def()),
                        |q, col| q.column_as(col, format!("{PLUGIN_PREFIX}{}", col.as_str())),
                    )
                    .into_model::<WinnerRow>()
            })
            .await
            .map_err(internal)?;
        let mut row = None;
        let mut methods: Vec<route_method::Model> = Vec::new();
        let mut plugins: BTreeMap<i32, route_plugin::Model> = BTreeMap::new();
        for WinnerRow {
            route,
            method,
            plugin,
        } in rows
        {
            if !methods.iter().any(|m| m.method == method) {
                methods.push(route_method::Model {
                    route_id: route.id,
                    tenant_id: route.tenant_id,
                    method,
                });
            }
            if let Some(plugin) = plugin {
                plugins.entry(plugin.position).or_insert(plugin);
            }
            row.get_or_insert(route);
        }
        let plugins: Vec<route_plugin::Model> = plugins.into_values().collect();
        // Deleted or disabled since the candidate load.
        let row = row.ok_or_else(no_match)?;

        let winner: [Parent; 1] = [(row.id, row.tenant_id)];
        let tags = match tags {
            Tags::Load => {
                children::<route_tag::Entity>(&conn, route_tag::Column::RouteId, &winner).await?
            }
            Tags::Skip => Vec::new(),
        };
        let http_match = route_http_match::Model {
            route_id: row.id,
            tenant_id: row.tenant_id,
            path_prefix,
        };
        route_from_rows(
            row,
            RouteChildren {
                http_match: Some(http_match),
                methods,
                grpc_match: None,
                tags,
                plugins,
            },
        )
    }

    async fn update(&self, route: Route) -> Result<Route, RepositoryError> {
        let rows = route_rows(&route, OffsetDateTime::now_utc())?;
        let stored = route.clone();
        let backend = self.db.db().backend();
        let (upstream_id, managed_by) = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::for_tenant(stored.tenant_id);
                    // The key, the upstream, the owner and `created_at` stay as stored.
                    let mut row = rows.row;
                    row.id = NotSet;
                    row.tenant_id = NotSet;
                    row.upstream_id = NotSet;
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
                            .map_err(internal)?;
                    let Some(updated) = updated else {
                        return Err(not_found(ENTITY, stored.id));
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
                        .map_err(|e| invalid_stored(ENTITY, stored.id, e))?;
                    Ok((updated.upstream_id, managed_by))
                })
            })
            .await?;
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

    async fn list_registry_keys(&self) -> Result<Vec<RowKey>, RepositoryError> {
        let conn = self.db.conn()?;
        let rows = route::Entity::find()
            .filter(route::Column::ManagedBy.eq(managed_by_text(ManagedBy::Registry)))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await
            .map_err(internal)?;
        // In key order, as the trait promises: sorted here rather than with a
        // two-column `ORDER BY`.
        let mut keys: Vec<RowKey> = rows
            .into_iter()
            .map(|r| RowKey {
                tenant_id: r.tenant_id,
                id: r.id,
            })
            .collect();
        keys.sort();
        Ok(keys)
    }
}

// Database-only behavior; the shared contract is in `conformance_tests`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::gts_helpers::HTTP_PROTOCOL_ID;
    use crate::domain::model::{
        Endpoint, HttpMatch, HttpMethod, MatchRules, PathSuffixMode, Scheme, Server, Upstream,
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

    /// The composite foreign key `(tenant_id, upstream_id)` rejects a route on
    /// a missing upstream or on another tenant's upstream; both read as the
    /// upstream not being found, and nothing is stored.
    #[tokio::test]
    async fn route_on_missing_or_foreign_upstream_is_upstream_not_found() {
        let db = DBProvider::<RepositoryError>::new(shared_sqlite().await);
        let upstreams = DbUpstreamRepo::new(db.clone());
        let routes = DbRouteRepo::new(db);
        let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
        let foreign = upstreams.create(upstream(other)).await.unwrap();

        for upstream_id in [Uuid::new_v4(), foreign.id] {
            let result = routes.create(route(tenant, upstream_id)).await;
            assert!(
                matches!(
                    result,
                    Err(RepositoryError::NotFound { entity: "upstream", id }) if id == upstream_id
                ),
                "got {result:?}"
            );
        }
        assert!(
            routes
                .list(tenant, None, &ListQuery::default())
                .await
                .unwrap()
                .is_empty()
        );
    }
}
