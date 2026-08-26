//! The built-in `PostgreSQL` store: one implementation of `GraphStoreV1`,
//! registered exactly as an external plugin would be.
//!
//! Every statement goes through the secure ORM — `.secure().scope_with(..)`
//! or `secure_insert`/`scope_unchecked` on inserts, which cannot subtree-clamp
//! a row that does not exist yet. There is no unscoped query API in this
//! module.

pub mod ingest;
pub mod reads;
pub mod search;
pub mod types;

use std::sync::Arc;

use async_trait::async_trait;
use graph_storage_sdk::models::{
    DeleteOutcome, DeleteRequest, GraphRevision, GtsTypeId, IngestOutcome, IngestRequest, LabelId,
    LabelRecord, LabelSpec, NodeId, NodeKey, NodeRow, NodeView, Page, ProjectionRequest,
    ReadSnapshot, RevisionOutcome, SearchRequest, SearchResponse, StoreCapabilities, TopologyPage,
    TopologyRequest, TypeIdSet, TypeQuery, TypeRecord, TypeRegistration,
};
use graph_storage_sdk::plugin_api::{GraphStoreError, GraphStoreV1, StoreCtx};
use toolkit_db::secure::{Db, ScopeError};

use crate::config::GraphStorageConfig;

/// The built-in store.
pub struct PgGraphStore {
    db: Arc<Db>,
    config: GraphStorageConfig,
    /// Whether this server parses SQL/PGQ, probed once at init. Reads use it
    /// to decide the hop backend; nothing else depends on it.
    pgq_available: bool,
}

impl PgGraphStore {
    #[must_use]
    pub fn new(db: Arc<Db>, config: GraphStorageConfig, pgq_available: bool) -> Self {
        Self {
            db,
            config,
            pgq_available,
        }
    }

    #[must_use]
    pub fn db(&self) -> &Db {
        &self.db
    }

    #[must_use]
    pub fn config(&self) -> &GraphStorageConfig {
        &self.config
    }

    #[must_use]
    pub fn pgq_available(&self) -> bool {
        self.pgq_available
    }
}

/// Scope failures are a denial, not an internal error: a scope the store
/// cannot render is a routing signal the gateway resolves.
#[must_use]
pub fn map_scope_err(error: ScopeError) -> GraphStoreError {
    match error {
        ScopeError::Denied(_) => GraphStoreError::NotFound,
        ScopeError::UnresolvedScopeProperty { element, property } => {
            GraphStoreError::ScopeUnservable {
                reason: format!(
                    "no constraint of the scope resolves on graph element `{element}` property `{property}`"
                ),
            }
        }
        ScopeError::Pgq(inner) => GraphStoreError::ScopeUnservable {
            reason: format!("graph pattern cannot carry this scope: {inner}"),
        },
        other => GraphStoreError::Internal(other.to_string()),
    }
}

/// Classify a database failure. **`PostgreSQL` 18+ reports an `ON DELETE
/// RESTRICT` refusal as SQLSTATE `23001` (`restrict_violation`); 17 and
/// earlier report `23503`.** Both must classify as a foreign-key violation,
/// or a live-edge refusal reads as an internal error on PG19.
#[must_use]
pub fn map_db_err(error: &sea_orm::DbErr) -> GraphStoreError {
    let text = error.to_string();
    if text.contains("23505") {
        return GraphStoreError::Conflict {
            reason: "unique violation".into(),
        };
    }
    if text.contains("23503") || text.contains("23001") {
        return GraphStoreError::Conflict {
            reason: "a live edge still references this node".into(),
        };
    }
    if text.contains("40001") {
        return GraphStoreError::Serialization;
    }
    GraphStoreError::Internal(text)
}

#[must_use]
pub fn map_db_error(error: &toolkit_db::DbError) -> GraphStoreError {
    GraphStoreError::Unavailable {
        reason: error.to_string(),
    }
}

/// Transaction-closure error type.
///
/// `Db::transaction_ref_mapped` needs `E: From<DbError>` so a begin/commit
/// failure can be reported in the closure's own error type. `GraphStoreError`
/// is defined in the SDK and `DbError` in the toolkit, so the impl cannot
/// live on either; this newtype is the bridge.
pub struct TxStoreError(pub GraphStoreError);

impl From<toolkit_db::DbError> for TxStoreError {
    fn from(error: toolkit_db::DbError) -> Self {
        Self(map_db_error(&error))
    }
}

impl From<GraphStoreError> for TxStoreError {
    fn from(error: GraphStoreError) -> Self {
        Self(error)
    }
}

