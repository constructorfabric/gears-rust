//! Transport-agnostic models of the graph-storage contract.
//!
//! These types cross three boundaries — the `ClientHub` trait, the REST DTO
//! layer (which owns all serde), and the plugin contracts — so they carry no
//! serde derives, no HTTP types and no database types. Payloads are arbitrary
//! GTS-validated JSON and travel as [`serde_json::Value`].

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use uuid::Uuid;

/// Tenant identity, as carried by the platform security context.
pub type TenantId = Uuid;

/// Internal node identity. Surrogate and per-tenant: two tenants may both own
/// a node `17`, so it is never meaningful outside a tenant-scoped call.
pub type NodeId = i64;

/// Internal edge identity, with the same per-tenant caveat as [`NodeId`].
pub type EdgeId = i64;

/// Producer-supplied stable node key, unique within a tenant.
pub type NodeKey = String;

/// Deterministic edge key derived from (type, src, dst, discriminator).
pub type EdgeKey = String;

/// Canonical GTS type identifier (`gts.vendor.package._.type.v1~` form).
pub type GtsTypeId = String;

/// Interned label identity.
pub type LabelId = i32;

// ---------------------------------------------------------------------------
// Ontology
// ---------------------------------------------------------------------------

/// Kind of a registrable GTS type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeKind {
    Node,
    Edge,
    Attribute,
}

impl TypeKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Edge => "edge",
            Self::Attribute => "attribute",
        }
    }
}

/// One type submitted for registration.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeRegistration {
    /// Canonical GTS identifier; must derive from one of the gear's family
    /// types (base -> family -> producer type, two derivations max).
    pub type_id: GtsTypeId,
    /// The type's draft-07 JSON Schema.
    pub schema: serde_json::Value,
}

/// Trait values resolved across the whole derivation chain, stored with the
/// registered type so batch validation never repeats the walk.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectiveTraits {
    /// `owned` / `reference` / `phantom` for nodes, `static` / `analysis` for
    /// edges. `None` only on abstract types, which are uninstantiable.
    pub family: Option<String>,
    pub scope_managed: bool,
    pub emit_events: bool,
    /// JSON-pointer payload paths admitted to `$filter` / `$orderby`.
    pub index: Vec<String>,
    /// JSON-pointer payload paths folded into the lexical search text.
    pub full_text_search: Vec<String>,
    /// JSON-pointer payload paths folded into the embedding input.
    pub vector_search: Vec<String>,
    /// Edge endpoint constraints, GTS patterns (edges only).
    pub src_types: Vec<String>,
    pub dst_types: Vec<String>,
}

/// A registered type as the gear reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeRecord {
    pub type_id: GtsTypeId,
    /// Deterministic `UUIDv5` of the GTS identifier (the platform derivation).
    pub type_uuid: Uuid,
    pub kind: TypeKind,
    /// Abstract types (the bases and families) cannot be instantiated.
    pub is_abstract: bool,
    pub schema: serde_json::Value,
    pub effective_traits: EffectiveTraits,
    pub created_at: OffsetDateTime,
    /// Which retained definition of this identifier is in force: `1` until the
    /// type is first updated in place, then one more per accepted update
    /// (types-registry ADR-0005 calls each of them a retained revision).
    pub revision: i32,
}

/// Filter for listing registered types.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypeQuery {
    pub kind: Option<TypeKind>,
    /// GTS identifier pattern, resolved by the shared GTS implementation —
    /// never compiled to SQL text.
    pub pattern: Option<String>,
    pub top: Option<u32>,
    pub cursor: Option<String>,
}

/// A resolved set of registered types, the single representation on which a
/// caller's type filter and an authorizing permission's pattern intersect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeIdSet(pub BTreeSet<GtsTypeId>);

impl TypeIdSet {
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        Self(self.0.intersection(&other.0).cloned().collect())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn contains(&self, type_id: &str) -> bool {
        self.0.contains(type_id)
    }
}

// ---------------------------------------------------------------------------
// Type evolution (registering a changed schema under a known identifier)
// ---------------------------------------------------------------------------

