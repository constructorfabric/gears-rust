//! Object-safe client trait registered in `ClientHub`.
//!
//! The in-process path is subject to the same admission limits and the same
//! authorization as REST: identical enforcement through the shared PEP, and
//! the same `CanonicalError` taxonomy (DESIGN § Error Model), so REST and
//! `ClientHub` never classify one failure differently.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::models::{
    DeleteOutcome, EdgeKey, GraphRevision, GtsTypeId, IngestOutcome, IngestRequest,
    NeighborhoodRequest, NodeCount, NodeKey, NodeRow, NodeView, Page, PurgeOptions, PurgeOutcome,
    RegisteredType, SearchRequest, SearchResponse, TraversalResponse, TraverseRequest, TypeQuery,
    TypeRecord, TypeRegistration, TypeRegistrationOptions,
};

/// Object-safe client for in-process consumption by other gears (version 1).
///
/// # Errors
///
/// Every method returns the same `CanonicalError` taxonomy the REST surface
/// renders (DESIGN § Error Model), and the same category for the same
/// failure, because both adapters call one service. Documented once here
/// rather than per method: the vocabulary is the contract, and repeating it
/// twelve times would let the copies drift.
///
/// - `invalid_argument` — a malformed request, a per-item schema violation
///   (`SCHEMA_VIOLATION`, addressed by JSON pointer), a request the gear
///   cannot interpret (`INVALID_ARGUMENT`), or two bounds that cannot hold at
///   once (`LIMIT_COMBINATION`).
/// - `out_of_range` (`LIMIT_EXCEEDED`) — a value outside a documented hard
///   range: batch size, depth, page size, an oversized key or query.
/// - `not_found` — the row is absent *or* the caller may not see it. The two
///   are indistinguishable by contract (anti-enumeration), so a client must
///   not read absence as permission to create.
/// - `permission_denied` (`SOURCE_NAMESPACE_FORBIDDEN`) — the one denial that
///   names itself, because the caller wrote under a source namespace another
///   producer owns, and that owner is a fact about the tenant rather than
///   about them.
/// - `aborted` — `CAS_CONFLICT` (a same-key type change, a stale
///   `expected_version`, a scope owned by another producer),
///   `SERIALIZATION`, or `IDEMPOTENCY_MISMATCH`. Re-read and retry.
/// - `failed_precondition` — `STALE_GENERATION`, `IDEMPOTENCY_KEY_EXPIRED`,
///   `SCOPE_UNSERVABLE`, `EMBEDDING_SPACE_MISMATCH`. Not retryable unchanged.
/// - `unavailable` — a dependency is down: the PDP, the database, the
///   embedding provider. Retry later.
/// - `deadline_exceeded`, `cancelled` — the operation ran out of the budget
///   it started with, or the caller went away.
/// - `unimplemented` — a capability the selected engine or store does not
///   provide (traversal on an engine without it, labels, topology).
/// - `unknown`, `data_loss` — an unexpected internal failure, or detected
///   corruption. Escalate rather than retry.
#[async_trait]
pub trait GraphStorageClientV1: Send + Sync {
    // --- ontology ---------------------------------------------------------

    /// Register a batch of GTS types, atomically, under `options` -- the same
    /// options `POST /types` takes.
    ///
    /// `on_existing: Update` admits a changed schema under a registered
    /// identifier when the change is admissible (ADR-0006): backward
    /// compatible, or -- with `revalidate` -- contradicted by no stored row,
    /// or carried by a migration. That is how a type registered before its
    /// consumers knew which paths to filter on gains an `index` declaration.
    /// `dry_run` computes every verdict and writes nothing, and reports a
    /// refusal in the result rather than as an error. Each item says what
    /// happened to its type (`TypeOutcome`) and on what ground.
    ///
    /// The batch stays atomic: one refused type registers none.
    async fn register_types_with(
        &self,
        ctx: &SecurityContext,
        batch: Vec<TypeRegistration>,
        options: TypeRegistrationOptions,
    ) -> Result<Vec<RegisteredType>, CanonicalError>;

