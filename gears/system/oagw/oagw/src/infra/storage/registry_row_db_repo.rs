//! `DbRegistryRowRepo`: `RegistryRowRepository` on the `oagw_upstream*` and
//! `oagw_route*` tables.
//!
//! The registry reconcile's reads are the only reads across all tenants: it
//! owns registry rows in any tenant (ADR-0018 *Tenant Scope*), so they run
//! under [`registry_reconcile_scope`] and filter on `managed_by`. Upstreams
//! take 3 queries and routes 6, as `list` does, each in one read-only
//! snapshot transaction. `delete_upstream_if_unused` locks the registry
//! upstream row (`upstream_lock`), checks for a route and deletes in one
//! READ COMMITTED transaction, retried on contention.

use async_trait::async_trait;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::secure::{AccessScope, ScopeError, SecureDeleteExt, SecureEntityExt};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use super::entity::{route, upstream};
use super::error::internal;
use super::load::snapshot;
use super::mapper::managed_by_text;
#[cfg(all(test, feature = "integration"))]
use super::upstream_lock::{Hold, HoldBeforeCommit};
use super::upstream_lock::{lock_upstream, read_committed_for};
use crate::domain::model::{ManagedBy, Route, Upstream};
use crate::domain::repo::{DeleteOutcome, RegistryRowRepository, RepositoryError};

/// Database-backed registry rows.
pub(crate) struct DbRegistryRowRepo {
    db: DBProvider<RepositoryError>,
    /// Holds a delete's transaction open before its commit.
    #[cfg(all(test, feature = "integration"))]
    hold: Hold,
}

impl DbRegistryRowRepo {
    /// Create a repository over the given database provider.
    pub(crate) fn new(db: DBProvider<RepositoryError>) -> Self {
        Self {
            db,
            #[cfg(all(test, feature = "integration"))]
            hold: Hold::default(),
        }
    }

    /// Hold the next `delete_upstream_if_unused` transaction open after its
    /// last statement: it signals `locked` and commits once `release` fires.
    #[cfg(all(test, feature = "integration"))]
    pub(crate) fn hold_next_delete(&self, hold: HoldBeforeCommit) {
        self.hold.set(hold);
    }
}

/// The cross-tenant scope of the registry reconcile's reads (see the module
/// doc).
fn registry_reconcile_scope() -> AccessScope {
    AccessScope::allow_all()
}

/// A failure inside the delete transaction. Keeps the driver error, which
/// [`RepositoryError`] turns into text, so the retry helper can tell
/// contention from other failures.
#[derive(Debug)]
enum DeleteTxError {
    Scope(ScopeError),
    Db(DbError),
}

impl From<DbError> for DeleteTxError {
    fn from(e: DbError) -> Self {
        Self::Db(e)
    }
}

/// The driver error of a [`DeleteTxError`], for contention classification.
fn delete_tx_db_err(e: &DeleteTxError) -> Option<&sea_orm::DbErr> {
    match e {
        DeleteTxError::Scope(ScopeError::Db(inner)) | DeleteTxError::Db(DbError::Sea(inner)) => {
            Some(inner)
        }
        _ => None,
    }
}

#[async_trait]
impl RegistryRowRepository for DbRegistryRowRepo {
    async fn list_upstreams(&self) -> Result<Vec<Upstream>, RepositoryError> {
        let mut upstreams = self
            .db
            .transaction_with_config(snapshot(), move |tx| {
                Box::pin(async move {
                    let rows = upstream::Entity::find()
                        .filter(
                            upstream::Column::ManagedBy.eq(managed_by_text(ManagedBy::Registry)),
                        )
                        .secure()
                        .scope_with(&registry_reconcile_scope())
                        .all(tx)
                        .await
                        .map_err(internal)?;
                    super::upstream_db_repo::assemble(tx, rows).await
                })
            })
            .await?;
        // In key order, as the trait promises: sorted here rather than with a
        // two-column `ORDER BY`.
        upstreams.sort_by_key(|u| (u.tenant_id, u.id));
        Ok(upstreams)
    }

    async fn list_routes(&self) -> Result<Vec<Route>, RepositoryError> {
        let mut routes = self
            .db
            .transaction_with_config(snapshot(), move |tx| {
                Box::pin(async move {
                    let rows = route::Entity::find()
                        .filter(route::Column::ManagedBy.eq(managed_by_text(ManagedBy::Registry)))
                        .secure()
                        .scope_with(&registry_reconcile_scope())
                        .all(tx)
                        .await
                        .map_err(internal)?;
                    super::route_db_repo::assemble(tx, rows).await
                })
            })
            .await?;
        // In key order, as the trait promises.
        routes.sort_by_key(|r| (r.tenant_id, r.id));
        Ok(routes)
    }

