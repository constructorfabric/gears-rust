//! `DbUpstreamRepo`: `UpstreamRepository` on the `oagw_upstream*` tables.
//!
//! `get_by_id` and `list` are one query for the upstream rows plus one batched
//! query per child table (tags, plugins): 3, whatever the page size.
//! `list_by_alias_for_tenants` joins the plugins into the upstream query and
//! loads tags only on request: 1 query (2 with tags), whatever the tenant count.
//! `get_by_id` and `list` read in one read-only snapshot transaction; the
//! proxy's `list_by_alias_for_tenants` runs in none (ADR-0018 *Transactions*).
//! Creates and updates run in one transaction and replace child rows by
//! delete-then-insert; `delete` is one statement (cascade).
//! `list_registry_keys` is the one read across all tenants: the startup
//! registry reconcile owns rows in any tenant (ADR-0018 *Tenant Scope*).

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use sea_orm::ActiveValue::NotSet;
use sea_orm::{
    ColumnTrait, EntityTrait, FromQueryResult, IdenStatic, Iterable, JoinType, Order, QueryFilter,
    QuerySelect, RelationTrait,
};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
    secure_insert_many,
};
use uuid::Uuid;

use super::entity::{upstream, upstream_plugin, upstream_tag};
use super::error::{internal, invalid_stored, not_found, upstream_create, upstream_update};
use super::load::{Parent, by_parent, children, max_in_list, snapshot, update_one};
use super::mapper::{managed_by_text, parse_managed_by, upstream_from_rows, upstream_rows};
use crate::domain::model::{ListQuery, ManagedBy, Upstream};
use crate::domain::repo::{RepositoryError, RowKey, Tags, UpstreamRepository};

const ENTITY: &str = "upstream";

/// Column prefix of the plugin binding in [`UpstreamPluginRow`].
const PLUGIN_PREFIX: &str = "plugin_";

/// An upstream joined with one of its plugin bindings; `plugin` is `None` for
/// an upstream without bindings (`LEFT JOIN`).
#[derive(FromQueryResult)]
struct UpstreamPluginRow {
    #[sea_orm(nested)]
    upstream: upstream::Model,
    #[sea_orm(nested(prefix = "plugin_"))]
    plugin: Option<upstream_plugin::Model>,
}

/// Database-backed upstream repository.
pub(crate) struct DbUpstreamRepo {
    db: DBProvider<RepositoryError>,
}

impl DbUpstreamRepo {
    /// Create a repository over the given database provider.
    pub(crate) fn new(db: DBProvider<RepositoryError>) -> Self {
        Self { db }
    }
}

/// Load the tags and plugins of `rows` (one query each) and build the
/// aggregates, in the order of `rows`.
async fn assemble(
    runner: &impl DBRunner,
    rows: Vec<upstream::Model>,
) -> Result<Vec<Upstream>, RepositoryError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let parents: Vec<Parent> = rows.iter().map(|r| (r.id, r.tenant_id)).collect();
    let mut tags = by_parent(
        children::<upstream_tag::Entity>(runner, upstream_tag::Column::UpstreamId, &parents)
            .await?,
        |t| t.upstream_id,
    );
    let mut plugins = by_parent(
        children::<upstream_plugin::Entity>(runner, upstream_plugin::Column::UpstreamId, &parents)
            .await?,
        |p| p.upstream_id,
    );
    rows.into_iter()
        .map(|row| {
            let id = row.id;
            upstream_from_rows(
                row,
                tags.remove(&id).unwrap_or_default(),
                plugins.remove(&id).unwrap_or_default(),
            )
        })
        .collect()
}

