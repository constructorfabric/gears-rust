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
    NeighborhoodRequest, NodeKey, NodeRow, NodeView, Page, SearchRequest, SearchResponse,
    TraversalResponse, TraverseRequest, TypeQuery, TypeRecord, TypeRegistration,
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

    /// Register a batch of GTS types, atomically. Byte-identical
    /// re-registration converges; a different schema for a registered
    /// identifier conflicts.
    async fn register_types(
        &self,
        ctx: &SecurityContext,
        batch: Vec<TypeRegistration>,
    ) -> Result<Vec<TypeRecord>, CanonicalError>;

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
}