    async fn delete_upstream_if_unused(
        &self,
        tenant_id: Uuid,
        id: Uuid,
    ) -> Result<DeleteOutcome, RepositoryError> {
        let db = self.db.db();
        let backend = db.backend();
        let config = read_committed_for(backend);
        #[cfg(all(test, feature = "integration"))]
        let hold = self.hold.clone();
        db.transaction_with_retry(config, delete_tx_db_err, move |tx| {
            #[cfg(all(test, feature = "integration"))]
            let hold = hold.clone();
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant_id);
                // Only a registry row qualifies: an `api` upstream reads as
                // absent.
                let locked = lock_upstream(tx, backend, tenant_id, id, Some(ManagedBy::Registry))
                    .await
                    .map_err(DeleteTxError::Scope)?;
                if !locked {
                    return Ok(DeleteOutcome::NotFound);
                }
                let route = route::Entity::find()
                    .filter(route::Column::UpstreamId.eq(id))
                    .secure()
                    .scope_with(&scope)
                    .limit(1)
                    .one(tx)
                    .await
                    .map_err(DeleteTxError::Scope)?;
                let outcome = if route.is_some() {
                    DeleteOutcome::InUse
                } else {
                    upstream::Entity::delete_many()
                        .filter(upstream::Column::Id.eq(id))
                        .secure()
                        .scope_with(&scope)
                        .exec(tx)
                        .await
                        .map_err(DeleteTxError::Scope)?;
                    DeleteOutcome::Deleted
                };
                #[cfg(all(test, feature = "integration"))]
                hold.wait().await;
                Ok(outcome)
            })
        })
        .await
        .map_err(|e| match e {
            DeleteTxError::Scope(e) => internal(e),
            DeleteTxError::Db(e) => RepositoryError::from(e),
        })
    }
}

/// `delete_upstream_if_unused` against a concurrent route insert on
/// `PostgreSQL`, `MySQL` and `SQLite`, with two connections: whichever takes the
/// upstream row first, the route is never orphaned and never cascaded away.
#[cfg(all(test, feature = "integration"))]
mod race_tests {
    use std::time::Duration;

    use time::OffsetDateTime;
    use tokio::sync::oneshot;
    use toolkit_db::Db;
    use toolkit_db::secure::secure_insert;

