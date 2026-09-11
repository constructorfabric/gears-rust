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

/// What a batch may do to an identifier that is already registered with a
/// different schema.
#[derive(Debug, Default)]
#[toolkit_macros::api_dto(request)]
pub struct GraphTypeRegisterOptionsDto {
    /// `reject` (the default) makes a changed schema a conflict, as it always
    /// was; `update` admits the change when it is admissible and refuses it
    /// with the offending schema locations when it is not.
    pub on_existing: Option<String>,
    /// Admit a change the schemas cannot prove compatible when every stored
    /// row of the type still validates against the candidate. A statement
    /// about *these rows*, not about the type, and it costs a scan bounded by
    /// `type_update_max_rows`.
    pub revalidate: Option<bool>,
}

/// One step of a payload migration.
///
/// A closed set spelled as `op` plus that step's fields, rather than a JSON
/// one-of: it is one `OpenAPI` object, and a misspelled `op` is refused by
/// name instead of silently matching nothing.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphMigrationStepDto {
    /// `rename` | `default` | `drop`.
    pub op: String,
    /// `rename`: the payload path to move the value from.
    pub from: Option<String>,
    /// `rename`: the payload path to move it to.
    pub to: Option<String>,
    /// `default` and `drop`: the payload path they act on.
    pub path: Option<String>,
    /// `default`: the value to set where nothing is there.
    pub value: Option<serde_json::Value>,
}

/// What to do with one type's stored payloads so they satisfy the candidate.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphTypeMigrationDto {
    pub type_id: String,
    pub steps: Vec<GraphMigrationStepDto>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphRegisterTypesRequest {
    pub types: Vec<GraphTypeRegistrationDto>,
    pub options: Option<GraphTypeRegisterOptionsDto>,
    /// Payload migrations, at most one per type in `types`. A migration needs
    /// `options.on_existing: "update"`, and it needs a schema change to
    /// migrate towards.
    pub migrations: Option<Vec<GraphTypeMigrationDto>>,
}

/// One reason a directional verdict does not hold, with the schema location
/// that carries it.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphSchemaDiagnosticDto {
    /// Location in the resolved schema; `$` is the document root.
    pub location: String,
    /// Machine-readable finding kind, as `gts` names it (`property_added`,
    /// `required_changed`, `enum_changed`, `not_provable`, ...).
    pub finding: String,
    pub message: String,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphTraitChangeDto {
    pub trait_name: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// The verdict on one candidate definition.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphTypeChangeDto {
    pub type_id: String,
    /// `new` | `unchanged` | `compatible` | `incompatible` | `undecidable`.
    pub state: String,
    /// `Valid(old) ⊆ Valid(new)` — the direction that gates admission.
    pub backward: String,
    /// Reported, never enforced: whether a reader pinned to the old
    /// definition still accepts payloads written against the new one.
    pub forward: String,
    pub diagnostics: Vec<GraphSchemaDiagnosticDto>,
    pub traits_changed: Vec<GraphTraitChangeDto>,
    /// Live rows of the type, when the operation needed to know.
    pub rows: Option<i64>,
    /// Rows a migration changed, or would change in a dry run.
    pub rows_rewritten: Option<i64>,
    /// Object levels where a *later* definition will not be able to add an
    /// optional property, so "your next edit is a major" is a warning now.
    pub levels_not_evolvable_in_place: Vec<String>,
    pub migration_required: bool,
    /// Whether the gear would admit this change under the request's options.
    pub admissible: bool,
}

/// A registered type and what the call did to it.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphRegisteredTypeDto {
    pub type_id: String,
    pub type_uuid: String,
    pub kind: String,
    pub is_abstract: bool,
    pub schema: serde_json::Value,
    pub effective_traits: GraphEffectiveTraitsDto,
    /// Which retained definition is in force (ADR-0005).
    pub revision: i32,
    /// `created` | `unchanged` | `updated`.
    pub outcome: String,
    /// `schema_proved` when the schemas prove inclusion, `data_backed` when
    /// the type's own rows were validated instead. Absent unless something
    /// was updated.
    pub admission_basis: Option<String>,
    /// Rows read for a `data_backed` or `migrated` admission.
    pub rows_validated: Option<i64>,
    /// Rows a `migrated` admission actually changed.
    pub rows_rewritten: Option<i64>,
    /// The verdict, present whenever the identifier was already registered,
    /// and always in a dry run.
    pub change: Option<GraphTypeChangeDto>,
}

/// What registering this batch would do. Nothing is written.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphTypeCompatibilityDto {
    pub items: Vec<GraphRegisteredTypeDto>,
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
    /// Which retained definition of this identifier is in force: `1` until it
    /// is first updated in place (types-registry ADR-0005).
    pub revision: i32,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphTypeListDto {
    pub items: Vec<GraphTypeDto>,
    pub next_cursor: Option<String>,
    pub revision: GraphRevisionDto,
}

