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

    async fn list_types(
        &self,
        ctx: &SecurityContext,
        query: TypeQuery,
    ) -> Result<Page<TypeRecord>, CanonicalError>;

    // --- write ------------------------------------------------------------

    /// Apply one atomic ingest batch. `request.idempotency_key` carries the
    /// same value the REST path reads from the `Idempotency-Key` header.
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