/// What a registration batch may do to an identifier that is already
/// registered with a *different* schema.
///
/// The platform decided the policy before the gear did: types-registry
/// ADR-0004 says a major-only GTS id names a mutable logical entity whose
/// backward-compatible updates keep that id, and ADR-0003 fixes the direction
/// (`BACKWARD`), the baseline (the current revision) and the posture (an
/// undecidable check is a refusal). This enum is only the per-request switch
/// between the gear's historical behaviour and that policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnExisting {
    /// A changed schema is a conflict — the gear's behaviour before type
    /// updates existed, and still the default so no existing caller changes.
    #[default]
    Reject,
    /// Admit the change when it is admissible; refuse it, with the offending
    /// schema locations, when it is not.
    Update,
}

/// Per-batch registration options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TypeRegistrationOptions {
    pub on_existing: OnExisting,
    /// Admit a change the schemas cannot prove compatible when every stored
    /// row of the type still validates against the candidate.
    ///
    /// This is deliberately a second, explicit ground for admission rather
    /// than a relaxation of the first: it is a statement about *this tenant's
    /// current rows*, not about the accepted instance sets, and it costs a
    /// scan of the type bounded by `type_update_max_rows`.
    pub revalidate: bool,
    /// Compute and report every verdict, write nothing.
    pub dry_run: bool,
}

/// How the candidate stands against the registered definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeChangeState {
    /// Nothing is registered under this identifier yet.
    New,
    /// Byte-identical to what is registered.
    Unchanged,
    /// `Valid(old) ⊆ Valid(new)` proved from the schemas.
    Compatible,
    /// Proved *not* to hold.
    Incompatible,
    /// Could be neither proved nor disproved (`gts` reports `Unknown`).
    /// ADR-0003 fails closed on this, so it is a refusal — but a distinct one,
    /// because the fix is a different one.
    Undecidable,
}

impl TypeChangeState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Unchanged => "unchanged",
            Self::Compatible => "compatible",
            Self::Incompatible => "incompatible",
            Self::Undecidable => "undecidable",
        }
    }
}

/// One reason a directional verdict does not hold, with the schema location
/// that carries it — so a refusal points at `$.payload` rather than saying
/// "incompatible".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaDiagnostic {
    /// Location in the resolved schema, `$` for the document root.
    pub location: String,
    /// Machine-readable finding kind, as `gts` names it.
    pub finding: String,
    pub message: String,
}

/// How one trait's declared paths changed between the two definitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraitChange {
    /// `index`, `full_text_search`, `vector_search`, `src_types`, `dst_types`.
    pub trait_name: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// The full verdict on one candidate: what the dry run reports and what a
/// refusal explains itself with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeChange {
    pub type_id: GtsTypeId,
    pub state: TypeChangeState,
    /// `compatible` / `incompatible` / `unknown`; the direction ADR-0003
    /// enforces.
    pub backward: String,
    /// Computed and reported, never enforced — the same posture as the
    /// registry. It tells a producer whether an old reader still accepts new
    /// payloads.
    pub forward: String,
    /// Evidence for the backward verdict.
    pub diagnostics: Vec<SchemaDiagnostic>,
    pub traits_changed: Vec<TraitChange>,
    /// Live rows of this type, when the operation needed to know.
    pub rows: Option<u64>,
    /// Object levels of the candidate where a *later* definition will not be
    /// able to add an optional property (`ContentModel::is_evolvable_in_place`).
    /// Reported so "your next edit will be a major" is a warning today rather
    /// than a surprise later.
    pub levels_not_evolvable_in_place: Vec<String>,
    /// Whether this change needs more than the schemas to be admitted.
    pub migration_required: bool,
    /// Whether the gear would admit it under the options of this request.
    pub admissible: bool,
}

/// Why an update was admitted. Two grounds, never conflated in a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionBasis {
    /// The schemas prove `Valid(old) ⊆ Valid(new)`. No row was read.
    SchemaProved,
    /// The schemas do not prove it; every live row of the type was validated
    /// against the candidate instead. True of *these rows*, not of the type.
    DataBacked { rows_validated: u64 },
}

/// What one registration did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeOutcome {
    Created,
    /// Already registered, byte-identical, nothing written.
    Unchanged,
    /// The stored definition was replaced under the same identifier.
    Updated,
}

