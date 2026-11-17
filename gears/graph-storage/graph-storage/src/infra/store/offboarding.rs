//! Tenant offboarding on the built-in store
//! (`cpt-cf-graph-storage-fr-tenant-offboarding`, DESIGN § Tenant Offboarding
//! and Deletion Monotonicity).
//!
//! **The fence.** A row in `tenant_offboarding` fences its tenant. It has to
//! stop a write that is already running, not only the next one: a write that
//! passed its checks before the fence and commits after it would recreate
//! rows behind the deleter. So every write transaction runs inside
//! [`fenced_write`], which ends by taking the tenant's revision row and then
//! reading the fence, and [`fence`] takes the same row before it writes the
//! fence. The row is the one every write already updates when it changes
//! state, and the database orders the two:
//!
//! - the write takes the row first: the fence waits for it, the write sees no
//!   fence and commits, and its rows are there for the deleter, which starts
//!   only after the fence committed;
//! - the fence takes the row first: the write waits for it, and its read of
//!   the fence -- a new statement under read committed, so a new snapshot --
//!   sees the committed fence, and the write rolls back.
//!
//! Either way nothing commits for the tenant after its fence. [`fenced_write`]
//! also checks the fence when the write starts, so one that begins after the
//! fence fails as fenced before it does anything else.
//!
//! **The deletion.** [`step`] removes up to a batch of one kind of row per
//! transaction, in an order the foreign keys admit: edges (their endpoints
//! are `ON DELETE RESTRICT`), then nodes, then the types both reference, then
//! the rest. Tombstones and nodes held by analysis edges go too: an erasure
//! outranks a conclusion. Each step adds what it removed to the record in the
//! same transaction, and the step that finds nothing left stamps
//! `completed_at`. Two steps for one tenant -- two workers, say -- take the
//! record's row first and so run one after the other.
//!
//! `embedding_space` is not listed: its rows belong to the nil tenant, which
//! cannot be offboarded, and no tenant has one of its own.

use std::future::Future;
use std::num::NonZeroU64;

use graph_storage_sdk::models::{
    DeletionGeneration, OffboardingCounts, OffboardingFailure, TenantId, TenantOffboarding,
};
use graph_storage_sdk::plugin_api::{GraphStoreError, StoreCtx};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveValue, ColumnTrait, Condition, EntityTrait, ExprTrait, QueryFilter, QuerySelect,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureOnConflict, SecureUpdateExt,
};
use toolkit_security::AccessScope;

use crate::infra::storage::entity::{
    edge, graph_meta, gts_type, ingest_idempotency, node, scope_registry, source_namespace_owner,
    tenant_offboarding,
};
use crate::infra::store::{PgGraphStore, TxStoreError, map_db_error, map_scope_err};

/// Run one write transaction's work for `tenant` inside the fence.
///
/// Refuses at once when the tenant is already fenced, runs `body`, and, when
/// `wrote` says the body changed something, ends by taking the tenant's
/// revision row and reading the fence again, so the lock is held only to the
/// commit right after. A body that wrote nothing -- a replayed ingest -- does
/// not take the row, so it does not queue behind the tenant's other writes.
/// Every write transaction goes through here: a write path that skipped
/// either check would be a write the fence cannot stop.
///
/// # Errors
///
/// `TenantFenced` when the tenant is fenced; whatever `body` fails with;
/// any database failure.
pub async fn fenced_write<T, F>(
    tenant: TenantId,
    scope: &AccessScope,
    tx: &impl DBRunner,
    wrote: impl FnOnce(&T) -> bool,
    body: F,
) -> Result<T, GraphStoreError>
where
    F: Future<Output = Result<T, GraphStoreError>>,
{
    if fence_of(tenant, scope, tx).await?.is_some() {
        return Err(GraphStoreError::TenantFenced { tenant });
    }
    let outcome = body.await?;
    if wrote(&outcome) {
        lock_revision_row(tenant, scope, tx).await?;
        if fence_of(tenant, scope, tx).await?.is_some() {
            return Err(GraphStoreError::TenantFenced { tenant });
        }
    }
    Ok(outcome)
}