    /// Register a batch of GTS types, atomically, under the default options.
    /// Byte-identical re-registration converges; a different schema for a
    /// registered identifier conflicts.
    ///
    /// Provided, not implemented: one path decides admission, so this form
    /// cannot drift from the one that carries the options.
    async fn register_types(
        &self,
        ctx: &SecurityContext,
        batch: Vec<TypeRegistration>,
    ) -> Result<Vec<TypeRecord>, CanonicalError> {
        let registered = self
            .register_types_with(ctx, batch, TypeRegistrationOptions::default())
            .await?;
        Ok(registered.into_iter().map(|item| item.record).collect())
    }

    async fn get_type(
        &self,
        ctx: &SecurityContext,
        type_id: &GtsTypeId,
    ) -> Result<TypeRecord, CanonicalError>;

    /// One page of the type catalogue.
    ///
    /// **Continue while `next_cursor` is `Some`, even when `items` is empty.**
    /// This endpoint does not follow the common "stop when the page is empty"
    /// convention. A `pattern` is applied after rows are read, and the scan
    /// gives up its pass after a bounded number of rows; when no row in that
    /// pass matches, the answer is an empty page carrying the cursor to resume
    /// from. An empty page therefore means "nothing here yet", not "nothing
    /// left" -- only a `next_cursor` of `None` means that. A client that stops
    /// on the empty page silently drops every match beyond it, which is most
    /// likely exactly where a selective pattern finds them.
    async fn list_types(
        &self,
        ctx: &SecurityContext,
        query: TypeQuery,
    ) -> Result<Page<TypeRecord>, CanonicalError>;

    // --- write ------------------------------------------------------------

    /// Apply one atomic ingest batch. `request.idempotency_key` carries the
    /// same value the REST path reads from the `Idempotency-Key` header.
    ///
    /// The key is optional, and it is what makes a retry safe after an unknown
    /// commit outcome -- the case where the batch committed and the response
    /// was lost. With a key, an identical retry returns the recorded outcome
    /// and touches no graph state; the same key with a different request is a
    /// conflict. **Without one, none of that happens**: no receipt is read and
    /// none is written, so a retry is a new logical request that re-runs the
    /// write path. That is not always harmless -- a batch that replaces a scope
    /// removes what it does not re-declare, and running it twice is not the
    /// same as running it once. A producer that retries on timeout should send
    /// a key.
    async fn ingest(
        &self,
        ctx: &SecurityContext,
        request: IngestRequest,
    ) -> Result<IngestOutcome, CanonicalError>;

    /// Soft-delete a node together with its incident edges.
    async fn delete_node(
        &self,
        ctx: &SecurityContext,
        node_key: &NodeKey,
    ) -> Result<DeleteOutcome, CanonicalError>;

    /// Soft-delete one edge.
    async fn delete_edge(
        &self,
        ctx: &SecurityContext,
        edge_key: &EdgeKey,
    ) -> Result<DeleteOutcome, CanonicalError>;

    /// Remove a node and every edge incident to it for good: the rows are
    /// deleted, not tombstoned, and the key is free again (gears-rust #5194).
    ///
    /// It takes a live node or one already soft-deleted, so the tombstones a
    /// `delete_node` left can be erased afterwards. A key with no stored node
    /// is a success with `purged_nodes: 0`, so a retry after an unknown
    /// outcome does not fail. That retry is safe only while the key cannot
    /// be written again in between: a purge frees the key, and a retry finds
    /// whatever node holds it by then. Where keys are reused, purge with
    /// `options.expected_created_at` -- the node's envelope `created_at` --
    /// which refuses any other node under the key with a conflict;
    /// `options.only_tombstoned` refuses a live one only. Analysis edges go with the node: an erasure
    /// outranks a conclusion. Authorized as its own action, `purge`, not as
    /// `delete`. Dead tuples, WAL and backups follow the deployment's
    /// retention, not this call.
    ///
    /// The provided implementation refuses as `unimplemented`, so other
    /// implementations of this trait keep compiling.
    async fn purge_node(
        &self,
        ctx: &SecurityContext,
        node_key: &NodeKey,
        options: PurgeOptions,
    ) -> Result<PurgeOutcome, CanonicalError> {
        let _ = (ctx, node_key, options);
        Err(no_target::unimplemented(
            "this GraphStorageClientV1 implementation does not support purge",
        ))
    }

    // --- read -------------------------------------------------------------