/// One capability's readiness, as DESIGN's matrix reports it.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphComponentReadinessDto {
    /// The matrix's own name for the component.
    pub component: String,
    /// `healthy` | `degraded` | `unhealthy` | `not_implemented`.
    pub state: String,
    /// What is wrong, named. Absent when healthy.
    pub problem: Option<String>,
    /// What this state rejects.
    pub blocked: Option<String>,
    /// The condition being waited on.
    pub recovery: Option<String>,
}

/// Readiness per capability, never one global boolean.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphReadinessDto {
    /// Ready when no component whose failure blocks everything is unhealthy.
    /// An embedding-space mismatch is unhealthy and leaves the gear ready:
    /// it blocks the vector arms and nothing else.
    pub ready: bool,
    pub components: Vec<GraphComponentReadinessDto>,
}

/// One source namespace and the producer principal bound to it.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphSourceNamespaceDto {
    /// The `source.system` value of a reference node's identity triple.
    pub namespace: String,
    pub owner_principal: String,
    #[serde(with = "time::serde::rfc3339")]
    pub claimed_at: time::OffsetDateTime,
    /// Who held it before the last transfer, if it was ever transferred.
    pub previous_owner: Option<String>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub transferred_at: Option<time::OffsetDateTime>,
    pub transferred_by: Option<GraphSubjectDto>,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphSourceNamespaceListDto {
    pub items: Vec<GraphSourceNamespaceDto>,
}

/// Who a namespace is being transferred to.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct GraphTransferNamespaceRequest {
    /// The producer principal that may write the namespace from now on.
    pub owner_principal: String,
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
// Element envelope
// ---------------------------------------------------------------------------

/// The acting party behind a write, in the platform's subject vocabulary
/// rather than as a user id -- most writes into this gear come from an
/// automation or a service integration.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphSubjectDto {
    pub subject_id: String,
    /// GTS type of the subject, e.g. `gts.cf.core.security.subject_user.v1~`.
    /// Absent when the security context carries none.
    pub subject_type: Option<String>,
}

/// The gear-assigned half of an element (`fr-audit-envelope`): identical for
/// every node and every edge, described here rather than by the element's GTS
/// type, and read-only on every write surface.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphElementEnvelopeDto {
    pub tenant_id: String,
    /// A node's producer-supplied `node_key`, an edge's derived `edge_key`.
    pub key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    pub created_by: GraphSubjectDto,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
    pub updated_by: GraphSubjectDto,
    /// Soft-delete tombstone; absent on a live element.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub deleted_at: Option<time::OffsetDateTime>,
    pub deleted_by: Option<GraphSubjectDto>,
    /// The revision the read that produced this element observed.
    pub graph_revision: GraphRevisionDto,
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
    /// Static content a scope replacement removed because the batch no longer
    /// named it. Zero unless `replace_scope` was set.
    pub scope_removed_nodes: u64,
    pub scope_removed_edges: u64,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphIngestResultDto {
    pub revision: GraphRevisionDto,
    /// True when an idempotency receipt answered without touching state.
    pub replayed: bool,
    pub counts: GraphIngestCountsDto,
    /// One of `inserted | updated | unchanged | materialized` per node of the
    /// batch, in batch order. Present only when `options.report_per_item`
    /// was set, and absent on a replayed call: the receipt keeps the counts,
    /// not the list.
    pub per_item_nodes: Option<Vec<String>>,
    /// The same for the batch's edges (`inserted | updated | unchanged`).
    pub per_item_edges: Option<Vec<String>>,
}

fn item_outcome_name(outcome: &m::ItemOutcome) -> String {
    match outcome {
        m::ItemOutcome::Inserted => "inserted",
        m::ItemOutcome::Updated => "updated",
        m::ItemOutcome::Unchanged => "unchanged",
        m::ItemOutcome::Materialized => "materialized",
    }
    .to_owned()
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
    pub envelope: GraphElementEnvelopeDto,
}

/// One edge as an element: what the topology references carry, plus the
/// payload and the envelope (`fr-audit-envelope`).
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GraphEdgeDto {
    pub edge_key: String,
    pub edge_type_id: String,
    pub src: String,
    pub dst: String,
    pub discriminator: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub envelope: GraphElementEnvelopeDto,
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
    /// On this path the envelope is also the only carrier of the observed
    /// revision: the page wrapper is the platform's and has no member for one.
    pub envelope: GraphElementEnvelopeDto,
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
    /// The seeds the walk started from: requested, deduped, and filtered to
    /// what the caller may see. Unknown and unauthorized seeds are absent
    /// alike.
    pub seeds: Vec<String>,
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
            revision: value.revision,
        }
    }
}

impl From<m::ComponentReadiness> for GraphComponentReadinessDto {
    fn from(value: m::ComponentReadiness) -> Self {
        Self {
            component: value.component,
            state: value.state.as_str().to_owned(),
            problem: value.problem,
            blocked: value.blocked,
            recovery: value.recovery,
        }
    }
}