/// Take the tenant's revision row for the rest of the transaction, changing
/// nothing.
///
/// An upsert rather than an update, because the row may not exist yet: an
/// update of no row locks nothing, and a write and a fence that both found
/// no row would not be ordered at all. The insert writes `0`, which reads as
/// the absent row always has.
async fn lock_revision_row(
    tenant: TenantId,
    scope: &AccessScope,
    runner: &impl DBRunner,
) -> Result<(), GraphStoreError> {
    let active = graph_meta::ActiveModel {
        tenant_id: ActiveValue::Set(tenant),
        key: ActiveValue::Set(graph_meta::KEY_GRAPH_REVISION.to_owned()),
        value: ActiveValue::Set(serde_json::json!(0)),
    };
    let on_conflict = SecureOnConflict::<graph_meta::Entity>::columns([
        graph_meta::Column::TenantId,
        graph_meta::Column::Key,
    ])
    .value(
        graph_meta::Column::Value,
        Expr::col((graph_meta::Entity, graph_meta::Column::Value)),
    )
    .map_err(map_scope_err)?;
    graph_meta::Entity::insert(active)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .on_conflict(on_conflict)
        .exec(runner)
        .await
        .map_err(map_scope_err)?;
    Ok(())
}

async fn fence_of(
    tenant: TenantId,
    scope: &AccessScope,
    runner: &impl DBRunner,
) -> Result<Option<tenant_offboarding::Model>, GraphStoreError> {
    tenant_offboarding::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(tenant_offboarding::Column::TenantId.eq(tenant)))
        .one(runner)
        .await
        .map_err(map_scope_err)
}

/// The `removed` column as stored: the counts this build knows, and every
/// other key kept as it was, so a replica that does not know a kind a newer
/// one counted does not drop it when it writes the column back.
#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct StoredCounts {
    #[serde(default)]
    edges: u64,
    #[serde(default)]
    nodes: u64,
    #[serde(default)]
    types: u64,
    #[serde(default)]
    scopes: u64,
    #[serde(default)]
    receipts: u64,
    #[serde(default)]
    namespaces: u64,
    #[serde(default)]
    meta: u64,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl StoredCounts {
    /// Decode the column. A key that is there with a value that is not a
    /// count is corruption, not zero.
    fn of(value: &serde_json::Value) -> Result<Self, GraphStoreError> {
        serde_json::from_value(value.clone()).map_err(|error| GraphStoreError::Corrupt {
            reason: format!("the offboarding counts do not decode: {error}"),
        })
    }

    fn json(&self) -> Result<serde_json::Value, GraphStoreError> {
        serde_json::to_value(self).map_err(|error| {
            GraphStoreError::Internal(format!("the offboarding counts do not encode: {error}"))
        })
    }

    fn counts(&self) -> OffboardingCounts {
        let mut counts = OffboardingCounts::default();
        counts.edges = self.edges;
        counts.nodes = self.nodes;
        counts.types = self.types;
        counts.scopes = self.scopes;
        counts.receipts = self.receipts;
        counts.namespaces = self.namespaces;
        counts.meta = self.meta;
        counts
    }
}

/// The record as stored. A value the record cannot hold -- a generation
/// that is not positive, a negative failure count -- is corruption, not a
/// default.
fn to_record(row: &tenant_offboarding::Model) -> Result<TenantOffboarding, GraphStoreError> {
    let corrupt = |what: &str| GraphStoreError::Corrupt {
        reason: format!("the offboarding record of {} has {what}", row.tenant_id),
    };
    let deletion_generation = row
        .deletion_generation
        .map(|value| DeletionGeneration::new(value).ok_or_else(|| corrupt("a generation below 1")))
        .transpose()?;
    let failures = u32::try_from(row.failures).map_err(|_| corrupt("a negative failure count"))?;
    let last_failure = match (&row.last_failure, row.last_failure_at) {
        (Some(class), Some(at)) => Some(OffboardingFailure {
            class: class.clone(),
            at,
        }),
        _ => None,
    };
    Ok(TenantOffboarding {
        tenant_id: row.tenant_id,
        deletion_generation,
        fenced_at: row.fenced_at,
        completed_at: row.completed_at,
        removed: StoredCounts::of(&row.removed)?.counts(),
        failures,
        last_failure,
    })
}