    /// Node by key with payload and bounded bidirectional adjacency.
    /// `adjacency_limit = None` uses the configured default.
    async fn get_node(
        &self,
        ctx: &SecurityContext,
        node_key: &NodeKey,
        adjacency_limit: Option<u32>,
    ) -> Result<NodeView, CanonicalError>;

    /// Tabular projection over declared `index` paths, bound to the platform
    /// `OData` options.
    ///
    /// `type_patterns` narrows the projection to the types they resolve to;
    /// the effective set is that intersected with the pattern of the
    /// permission that authorized the request. Empty means every authorized
    /// type. Patterns are resolved by the shared GTS implementation, never
    /// compiled into SQL.
    async fn project_nodes(
        &self,
        ctx: &SecurityContext,
        type_patterns: &[String],
        query: toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<NodeRow>, CanonicalError>;

    /// How many nodes `project_nodes` lists for the same `type_patterns` and
    /// `$filter`, across all its pages -- the total a listing shows beside
    /// its page ("1-50 of 3 200").
    ///
    /// Exact up to the deployment's `projection_count_max`; past it the
    /// answer is that ceiling with `capped` set. Only the filter applies: a
    /// `query` carrying `$orderby`, `$top`, a cursor or `$select` is refused
    /// (`INVALID_ARGUMENT`), because a count has no pages.
    async fn count_nodes(
        &self,
        ctx: &SecurityContext,
        type_patterns: &[String],
        query: toolkit_odata::ODataQuery,
    ) -> Result<NodeCount, CanonicalError>;

    /// Lexical, vector or hybrid search.
    async fn search(
        &self,
        ctx: &SecurityContext,
        request: SearchRequest,
    ) -> Result<SearchResponse, CanonicalError>;

    /// Seeded, depth-bounded traversal.
    async fn traverse(
        &self,
        ctx: &SecurityContext,
        request: TraverseRequest,
    ) -> Result<TraversalResponse, CanonicalError>;

    /// Bounded neighborhood projection.
    async fn neighborhood(
        &self,
        ctx: &SecurityContext,
        request: NeighborhoodRequest,
    ) -> Result<TraversalResponse, CanonicalError>;

    /// The caller-visible `(source_epoch, graph_revision)` identity.
    async fn revision(&self, ctx: &SecurityContext) -> Result<GraphRevision, CanonicalError>;

    // --- tenancy ----------------------------------------------------------

    /// A client whose every call is about `tenant` rather than the caller's
    /// own tenant (gears-rust #5623).
    ///
    /// For a caller that acts in tenants it is not a subject of, such as a
    /// service with one platform-wide identity: the PDP is asked whether the
    /// caller may act in `tenant`, a refusal is `not_found` on every method
    /// (the same answer as an unknown tenant), and a granted call reads and
    /// writes `tenant`'s rows alone, reads included -- no answer mixes two
    /// tenants. The acting subject recorded on rows stays the caller. Calling
    /// `in_tenant` on a scoped client replaces its tenant. The caller's own
    /// tenant is a target like any other: a read then leaves out the subtree
    /// it would include without one.
    ///
    /// The provided implementation refuses every call as `unimplemented`, so
    /// an implementation that does not support a target tenant -- a test
    /// double, say -- keeps compiling and never answers for the caller's own
    /// tenant when another one was asked for.
    fn in_tenant(&self, tenant: uuid::Uuid) -> std::sync::Arc<dyn GraphStorageClientV1> {
        let _ = tenant;
        std::sync::Arc::new(no_target::NoTargetTenant)
    }
}

// In this file rather than its own: the trait's provided `in_tenant` names
// this type, and the type implements the trait, so two files would depend on
// each other.
mod no_target {
    //! The client `GraphStorageClientV1::in_tenant` returns from an
    //! implementation that does not support a target tenant.
    //!
    //! It refuses every call. Answering for the caller's own tenant instead would
    //! write and read the wrong tenant's rows without a trace, which is the one
    //! outcome a target tenant exists to rule out.

    use async_trait::async_trait;
    use toolkit_canonical_errors::{CanonicalError, resource_error};
    use toolkit_security::SecurityContext;