impl From<m::Readiness> for GraphReadinessDto {
    fn from(value: m::Readiness) -> Self {
        Self {
            ready: value.ready,
            components: value.components.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<m::SourceNamespaceOwner> for GraphSourceNamespaceDto {
    fn from(value: m::SourceNamespaceOwner) -> Self {
        Self {
            namespace: value.namespace,
            owner_principal: value.owner_principal,
            claimed_at: value.claimed_at,
            previous_owner: value.previous_owner,
            transferred_at: value.transferred_at,
            transferred_by: value.transferred_by.map(Into::into),
        }
    }
}

impl From<m::SchemaDiagnostic> for GraphSchemaDiagnosticDto {
    fn from(value: m::SchemaDiagnostic) -> Self {
        Self {
            location: value.location,
            finding: value.finding,
            message: value.message,
        }
    }
}

impl From<m::TraitChange> for GraphTraitChangeDto {
    fn from(value: m::TraitChange) -> Self {
        Self {
            trait_name: value.trait_name,
            added: value.added,
            removed: value.removed,
        }
    }
}

impl From<m::TypeChange> for GraphTypeChangeDto {
    fn from(value: m::TypeChange) -> Self {
        Self {
            type_id: value.type_id,
            state: value.state.as_str().to_owned(),
            backward: value.backward,
            forward: value.forward,
            diagnostics: value.diagnostics.into_iter().map(Into::into).collect(),
            traits_changed: value.traits_changed.into_iter().map(Into::into).collect(),
            // A count crosses the boundary as a signed integer: `OpenAPI`
            // integers are signed, and a row count cannot approach the bound.
            rows: value.rows.and_then(|rows| i64::try_from(rows).ok()),
            rows_rewritten: value
                .rows_rewritten
                .and_then(|rows| i64::try_from(rows).ok()),
            levels_not_evolvable_in_place: value.levels_not_evolvable_in_place,
            migration_required: value.migration_required,
            admissible: value.admissible,
        }
    }
}

impl From<m::RegisteredType> for GraphRegisteredTypeDto {
    fn from(value: m::RegisteredType) -> Self {
        let (admission_basis, rows_validated, rows_rewritten) = match value.basis {
            None => (None, None, None),
            Some(m::AdmissionBasis::SchemaProved) => (Some("schema_proved".to_owned()), None, None),
            Some(m::AdmissionBasis::DataBacked { rows_validated }) => (
                Some("data_backed".to_owned()),
                i64::try_from(rows_validated).ok(),
                None,
            ),
            Some(m::AdmissionBasis::Migrated {
                rows_scanned,
                rows_rewritten,
            }) => (
                Some("migrated".to_owned()),
                i64::try_from(rows_scanned).ok(),
                i64::try_from(rows_rewritten).ok(),
            ),
        };
        let record = value.record;
        Self {
            type_id: record.type_id,
            type_uuid: record.type_uuid.to_string(),
            kind: record.kind.as_str().to_owned(),
            is_abstract: record.is_abstract,
            schema: record.schema,
            effective_traits: record.effective_traits.into(),
            revision: record.revision,
            outcome: value.outcome.as_str().to_owned(),
            admission_basis,
            rows_validated,
            rows_rewritten,
            change: value.change.map(Into::into),
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
                scope_removed_nodes: value.counts.scope_removed_nodes,
                scope_removed_edges: value.counts.scope_removed_edges,
            },
            per_item_nodes: value
                .per_item_nodes
                .map(|items| items.iter().map(item_outcome_name).collect()),
            per_item_edges: value
                .per_item_edges
                .map(|items| items.iter().map(item_outcome_name).collect()),
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

impl From<m::Subject> for GraphSubjectDto {
    fn from(value: m::Subject) -> Self {
        Self {
            subject_id: value.subject_id.to_string(),
            subject_type: value.subject_type,
        }
    }
}

impl From<m::ElementEnvelope> for GraphElementEnvelopeDto {
    fn from(value: m::ElementEnvelope) -> Self {
        Self {
            tenant_id: value.tenant_id.to_string(),
            key: value.key,
            created_at: value.created_at,
            created_by: value.created_by.into(),
            updated_at: value.updated_at,
            updated_by: value.updated_by.into(),
            deleted_at: value.deleted_at,
            deleted_by: value.deleted_by.map(Into::into),
            graph_revision: value.graph_revision.into(),
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
            envelope: value.envelope.into(),
        }
    }
}

impl From<m::EdgeView> for GraphEdgeDto {
    fn from(value: m::EdgeView) -> Self {
        Self {
            edge_key: value.edge_key,
            edge_type_id: value.edge_type_id,
            src: value.src,
            dst: value.dst,
            discriminator: value.discriminator,
            payload: value.payload,
            envelope: value.envelope.into(),
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
            envelope: value.envelope.into(),
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
            seeds: value.seeds,
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