/// Refuse the nil tenant: it holds the deployment's own rows (the source
/// epoch, the embedding spaces), which no tenant's offboarding may take.
fn offboardable(tenant: TenantId) -> Result<(), GraphStoreError> {
    if tenant.is_nil() {
        return Err(GraphStoreError::InvalidQuery {
            what: "the nil tenant holds the deployment's own state and cannot be offboarded"
                .to_owned(),
        });
    }
    Ok(())
}

/// Fence `ctx.tenant`, once, under the scope the policy granted, and answer
/// its record.
pub async fn fence(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    deletion_generation: Option<DeletionGeneration>,
) -> Result<TenantOffboarding, GraphStoreError> {
    let tenant = ctx.tenant;
    offboardable(tenant)?;
    let scope = ctx.scope.clone();
    store
        .db()
        .transaction_ref_mapped::<_, TenantOffboarding, TxStoreError>(move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                // The revision row first, as every write takes it: this is
                // what orders the fence against a write still running.
                lock_revision_row(tenant, &scope, tx).await?;
                let active = tenant_offboarding::ActiveModel {
                    tenant_id: ActiveValue::Set(tenant),
                    deletion_generation: ActiveValue::Set(
                        deletion_generation.map(DeletionGeneration::get),
                    ),
                    fenced_at: ActiveValue::Set(OffsetDateTime::now_utc()),
                    completed_at: ActiveValue::Set(None),
                    removed: ActiveValue::Set(StoredCounts::default().json()?),
                    failures: ActiveValue::Set(0),
                    last_failure: ActiveValue::Set(None),
                    last_failure_at: ActiveValue::Set(None),
                };
                // A tenant already fenced keeps its record as it is: the
                // first fence's time and generation stand.
                let on_conflict = SecureOnConflict::<tenant_offboarding::Entity>::columns([
                    tenant_offboarding::Column::TenantId,
                ])
                .value(
                    tenant_offboarding::Column::FencedAt,
                    Expr::col((
                        tenant_offboarding::Entity,
                        tenant_offboarding::Column::FencedAt,
                    )),
                )
                .map_err(map_scope_err)?;
                tenant_offboarding::Entity::insert(active)
                    .secure()
                    .scope_unchecked(&scope)
                    .map_err(map_scope_err)?
                    .on_conflict(on_conflict)
                    .exec(tx)
                    .await
                    .map_err(map_scope_err)?;
                let row = fence_of(tenant, &scope, tx).await?.ok_or_else(|| {
                    GraphStoreError::Internal("the fence just written is not there".to_owned())
                })?;
                Ok(to_record(&row)?)
            })
        })
        .await
        .map_err(|error| error.0)
}

/// `ctx.tenant`'s offboarding record, if it was ever fenced, read under
/// `ctx.scope`.
pub async fn record(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
) -> Result<Option<TenantOffboarding>, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    fence_of(ctx.tenant, ctx.scope, &conn)
        .await?
        .as_ref()
        .map(to_record)
        .transpose()
}

/// The tenants under `ctx.scope` whose deletion is running: the id column
/// only, nothing decoded, for the read path.
pub async fn fenced_ids(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
) -> Result<Vec<TenantId>, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let rows = tenant_offboarding::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(tenant_offboarding::Column::CompletedAt.is_null()))
        .project_all(&conn, |query| {
            query
                .select_only()
                .column(tenant_offboarding::Column::TenantId)
                .into_model::<TenantRowId>()
        })
        .await
        .map_err(map_scope_err)?;
    Ok(rows.into_iter().map(|row| row.tenant_id).collect())
}

/// A record's tenant id.
#[derive(Debug, sea_orm::FromQueryResult)]
struct TenantRowId {
    tenant_id: TenantId,
}

/// The records of fenced tenants whose deletion has not finished, oldest
/// fence first. A record that does not decode is left out and logged, so
/// one damaged row cannot stop every other tenant's deletion.
pub async fn pending(store: &PgGraphStore) -> Result<Vec<TenantOffboarding>, GraphStoreError> {
    // Across tenants by design: the worker serves every fenced tenant, and a
    // fence row reveals nothing but the id it is keyed by.
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let rows = tenant_offboarding::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(tenant_offboarding::Column::CompletedAt.is_null()))
        .order_by(tenant_offboarding::Column::FencedAt, sea_orm::Order::Asc)
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(rows
        .iter()
        .filter_map(|row| match to_record(row) {
            Ok(record) => Some(record),
            Err(error) => {
                tracing::error!(
                    tenant = %row.tenant_id,
                    error = %crate::infra::logged(&error),
                    "offboarding: a record does not decode; its deletion is skipped until it \
                     is repaired"
                );
                None
            }
        })
        .collect())
}