    use super::GraphStorageClientV1;
    use crate::models::{
        DeleteOutcome, EdgeKey, GraphRevision, GtsTypeId, IngestOutcome, IngestRequest,
        NeighborhoodRequest, NodeCount, NodeKey, NodeRow, NodeView, Page, RegisteredType,
        SearchRequest, SearchResponse, TraversalResponse, TraverseRequest, TypeQuery, TypeRecord,
        TypeRegistration, TypeRegistrationOptions,
    };

    #[resource_error(gts_id!("cf.core.graph.node.v1~"))]
    struct TargetTenantError;

    /// A client that refuses every call: the provided `in_tenant` of an
    /// implementation that cannot honour a target tenant.
    pub struct NoTargetTenant;

    /// An `unimplemented` answer of the graph node resource, for a provided
    /// method an implementation of the client trait does not override.
    pub fn unimplemented(detail: &str) -> CanonicalError {
        TargetTenantError::unimplemented(detail.to_owned()).create()
    }

    fn refused() -> CanonicalError {
        unimplemented("this GraphStorageClientV1 implementation does not support a target tenant")
    }

    #[async_trait]
    impl GraphStorageClientV1 for NoTargetTenant {
        async fn register_types_with(
            &self,
            _: &SecurityContext,
            _: Vec<TypeRegistration>,
            _: TypeRegistrationOptions,
        ) -> Result<Vec<RegisteredType>, CanonicalError> {
            Err(refused())
        }

        async fn get_type(
            &self,
            _: &SecurityContext,
            _: &GtsTypeId,
        ) -> Result<TypeRecord, CanonicalError> {
            Err(refused())
        }

        async fn list_types(
            &self,
            _: &SecurityContext,
            _: TypeQuery,
        ) -> Result<Page<TypeRecord>, CanonicalError> {
            Err(refused())
        }

        async fn ingest(
            &self,
            _: &SecurityContext,
            _: IngestRequest,
        ) -> Result<IngestOutcome, CanonicalError> {
            Err(refused())
        }

        async fn delete_node(
            &self,
            _: &SecurityContext,
            _: &NodeKey,
        ) -> Result<DeleteOutcome, CanonicalError> {
            Err(refused())
        }

        async fn delete_edge(
            &self,
            _: &SecurityContext,
            _: &EdgeKey,
        ) -> Result<DeleteOutcome, CanonicalError> {
            Err(refused())
        }

        async fn get_node(
            &self,
            _: &SecurityContext,
            _: &NodeKey,
            _: Option<u32>,
        ) -> Result<NodeView, CanonicalError> {
            Err(refused())
        }

        async fn project_nodes(
            &self,
            _: &SecurityContext,
            _: &[String],
            _: toolkit_odata::ODataQuery,
        ) -> Result<toolkit_odata::Page<NodeRow>, CanonicalError> {
            Err(refused())
        }

        async fn count_nodes(
            &self,
            _: &SecurityContext,
            _: &[String],
            _: toolkit_odata::ODataQuery,
        ) -> Result<NodeCount, CanonicalError> {
            Err(refused())
        }

        async fn search(
            &self,
            _: &SecurityContext,
            _: SearchRequest,
        ) -> Result<SearchResponse, CanonicalError> {
            Err(refused())
        }

        async fn traverse(
            &self,
            _: &SecurityContext,
            _: TraverseRequest,
        ) -> Result<TraversalResponse, CanonicalError> {
            Err(refused())
        }

        async fn neighborhood(
            &self,
            _: &SecurityContext,
            _: NeighborhoodRequest,
        ) -> Result<TraversalResponse, CanonicalError> {
            Err(refused())
        }

        async fn revision(&self, _: &SecurityContext) -> Result<GraphRevision, CanonicalError> {
            Err(refused())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A target the implementation cannot honour is refused, never served
        /// for the caller's own tenant.
        #[tokio::test]
        async fn a_target_tenant_an_implementation_cannot_honour_is_refused() {
            let ctx = SecurityContext::anonymous();
            let scoped = NoTargetTenant.in_tenant(uuid::Uuid::from_u128(7));
            let error = scoped.revision(&ctx).await.expect_err("refused");
            assert!(
                format!("{error:?}")
                    .to_lowercase()
                    .contains("unimplemented"),
                "{error:?}"
            );
        }
    }
}
