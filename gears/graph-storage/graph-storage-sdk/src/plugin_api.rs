//! The three plugin contracts of the graph-storage gateway.
//!
//! The gear is a stateless gateway over a pluggable store: every byte it
//! serves comes from a [`GraphStoreV1`] implementation behind the port, and
//! the built-in `PostgreSQL` store is registered exactly as an external plugin
//! would be. `GraphEngineV1` serves traversal expansion;
//! `EmbeddingProviderV1` turns text into vectors.
//!
//! An implementation that cannot provide an obligation declares the
//! corresponding capability absent and returns `Unsupported` from the
//! affected method. It never implements a weaker version — a silently
//! weakened guarantee is worse than an absent capability, because the gear
//! can route around the second and not the first.

use async_trait::async_trait;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use toolkit_security::AccessScope;

use crate::models::{
    ComponentReadiness, DeleteOutcome, DeleteRequest, Direction, EdgeKey, EdgeRef, EdgeView,
    EmbeddingSpaceId, EngineCapabilities, GraphRevision, GtsTypeId, HopBudget, IngestOutcome,
    IngestRequest, ItemError, LabelAssignment, LabelFilter, LabelId, LabelRecord, LabelSpec,
    NodeId, NodeKey, NodeRow, NodeView, Page, ProjectionRequest, ReadSnapshot, RegisteredType,
    RemainingBudget, RevisionOutcome, SearchRequest, SearchResponse, SourceNamespaceOwner,
    StoreCapabilities, Subject, TenantId, TopologyPage, TopologyRequest, TruncationReason,
    TypeIdSet, TypeQuery, TypeRecord, TypeRegistration, TypeRegistrationOptions,
};

/// Per-call context. The compiled scope is mandatory, not optional:
/// authorization has to reach inside the statements (a search arm applies it
/// before ranking and LIMIT), so it cannot be a filter the gear applies to
/// whatever the plugin returns.
///
/// `scope` living here rather than in a per-method argument is what makes the
/// request-level decline possible: an implementation inspects the compiled
/// scope it is about to serve and may answer `ScopeUnservable` instead of a
/// result, which the gateway resolves by falling back. It is a routing
/// signal, not a failure, and it never reaches the caller.
pub struct StoreCtx<'a> {
    pub tenant: TenantId,
    pub scope: &'a AccessScope,
    /// The acting subject, stamped onto the audit envelope of every element a
    /// write in this call creates, updates or tombstones (`fr-audit-envelope`).
    pub subject: Subject,
    /// Present when the call participates in a compound read that must
    /// observe one graph state (Read Consistency Contract).
    pub snapshot: Option<&'a ReadSnapshot>,
    /// What is left of the operation's absolute deadline.
    pub budget: RemainingBudget,
    pub cancel: CancellationToken,
}