/// Note a failed step on the record, for the caller who reads it and for the
/// worker's backoff, and answer the record as it now stands.
pub async fn note_failure(
    store: &PgGraphStore,
    tenant: TenantId,
    error: &GraphStoreError,
) -> Result<TenantOffboarding, GraphStoreError> {
    let scope = AccessScope::for_tenant(tenant);
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    tenant_offboarding::Entity::update_many()
        .col_expr(
            tenant_offboarding::Column::Failures,
            Expr::col((
                tenant_offboarding::Entity,
                tenant_offboarding::Column::Failures,
            ))
            .add(1),
        )
        .col_expr(
            tenant_offboarding::Column::LastFailure,
            Expr::value(Some(error.class().to_owned())),
        )
        .col_expr(
            tenant_offboarding::Column::LastFailureAt,
            Expr::value(Some(OffsetDateTime::now_utc())),
        )
        .filter(Condition::all().add(tenant_offboarding::Column::TenantId.eq(tenant)))
        .secure()
        .scope_with(&scope)
        .exec(&conn)
        .await
        .map_err(map_scope_err)?;
    let row = fence_of(tenant, &scope, &conn)
        .await?
        .ok_or(GraphStoreError::NotFound)?;
    to_record(&row)
}

/// One kind of tenant-keyed state, in the order the deletion takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::EnumCount, strum::EnumIter)]
enum Kind {
    Edges,
    Nodes,
    Types,
    Scopes,
    Receipts,
    Namespaces,
    Meta,
}

/// The order the deletion takes the kinds in. The test below checks it
/// names every kind once, so a kind added to the enum and left out here
/// fails it rather than leave its rows behind a finished record.
const ORDER: [Kind; 7] = [
    Kind::Edges,
    Kind::Nodes,
    Kind::Types,
    Kind::Scopes,
    Kind::Receipts,
    Kind::Namespaces,
    Kind::Meta,
];

/// A row id, for the kinds whose rows are deleted by id.
#[derive(Debug, sea_orm::FromQueryResult)]
struct RowId {
    id: i64,
}

/// A type row id: `gts_type.id` is an `INTEGER`.
#[derive(Debug, sea_orm::FromQueryResult)]
struct TypeRowId {
    id: i32,
}

/// An idempotency receipt's key.
#[derive(Debug, sea_orm::FromQueryResult)]
struct ReceiptKey {
    producer: String,
    idempotency_key: String,
}

impl Kind {
    fn add_to(self, counts: &mut StoredCounts, removed: u64) {
        let slot = match self {
            Self::Edges => &mut counts.edges,
            Self::Nodes => &mut counts.nodes,
            Self::Types => &mut counts.types,
            Self::Scopes => &mut counts.scopes,
            Self::Receipts => &mut counts.receipts,
            Self::Namespaces => &mut counts.namespaces,
            Self::Meta => &mut counts.meta,
        };
        *slot += removed;
    }