    use super::*;
    use crate::domain::gts_helpers::HTTP_PROTOCOL_ID;
    use crate::domain::model::{
        Endpoint, HttpMatch, HttpMethod, MatchRules, PathSuffixMode, Scheme, Server,
    };
    use crate::domain::repo::{RouteRepository, UpstreamRepository};
    use crate::infra::storage::mapper::route_rows;
    use crate::infra::storage::route_db_repo::DbRouteRepo;
    use crate::infra::storage::testing;
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
            managed_by: ManagedBy::Registry,
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
            tags: vec![],
            priority: 0,
            enabled: true,
            managed_by: ManagedBy::Api,
        }
    }

    /// Long enough for a blocked statement to show it is blocked.
    const BLOCKED: Duration = Duration::from_millis(300);

    /// The delete takes the row lock first and is held open after its
    /// delete. An enabled route's write waits on the same upstream lock,
    /// then finds no upstream; a disabled route takes no lock, so its
    /// foreign key check waits on the row lock, then fails. Both read as
    /// the upstream not being found.
    ///
    /// ADR-0018: PERS-RACE-001.
    async fn a_route_insert_behind_the_lock_is_rejected(db: Db, enabled: bool) {
        let db = DBProvider::<RepositoryError>::new(db);
        let upstreams = DbUpstreamRepo::new(db.clone());
        let routes = DbRouteRepo::new(db.clone());
        let rows = std::sync::Arc::new(DbRegistryRowRepo::new(db));
        let tenant = Uuid::new_v4();
        let target = upstreams.create(upstream(tenant)).await.unwrap();

        let (locked_tx, locked_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        rows.hold_next_delete(HoldBeforeCommit {
            locked: locked_tx,
            release: release_rx,
        });
        let delete = tokio::spawn({
            let rows = rows.clone();
            async move { rows.delete_upstream_if_unused(tenant, target.id).await }
        });
        locked_rx.await.expect("the delete locked the row");

        let new_route = Route {
            enabled,
            ..route(tenant, target.id)
        };
        let insert = tokio::spawn(async move { routes.create(new_route).await });
        tokio::time::sleep(BLOCKED).await;
        assert!(
            !insert.is_finished(),
            "the insert must wait on the row lock"
        );

        release_tx.send(()).unwrap();
        assert_eq!(delete.await.unwrap().unwrap(), DeleteOutcome::Deleted);
        let result = insert.await.unwrap();
        assert!(
            matches!(
                result,
                Err(RepositoryError::NotFound {
                    entity: "upstream",
                    ..
                })
            ),
            "got {result:?}"
        );
    }

    /// The route insert comes first and is still uncommitted: the delete
    /// waits on the row lock, then sees the committed route and keeps the
    /// upstream.
    ///
    /// ADR-0018: PERS-RACE-002.
    async fn a_delete_behind_an_uncommitted_route_insert_keeps_the_upstream(db: Db) {
        let db = DBProvider::<RepositoryError>::new(db);
        let upstreams = DbUpstreamRepo::new(db.clone());
        let rows = DbRegistryRowRepo::new(db.clone());
        let tenant = Uuid::new_v4();
        let target = upstreams.create(upstream(tenant)).await.unwrap();
        let new_route = route(tenant, target.id);

        let (inserted_tx, inserted_rx) = oneshot::channel();
        let (commit_tx, commit_rx) = oneshot::channel::<()>();
        let inserter = tokio::spawn({
            let db = db.clone();
            let new_route = new_route.clone();
            async move {
                let rows = route_rows(&new_route, OffsetDateTime::now_utc())?;
                db.transaction(move |tx| {
                    Box::pin(async move {
                        let scope = AccessScope::for_tenant(tenant);
                        secure_insert::<route::Entity>(rows.row, &scope, tx)
                            .await
                            .map_err(internal)?;
                        let _ = inserted_tx.send(());
                        let _ = commit_rx.await;
                        Ok(())
                    })
                })
                .await
            }
        });
        inserted_rx.await.expect("the route row is inserted");

        let delete = tokio::spawn(async move {
            rows.delete_upstream_if_unused(tenant, target.id)
                .await
                .map(|outcome| (outcome, upstreams))
        });
        tokio::time::sleep(BLOCKED).await;
        assert!(
            !delete.is_finished(),
            "the delete must wait on the row lock"
        );

        commit_tx.send(()).unwrap();
        inserter.await.unwrap().unwrap();
        let (outcome, upstreams) = delete.await.unwrap().unwrap();
        assert_eq!(outcome, DeleteOutcome::InUse);
        assert!(upstreams.get_by_id(tenant, target.id).await.is_ok());
        // Only the route row was inserted, so look at the row itself.
        let conn = db.conn().unwrap();
        let row = route::Entity::find()
            .filter(route::Column::Id.eq(new_route.id))
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant))
            .one(&conn)
            .await
            .unwrap();
        assert!(row.is_some(), "the route survives");
    }

    #[tokio::test]
    async fn races_on_postgres() {
        let server = testing::postgres_server().await;
        for enabled in [true, false] {
            a_route_insert_behind_the_lock_is_rejected(server.fresh().await, enabled).await;
        }
        a_delete_behind_an_uncommitted_route_insert_keeps_the_upstream(server.fresh().await).await;
    }

    #[tokio::test]
    async fn races_on_mysql() {
        let server = testing::mysql_server().await;
        for enabled in [true, false] {
            a_route_insert_behind_the_lock_is_rejected(server.fresh().await, enabled).await;
        }
        a_delete_behind_an_uncommitted_route_insert_keeps_the_upstream(server.fresh().await).await;
    }

    /// `SQLite` has no row lock: the delete's lock is the database's write
    /// lock, so the other writer waits for it (a file, so the pool's
    /// connections lock as separate processes would).
    #[tokio::test]
    async fn races_on_sqlite() {
        for enabled in [true, false] {
            let file = testing::SqliteFile::new();
            a_route_insert_behind_the_lock_is_rejected(file.open().await, enabled).await;
        }
        let file = testing::SqliteFile::new();
        a_delete_behind_an_uncommitted_route_insert_keeps_the_upstream(file.open().await).await;
    }
}
