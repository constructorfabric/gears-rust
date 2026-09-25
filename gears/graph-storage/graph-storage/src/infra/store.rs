//! The built-in `PostgreSQL` store: one implementation of `GraphStoreV1`,
//! registered exactly as an external plugin would be.
//!
//! Every statement goes through the secure ORM — `.secure().scope_with(..)`
//! or `secure_insert`/`scope_unchecked` on inserts, which cannot subtree-clamp
//! a row that does not exist yet. There is no unscoped query API in this
//! module.

pub mod evolution;
pub mod ingest;
pub mod namespaces;
pub mod projection;
pub mod reads;
pub mod scope;
pub mod search;
pub mod spaces;
pub mod types;

use std::sync::Arc;

use async_trait::async_trait;
use graph_storage_sdk::models::{
    ComponentReadiness, DeleteOutcome, DeleteRequest, EdgeKey, EdgeView, GraphRevision, GtsTypeId,
    IngestOutcome, IngestRequest, NodeId, NodeKey, NodeRow, NodeView, Page, ProjectionRequest,
    ReadSnapshot, ReadinessState, RegisteredType, SearchRequest, SearchResponse,
    SourceNamespaceOwner, StoreCapabilities, TopologyPage, TopologyRequest, TypeIdSet, TypeQuery,
    TypeRecord, TypeRegistration, TypeRegistrationOptions,
};
use graph_storage_sdk::plugin_api::{
    EmbeddingPlan, EmbeddingState, GraphStoreError, GraphStoreV1, StoreCtx, VectorArm,
};
use toolkit_db::secure::{Db, ScopeError};

use crate::config::{GraphStorageConfig, ValidatedConfig};

/// The built-in store.
pub struct PgGraphStore {
    db: Arc<Db>,
    config: GraphStorageConfig,
    /// Whether this server parses SQL/PGQ, probed once at init. Reads use it
    /// to decide the hop backend; nothing else depends on it.
    pgq_available: bool,
}