/// Store-side failure vocabulary. The gear normalizes these into canonical
/// errors before they cross the public boundary; no vendor text survives.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GraphStoreError {
    /// Request-level decline: this implementation cannot serve the compiled
    /// scope. Routed by the gateway, never surfaced to the caller directly.
    #[error("scope unservable: {reason}")]
    ScopeUnservable { reason: String },
    /// The capability is declared absent for this store.
    #[error("unsupported: {what}")]
    Unsupported { what: &'static str },
    /// Per-item validation failures; the batch committed nothing.
    #[error("{} item(s) failed validation", items.len())]
    Validation { items: Vec<ItemError> },
    /// Same-key different-type ingest, expected-version mismatch, or an
    /// equal-generation replacement with different content.
    #[error("conflict: {reason}")]
    Conflict { reason: String },
    /// Serialization failure under concurrent ingest; retry unchanged.
    #[error("serialization failure")]
    Serialization,
    /// Older source generation for a scope; drop the stale run.
    #[error("stale generation: recorded {recorded}, offered {offered}")]
    StaleGeneration { recorded: i64, offered: i64 },
    /// Idempotency key reused with a different request.
    #[error("idempotency key reused with a different request")]
    IdempotencyMismatch,
    /// Receipt expired (or from a previous source epoch); reconcile first.
    #[error("idempotency receipt expired")]
    IdempotencyExpired,
    /// Unauthorized or unknown resource — indistinguishable by contract.
    #[error("not found")]
    NotFound,
    /// A documented hard bound was exceeded.
    #[error("limit exceeded: {what}")]
    LimitExceeded { what: String },
    /// A write under a source namespace bound to another producer principal.
    ///
    /// Deliberately **not** `NotFound`: everywhere else a denial is
    /// indistinguishable from absence (anti-enumeration), but here the caller
    /// named a namespace whose owner is a fact about the tenant, not about
    /// them, and telling them it does not exist would send them to create it.
    /// DESIGN § Error Model maps this to `permission_denied` /
    /// `SOURCE_NAMESPACE_FORBIDDEN`, "never retry; request ownership
    /// transfer".
    #[error("source namespace `{namespace}` is owned by another producer")]
    SourceNamespaceForbidden { namespace: String },
    /// The query itself is malformed — an unknown filter field, an
    /// unparseable cursor, an ordering the store cannot serve. Distinct from
    /// `LimitExceeded`: nothing here is about a bound, and telling a caller
    /// "reduce the value" when they named a field that does not exist sends
    /// them the wrong way.
    #[error("invalid query: {what}")]
    InvalidQuery { what: String },
    /// Durable corruption detected; operator action.
    #[error("store corrupt: {reason}")]
    Corrupt { reason: String },
    #[error("store unavailable: {reason}")]
    Unavailable { reason: String },
    #[error("deadline exceeded")]
    Deadline,
    #[error("cancelled")]
    Cancelled,
    /// Unexpected failure; details stay in access-controlled logs.
    #[error("internal store error: {0}")]
    Internal(String),
}