impl TypeOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Unchanged => "unchanged",
            Self::Updated => "updated",
        }
    }
}

/// A registered type plus what this call did to it.
#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredType {
    pub record: TypeRecord,
    pub outcome: TypeOutcome,
    /// Present when `outcome` is `Updated`.
    pub basis: Option<AdmissionBasis>,
    /// Present when the identifier was already registered, and always in a
    /// dry run.
    pub change: Option<TypeChange>,
}

// ---------------------------------------------------------------------------
// Revision-bound identity (Read Consistency Contract)
// ---------------------------------------------------------------------------

/// The snapshot identity every compound read observes and reports: the
/// deployment-wide, non-reusable source epoch paired with the per-tenant
/// monotonic revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphRevision {
    pub source_epoch: i64,
    pub revision: i64,
}

/// Handle to one open compound-read snapshot. Opaque to callers; the store
/// that issued it resolves it back to a live snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadSnapshot {
    pub id: Uuid,
    pub revision: GraphRevision,
}

// ---------------------------------------------------------------------------
// Element envelope (fr-audit-envelope)
// ---------------------------------------------------------------------------

/// The party behind a write, in the platform's own vocabulary rather than in
/// a vocabulary of this gear's own: `SecurityContext`'s `subject_id` and
/// optional `subject_type`.
///
/// A subject and not a user because most writes into this gear arrive from an
/// automation or a service integration, so a `user_id` member would be empty
/// on the majority of rows and would need a second member beside it for the
/// rest (DESIGN § API element envelope).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subject {
    pub subject_id: Uuid,
    /// GTS type of the acting subject, e.g.
    /// `gts.cf.core.security.subject_user.v1~`. Optional, matching
    /// `SecurityContext`, which does not always carry one.
    pub subject_type: Option<GtsTypeId>,
}

impl Subject {
    /// The subject a `SecurityContext` names.
    #[must_use]
    pub fn from_security_context(ctx: &toolkit_security::SecurityContext) -> Self {
        Self {
            subject_id: ctx.subject_id(),
            subject_type: ctx.subject_type().map(ToOwned::to_owned),
        }
    }
}

/// The gear-assigned half of an element, identical for every node and every
/// edge and described by the API schema rather than by the element's GTS type
/// -- a producer can neither supply nor extend it, and a type registered
/// statically in the types-registry has nothing to put in it.
///
/// It is read-only on every write surface: an envelope member a producer
/// sends is ignored rather than rejected, so a document read from the API can
/// be sent back unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementEnvelope {
    pub tenant_id: Uuid,
    /// The element's key: a node's producer-supplied `node_key`, an edge's
    /// gear-derived `edge_key`.
    pub key: String,
    pub created_at: OffsetDateTime,
    pub created_by: Subject,
    pub updated_at: OffsetDateTime,
    pub updated_by: Subject,
    /// Soft-delete tombstone; absent on a live element.
    pub deleted_at: Option<OffsetDateTime>,
    pub deleted_by: Option<Subject>,
    /// The revision the read that produced this element observed.
    ///
    /// Per element rather than per response because the tabular projection
    /// answers inside `toolkit_odata::Page`, which carries items and cursors
    /// and nothing else -- so this is the only place that read path can
    /// report the snapshot it observed (PRD § fr-tabular-projection).
    pub graph_revision: GraphRevision,
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

/// A node submitted for ingest. An upsert replaces the row's mutable state
/// wholesale: a field the request omits is cleared, never preserved.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeSpec {
    pub node_key: NodeKey,
    pub type_id: GtsTypeId,
    pub name: Option<String>,
    /// GTS-validated attributes. `None` = no opinion on an existing row's
    /// payload is *not* offered — ingest is replace, so `None` clears.
    pub payload: Option<serde_json::Value>,
    /// Optional compare-and-set on the node's stored version.
    pub expected_version: Option<i64>,
}

/// An edge submitted for ingest, addressed by its endpoint node keys.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EdgeSpec {
    pub type_id: GtsTypeId,
    pub src_node_key: NodeKey,
    pub dst_node_key: NodeKey,
    /// Distinguishes parallel edges of one type between one endpoint pair.
    pub discriminator: Option<String>,
    pub payload: Option<serde_json::Value>,
}