impl PgGraphStore {
    /// Takes a [`ValidatedConfig`] rather than a [`GraphStorageConfig`]: the
    /// ranges are refused at startup so a deployment asking for the impossible
    /// does not boot into something else, and asking for the checked type here
    /// is what keeps a second construction path from skipping that.
    #[must_use]
    pub fn new(db: Arc<Db>, config: ValidatedConfig, pgq_available: bool) -> Self {
        Self {
            db,
            config: config.into_inner(),
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
        // Every statement goes through the secure ORM, so a database failure
        // arrives wrapped. Classifying it here rather than at each call site
        // is what keeps a unique violation, a live-edge refusal and a
        // serialization failure from all reading as an internal error.
        ScopeError::Db(inner) => map_db_err(&inner),
        ScopeError::Denied(_) => GraphStoreError::NotFound,
        ScopeError::UnresolvedScopeProperty { element, property } => {
            GraphStoreError::ScopeUnservable {
                reason: format!(
                    "no constraint of the scope resolves on graph element `{element}` property `{property}`"
                ),
            }
        }
        // Not `ScopeUnservable`: a syntax refusal is a malformed declaration
        // of ours rather than a scope this store cannot carry, and routing it
        // to the fallback backend would hide it indefinitely.
        ScopeError::GraphSyntax(inner) => {
            GraphStoreError::Internal(format!("graph pattern is malformed: {inner}"))
        }
        other => GraphStoreError::Internal(other.to_string()),
    }
}

/// Read the driver's SQLSTATE out of the structured error, when it left one.
///
/// `DbErr::sql_err` only names unique- and foreign-key violations, so it
/// cannot see `23001` or `40001`; sea-orm's own documentation points at the
/// underlying driver error for every other code, which is what this reads.
/// Only `Exec` and `Query` carry one -- a connection failure has no statement
/// behind it -- and that is the pair `sql_err` itself inspects.
fn sqlstate_of(error: &sea_orm::DbErr) -> Option<String> {
    use sea_orm::{DbErr, RuntimeErr, sqlx};

    let (DbErr::Exec(RuntimeErr::SqlxError(inner)) | DbErr::Query(RuntimeErr::SqlxError(inner))) =
        error
    else {
        return None;
    };
    let sqlx::Error::Database(db) = inner.as_ref() else {
        return None;
    };
    db.code().map(std::borrow::Cow::into_owned)
}

/// The SQLSTATEs this store answers for, and nothing else.
fn classify_sqlstate(sqlstate: &str) -> Option<GraphStoreError> {
    match sqlstate {
        "23505" => Some(GraphStoreError::Conflict {
            reason: "unique violation".into(),
        }),
        "23503" | "23001" => Some(GraphStoreError::Conflict {
            reason: "a live edge still references this node".into(),
        }),
        // Two codes, one answer. `40001` is the serialization failure a
        // conflicting snapshot raises; `40P01` is a deadlock the server broke
        // by killing one side. Both mean the transaction did not happen and
        // the same statements may succeed if sent again, which is what
        // `Serialization` tells a caller -- retry unchanged, as against
        // `Internal`'s retry once and escalate.
        //
        // `40P01` is not hypothetical here. Ingest takes row locks in a fixed
        // order because the secure ORM exposes no `FOR UPDATE`, and fixed-order
        // locking across concurrent writers is the shape that produces
        // deadlocks. Leaving it out meant the one failure this design invites
        // was the one the classifier could not name.
        "40001" | "40P01" => Some(GraphStoreError::Serialization),
        _ => None,
    }
}

/// Classify a database failure. **`PostgreSQL` 18+ reports an `ON DELETE
/// RESTRICT` refusal as SQLSTATE `23001` (`restrict_violation`); 17 and
/// earlier report `23503`.** Both must classify as a foreign-key violation,
/// or a live-edge refusal reads as an internal error on PG19.
///
/// The driver's own code is authoritative and is read first, out of the
/// structured error rather than the rendered message. When the driver stated
/// one, it decides alone -- including when it names a class this store does
/// not handle, which is `Internal` and never a conflict.
///
/// Matching the message is the fallback, for an error that arrived without a
/// structured code because something between the driver and here re-wrapped it
/// through `to_string()`. It is a fallback rather than the rule because a
/// rendered message quotes user-supplied values: `PostgreSQL` echoes the
/// offending input into `22P02 invalid_text_representation`, so a node key of
/// `23505-retry` reads as a unique violation to a substring search. Reaching
/// for the code first means such an error is classified by what the server
/// said, not by what the caller managed to get quoted back.
#[must_use]
pub fn map_db_err(error: &sea_orm::DbErr) -> GraphStoreError {
    let text = error.to_string();

    if let Some(sqlstate) = sqlstate_of(error) {
        return classify_sqlstate(&sqlstate).unwrap_or(GraphStoreError::Internal(text));
    }

    for sqlstate in ["23505", "23503", "23001", "40001", "40P01"] {
        if text.contains(sqlstate)
            && let Some(classified) = classify_sqlstate(sqlstate)
        {
            return classified;
        }
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
            // returns a revision-stamped handle instead (DESIGN § 3.3, obligation 5, which the built-in store declines).
            snapshots: false,
            vector_search: true,
            labels: false,
            chunks: false,
            topology: false,
        }
    }