/// The store plugin contract (`cpt-cf-graph-storage-contract-graph-store-plugin`).
///
/// Five obligations are carried by specific methods and asserted by the
/// conformance suite against both the built-in store and the in-memory fake:
/// batch atomicity (`ingest`), single-writer serialization per scope identity
/// (`ingest` + `replace_scope`), monotonic generation fencing
/// (`replace_scope.generation`), no node removed while a live edge references
/// it (`soft_delete`), and one snapshot across every arm of one read
/// (`begin_read`).
#[async_trait]
pub trait GraphStoreV1: Send + Sync + 'static {
    /// What this store provides. Anything absent here is answered
    /// `Unsupported` by the methods below, never approximated.
    fn capabilities(&self) -> StoreCapabilities;

    // --- ontology ---------------------------------------------------------
    /// Register a batch atomically.
    ///
    /// `options.on_existing` decides what a changed schema under a registered
    /// identifier means: `Reject` (the default) conflicts, `Update` admits the
    /// change when it is admissible. A byte-identical re-registration
    /// converges under either. `options.dry_run` computes every verdict and
    /// writes nothing, so a caller can ask what an edit costs before making
    /// it; a dry run therefore reports a refusal in the result rather than as
    /// an error.
    async fn register_types_with(
        &self,
        ctx: &StoreCtx<'_>,
        batch: Vec<TypeRegistration>,
        options: TypeRegistrationOptions,
    ) -> Result<Vec<RegisteredType>, GraphStoreError>;

    /// Register a batch under the default options, keeping only the records.
    ///
    /// Provided, not implemented: one code path decides admission, so the
    /// convenience form cannot drift from the form that carries the options.
    async fn register_types(
        &self,
        ctx: &StoreCtx<'_>,
        batch: Vec<TypeRegistration>,
    ) -> Result<Vec<TypeRecord>, GraphStoreError> {
        let registered = self
            .register_types_with(ctx, batch, TypeRegistrationOptions::default())
            .await?;
        Ok(registered.into_iter().map(|item| item.record).collect())
    }
    async fn get_type(
        &self,
        ctx: &StoreCtx<'_>,
        id: &GtsTypeId,
    ) -> Result<TypeRecord, GraphStoreError>;
    async fn list_types(
        &self,
        ctx: &StoreCtx<'_>,
        query: TypeQuery,
    ) -> Result<Page<TypeRecord>, GraphStoreError>;
    /// Probe what this store can answer for, without a tenant or a scope.
    ///
    /// Readiness is reached before authentication — the matrix leaves the
    /// health endpoints available when the authorization resolver is down —
    /// so this is the one store call that takes no `StoreCtx`. It reports the
    /// rows only the store can answer: the database and its migrations, and
    /// the traversal backend the server actually provides.
    async fn probe_readiness(&self) -> Vec<ComponentReadiness>;

    // --- source namespaces ------------------------------------------------
    /// The namespaces claimed in this tenant, with the principal bound to
    /// each. A read of the ownership boundary itself, for an operator who has
    /// to answer "who owns this source".
    async fn list_source_namespaces(
        &self,
        ctx: &StoreCtx<'_>,
    ) -> Result<Vec<SourceNamespaceOwner>, GraphStoreError>;

    /// Bind `namespace` to `owner_principal`, recording who moved it.
    ///
    /// The only way a namespace changes hands: writing under someone else's
    /// namespace is refused rather than treated as a claim, so there is no
    /// implicit transfer. The caller is authorized for ontology
    /// administration, not merely for writing.
    async fn transfer_source_namespace(
        &self,
        ctx: &StoreCtx<'_>,
        namespace: &str,
        owner_principal: &str,
    ) -> Result<SourceNamespaceOwner, GraphStoreError>;

    /// Resolve GTS patterns to the set of registered types they cover, so a
    /// caller's type filter and an authorizing permission's pattern can be
    /// intersected on one representation.
    async fn resolve_type_set(
        &self,
        ctx: &StoreCtx<'_>,
        patterns: &[String],
    ) -> Result<TypeIdSet, GraphStoreError>;

    // --- write ------------------------------------------------------------
    /// Nodes, edges and the idempotency record commit together or not at all.
    /// A replay of a recorded key returns `IngestOutcome { replayed: true }`
    /// without touching state.
    ///
    /// `embedding` carries one entry per node of `req`, in order: the vector
    /// to store if this request embedded, and the canonical hash of the text
    /// it was composed from either way. A store does not compose or embed
    /// anything — that is the coordinator's, so that ingest and query cannot
    /// diverge.
    async fn ingest(
        &self,
        ctx: &StoreCtx<'_>,
        req: IngestRequest,
        embedding: EmbeddingPlan,
    ) -> Result<IngestOutcome, GraphStoreError>;
    /// Tombstone a node with its incident edges, or a single edge.
    async fn soft_delete(
        &self,
        ctx: &StoreCtx<'_>,
        req: DeleteRequest,
    ) -> Result<DeleteOutcome, GraphStoreError>;

    // --- labels -----------------------------------------------------------
    async fn upsert_label(
        &self,
        ctx: &StoreCtx<'_>,
        label: LabelSpec,
    ) -> Result<LabelRecord, GraphStoreError>;
    async fn delete_label(
        &self,
        ctx: &StoreCtx<'_>,
        id: LabelId,
    ) -> Result<RevisionOutcome, GraphStoreError>;
    async fn list_labels(&self, ctx: &StoreCtx<'_>) -> Result<Vec<LabelRecord>, GraphStoreError>;
    async fn assign_labels(
        &self,
        ctx: &StoreCtx<'_>,
        req: LabelAssignment,
    ) -> Result<RevisionOutcome, GraphStoreError>;

    // --- read -------------------------------------------------------------
    /// Open a snapshot for a compound read. Every subsequent call carrying it
    /// in `StoreCtx` observes one graph state.
    async fn begin_read(&self, ctx: &StoreCtx<'_>) -> Result<ReadSnapshot, GraphStoreError>;
    /// Close a snapshot opened by `begin_read`, releasing whatever holds it.
    async fn end_read(&self, snapshot: ReadSnapshot) -> Result<(), GraphStoreError>;
    async fn revision(&self, ctx: &StoreCtx<'_>) -> Result<GraphRevision, GraphStoreError>;
    async fn get_node(
        &self,
        ctx: &StoreCtx<'_>,
        key: &NodeKey,
        adjacency_limit: u32,
    ) -> Result<NodeView, GraphStoreError>;
    async fn hydrate_nodes(
        &self,
        ctx: &StoreCtx<'_>,
        ids: &[NodeId],
    ) -> Result<Vec<NodeView>, GraphStoreError>;
    /// One edge as an element, with its payload and audit envelope
    /// (`fr-audit-envelope`, which asks for the envelope on every node *and
    /// edge* a read surface returns). Scoped like a node read: an edge either
    /// of whose endpoints lies outside the caller's scope reads as absent.
    async fn get_edge(
        &self,
        ctx: &StoreCtx<'_>,
        key: &EdgeKey,
    ) -> Result<EdgeView, GraphStoreError>;
    /// One call, not one per arm: the scope must apply inside each arm before
    /// UNION, ranking and LIMIT, and RRF needs each arm's ranks.
    async fn search(
        &self,
        ctx: &StoreCtx<'_>,
        req: SearchRequest,
        vector: Option<VectorArm>,
    ) -> Result<SearchResponse, GraphStoreError>;
    async fn project_table(
        &self,
        ctx: &StoreCtx<'_>,
        req: ProjectionRequest,
    ) -> Result<toolkit_odata::Page<NodeRow>, GraphStoreError>;
    /// Node keys with their type and typed edge pairs, tombstoned rows
    /// excluded, paged. A store that cannot expose it declares the capability
    /// absent, and analytics is unavailable in that deployment (ADR-0007).
    async fn load_topology(
        &self,
        ctx: &StoreCtx<'_>,
        req: TopologyRequest,
    ) -> Result<TopologyPage, GraphStoreError>;

    // --- keys -------------------------------------------------------------
    /// Resolve producer keys to internal ids under the caller's scope.
    /// Unknown and unauthorized keys are absent from the answer alike
    /// (anti-enumeration).
    async fn resolve_node_ids(
        &self,
        ctx: &StoreCtx<'_>,
        keys: &[NodeKey],
    ) -> Result<Vec<(NodeKey, NodeId)>, GraphStoreError>;

    // --- embeddings -------------------------------------------------------
    /// What this store already holds of each key's vector, index-aligned with
    /// `keys`: `None` for a key that is unknown, tombstoned or outside the
    /// caller's scope (anti-enumeration, as `resolve_node_ids`).
    ///
    /// The coordinator reads this *before* embedding a batch so a node whose
    /// text has not changed is not embedded again — the difference between a
    /// re-sync that costs what it changes and one that costs a full import.
    /// Read outside the write transaction on purpose: it is an optimization,
    /// and the transaction's own `decide_vector` still settles every state.
    async fn embedding_state(
        &self,
        ctx: &StoreCtx<'_>,
        keys: &[NodeKey],
    ) -> Result<Vec<Option<EmbeddingState>>, GraphStoreError>;
}

/// What a store holds of one node's vector, for the coordinator's skip
/// decision. Two facts, because both are needed: a vector is worth keeping
/// only if it was made from the node's *current* text (`input_hash`) and is
/// rankable under the *current* space (`vector_epoch`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingState {
    /// Hash of the text the stored vector was made from, when a vector is
    /// stored; the hash of the current input otherwise.
    pub input_hash: Option<String>,
    /// The epoch the stored vector is current under. `None` when there is no
    /// vector, or when it is stale.
    pub vector_epoch: Option<i64>,
}

/// Engine-side failure vocabulary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GraphEngineError {
    /// The engine cannot enforce a property of this scope. The port serves
    /// the request on the fallback hop and logs the reason — a typed error,
    /// never a best-effort weaker predicate.
    #[error("scope not enforceable: {reason}")]
    ScopeNotEnforceable { reason: String },
    #[error("unsupported: {what}")]
    Unsupported { what: &'static str },
    #[error("engine unavailable: {reason}")]
    Unavailable { reason: String },
    #[error("deadline exceeded")]
    Deadline,
    #[error("cancelled")]
    Cancelled,
    #[error("internal engine error: {0}")]
    Internal(String),
}

/// Directed one-hop expansion request. Direction is explicit because the
/// undirected shorthand plans as an all-vertex probe; expansion is a one-hop
/// primitive because multi-hop chain patterns enumerate paths and explode on
/// hubs.
pub struct ExpandRequest {
    pub frontier: Vec<NodeId>,
    pub direction: Direction,
    /// Per-hop restriction, already resolved to registered types.
    pub edge_types: Option<TypeIdSet>,
    /// Per-hop restriction (labels are deferred; engines may answer
    /// `Unsupported`).
    pub labels: Option<LabelFilter>,
    pub budget: HopBudget,
    /// Ask for `ExpandResponse::degrees` to be filled.
    ///
    /// Off by default because it costs a second scoped read: the edges
    /// incident to the *reached* set, not only to the frontier. A traversal
    /// does not need it — its caller post-processes the whole region — and a
    /// neighborhood projection cannot do without it, because that is what
    /// decides which neighbours of a hub survive the node budget.
    pub with_degrees: bool,
}

pub struct ExpandResponse {
    pub reached: Vec<NodeId>,
    /// Each reached node's degree in the authorized subgraph, index-aligned
    /// with `reached`. Empty unless the request asked for it.
    ///
    /// The engine is the only party that can say: a reached node is an
    /// internal id and an `EdgeRef` names its endpoints by producer key, so
    /// nothing above this port can join the two without another read. Every
    /// edge counted has passed the caller's scope, so this is the degree
    /// *inside the authorized subgraph* and never a global one the caller
    /// cannot see (Authorization Model: "degree ordering, budgets and
    /// truncation are computed on authorized rows only"). It is what lets a
    /// neighborhood projection keep the structural core when a hub exceeds
    /// the node budget (`fr-neighborhood-projection`).
    ///
    /// A count is a lower bound when the hop reports `EdgeScanCap`: the scan
    /// stopped at the budget, so a node may have edges it did not see. The
    /// ranking is then approximate — which the truncation reason says.
    pub degrees: Vec<u32>,
    pub edges: Vec<EdgeRef>,
    /// Never silent.
    pub truncated: Option<TruncationReason>,
    /// Which backend produced this answer.
    ///
    /// **Found while building the prototype.** The pattern backend declines by
    /// falling back, and the decline was recorded only in a log line. A log
    /// line is not something a test can assert on, so the suite could not tell
    /// a pattern hop that ran from one that failed and was silently served by
    /// the two-query hop instead -- which is exactly what happened, for every
    /// traversal, when the pattern lost its anchor. Reporting the backend on
    /// the response is what makes "the pattern actually served this" an
    /// assertion rather than an assumption.
    pub served_by: HopBackend,
}

/// The hop backends of ADR-0005, as the answer reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HopBackend {
    /// One scoped `GRAPH_TABLE` statement.
    Pattern,
    /// Two scoped queries per hop; always available.
    TwoQuery,
}