#[async_trait]
impl UpstreamRepository for DbUpstreamRepo {
    async fn create(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        let rows = upstream_rows(&upstream, OffsetDateTime::now_utc())?;
        let stored = upstream.clone();
        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::for_tenant(stored.tenant_id);
                    secure_insert::<upstream::Entity>(rows.row, &scope, tx)
                        .await
                        .map_err(|e| upstream_create(e, &stored))?;
                    secure_insert_many::<upstream_tag::Entity>(rows.tags, &scope, tx)
                        .await
                        .map_err(internal)?;
                    secure_insert_many::<upstream_plugin::Entity>(rows.plugins, &scope, tx)
                        .await
                        .map_err(internal)?;
                    Ok(())
                })
            })
            .await?;
        Ok(upstream)
    }

    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Upstream, RepositoryError> {
        self.db
            .transaction_with_config(snapshot(), move |tx| {
                Box::pin(async move {
                    let row = upstream::Entity::find()
                        .filter(upstream::Column::Id.eq(id))
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
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        let (skip, top) = (u64::from(query.skip), u64::from(query.top));
        self.db
            .transaction_with_config(snapshot(), move |tx| {
                Box::pin(async move {
                    let rows = upstream::Entity::find()
                        .secure()
                        .scope_with(&AccessScope::for_tenant(tenant_id))
                        .order_by(upstream::Column::Id, Order::Asc)
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

    async fn update(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        let rows = upstream_rows(&upstream, OffsetDateTime::now_utc())?;
        let stored = upstream.clone();
        let backend = self.db.db().backend();
        let managed_by = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::for_tenant(stored.tenant_id);
                    // The key, the owner and `created_at` stay as stored.
                    let mut row = rows.row;
                    row.id = NotSet;
                    row.tenant_id = NotSet;
                    row.managed_by = NotSet;
                    row.created_at = NotSet;
                    let update = upstream::Entity::update_many()
                        .set(row)
                        .filter(upstream::Column::Id.eq(stored.id))
                        .secure()
                        .scope_with(&scope);
                    let updated =
                        update_one(tx, backend, update, upstream::Column::Id, stored.id, &scope)
                            .await
                            .map_err(|e| upstream_update(e, &stored))?;
                    let Some(updated) = updated else {
                        return Err(not_found(ENTITY, stored.id));
                    };

                    upstream_tag::Entity::delete_many()
                        .filter(upstream_tag::Column::UpstreamId.eq(stored.id))
                        .secure()
                        .scope_with(&scope)
                        .exec(tx)
                        .await
                        .map_err(internal)?;
                    upstream_plugin::Entity::delete_many()
                        .filter(upstream_plugin::Column::UpstreamId.eq(stored.id))
                        .secure()
                        .scope_with(&scope)
                        .exec(tx)
                        .await
                        .map_err(internal)?;
                    secure_insert_many::<upstream_tag::Entity>(rows.tags, &scope, tx)
                        .await
                        .map_err(internal)?;
                    secure_insert_many::<upstream_plugin::Entity>(rows.plugins, &scope, tx)
                        .await
                        .map_err(internal)?;
                    parse_managed_by(&updated.managed_by)
                        .map_err(|e| invalid_stored(ENTITY, stored.id, e))
                })
            })
            .await?;
        Ok(Upstream {
            managed_by,
            ..upstream
        })
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        let conn = self.db.conn()?;
        // Tags, plugins, routes and the routes' children go by cascade.
        let deleted = upstream::Entity::delete_many()
            .filter(upstream::Column::Id.eq(id))
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

    /// One query per `max_in_list` tenants (one for any realistic tree),
    /// with the plugins joined in, plus one for the tags when they are loaded.
    async fn list_by_alias_for_tenants(
        &self,
        alias: &str,
        tenant_ids: &HashSet<Uuid>,
        tags: Tags,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        if tenant_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.conn()?;
        let tenants: Vec<Uuid> = tenant_ids.iter().copied().collect();
        let mut rows: Vec<(upstream::Model, Vec<upstream_plugin::Model>)> = Vec::new();
        let mut index: HashMap<Uuid, usize> = HashMap::new();
        // The alias takes one parameter.
        for chunk in tenants.chunks(max_in_list(&conn, 1)) {
            let joined: Vec<UpstreamPluginRow> = upstream::Entity::find()
                .filter(upstream::Column::Alias.eq(alias))
                .secure()
                .scope_with(&AccessScope::for_tenants(chunk.to_vec()))
                .project_all(&conn, |q| {
                    // The join key includes the tenant, so every binding
                    // shares its scoped upstream's tenant
                    // (`entity::upstream_plugin::Relation`).
                    upstream_plugin::Column::iter()
                        .fold(
                            q.join_rev(
                                JoinType::LeftJoin,
                                upstream_plugin::Relation::Upstream.def(),
                            ),
                            |q, col| q.column_as(col, format!("{PLUGIN_PREFIX}{}", col.as_str())),
                        )
                        .into_model::<UpstreamPluginRow>()
                })
                .await
                .map_err(internal)?;
            for UpstreamPluginRow { upstream, plugin } in joined {
                let at = *index.entry(upstream.id).or_insert_with(|| {
                    rows.push((upstream, Vec::new()));
                    rows.len() - 1
                });
                rows[at].1.extend(plugin);
            }
        }
        let mut tag_rows = match tags {
            Tags::Load => {
                let parents: Vec<Parent> = rows.iter().map(|(r, _)| (r.id, r.tenant_id)).collect();
                by_parent(
                    children::<upstream_tag::Entity>(
                        &conn,
                        upstream_tag::Column::UpstreamId,
                        &parents,
                    )
                    .await?,
                    |t| t.upstream_id,
                )
            }
            Tags::Skip => HashMap::new(),
        };
        rows.into_iter()
            .map(|(row, plugins)| {
                let tags = tag_rows.remove(&row.id).unwrap_or_default();
                upstream_from_rows(row, tags, plugins)
            })
            .collect()
    }

    async fn list_registry_keys(&self) -> Result<Vec<RowKey>, RepositoryError> {
        let conn = self.db.conn()?;
        let rows = upstream::Entity::find()
            .filter(upstream::Column::ManagedBy.eq(managed_by_text(ManagedBy::Registry)))
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
