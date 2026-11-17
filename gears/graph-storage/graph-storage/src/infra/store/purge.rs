//! Purge: a node and every edge incident to it removed for good
//! (gears-rust #5194).
//!
//! A soft delete keeps the row -- payload, search text and vector included --
//! and the key with it; scope replacement removes rows, but only what a
//! producer stops declaring. Purge is the one call that removes a single
//! node's data on request, which an erasure obligation needs. It takes a live
//! node or a tombstoned one, so the tombstones soft deletes left behind can be
//! erased afterwards.
//!
//! Every incident edge goes first -- static, analysis, live or tombstoned --
//! because the endpoint foreign keys are `ON DELETE RESTRICT` and because an
//! erasure outranks a conclusion drawn about what it erases. Then the node.
//! An edge another batch attaches between the two statements makes the node
//! delete fail on the foreign key, which is a conflict the caller retries,
//! never a node removed from under a live edge.

use graph_storage_sdk::models::{GraphRevision, NodeKey, PurgeOptions, PurgeOutcome};
use graph_storage_sdk::plugin_api::{GraphStoreError, StoreCtx};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use toolkit_db::secure::{SecureDeleteExt, SecureEntityExt};

use crate::infra::projections::{PurgeTarget, purge_target_columns};
use crate::infra::storage::entity::{edge, node};
use crate::infra::store::ingest::{bump_revision, current_revision, source_epoch};
use crate::infra::store::{PgGraphStore, TxStoreError, map_scope_err};

pub async fn purge_node(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    key: NodeKey,
    options: PurgeOptions,
) -> Result<PurgeOutcome, GraphStoreError> {
    let tenant = ctx.tenant;
    let scope = ctx.scope.clone();
    store
        .db()
        .transaction_ref_mapped::<_, PurgeOutcome, TxStoreError>(move |tx| {
            let key = key.clone();
            let scope = scope.clone();
            Box::pin(async move {
                let epoch = source_epoch(&scope, tx).await?;
                // Live or tombstoned: both hold the key and the data.
                let stored = node::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(Condition::all().add(node::Column::NodeKey.eq(key.clone())))
                    .limit(1)
                    .project_all(tx, |query| {
                        purge_target_columns(query).into_model::<PurgeTarget>()
                    })
                    .await
                    .map_err(map_scope_err)?
                    .into_iter()
                    .next();
                let Some(stored) = stored else {
                    return Ok(PurgeOutcome {
                        revision: GraphRevision {
                            source_epoch: epoch,
                            revision: current_revision(&scope, tx).await?,
                        },
                        purged_nodes: 0,
                        purged_edges: 0,
                    });
                };

                if let Some(expected) = options.expected_created_at
                    && stored.created_at != expected
                {
                    return Err(TxStoreError(another_instance(
                        &key,
                        expected,
                        stored.created_at,
                    )));
                }
                if options.only_tombstoned && stored.deleted_at.is_none() {
                    return Err(TxStoreError(GraphStoreError::Conflict {
                        reason: format!(
                            "node `{key}` is live and the purge asked for a tombstoned one \
                             only; soft-delete it first, or purge without only_tombstoned"
                        ),
                    }));
                }

                // The node was found under the caller's whole scope; its edges
                // are deleted in the node's own tenant. The caller's scope
                // cannot serve: it can name resources by id, and `id` is each
                // entity's own column, so the node's id applied to `edge.id`
                // keeps its incident edges; and a scope with no tenant
                // constraint at all has no tenant part to narrow to. Either
                // way the edges stay and the foreign key refuses the node.
                // Erasing a node erases its edges in its tenant, whoever
                // declared them.
                let purged_edges = edge::Entity::delete_many()
                    .filter(
                        Condition::any()
                            .add(edge::Column::SrcNodeId.eq(stored.id))
                            .add(edge::Column::DstNodeId.eq(stored.id)),
                    )
                    .secure()
                    .scope_with(&toolkit_security::AccessScope::for_tenant(stored.tenant_id))
                    .exec(tx)
                    .await
                    .map_err(map_scope_err)?
                    .rows_affected;
                let purged_nodes = node::Entity::delete_many()
                    .filter(Condition::all().add(node::Column::Id.eq(stored.id)))
                    .secure()
                    .scope_with(&scope)
                    .exec(tx)
                    .await
                    .map_err(map_scope_err)?
                    .rows_affected;

                let revision = if purged_nodes + purged_edges > 0 {
                    bump_revision(tenant, &scope, tx).await?
                } else {
                    current_revision(&scope, tx).await?
                };
                Ok(PurgeOutcome {
                    revision: GraphRevision {
                        source_epoch: epoch,
                        revision,
                    },
                    purged_nodes,
                    purged_edges,
                })
            })
        })
        .await
        .map_err(|TxStoreError(error)| error)
}

/// The refusal of a purge bound by `expected_created_at` to a node that is
/// not the one stored under the key now. Shared with the in-memory store.
pub(crate) fn another_instance(
    key: &str,
    expected: time::OffsetDateTime,
    stored: time::OffsetDateTime,
) -> GraphStoreError {
    GraphStoreError::Conflict {
        reason: format!(
            "the node under key `{key}` was created at {stored}, not at {expected}: it is \
             another node than the one this purge names, and it is left alone"
        ),
    }
}