/// The engine's applied `(source epoch, graph revision)` position. The epoch
/// is a non-reusable timeline identifier, so a projection that survived a
/// point-in-time restore of the source database is detected rather than
/// served.
pub struct EngineCursor {
    pub revision: GraphRevision,
}

pub struct ShortestPathRequest {
    pub from: NodeId,
    pub to: NodeId,
    pub max_depth: u8,
}

pub struct PathResponse {
    pub nodes: Vec<NodeId>,
    pub edges: Vec<EdgeRef>,
}

/// Declared-capability pattern matching (not shipped by the built-in engine).
pub struct PatternRequest {
    pub pattern: String,
}

pub struct PatternResponse {
    pub rows: Vec<Vec<NodeId>>,
}

/// The traversal-engine plugin contract
/// (`cpt-cf-graph-storage-contract-graph-engine-plugin`).
#[async_trait]
pub trait GraphEngineV1: Send + Sync + 'static {
    fn capabilities(&self) -> EngineCapabilities;

    async fn cursor(&self, ctx: &StoreCtx<'_>) -> Result<EngineCursor, GraphEngineError>;

    /// Directed one-hop expansion of an authorized frontier. Chained by the
    /// caller with per-hop dedup; the engine never expands beyond one hop, so
    /// budgets and authorization are re-evaluated between hops rather than
    /// inside an opaque traversal.
    async fn expand(
        &self,
        ctx: &StoreCtx<'_>,
        req: ExpandRequest,
    ) -> Result<ExpandResponse, GraphEngineError>;

    /// Declared capabilities only; otherwise `GraphEngineError::Unsupported`.
    async fn shortest_path(
        &self,
        ctx: &StoreCtx<'_>,
        req: ShortestPathRequest,
    ) -> Result<PathResponse, GraphEngineError>;
    async fn match_pattern(
        &self,
        ctx: &StoreCtx<'_>,
        req: PatternRequest,
    ) -> Result<PatternResponse, GraphEngineError>;
}