/// Declarative scope replacement carried by an ingest batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaceScope {
    /// Scope attribute of the canonical identity
    /// `(tenant, owning producer, scope attribute, scope value)`.
    pub attribute: String,
    pub value: String,
    /// Monotonic source generation. Older than the recorded one is rejected as
    /// stale; equal with identical content is a replay; equal with different
    /// content conflicts.
    pub generation: i64,
}

/// Per-request ingest options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IngestOptions {
    /// Create phantom endpoint nodes for edges whose endpoints are not in the
    /// batch and not stored. `None` = the deployment default (on).
    pub create_phantoms: Option<bool>,
    /// Return per-item outcomes on success (errors are always per item).
    pub report_per_item: bool,
    /// Whether this batch's nodes are embedded. `None` = the deployment
    /// default (on). `false` keeps existing vectors rather than clearing
    /// them: a metadata-only re-sync should not cost a re-embedding pass, and
    /// should not silently empty the vector arm either.
    pub embed: Option<bool>,
}

/// One atomic ingest batch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IngestRequest {
    pub nodes: Vec<NodeSpec>,
    pub edges: Vec<EdgeSpec>,
    pub options: IngestOptions,
    pub replace_scope: Option<ReplaceScope>,
    /// Producer-chosen idempotency key (the REST layer reads the same value
    /// from the `Idempotency-Key` header).
    pub idempotency_key: Option<String>,
}

/// Aggregate counters of one committed batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IngestCounts {
    pub nodes_inserted: u64,
    pub nodes_updated: u64,
    pub nodes_unchanged: u64,
    pub edges_inserted: u64,
    pub edges_updated: u64,
    pub edges_unchanged: u64,
    pub phantoms_created: u64,
    pub phantoms_materialized: u64,
    /// Rows tombstoned by scope replacement.
    pub scope_removed_nodes: u64,
    pub scope_removed_edges: u64,
}

/// Which collection an ingest item belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemFamily {
    Node,
    Edge,
}

/// Per-item outcome, reported when `options.report_per_item` is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemOutcome {
    Inserted,
    Updated,
    Unchanged,
    Materialized,
}

/// One per-item validation failure. A batch with any of these commits nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemError {
    pub index: usize,
    pub family: ItemFamily,
    pub gts_type: Option<GtsTypeId>,
    /// JSON pointer to the offending value, when the failure is positional.
    pub pointer: Option<String>,
    pub message: String,
}

/// Outcome of one ingest call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestOutcome {
    /// Revision the graph reached once the batch committed (unchanged when
    /// the batch converged without modifying anything).
    pub revision: GraphRevision,
    /// True when an idempotency receipt answered the call without touching
    /// state.
    pub replayed: bool,
    pub counts: IngestCounts,
    pub per_item_nodes: Option<Vec<ItemOutcome>>,
    pub per_item_edges: Option<Vec<ItemOutcome>>,
}

/// Soft-delete target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeleteRequest {
    /// Tombstone a node together with its incident edges.
    Node(NodeKey),
    /// Tombstone one edge.
    Edge(EdgeKey),
}

/// Outcome of a soft delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeleteOutcome {
    pub revision: GraphRevision,
    pub tombstoned_nodes: u64,
    pub tombstoned_edges: u64,
}

// ---------------------------------------------------------------------------
// Node read / projection
// ---------------------------------------------------------------------------

/// Edge incidence direction relative to a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjacencySide {
    Outgoing,
    Incoming,
}

/// One incident edge in a node read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdjacencyEntry {
    pub edge_key: EdgeKey,
    pub edge_type_id: GtsTypeId,
    pub side: AdjacencySide,
    pub neighbor_key: NodeKey,
    pub neighbor_type_id: GtsTypeId,
}

/// A node as read paths return it.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeView {
    pub node_key: NodeKey,
    pub type_id: GtsTypeId,
    pub name: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub has_embedding: bool,
    pub labels: Vec<String>,
    pub adjacency: Vec<AdjacencyEntry>,
    pub adjacency_truncated: bool,
    /// Gear-assigned audit envelope (`fr-audit-envelope`).
    pub envelope: ElementEnvelope,
}