    async fn register_types_with(
        &self,
        ctx: &StoreCtx<'_>,
        batch: Vec<TypeRegistration>,
        options: TypeRegistrationOptions,
    ) -> Result<Vec<RegisteredType>, GraphStoreError> {
        types::register_types(self, ctx, batch, options).await
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

    async fn probe_readiness(&self) -> Vec<ComponentReadiness> {
        let mut out = Vec::new();

        // The database row first, because every other row is meaningless
        // without it. Two questions, not one: a reachable server whose
        // migrations have not run serves a schema the gear does not know.
        //
        // What the database said stays in the operator log: a driver error
        // can name the server it failed to reach, and this route answers
        // anyone who can reach it. The row says what is wrong in words that
        // are the same for every deployment.
        if let Err(error) = self.db().conn() {
            tracing::warn!(%error, "readiness: the database is unreachable");
            out.push(ComponentReadiness::new(
                graph_storage_sdk::models::DATABASE,
                ReadinessState::Unhealthy,
                "the database is unreachable; the reason is in the gear's log",
                "everything; no traffic is admitted",
                "connectivity restored; the probe re-runs on the next request and flips \
                 without a restart",
            ));
        } else {
            let migrations =
                <crate::infra::storage::migrations::Migrator as sea_orm_migration::MigratorTrait>::migrations();
            match toolkit_db::migration_runner::get_pending_migrations(
                self.db(),
                "graph-storage",
                &migrations,
            )
            .await
            {
                Ok(pending) if pending.is_empty() => {
                    out.push(ComponentReadiness::healthy(
                        graph_storage_sdk::models::DATABASE,
                    ));
                }
                Ok(pending) => {
                    tracing::warn!(
                        pending = %pending.join(", "),
                        "readiness: migrations have not been applied"
                    );
                    out.push(ComponentReadiness::new(
                        graph_storage_sdk::models::DATABASE,
                        ReadinessState::Unhealthy,
                        &format!(
                            "{} migration(s) have not been applied; the gear's log names them",
                            pending.len(),
                        ),
                        "everything; no traffic is admitted",
                        "apply the migrations; the probe re-runs without a restart",
                    ));
                }
                Err(error) => {
                    tracing::warn!(%error, "readiness: the migration history cannot be read");
                    out.push(ComponentReadiness::new(
                        graph_storage_sdk::models::DATABASE,
                        ReadinessState::Unhealthy,
                        "the migration history cannot be read; the reason is in the gear's log",
                        "everything; no traffic is admitted",
                        "restore access to the migration table",
                    ));
                }
            }
        }

        // The traversal backend, as probed at init, read against what the
        // configuration asked for. The matrix has two rows for a server
        // without SQL/PGQ, and they differ only in intent: `Degraded` where
        // the backend was preferred, `Unhealthy` where it was demanded. This
        // used to report `Degraded` for both, because a single `pgq` value
        // could not say which it was; `auto` is the preference now, so a
        // named `pgq` is the demand, and an operator who named it is told
        // the gear is not ready rather than being quietly served something
        // else (DESIGN § Readiness Matrix, ADR-0001 point 2).
        let row = match (self.config().traversal_hop, self.pgq_available()) {
            (_, true) | (crate::config::HopStrategy::TwoQuery, false) => {
                ComponentReadiness::healthy(graph_storage_sdk::models::SQLPGQ)
            }
            (crate::config::HopStrategy::Auto, false) => ComponentReadiness::new(
                graph_storage_sdk::models::SQLPGQ,
                ReadinessState::Degraded,
                "the declared property graph did not answer a pattern at startup; the server \
                 major is not reported, because the attempt says the pattern did not run and \
                 not why",
                "nothing: every traversal is served by the two-query hop",
                "restart after the property-graph migration runs on a server that supports \
                 SQL/PGQ, or set traversal_hop to `two_query` to state the choice",
            ),
            (crate::config::HopStrategy::Pgq, false) => ComponentReadiness::new(
                graph_storage_sdk::models::SQLPGQ,
                ReadinessState::Unhealthy,
                "traversal_hop is `pgq` and this server does not provide SQL/PGQ",
                "everything: the gear is not ready, because an explicitly configured backend \
                 is not substituted",
                "run on PostgreSQL 19 with the property-graph migration applied, or set \
                 traversal_hop to `auto` or `two_query`",
            ),
        };
        out.push(row);

        out
    }

    async fn list_source_namespaces(
        &self,
        ctx: &StoreCtx<'_>,
    ) -> Result<Vec<SourceNamespaceOwner>, GraphStoreError> {
        namespaces::list(self, ctx).await
    }

    async fn transfer_source_namespace(
        &self,
        ctx: &StoreCtx<'_>,
        namespace: &str,
        owner_principal: &str,
    ) -> Result<SourceNamespaceOwner, GraphStoreError> {
        namespaces::transfer(self, ctx, namespace, owner_principal).await
    }

    async fn ingest(
        &self,
        ctx: &StoreCtx<'_>,
        req: IngestRequest,
        embedding: EmbeddingPlan,
    ) -> Result<IngestOutcome, GraphStoreError> {
        ingest::ingest(self, ctx, req, embedding).await
    }

    async fn soft_delete(
        &self,
        ctx: &StoreCtx<'_>,
        req: DeleteRequest,
    ) -> Result<DeleteOutcome, GraphStoreError> {
        ingest::soft_delete(self, ctx, req).await
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

    async fn get_edge(
        &self,
        ctx: &StoreCtx<'_>,
        key: &EdgeKey,
    ) -> Result<EdgeView, GraphStoreError> {
        reads::get_edge(self, ctx, key).await
    }

    async fn hydrate_nodes(
        &self,
        ctx: &StoreCtx<'_>,
        ids: &[NodeId],
    ) -> Result<Vec<NodeView>, GraphStoreError> {
        reads::hydrate_nodes(self, ctx, ids).await
    }

    async fn node_types(
        &self,
        ctx: &StoreCtx<'_>,
        ids: &[NodeId],
    ) -> Result<Vec<(NodeId, graph_storage_sdk::models::GtsTypeId)>, GraphStoreError> {
        reads::node_types(self, ctx, ids).await
    }

    async fn search(
        &self,
        ctx: &StoreCtx<'_>,
        req: SearchRequest,
        vector: Option<VectorArm>,
    ) -> Result<SearchResponse, GraphStoreError> {
        search::search(self, ctx, req, vector).await
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

    async fn embedding_state(
        &self,
        ctx: &StoreCtx<'_>,
        keys: &[NodeKey],
    ) -> Result<Vec<Option<EmbeddingState>>, GraphStoreError> {
        reads::embedding_state(self, ctx, keys).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PostgreSQL` 18 changed the SQLSTATE of an `ON DELETE RESTRICT` refusal
    /// from `23503` to `23001`. Both must read as a live-edge conflict, or a
    /// refusal to delete a referenced node surfaces as an internal error on
    /// PG19 — which is exactly what happened in another gear before this was
    /// understood.
    #[test]
    fn both_restrict_sqlstates_classify_as_a_conflict() {
        for sqlstate in ["23503", "23001"] {
            let error = sea_orm::DbErr::Custom(format!(
                "error returned from database: {sqlstate} update or delete violates foreign key"
            ));
            assert!(
                matches!(map_db_err(&error), GraphStoreError::Conflict { .. }),
                "SQLSTATE {sqlstate} must classify as a conflict"
            );
        }
    }

    /// A driver error whose stated SQLSTATE and whose rendered message
    /// disagree. `PostgreSQL` echoes the offending input into a `22P02`
    /// (`invalid_text_representation`), so this is the shape a caller produces
    /// by naming a node `23505-retry` and letting it reach a `uuid` cast.
    #[derive(Debug)]
    struct DriverError {
        code: &'static str,
        message: String,
    }

    impl std::fmt::Display for DriverError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.message)
        }
    }

    impl std::error::Error for DriverError {}

    impl sea_orm::sqlx::error::DatabaseError for DriverError {
        fn message(&self) -> &str {
            &self.message
        }

        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(std::borrow::Cow::Borrowed(self.code))
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> sea_orm::sqlx::error::ErrorKind {
            sea_orm::sqlx::error::ErrorKind::Other
        }
    }

    fn driver_error(code: &'static str, message: &str) -> sea_orm::DbErr {
        sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(std::sync::Arc::new(
            sea_orm::sqlx::Error::Database(Box::new(DriverError {
                code,
                message: message.to_owned(),
            })),
        )))
    }

    /// The message is the caller's to influence; the SQLSTATE is not. A
    /// substring search cannot tell the two apart, so it read this as a unique
    /// violation and handed the caller a `409` -- and, where a conflict is
    /// retried, a retry that could never succeed.
    #[test]
    fn a_stated_sqlstate_decides_over_a_message_quoting_the_callers_value() {
        let error = driver_error(
            "22P02",
            r#"invalid input syntax for type uuid: "23505-retry""#,
        );
        assert!(
            error.to_string().contains("23505"),
            "the message must carry the digits, or this proves nothing"
        );
        assert!(
            matches!(map_db_err(&error), GraphStoreError::Internal(_)),
            "a 22P02 whose message quotes 23505 must classify as internal"
        );
    }

    /// The other half of the same rule: reading the code first must not lose
    /// the codes this store does answer for.
    #[test]
    fn a_stated_sqlstate_still_classifies_what_this_store_answers_for() {
        for sqlstate in ["23505", "23503", "23001"] {
            let error = driver_error_for(sqlstate);
            assert!(
                matches!(map_db_err(&error), GraphStoreError::Conflict { .. }),
                "SQLSTATE {sqlstate} must classify as a conflict"
            );
        }
        for sqlstate in ["40001", "40P01"] {
            assert!(
                matches!(
                    map_db_err(&driver_error_for(sqlstate)),
                    GraphStoreError::Serialization
                ),
                "SQLSTATE {sqlstate} must classify as a serialization failure: both say the \
                 transaction did not happen and the same statements may succeed if sent again"
            );
        }
    }

    /// Deliberately message-free: the classification has to come from the code
    /// alone, not from a phrase the message happens to carry.
    fn driver_error_for(sqlstate: &'static str) -> sea_orm::DbErr {
        driver_error(sqlstate, "the server said no")
    }

    #[test]
    fn a_scope_wrapped_database_error_is_still_classified() {
        let inner = sea_orm::DbErr::Custom(
            "error returned from database: 23505 duplicate key value".to_owned(),
        );
        assert!(
            matches!(
                map_scope_err(ScopeError::Db(inner)),
                GraphStoreError::Conflict { .. }
            ),
            "a database error wrapped by the secure ORM must not read as internal"
        );
    }

    #[test]
    fn a_denial_is_not_found_rather_than_forbidden() {
        assert!(matches!(
            map_scope_err(ScopeError::Denied("nope")),
            GraphStoreError::NotFound
        ));
    }
}