/// What the Embedding Coordinator decided about one node, for the store to
/// write. Index-aligned with `IngestRequest::nodes` — the same convention
/// [`EmbedResponse::vectors`] uses, and for the same reason: any other
/// association would have to be keyed on something a batch may legitimately
/// repeat.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeEmbedding {
    /// The vector, or `None` when this request did not ask for embedding.
    pub vector: Option<Vec<f32>>,
    /// Canonical hash of the text this node embeds from, computed whether or
    /// not it was embedded. It is what tells a later ingest whether a
    /// preserved vector still describes the node — the difference between a
    /// *preserved* vector and a *stale* one.
    pub input_hash: String,
}

impl NodeEmbedding {
    #[must_use]
    pub fn computed(vector: Vec<f32>, input_hash: String) -> Self {
        Self {
            vector: Some(vector),
            input_hash,
        }
    }

    #[must_use]
    pub fn skipped(input_hash: String) -> Self {
        Self {
            vector: None,
            input_hash,
        }
    }
}

/// The vector arm of one search, as the coordinator resolved it.
///
/// Absent when the request asked for no vector arm, or when no comparable
/// embedding space is in force. A store never embeds anything itself: query
/// text and ingest text must go through one provider, and only the
/// coordinator holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorArm {
    pub query_vector: Vec<f32>,
    /// Only vectors of this epoch may be ranked. Vectors of any other epoch,
    /// and vectors whose input has since changed, are not comparable with the
    /// query and must not appear.
    pub epoch: i64,
}