/// One row of the tabular projection.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeRow {
    pub node_key: NodeKey,
    pub type_id: GtsTypeId,
    pub name: Option<String>,
    pub payload: Option<serde_json::Value>,
    /// Gear-assigned audit envelope (`fr-audit-envelope`). On this path it is
    /// also the only carrier of the observed revision: the page wrapper is
    /// the platform's and has no member for one.
    pub envelope: ElementEnvelope,
}

/// A page of results with an opaque continuation token bound to the observed
/// revision (Read Consistency Contract).
#[derive(Clone, Debug, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub revision: GraphRevision,
}

/// Filterable-field schema of the node projection.
///
/// Never constructed: it exists to feed `#[derive(ODataFilterable)]`, which
/// generates [`NodeQueryFilterField`] and its `FilterField` impl. Declaring it
/// here rather than on the REST DTO keeps one authority for what `$filter` and
/// `$orderby` may name — the store's column mapping is written against this
/// type, so a field nobody mapped cannot reach a query.
///
/// Payload paths are deliberately absent: they are admissible only where a
/// type's `index` trait declares them *and* an index backs them, which this
/// iteration does not yet build.
#[derive(toolkit_odata_macros::ODataFilterable)]
pub struct NodeQuery {
    /// The producer-supplied node key.
    #[odata(filter(kind = "String"))]
    pub node_key: String,
    /// The node's display name.
    #[odata(filter(kind = "String"))]
    pub name: String,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: time::OffsetDateTime,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub updated_at: time::OffsetDateTime,
}

pub use NodeQueryFilterField as NodeFilterField;

/// Tabular projection query.
///
/// Filtering, ordering and pagination are the **platform** `OData` binding —
/// the parsed [`toolkit_odata::ODataQuery`], carrying the `CursorV1`
/// continuation token and its filter hash — not a second dialect of our own.
#[derive(Clone, Debug, Default)]
pub struct ProjectionRequest {
    /// Restrict to these types (already intersected with the authorizing
    /// permission's pattern by the domain layer).
    pub type_set: Option<TypeIdSet>,
    /// The accepted system query options, already parsed and validated.
    pub query: toolkit_odata::ODataQuery,
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Which arm produced a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchArm {
    Lexical,
    Vector,
}

/// Search mode. Hybrid runs both arms independently and fuses them with RRF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Lexical,
    Vector,
    Hybrid,
}

/// One search request.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchRequest {
    pub mode: SearchMode,
    /// Query text for the lexical arm.
    pub query: Option<String>,
    /// Per-arm candidate limit before fusion.
    pub arm_limit: u32,
    /// Result limit after fusion.
    pub limit: u32,
    /// GTS type patterns narrowing the searched set.
    pub type_patterns: Vec<String>,
}

/// A hit's per-arm provenance: which arm matched, at what rank and raw score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArmHit {
    pub arm: SearchArm,
    pub rank: u32,
    pub score: f64,
}

/// One fused search hit.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub node_key: NodeKey,
    pub type_id: GtsTypeId,
    pub name: Option<String>,
    /// Fused (RRF) score.
    pub score: f64,
    pub arms: Vec<ArmHit>,
    /// Highlighted snippet from the lexical arm, when it matched.
    pub snippet: Option<String>,
}

/// Search response, revision-stamped like every compound read.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub revision: GraphRevision,
}

// ---------------------------------------------------------------------------
// Traversal
// ---------------------------------------------------------------------------

/// Expansion direction. `Either` is the union of the two directed scans in
/// one semi-join — never the undirected pattern shorthand, which plans as an
/// all-vertex probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Outgoing,
    Incoming,
    Either,
}

/// Per-hop budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HopBudget {
    pub max_frontier: u32,
    pub max_edges_scanned: u64,
}

/// Why an expansion or traversal stopped early. Never silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TruncationReason {
    FrontierCap,
    EdgeScanCap,
    NodeBudget,
}

/// A traversed edge reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeRef {
    pub edge_key: EdgeKey,
    pub edge_type_id: GtsTypeId,
    pub src: NodeKey,
    pub dst: NodeKey,
}