#[async_trait]
impl GraphStoreV1 for PgGraphStore {
    fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            scope_replace: true,
            // A true repeatable-read snapshot needs a transaction held across
            // calls, which the sealed runner cannot express; `begin_read`
            // returns a revision-stamped handle instead (see dev/DEVIATIONS).
            snapshots: false,
            vector_search: true,
            labels: false,
            chunks: false,
            topology: false,
        }
    }

    async fn register_types(
        &self,
        ctx: &StoreCtx<'_>,
        batch: Vec<TypeRegistration>,
    ) -> Result<Vec<TypeRecord>, GraphStoreError> {
        types::register_types(self, ctx, batch).await
    }

    async fn get_type(
        &self,
        ctx: &StoreCtx<'_>,
        id: &GtsTypeId,
    ) -> Result<TypeRecord, GraphStoreError> {
        types::get_type(self, ctx, id).await
    }

    async fn list_types(
        &self,
        ctx: &StoreCtx<'_>,
        query: TypeQuery,
    ) -> Result<Page<TypeRecord>, GraphStoreError> {
        types::list_types(self, ctx, query).await
    }

    async fn resolve_type_set(
        &self,
        ctx: &StoreCtx<'_>,
        patterns: &[String],
    ) -> Result<TypeIdSet, GraphStoreError> {
        types::resolve_type_set(self, ctx, patterns).await
    }

    async fn ingest(
        &self,
        ctx: &StoreCtx<'_>,
        req: IngestRequest,
    ) -> Result<IngestOutcome, GraphStoreError> {
        ingest::ingest(self, ctx, req).await
    }

    async fn soft_delete(
        &self,
        ctx: &StoreCtx<'_>,
        req: DeleteRequest,
    ) -> Result<DeleteOutcome, GraphStoreError> {
        ingest::soft_delete(self, ctx, req).await
    }

    async fn upsert_label(
        &self,
        _ctx: &StoreCtx<'_>,
        _label: LabelSpec,
    ) -> Result<LabelRecord, GraphStoreError> {
        Err(GraphStoreError::Unsupported { what: "labels" })
    }

    async fn delete_label(
        &self,
        _ctx: &StoreCtx<'_>,
        _id: LabelId,
    ) -> Result<RevisionOutcome, GraphStoreError> {
        Err(GraphStoreError::Unsupported { what: "labels" })
    }

    async fn list_labels(&self, _ctx: &StoreCtx<'_>) -> Result<Vec<LabelRecord>, GraphStoreError> {
        Err(GraphStoreError::Unsupported { what: "labels" })
    }

    async fn assign_labels(
        &self,
        _ctx: &StoreCtx<'_>,
        _req: graph_storage_sdk::models::LabelAssignment,
    ) -> Result<RevisionOutcome, GraphStoreError> {
        Err(GraphStoreError::Unsupported { what: "labels" })
    }

    async fn begin_read(&self, ctx: &StoreCtx<'_>) -> Result<ReadSnapshot, GraphStoreError> {
        reads::begin_read(self, ctx).await
    }

    async fn end_read(&self, _snapshot: ReadSnapshot) -> Result<(), GraphStoreError> {
        Ok(())
    }

    async fn revision(&self, ctx: &StoreCtx<'_>) -> Result<GraphRevision, GraphStoreError> {
        reads::revision(self, ctx).await
    }

    async fn get_node(
        &self,
        ctx: &StoreCtx<'_>,
        key: &NodeKey,
        adjacency_limit: u32,
    ) -> Result<NodeView, GraphStoreError> {
        reads::get_node(self, ctx, key, adjacency_limit).await
    }

    async fn hydrate_nodes(
        &self,
        ctx: &StoreCtx<'_>,
        ids: &[NodeId],
    ) -> Result<Vec<NodeView>, GraphStoreError> {
        reads::hydrate_nodes(self, ctx, ids).await
    }

    async fn search(
        &self,
        ctx: &StoreCtx<'_>,
        req: SearchRequest,
    ) -> Result<SearchResponse, GraphStoreError> {
        search::search(self, ctx, req).await
    }

    async fn project_table(
        &self,
        ctx: &StoreCtx<'_>,
        req: ProjectionRequest,
    ) -> Result<toolkit_odata::Page<NodeRow>, GraphStoreError> {
        reads::project_table(self, ctx, req).await
    }

    async fn load_topology(
        &self,
        _ctx: &StoreCtx<'_>,
        _req: TopologyRequest,
    ) -> Result<TopologyPage, GraphStoreError> {
        // The analytics gear reads topology through its own read-only role
        // (ADR-0007); this deployment does not expose it over the port.
        Err(GraphStoreError::Unsupported { what: "topology" })
    }

    async fn resolve_node_ids(
        &self,
        ctx: &StoreCtx<'_>,
        keys: &[NodeKey],
    ) -> Result<Vec<(NodeKey, NodeId)>, GraphStoreError> {
        reads::resolve_node_ids(self, ctx, keys).await
    }
}
