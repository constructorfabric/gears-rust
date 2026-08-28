//! REST DTOs. All serde and `OpenAPI` schema live here; the SDK models stay
//! transport-agnostic. Names are `Graph*`-prefixed so they cannot collide in
//! a shared `OpenAPI` component registry.

use graph_storage_sdk::models as m;

// ---------------------------------------------------------------------------
// Ontology
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphTypeRegistrationDto {
    /// Canonical GTS identifier of the type being registered.
    pub type_id: String,
    /// Its draft-07 JSON Schema.
    pub schema: serde_json::Value,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphRegisterTypesRequest {
    pub types: Vec<GraphTypeRegistrationDto>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphEffectiveTraitsDto {
    pub family: Option<String>,
    pub scope_managed: bool,
    pub emit_events: bool,
    pub index: Vec<String>,
    pub full_text_search: Vec<String>,
    pub vector_search: Vec<String>,
    pub src_types: Vec<String>,
    pub dst_types: Vec<String>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphTypeDto {
    pub type_id: String,
    pub type_uuid: String,
    pub kind: String,
    pub is_abstract: bool,
    pub schema: serde_json::Value,
    pub effective_traits: GraphEffectiveTraitsDto,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphTypeListDto {
    pub items: Vec<GraphTypeDto>,
    pub next_cursor: Option<String>,
    pub revision: GraphRevisionDto,
}

// ---------------------------------------------------------------------------
// Revision
// ---------------------------------------------------------------------------

/// The snapshot identity every read reports: the deployment-wide,
/// non-reusable source epoch paired with the per-tenant revision.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphRevisionDto {
    pub source_epoch: i64,
    pub revision: i64,
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphNodeSpecDto {
    pub node_key: String,
    pub type_id: String,
    pub name: Option<String>,
    /// Omitted clears the stored payload: ingest replaces, never merges.
    pub payload: Option<serde_json::Value>,
    pub expected_version: Option<i64>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphEdgeSpecDto {
    pub type_id: String,
    pub src_node_key: String,
    pub dst_node_key: String,
    pub discriminator: Option<String>,
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Default)]
#[toolkit_macros::api_dto(request)]
pub struct GraphIngestOptionsDto {
    pub create_phantoms: Option<bool>,
    #[serde(default)]
    pub report_per_item: bool,
    /// `false` skips embedding; existing vectors are kept, not cleared.
    /// Omitted means the deployment default (on).
    pub embed: Option<bool>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphReplaceScopeDto {
    pub attribute: String,
    pub value: String,
    pub generation: i64,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphIngestRequest {
    #[serde(default)]
    pub nodes: Vec<GraphNodeSpecDto>,
    #[serde(default)]
    pub edges: Vec<GraphEdgeSpecDto>,
    #[serde(default)]
    pub options: GraphIngestOptionsDto,
    pub replace_scope: Option<GraphReplaceScopeDto>,
    /// Mirrors the `Idempotency-Key` header for SDK callers; the header wins
    /// when both are present.
    pub idempotency_key: Option<String>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphIngestCountsDto {
    pub nodes_inserted: u64,
    pub nodes_updated: u64,
    pub nodes_unchanged: u64,
    pub edges_inserted: u64,
    pub edges_updated: u64,
    pub edges_unchanged: u64,
    pub phantoms_created: u64,
    pub phantoms_materialized: u64,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphIngestResultDto {
    pub revision: GraphRevisionDto,
    /// True when an idempotency receipt answered without touching state.
    pub replayed: bool,
    pub counts: GraphIngestCountsDto,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphDeleteResultDto {
    pub revision: GraphRevisionDto,
    pub tombstoned_nodes: u64,
    pub tombstoned_edges: u64,
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphAdjacencyEntryDto {
    pub edge_key: String,
    pub edge_type_id: String,
    pub direction: String,
    pub neighbor_key: String,
    pub neighbor_type_id: String,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphNodeDto {
    pub node_key: String,
    pub type_id: String,
    pub name: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub has_embedding: bool,
    pub adjacency: Vec<GraphAdjacencyEntryDto>,
    pub adjacency_truncated: bool,
}

/// One projection row. What `$filter` and `$orderby` may name is declared
/// once, on `graph_storage_sdk::NodeQuery`.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphNodeRowDto {
    pub node_key: String,
    pub type_id: String,
    pub name: Option<String>,
    pub payload: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphSearchRequest {
    /// `lexical`, `vector` or `hybrid`.
    pub mode: String,
    /// Required by every mode. The vector arm embeds this same text through
    /// the deployment's provider -- the one ingest used -- so a caller never
    /// supplies a vector of its own.
    pub query: Option<String>,
    pub arm_limit: Option<u32>,
    pub limit: Option<u32>,
    #[serde(default)]
    pub type_patterns: Vec<String>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphArmHitDto {
    pub arm: String,
    pub rank: u32,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphSearchHitDto {
    pub node_key: String,
    pub type_id: String,
    pub name: Option<String>,
    /// Fused (RRF) score.
    pub score: f64,
    /// Which arms matched, and at what rank in each.
    pub arms: Vec<GraphArmHitDto>,
    pub snippet: Option<String>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphSearchResponseDto {
    pub hits: Vec<GraphSearchHitDto>,
    pub revision: GraphRevisionDto,
}

// ---------------------------------------------------------------------------
// Traversal
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphTraverseRequest {
    pub seeds: Vec<String>,
    pub depth: u8,
    #[serde(default)]
    pub edge_type_patterns: Vec<String>,
    #[serde(default)]
    pub node_type_patterns: Vec<String>,
    pub max_nodes: Option<u32>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphNeighborhoodRequest {
    pub root: String,
    pub depth: u8,
    pub node_budget: Option<u32>,
    #[serde(default)]
    pub include_phantoms: bool,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphEdgeRefDto {
    pub edge_key: String,
    pub edge_type_id: String,
    pub src: String,
    pub dst: String,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphTraversalResponseDto {
    pub nodes: Vec<GraphNodeDto>,
    pub edges: Vec<GraphEdgeRefDto>,
    /// Present when a budget stopped the walk. Never silent.
    pub truncated: Option<String>,
    pub revision: GraphRevisionDto,
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

impl From<m::GraphRevision> for GraphRevisionDto {
    fn from(value: m::GraphRevision) -> Self {
        Self {
            source_epoch: value.source_epoch,
            revision: value.revision,
        }
    }
}

impl From<m::EffectiveTraits> for GraphEffectiveTraitsDto {
    fn from(value: m::EffectiveTraits) -> Self {
        Self {
            family: value.family,
            scope_managed: value.scope_managed,
            emit_events: value.emit_events,
            index: value.index,
            full_text_search: value.full_text_search,
            vector_search: value.vector_search,
            src_types: value.src_types,
            dst_types: value.dst_types,
        }
    }
}

impl From<m::TypeRecord> for GraphTypeDto {
    fn from(value: m::TypeRecord) -> Self {
        Self {
            type_id: value.type_id,
            type_uuid: value.type_uuid.to_string(),
            kind: value.kind.as_str().to_owned(),
            is_abstract: value.is_abstract,
            schema: value.schema,
            effective_traits: value.effective_traits.into(),
        }
    }
}

impl From<GraphTypeRegistrationDto> for m::TypeRegistration {
    fn from(value: GraphTypeRegistrationDto) -> Self {
        Self {
            type_id: value.type_id,
            schema: value.schema,
        }
    }
}

impl From<GraphNodeSpecDto> for m::NodeSpec {
    fn from(value: GraphNodeSpecDto) -> Self {
        Self {
            node_key: value.node_key,
            type_id: value.type_id,
            name: value.name,
            payload: value.payload,
            expected_version: value.expected_version,
        }
    }
}

impl From<GraphEdgeSpecDto> for m::EdgeSpec {
    fn from(value: GraphEdgeSpecDto) -> Self {
        Self {
            type_id: value.type_id,
            src_node_key: value.src_node_key,
            dst_node_key: value.dst_node_key,
            discriminator: value.discriminator,
            payload: value.payload,
        }
    }
}

impl From<GraphReplaceScopeDto> for m::ReplaceScope {
    fn from(value: GraphReplaceScopeDto) -> Self {
        Self {
            attribute: value.attribute,
            value: value.value,
            generation: value.generation,
        }
    }
}

impl From<m::IngestOutcome> for GraphIngestResultDto {
    fn from(value: m::IngestOutcome) -> Self {
        Self {
            revision: value.revision.into(),
            replayed: value.replayed,
            counts: GraphIngestCountsDto {
                nodes_inserted: value.counts.nodes_inserted,
                nodes_updated: value.counts.nodes_updated,
                nodes_unchanged: value.counts.nodes_unchanged,
                edges_inserted: value.counts.edges_inserted,
                edges_updated: value.counts.edges_updated,
                edges_unchanged: value.counts.edges_unchanged,
                phantoms_created: value.counts.phantoms_created,
                phantoms_materialized: value.counts.phantoms_materialized,
            },
        }
    }
}

impl From<m::DeleteOutcome> for GraphDeleteResultDto {
    fn from(value: m::DeleteOutcome) -> Self {
        Self {
            revision: value.revision.into(),
            tombstoned_nodes: value.tombstoned_nodes,
            tombstoned_edges: value.tombstoned_edges,
        }
    }
}

impl From<m::AdjacencyEntry> for GraphAdjacencyEntryDto {
    fn from(value: m::AdjacencyEntry) -> Self {
        Self {
            edge_key: value.edge_key,
            edge_type_id: value.edge_type_id,
            direction: match value.side {
                m::AdjacencySide::Outgoing => "outgoing".to_owned(),
                m::AdjacencySide::Incoming => "incoming".to_owned(),
            },
            neighbor_key: value.neighbor_key,
            neighbor_type_id: value.neighbor_type_id,
        }
    }
}

impl From<m::NodeView> for GraphNodeDto {
    fn from(value: m::NodeView) -> Self {
        Self {
            node_key: value.node_key,
            type_id: value.type_id,
            name: value.name,
            payload: value.payload,
            has_embedding: value.has_embedding,
            adjacency: value.adjacency.into_iter().map(Into::into).collect(),
            adjacency_truncated: value.adjacency_truncated,
        }
    }
}

impl From<m::NodeRow> for GraphNodeRowDto {
    fn from(value: m::NodeRow) -> Self {
        Self {
            node_key: value.node_key,
            type_id: value.type_id,
            name: value.name,
            payload: value.payload,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

impl From<m::SearchHit> for GraphSearchHitDto {
    fn from(value: m::SearchHit) -> Self {
        Self {
            node_key: value.node_key,
            type_id: value.type_id,
            name: value.name,
            score: value.score,
            arms: value
                .arms
                .into_iter()
                .map(|arm| GraphArmHitDto {
                    arm: match arm.arm {
                        m::SearchArm::Lexical => "lexical".to_owned(),
                        m::SearchArm::Vector => "vector".to_owned(),
                    },
                    rank: arm.rank,
                })
                .collect(),
            snippet: value.snippet,
        }
    }
}

impl From<m::SearchResponse> for GraphSearchResponseDto {
    fn from(value: m::SearchResponse) -> Self {
        Self {
            hits: value.hits.into_iter().map(Into::into).collect(),
            revision: value.revision.into(),
        }
    }
}

impl From<m::EdgeRef> for GraphEdgeRefDto {
    fn from(value: m::EdgeRef) -> Self {
        Self {
            edge_key: value.edge_key,
            edge_type_id: value.edge_type_id,
            src: value.src,
            dst: value.dst,
        }
    }
}

impl From<m::TraversalResponse> for GraphTraversalResponseDto {
    fn from(value: m::TraversalResponse) -> Self {
        Self {
            nodes: value.nodes.into_iter().map(Into::into).collect(),
            edges: value.edges.into_iter().map(Into::into).collect(),
            truncated: value.truncated.map(|reason| {
                match reason {
                    m::TruncationReason::FrontierCap => "frontier_cap",
                    m::TruncationReason::EdgeScanCap => "edge_scan_cap",
                    m::TruncationReason::NodeBudget => "node_budget",
                }
                .to_owned()
            }),
            revision: value.revision.into(),
        }
    }
}