    /// Remove up to `batch` rows of this kind; answer how many went.
    ///
    /// The keys are read first -- the key columns only -- and the rows
    /// deleted by key, since a `DELETE` takes no `LIMIT`. Every statement is
    /// under the tenant's scope.
    async fn remove(
        self,
        scope: &AccessScope,
        tx: &impl DBRunner,
        batch: u64,
    ) -> Result<u64, GraphStoreError> {
        let removed = match self {
            Self::Edges => {
                let ids: Vec<i64> = edge::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .limit(batch)
                    .project_all(tx, |query| {
                        query
                            .select_only()
                            .column(edge::Column::Id)
                            .into_model::<RowId>()
                    })
                    .await
                    .map_err(map_scope_err)?
                    .into_iter()
                    .map(|row| row.id)
                    .collect();
                if ids.is_empty() {
                    return Ok(0);
                }
                edge::Entity::delete_many()
                    .filter(Condition::all().add(edge::Column::Id.is_in(ids)))
                    .secure()
                    .scope_with(scope)
                    .exec(tx)
                    .await
            }
            Self::Nodes => {
                let ids: Vec<i64> = node::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .limit(batch)
                    .project_all(tx, |query| {
                        query
                            .select_only()
                            .column(node::Column::Id)
                            .into_model::<RowId>()
                    })
                    .await
                    .map_err(map_scope_err)?
                    .into_iter()
                    .map(|row| row.id)
                    .collect();
                if ids.is_empty() {
                    return Ok(0);
                }
                node::Entity::delete_many()
                    .filter(Condition::all().add(node::Column::Id.is_in(ids)))
                    .secure()
                    .scope_with(scope)
                    .exec(tx)
                    .await
            }
            Self::Types => {
                let ids: Vec<i32> = gts_type::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .limit(batch)
                    .project_all(tx, |query| {
                        query
                            .select_only()
                            .column(gts_type::Column::Id)
                            .into_model::<TypeRowId>()
                    })
                    .await
                    .map_err(map_scope_err)?
                    .into_iter()
                    .map(|row| row.id)
                    .collect();
                if ids.is_empty() {
                    return Ok(0);
                }
                gts_type::Entity::delete_many()
                    .filter(Condition::all().add(gts_type::Column::Id.is_in(ids)))
                    .secure()
                    .scope_with(scope)
                    .exec(tx)
                    .await
            }
            Self::Scopes => {
                let rows = scope_registry::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .limit(batch)
                    .all(tx)
                    .await
                    .map_err(map_scope_err)?;
                if rows.is_empty() {
                    return Ok(0);
                }
                let mut which = Condition::any();
                for row in rows {
                    which = which.add(
                        Condition::all()
                            .add(scope_registry::Column::ScopeAttribute.eq(row.scope_attribute))
                            .add(scope_registry::Column::ScopeValue.eq(row.scope_value)),
                    );
                }
                scope_registry::Entity::delete_many()
                    .filter(which)
                    .secure()
                    .scope_with(scope)
                    .exec(tx)
                    .await
            }
            Self::Receipts => {
                // The response column is the batch's whole outcome; only the
                // key is needed to delete it.
                let rows = ingest_idempotency::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .limit(batch)
                    .project_all(tx, |query| {
                        query
                            .select_only()
                            .column(ingest_idempotency::Column::Producer)
                            .column(ingest_idempotency::Column::IdempotencyKey)
                            .into_model::<ReceiptKey>()
                    })
                    .await
                    .map_err(map_scope_err)?;
                if rows.is_empty() {
                    return Ok(0);
                }
                let mut which = Condition::any();
                for row in rows {
                    which = which.add(
                        Condition::all()
                            .add(ingest_idempotency::Column::Producer.eq(row.producer))
                            .add(
                                ingest_idempotency::Column::IdempotencyKey.eq(row.idempotency_key),
                            ),
                    );
                }
                ingest_idempotency::Entity::delete_many()
                    .filter(which)
                    .secure()
                    .scope_with(scope)
                    .exec(tx)
                    .await
            }
            Self::Namespaces => {
                let names: Vec<String> = source_namespace_owner::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .limit(batch)
                    .all(tx)
                    .await
                    .map_err(map_scope_err)?
                    .into_iter()
                    .map(|row| row.namespace)
                    .collect();
                if names.is_empty() {
                    return Ok(0);
                }
                source_namespace_owner::Entity::delete_many()
                    .filter(
                        Condition::all()
                            .add(source_namespace_owner::Column::Namespace.is_in(names)),
                    )
                    .secure()
                    .scope_with(scope)
                    .exec(tx)
                    .await
            }
            Self::Meta => {
                // The scope keeps the nil tenant's rows -- the deployment's
                // source epoch -- out of it.
                let keys: Vec<String> = graph_meta::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .limit(batch)
                    .all(tx)
                    .await
                    .map_err(map_scope_err)?
                    .into_iter()
                    .map(|row| row.key)
                    .collect();
                if keys.is_empty() {
                    return Ok(0);
                }
                graph_meta::Entity::delete_many()
                    .filter(Condition::all().add(graph_meta::Column::Key.is_in(keys)))
                    .secure()
                    .scope_with(scope)
                    .exec(tx)
                    .await
            }
        };
        Ok(removed.map_err(map_scope_err)?.rows_affected)
    }
}