/// Label filter placeholder (labels are not shipped in this iteration; the
/// field exists so the plugin contract does not change when they are).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelFilter {
    pub any_of: Vec<String>,
}

/// Seeded, depth-bounded traversal request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TraverseRequest {
    pub seeds: Vec<NodeKey>,
    pub depth: u8,
    /// Per-hop edge-type restriction (GTS patterns).
    pub edge_type_patterns: Vec<String>,
    /// Node-type filter applied to the output set (seeds always survive).
    pub node_type_patterns: Vec<String>,
    pub max_nodes: Option<u32>,
}

/// Bounded neighborhood projection request.
#[derive(Clone, Debug, PartialEq)]
pub struct NeighborhoodRequest {
    pub root: NodeKey,
    pub depth: u8,
    pub node_budget: Option<u32>,
    pub include_phantoms: bool,
}

/// Traversal / neighborhood response.
#[derive(Clone, Debug, PartialEq)]
pub struct TraversalResponse {
    pub nodes: Vec<NodeView>,
    pub edges: Vec<EdgeRef>,
    pub truncated: Option<TruncationReason>,
    pub revision: GraphRevision,
}

// ---------------------------------------------------------------------------
// Labels (contract present, implementation deferred)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct LabelSpec {
    pub name: String,
    pub description: Option<String>,
    pub style: Option<serde_json::Value>,
    pub applies_to: LabelAppliesTo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelAppliesTo {
    Node,
    Edge,
    Both,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LabelRecord {
    pub id: LabelId,
    pub spec: LabelSpec,
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelAssignment {
    pub target: LabelTarget,
    pub attach: Vec<LabelId>,
    pub detach: Vec<LabelId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LabelTarget {
    Node(NodeKey),
    Edge(EdgeKey),
}

/// Revision-only outcome for label mutations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevisionOutcome {
    pub revision: GraphRevision,
}

// ---------------------------------------------------------------------------
// Topology (analytics boundary; capability optional)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopologyRequest {
    pub cursor: Option<String>,
    pub page_size: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopologyPage {
    pub nodes: Vec<(NodeKey, GtsTypeId)>,
    pub edges: Vec<EdgeRef>,
    pub next_cursor: Option<String>,
    pub schema_version: u32,
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// What a store implementation provides. Anything absent is answered
/// `Unsupported`, never approximated.
#[expect(
    clippy::struct_excessive_bools,
    reason = "a capability set is independent yes/no facts read by name, not a \
              parameter list; collapsing them into flags would hide which \
              capability a store lacks at the call site"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreCapabilities {
    pub scope_replace: bool,
    pub snapshots: bool,
    pub vector_search: bool,
    pub labels: bool,
    pub chunks: bool,
    pub topology: bool,
}

/// What an engine implementation provides beyond one-hop expansion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineCapabilities {
    pub shortest_path: bool,
    pub match_pattern: bool,
}

// ---------------------------------------------------------------------------
// Budget
// ---------------------------------------------------------------------------

/// What is left of an operation's absolute deadline — never a fresh timeout,
/// so a slow earlier step shortens the next one rather than extending the
/// total.
#[derive(Clone, Copy, Debug)]
pub struct RemainingBudget {
    deadline: Instant,
}

impl RemainingBudget {
    /// Open a budget expiring `total` from now.
    #[must_use]
    pub fn starting_now(total: Duration) -> Self {
        Self {
            deadline: Instant::now() + total,
        }
    }

    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.remaining().is_zero()
    }
}

// ---------------------------------------------------------------------------
// Embedding space
// ---------------------------------------------------------------------------

/// Full embedding-space identity. Two providers with the same dimension and
/// different identities produce incomparable vectors, so the identity is more
/// than a width.
///
/// The fields below are exactly the ones the `embedding_space` table records
/// (DESIGN § Table `embedding_space`), so a provider's declaration and the
/// durable row cannot describe different things.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingSpaceId {
    /// Canonical hash over the artifact/preprocessing identity below. Derived
    /// by [`EmbeddingSpaceId::new`] — never assembled by hand, or two
    /// providers describing one space would disagree about its name.
    pub identity_hash: String,
    /// Exact model artifact: name plus version or content hash.
    pub model_artifact: String,
    /// Exact tokenizer artifact, on the same terms.
    pub tokenizer_artifact: String,
    /// Declared preprocessing, pooling and normalization configuration. A
    /// different pooling rule over identical weights still yields vectors
    /// that must not be compared, so these are part of the identity rather
    /// than documentation of it.
    pub preprocessing: serde_json::Value,
    pub pooling: serde_json::Value,
    pub normalization: serde_json::Value,
    pub dimension: u32,
}