/// The embedding decisions of one ingest batch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmbeddingPlan {
    /// Epoch to stamp new vectors with. `None` means no comparable space is
    /// in force, so no vector may be written or read.
    pub epoch: Option<i64>,
    /// One entry per node of the request, in order.
    pub nodes: Vec<NodeEmbedding>,
}

/// Provider-side failure vocabulary. A provider failure fails the ingest
/// batch; it is never downgraded to an unembedded write.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EmbeddingProviderError {
    #[error("provider unavailable: {reason}")]
    Unavailable { reason: String },
    #[error("embedding space mismatch")]
    SpaceMismatch,
    #[error("deadline exceeded")]
    Deadline,
    #[error("cancelled")]
    Cancelled,
    #[error("internal provider error: {0}")]
    Internal(String),
}

/// Batched, not per item: the batch is where a remote provider's round trip
/// is amortized.
pub struct EmbedRequest {
    pub inputs: Vec<String>,
    pub budget: RemainingBudget,
    pub cancel: CancellationToken,
}

pub struct EmbedResponse {
    /// Aligned with `inputs` by index; a provider that cannot return one
    /// vector per input fails the call rather than returning a short vector.
    pub vectors: Vec<Vec<f32>>,
    /// Echoed so a mismatch is caught at use, not only at configuration.
    pub space: EmbeddingSpaceId,
}

/// The embedding-provider plugin contract
/// (`cpt-cf-graph-storage-contract-embedding-provider`).
#[async_trait]
pub trait EmbeddingProviderV1: Send + Sync + 'static {
    /// Model artifact, tokenizer artifact, preprocessing and pooling
    /// configuration — not just a dimension.
    fn embedding_space(&self) -> &EmbeddingSpaceId;
    fn dimension(&self) -> u32;

    async fn embed(&self, req: EmbedRequest) -> Result<EmbedResponse, EmbeddingProviderError>;

    async fn health(&self) -> Result<(), EmbeddingProviderError>;
}