/// Remove up to `batch` more of a fenced tenant's rows and answer its
/// record.
pub async fn step(
    store: &PgGraphStore,
    tenant: TenantId,
    batch: NonZeroU64,
) -> Result<TenantOffboarding, GraphStoreError> {
    offboardable(tenant)?;
    let batch = batch.get();
    store
        .db()
        .transaction_ref_mapped::<_, TenantOffboarding, TxStoreError>(move |tx| {
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant);
                // Take the record's row before reading it, so two steps for
                // one tenant run one after the other and neither overwrites
                // the counts the other added. A tenant never fenced has no
                // row to take.
                let taken = tenant_offboarding::Entity::update_many()
                    .col_expr(
                        tenant_offboarding::Column::FencedAt,
                        Expr::col((
                            tenant_offboarding::Entity,
                            tenant_offboarding::Column::FencedAt,
                        )),
                    )
                    .filter(Condition::all().add(tenant_offboarding::Column::TenantId.eq(tenant)))
                    .secure()
                    .scope_with(&scope)
                    .exec(tx)
                    .await
                    .map_err(map_scope_err)?;
                if taken.rows_affected == 0 {
                    return Err(GraphStoreError::NotFound.into());
                }
                let row = fence_of(tenant, &scope, tx)
                    .await?
                    .ok_or(GraphStoreError::NotFound)?;
                if row.completed_at.is_some() {
                    return Ok(to_record(&row)?);
                }
                let mut counts = StoredCounts::of(&row.removed)?;
                let mut removed_any = false;
                for kind in ORDER {
                    let removed = kind.remove(&scope, tx, batch).await?;
                    if removed > 0 {
                        kind.add_to(&mut counts, removed);
                        removed_any = true;
                        break;
                    }
                }
                let completed_at = (!removed_any).then(OffsetDateTime::now_utc);
                // A step that ran clears the failure streak: the deletion is
                // moving again.
                let mut update = tenant_offboarding::Entity::update_many()
                    .col_expr(
                        tenant_offboarding::Column::Removed,
                        Expr::value(counts.json()?),
                    )
                    .col_expr(tenant_offboarding::Column::Failures, Expr::value(0))
                    .col_expr(
                        tenant_offboarding::Column::LastFailure,
                        Expr::value(Option::<String>::None),
                    )
                    .col_expr(
                        tenant_offboarding::Column::LastFailureAt,
                        Expr::value(Option::<OffsetDateTime>::None),
                    )
                    .filter(Condition::all().add(tenant_offboarding::Column::TenantId.eq(tenant)));
                if let Some(done) = completed_at {
                    update = update.col_expr(
                        tenant_offboarding::Column::CompletedAt,
                        Expr::value(Some(done)),
                    );
                }
                update
                    .secure()
                    .scope_with(&scope)
                    .exec(tx)
                    .await
                    .map_err(map_scope_err)?;
                Ok(TenantOffboarding {
                    completed_at,
                    removed: counts.counts(),
                    failures: 0,
                    last_failure: None,
                    ..to_record(&row)?
                })
            })
        })
        .await
        .map_err(|error| error.0)
}

#[cfg(test)]
mod tests {
    use strum::{EnumCount as _, IntoEnumIterator as _};

    use super::{Kind, ORDER, StoredCounts};

    /// Every kind is in the order, once: a kind left out would never be
    /// deleted while the record says the deletion finished.
    #[test]
    fn the_order_names_every_kind_once() {
        assert_eq!(ORDER.len(), Kind::COUNT);
        for kind in Kind::iter() {
            assert_eq!(
                ORDER.iter().filter(|listed| **listed == kind).count(),
                1,
                "{kind:?}"
            );
        }
    }

    /// A key this build does not know survives a write back; a known key
    /// with a value that is not a count is an error, not zero.
    #[test]
    fn the_counts_keep_unknown_keys_and_refuse_a_bad_value() {
        let stored = serde_json::json!({"edges": 2, "chunks": 7});
        let mut counts = StoredCounts::of(&stored).expect("decodes");
        Kind::Nodes.add_to(&mut counts, 3);
        let written = counts.json().expect("encodes");
        assert_eq!(written["chunks"], 7, "{written}");
        assert_eq!(written["edges"], 2);
        assert_eq!(written["nodes"], 3);

        assert!(StoredCounts::of(&serde_json::json!({"edges": "two"})).is_err());
        assert!(StoredCounts::of(&serde_json::json!({"edges": -1})).is_err());
    }
}