impl EmbeddingSpaceId {
    /// Build an identity and derive its canonical hash.
    ///
    /// The hash lives here rather than in each provider because it is the name
    /// readiness compares against: the ONNX plugin, a remote plugin and the
    /// deterministic fake must all arrive at the same string for the same
    /// space, and at different strings for different ones.
    #[must_use]
    pub fn new(
        model_artifact: impl Into<String>,
        tokenizer_artifact: impl Into<String>,
        preprocessing: serde_json::Value,
        pooling: serde_json::Value,
        normalization: serde_json::Value,
        dimension: u32,
    ) -> Self {
        let model_artifact = model_artifact.into();
        let tokenizer_artifact = tokenizer_artifact.into();
        let identity_hash = identity_hash(
            &model_artifact,
            &tokenizer_artifact,
            &preprocessing,
            &pooling,
            &normalization,
            dimension,
        );
        Self {
            identity_hash,
            model_artifact,
            tokenizer_artifact,
            preprocessing,
            pooling,
            normalization,
            dimension,
        }
    }
}

/// Recursively sort object keys so two semantically identical configurations
/// hash identically regardless of member order.
fn canonical(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, inner)| (key.clone(), canonical(inner)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical).collect())
        }
        other => other.clone(),
    }
}

fn identity_hash(
    model_artifact: &str,
    tokenizer_artifact: &str,
    preprocessing: &serde_json::Value,
    pooling: &serde_json::Value,
    normalization: &serde_json::Value,
    dimension: u32,
) -> String {
    // `aws-lc-rs` is the workspace's FIPS-capable backend; a pure-Rust hasher
    // is refused by the DE0708 lint. Field boundaries are length-prefixed so
    // no concatenation of distinct identities can collide.
    let mut hasher = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    let preprocessing = canonical(preprocessing).to_string();
    let pooling = canonical(pooling).to_string();
    let normalization = canonical(normalization).to_string();
    for part in [
        model_artifact.as_bytes(),
        tokenizer_artifact.as_bytes(),
        preprocessing.as_bytes(),
        pooling.as_bytes(),
        normalization.as_bytes(),
        &dimension.to_be_bytes(),
    ] {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finish())
}

#[cfg(test)]
mod embedding_space_tests {
    use super::EmbeddingSpaceId;

    fn space(pooling: &str, dimension: u32) -> EmbeddingSpaceId {
        EmbeddingSpaceId::new(
            "all-MiniLM-L6-v2@sha256:abc",
            "bert-wordpiece@sha256:def",
            serde_json::json!({ "lowercase": true }),
            serde_json::json!({ "strategy": pooling }),
            serde_json::json!({ "l2": true }),
            dimension,
        )
    }

    #[test]
    fn the_same_identity_hashes_the_same_however_the_json_is_ordered() {
        let one = EmbeddingSpaceId::new(
            "m",
            "t",
            serde_json::json!({ "a": 1, "b": 2 }),
            serde_json::json!({}),
            serde_json::json!({}),
            384,
        );
        let other = EmbeddingSpaceId::new(
            "m",
            "t",
            serde_json::json!({ "b": 2, "a": 1 }),
            serde_json::json!({}),
            serde_json::json!({}),
            384,
        );
        assert_eq!(one.identity_hash, other.identity_hash);
    }

    /// The case ADR-0005 exists for: same weights, same width, different
    /// pooling — incomparable vectors that a dimension check cannot see.
    #[test]
    fn pooling_alone_changes_the_identity() {
        assert_ne!(
            space("mean", 384).identity_hash,
            space("cls", 384).identity_hash
        );
    }

    #[test]
    fn dimension_alone_changes_the_identity() {
        assert_ne!(
            space("mean", 384).identity_hash,
            space("mean", 768).identity_hash
        );
    }
}
