//! An in-memory `GraphStoreV1`, the conformance suite's second
//! implementation.
//!
//! It exists so the trait's obligations are asserted against something other
//! than the code that motivated them: a change to the contract that only the
//! `PostgreSQL` store can satisfy fails here. It is deliberately simple —
//! correctness of the *obligations*, not of a database.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use graph_storage_sdk::models::{
    AdjacencyEntry, AdjacencySide, DeleteOutcome, DeleteRequest, GraphRevision, GtsTypeId,
    IngestCounts, IngestOutcome, IngestRequest, ItemError, ItemFamily, LabelAssignment, LabelId,
    LabelRecord, LabelSpec, NodeId, NodeKey, NodeRow, NodeView, Page, ProjectionRequest,
    ReadSnapshot, RevisionOutcome, SearchRequest, SearchResponse, StoreCapabilities, TopologyPage,
    TopologyRequest, TypeIdSet, TypeQuery, TypeRecord, TypeRegistration,
};
use graph_storage_sdk::plugin_api::{GraphStoreError, GraphStoreV1, StoreCtx};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::{identity, ontology};

#[derive(Clone)]
struct FakeNode {
    id: i64,
    key: String,
    type_id: String,
    name: Option<String>,
    payload: Option<serde_json::Value>,
    has_embedding: bool,
    version: i64,
    deleted: bool,
}

#[derive(Clone)]
struct FakeEdge {
    key: String,
    type_id: String,
    src: i64,
    dst: i64,
    payload: Option<serde_json::Value>,
    deleted: bool,
}

#[derive(Clone)]
struct Receipt {
    request_hash: String,
    epoch: i64,
    outcome: IngestOutcome,
}

#[derive(Default)]
struct Tenant {
    types: BTreeMap<String, TypeRecord>,
    nodes: Vec<FakeNode>,
    edges: Vec<FakeEdge>,
    revision: i64,
    next_id: i64,
    receipts: BTreeMap<String, Receipt>,
    scopes: BTreeMap<(String, String), (i64, String)>,
    /// Snapshots taken by `begin_read`: a full copy, which is what makes this
    /// implementation able to honour the one-snapshot obligation the
    /// `PostgreSQL` store currently cannot.
    snapshots: BTreeMap<Uuid, Box<TenantData>>,
}

#[derive(Clone, Default)]
struct TenantData {
    nodes: Vec<FakeNode>,
    edges: Vec<FakeEdge>,
    revision: i64,
}

#[derive(Default)]
pub struct FakeGraphStore {
    epoch: i64,
    tenants: Mutex<BTreeMap<Uuid, Tenant>>,
}

impl FakeGraphStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: 1,
            tenants: Mutex::new(BTreeMap::new()),
        }
    }

    fn revision_of(&self, tenant: &Tenant) -> GraphRevision {
        GraphRevision {
            source_epoch: self.epoch,
            revision: tenant.revision,
        }
    }
}

fn snapshot_data(tenant: &Tenant) -> TenantData {
    TenantData {
        nodes: tenant.nodes.clone(),
        edges: tenant.edges.clone(),
        revision: tenant.revision,
    }
}

/// Whether the compiled scope admits this call's tenant at all.
///
/// The fake models only the coarse decision — deny-all, allow-all, and the
/// tenant arm — because that is what the obligations turn on. Anything finer
/// belongs to a real store's statement, and a fake that pretended otherwise
/// would let a scoping bug pass here and fail in production.
fn scope_admits(scope: &toolkit_security::AccessScope, tenant: Uuid) -> bool {
    if scope.is_deny_all() {
        return false;
    }
    if scope.is_unconstrained() {
        return true;
    }
    let tenants = scope.all_uuid_values_for(toolkit_security::pep_properties::OWNER_TENANT_ID);
    tenants.is_empty() || tenants.contains(&tenant)
}

/// Read the rows a call should see: the snapshot's copy when one is carried,
/// the live rows otherwise.
fn visible<'a>(tenant: &'a Tenant, ctx: &StoreCtx<'_>) -> (&'a [FakeNode], &'a [FakeEdge], i64) {
    if let Some(snapshot) = ctx.snapshot
        && let Some(data) = tenant.snapshots.get(&snapshot.id)
    {
        return (&data.nodes, &data.edges, data.revision);
    }
    (&tenant.nodes, &tenant.edges, tenant.revision)
}

#[async_trait]
impl GraphStoreV1 for FakeGraphStore {
    fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            scope_replace: true,
            snapshots: true,
            vector_search: false,
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
        let mut tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let tenant = tenants.entry(ctx.tenant).or_default();

        // Atomic: analyze everything before storing anything.
        let mut prepared = Vec::new();
        for registration in &batch {
            let chain = ontology::ancestors(&registration.type_id);
            let mut ancestors = Vec::new();
            for ancestor in &chain[..chain.len().saturating_sub(1)] {
                let schema = batch
                    .iter()
                    .find(|r| &r.type_id == ancestor)
                    .map(|r| r.schema.clone())
                    .or_else(|| tenant.types.get(ancestor).map(|t| t.schema.clone()))
                    .ok_or_else(|| GraphStoreError::Validation {
                        items: vec![ItemError {
                            index: 0,
                            family: ItemFamily::Node,
                            gts_type: Some(registration.type_id.clone()),
                            pointer: None,
                            message: format!("ancestor `{ancestor}` is not registered"),
                        }],
                    })?;
                ancestors.push(schema);
            }
            let refs: Vec<&serde_json::Value> = ancestors.iter().collect();
            let descriptor = ontology::analyze(&registration.type_id, &registration.schema, &refs)
                .map_err(|error| GraphStoreError::Validation {
                    items: vec![ItemError {
                        index: 0,
                        family: ItemFamily::Node,
                        gts_type: Some(registration.type_id.clone()),
                        pointer: None,
                        message: error.to_string(),
                    }],
                })?;

            if let Some(existing) = tenant.types.get(&descriptor.type_id)
                && existing.schema != descriptor.schema
            {
                return Err(GraphStoreError::Conflict {
                    reason: format!(
                        "type `{}` is already registered with a different schema",
                        descriptor.type_id
                    ),
                });
            }
            prepared.push(TypeRecord {
                type_id: descriptor.type_id,
                type_uuid: descriptor.type_uuid,
                kind: descriptor.kind,
                is_abstract: descriptor.is_abstract,
                schema: descriptor.schema,
                effective_traits: descriptor.effective_traits,
                created_at: OffsetDateTime::now_utc(),
            });
        }

        for record in &prepared {
            tenant
                .types
                .entry(record.type_id.clone())
                .or_insert_with(|| record.clone());
        }
        Ok(prepared)
    }

    async fn get_type(
        &self,
        ctx: &StoreCtx<'_>,
        id: &GtsTypeId,
    ) -> Result<TypeRecord, GraphStoreError> {
        if !scope_admits(ctx.scope, ctx.tenant) {
            return Err(GraphStoreError::NotFound);
        }
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        tenants
            .get(&ctx.tenant)
            .and_then(|t| t.types.get(id).cloned())
            .ok_or(GraphStoreError::NotFound)
    }

    async fn list_types(
        &self,
        ctx: &StoreCtx<'_>,
        query: TypeQuery,
    ) -> Result<Page<TypeRecord>, GraphStoreError> {
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let Some(tenant) = tenants.get(&ctx.tenant) else {
            return Ok(Page {
                items: Vec::new(),
                next_cursor: None,
                revision: GraphRevision {
                    source_epoch: self.epoch,
                    revision: 0,
                },
            });
        };
        let items = tenant
            .types
            .values()
            .filter(|record| query.kind.is_none_or(|kind| kind == record.kind))
            .filter(|record| {
                query.pattern.as_ref().is_none_or(|pattern| {
                    ontology::matches_any_pattern(&record.type_id, std::slice::from_ref(pattern))
                        .unwrap_or(false)
                })
            })
            .cloned()
            .collect();
        Ok(Page {
            items,
            next_cursor: None,
            revision: self.revision_of(tenant),
        })
    }

    async fn resolve_type_set(
        &self,
        ctx: &StoreCtx<'_>,
        patterns: &[String],
    ) -> Result<TypeIdSet, GraphStoreError> {
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let Some(tenant) = tenants.get(&ctx.tenant) else {
            return Ok(TypeIdSet::default());
        };
        Ok(TypeIdSet(
            tenant
                .types
                .keys()
                .filter(|id| ontology::matches_any_pattern(id, patterns).unwrap_or(false))
                .cloned()
                .collect(),
        ))
    }

    async fn ingest(
        &self,
        ctx: &StoreCtx<'_>,
        req: IngestRequest,
    ) -> Result<IngestOutcome, GraphStoreError> {
        let mut tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let tenant = tenants.entry(ctx.tenant).or_default();
        let request_hash = identity::ingest_request_hash(&req);

        if let Some(key) = &req.idempotency_key
            && let Some(receipt) = tenant.receipts.get(key)
        {
            if receipt.request_hash != request_hash {
                return Err(GraphStoreError::IdempotencyMismatch);
            }
            if receipt.epoch != self.epoch {
                return Err(GraphStoreError::IdempotencyExpired);
            }
            let mut outcome = receipt.outcome.clone();
            outcome.replayed = true;
            return Ok(outcome);
        }

        if let Some(replace) = &req.replace_scope {
            fence(tenant, replace, &request_hash)?;
        }

        // Working copies: written back only once the whole batch succeeded, so
        // a partway failure leaves nothing.
        let mut nodes = tenant.nodes.clone();
        let mut edges = tenant.edges.clone();
        let mut next_id = tenant.next_id;
        let mut counts = IngestCounts::default();
        let mut changed = false;

        for (index, spec) in req.nodes.iter().enumerate() {
            changed |= apply_node(
                tenant,
                &mut nodes,
                &edges,
                &mut next_id,
                &mut counts,
                index,
                spec,
            )?;
        }
        for (index, spec) in req.edges.iter().enumerate() {
            changed |= apply_edge(
                tenant,
                &mut nodes,
                &mut edges,
                &mut next_id,
                &mut counts,
                index,
                spec,
                req.options.create_phantoms.unwrap_or(true),
            )?;
        }

        tenant.nodes = nodes;
        tenant.edges = edges;
        tenant.next_id = next_id;
        if changed {
            tenant.revision += 1;
        }
        if let Some(replace) = &req.replace_scope {
            tenant.scopes.insert(
                (replace.attribute.clone(), replace.value.clone()),
                (replace.generation, request_hash.clone()),
            );
        }

        let outcome = IngestOutcome {
            revision: self.revision_of(tenant),
            replayed: false,
            counts,
            per_item_nodes: None,
            per_item_edges: None,
        };
        if let Some(key) = &req.idempotency_key {
            tenant.receipts.insert(
                key.clone(),
                Receipt {
                    request_hash,
                    epoch: self.epoch,
                    outcome: outcome.clone(),
                },
            );
        }
        Ok(outcome)
    }

    async fn soft_delete(
        &self,
        ctx: &StoreCtx<'_>,
        req: DeleteRequest,
    ) -> Result<DeleteOutcome, GraphStoreError> {
        let mut tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let tenant = tenants.entry(ctx.tenant).or_default();

        let (nodes, edges) = match req {
            DeleteRequest::Node(key) => {
                let Some(index) = tenant.nodes.iter().position(|n| n.key == key && !n.deleted)
                else {
                    return Err(GraphStoreError::NotFound);
                };
                let id = tenant.nodes[index].id;
                let mut tombstoned = 0u64;
                for edge in &mut tenant.edges {
                    if !edge.deleted && (edge.src == id || edge.dst == id) {
                        edge.deleted = true;
                        tombstoned += 1;
                    }
                }
                tenant.nodes[index].deleted = true;
                (1u64, tombstoned)
            }
            DeleteRequest::Edge(key) => {
                let Some(edge) = tenant.edges.iter_mut().find(|e| e.key == key && !e.deleted)
                else {
                    return Err(GraphStoreError::NotFound);
                };
                edge.deleted = true;
                (0u64, 1u64)
            }
        };

        tenant.revision += 1;
        Ok(DeleteOutcome {
            revision: self.revision_of(tenant),
            tombstoned_nodes: nodes,
            tombstoned_edges: edges,
        })
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
        _req: LabelAssignment,
    ) -> Result<RevisionOutcome, GraphStoreError> {
        Err(GraphStoreError::Unsupported { what: "labels" })
    }

    async fn begin_read(&self, ctx: &StoreCtx<'_>) -> Result<ReadSnapshot, GraphStoreError> {
        let mut tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let tenant = tenants.entry(ctx.tenant).or_default();
        let id = Uuid::now_v7();
        let data = snapshot_data(tenant);
        let revision = self.revision_of(tenant);
        tenant.snapshots.insert(id, Box::new(data));
        Ok(ReadSnapshot { id, revision })
    }

    async fn end_read(&self, snapshot: ReadSnapshot) -> Result<(), GraphStoreError> {
        let mut tenants = self.tenants.lock().map_err(|_| poisoned())?;
        for tenant in tenants.values_mut() {
            tenant.snapshots.remove(&snapshot.id);
        }
        Ok(())
    }

    async fn revision(&self, ctx: &StoreCtx<'_>) -> Result<GraphRevision, GraphStoreError> {
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        Ok(tenants.get(&ctx.tenant).map_or(
            GraphRevision {
                source_epoch: self.epoch,
                revision: 0,
            },
            |t| self.revision_of(t),
        ))
    }

    async fn get_node(
        &self,
        ctx: &StoreCtx<'_>,
        key: &NodeKey,
        adjacency_limit: u32,
    ) -> Result<NodeView, GraphStoreError> {
        if !scope_admits(ctx.scope, ctx.tenant) {
            return Err(GraphStoreError::NotFound);
        }
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let tenant = tenants.get(&ctx.tenant).ok_or(GraphStoreError::NotFound)?;
        let (nodes, edges, _) = visible(tenant, ctx);
        let node = nodes
            .iter()
            .find(|n| &n.key == key && !n.deleted)
            .ok_or(GraphStoreError::NotFound)?;

        let by_id: BTreeMap<i64, &FakeNode> = nodes
            .iter()
            .filter(|n| !n.deleted)
            .map(|n| (n.id, n))
            .collect();
        let mut adjacency = Vec::new();
        let mut truncated = false;
        for edge in edges.iter().filter(|e| !e.deleted) {
            let (side, other) = if edge.src == node.id {
                (AdjacencySide::Outgoing, edge.dst)
            } else if edge.dst == node.id {
                (AdjacencySide::Incoming, edge.src)
            } else {
                continue;
            };
            let Some(neighbour) = by_id.get(&other) else {
                continue;
            };
            if u32::try_from(adjacency.len()).unwrap_or(u32::MAX) >= adjacency_limit {
                truncated = true;
                break;
            }
            adjacency.push(AdjacencyEntry {
                edge_key: edge.key.clone(),
                edge_type_id: edge.type_id.clone(),
                side,
                neighbor_key: neighbour.key.clone(),
                neighbor_type_id: neighbour.type_id.clone(),
            });
        }
        Ok(view_of(node, adjacency, truncated))
    }

    async fn hydrate_nodes(
        &self,
        ctx: &StoreCtx<'_>,
        ids: &[NodeId],
    ) -> Result<Vec<NodeView>, GraphStoreError> {
        if !scope_admits(ctx.scope, ctx.tenant) {
            return Ok(Vec::new());
        }
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let Some(tenant) = tenants.get(&ctx.tenant) else {
            return Ok(Vec::new());
        };
        let (nodes, _, _) = visible(tenant, ctx);
        Ok(ids
            .iter()
            .filter_map(|id| nodes.iter().find(|n| n.id == *id && !n.deleted))
            .map(|n| view_of(n, Vec::new(), false))
            .collect())
    }

    async fn search(
        &self,
        ctx: &StoreCtx<'_>,
        req: SearchRequest,
    ) -> Result<SearchResponse, GraphStoreError> {
        if !scope_admits(ctx.scope, ctx.tenant) {
            return Ok(SearchResponse {
                hits: Vec::new(),
                revision: GraphRevision {
                    source_epoch: self.epoch,
                    revision: 0,
                },
            });
        }
        // Substring matching, not a text-search engine: enough to assert that
        // scoping and revision stamping hold on every arm.
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let Some(tenant) = tenants.get(&ctx.tenant) else {
            return Err(GraphStoreError::NotFound);
        };
        let (nodes, _, revision) = visible(tenant, ctx);
        let needle = req.query.unwrap_or_default().to_lowercase();
        let hits = nodes
            .iter()
            .filter(|n| !n.deleted)
            .filter(|n| {
                needle.is_empty()
                    || n.name
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&needle)
            })
            .take(req.limit as usize)
            .enumerate()
            .map(|(position, n)| graph_storage_sdk::models::SearchHit {
                node_key: n.key.clone(),
                type_id: n.type_id.clone(),
                name: n.name.clone(),
                score: 1.0 / (60.0 + f64::from(u32::try_from(position).unwrap_or(u32::MAX)) + 1.0),
                arms: vec![graph_storage_sdk::models::ArmHit {
                    arm: graph_storage_sdk::models::SearchArm::Lexical,
                    rank: u32::try_from(position)
                        .unwrap_or(u32::MAX)
                        .saturating_add(1),
                    score: 1.0,
                }],
                snippet: None,
            })
            .collect();
        Ok(SearchResponse {
            hits,
            revision: GraphRevision {
                source_epoch: self.epoch,
                revision,
            },
        })
    }

    async fn project_table(
        &self,
        ctx: &StoreCtx<'_>,
        req: ProjectionRequest,
    ) -> Result<toolkit_odata::Page<NodeRow>, GraphStoreError> {
        if !scope_admits(ctx.scope, ctx.tenant) {
            return Ok(toolkit_odata::Page {
                items: Vec::new(),
                page_info: empty_page_info(),
            });
        }
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let Some(tenant) = tenants.get(&ctx.tenant) else {
            return Err(GraphStoreError::NotFound);
        };
        let (nodes, _, _) = visible(tenant, ctx);
        // Filter and ordering are the platform's on the real store; the fake
        // answers the unfiltered page, which is all the obligations need.
        let limit = req.query.limit.unwrap_or(200);
        let items = nodes
            .iter()
            .filter(|n| !n.deleted)
            .filter(|n| {
                req.type_set
                    .as_ref()
                    .is_none_or(|set| set.contains(&n.type_id))
            })
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .map(|n| NodeRow {
                node_key: n.key.clone(),
                type_id: n.type_id.clone(),
                name: n.name.clone(),
                payload: n.payload.clone(),
                created_at: OffsetDateTime::UNIX_EPOCH,
                updated_at: OffsetDateTime::UNIX_EPOCH,
            })
            .collect();
        Ok(toolkit_odata::Page {
            items,
            page_info: toolkit_odata::page::PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
            },
        })
    }

    async fn load_topology(
        &self,
        _ctx: &StoreCtx<'_>,
        _req: TopologyRequest,
    ) -> Result<TopologyPage, GraphStoreError> {
        Err(GraphStoreError::Unsupported { what: "topology" })
    }

    async fn resolve_node_ids(
        &self,
        ctx: &StoreCtx<'_>,
        keys: &[NodeKey],
    ) -> Result<Vec<(NodeKey, NodeId)>, GraphStoreError> {
        if !scope_admits(ctx.scope, ctx.tenant) {
            return Ok(Vec::new());
        }
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let Some(tenant) = tenants.get(&ctx.tenant) else {
            return Ok(Vec::new());
        };
        let (nodes, _, _) = visible(tenant, ctx);
        Ok(keys
            .iter()
            .filter_map(|key| {
                nodes
                    .iter()
                    .find(|n| &n.key == key && !n.deleted)
                    .map(|n| (key.clone(), n.id))
            })
            .collect())
    }
}

/// The fake's own engine, so traversal conformance has a second
/// implementation too.
pub struct FakeGraphEngine {
    store: std::sync::Arc<FakeGraphStore>,
}

impl FakeGraphEngine {
    pub fn new(store: std::sync::Arc<FakeGraphStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl graph_storage_sdk::plugin_api::GraphEngineV1 for FakeGraphEngine {
    fn capabilities(&self) -> graph_storage_sdk::models::EngineCapabilities {
        graph_storage_sdk::models::EngineCapabilities::default()
    }

    async fn cursor(
        &self,
        ctx: &StoreCtx<'_>,
    ) -> Result<
        graph_storage_sdk::plugin_api::EngineCursor,
        graph_storage_sdk::plugin_api::GraphEngineError,
    > {
        let revision = GraphStoreV1::revision(self.store.as_ref(), ctx)
            .await
            .map_err(|error| {
                graph_storage_sdk::plugin_api::GraphEngineError::Internal(error.to_string())
            })?;
        Ok(graph_storage_sdk::plugin_api::EngineCursor { revision })
    }

    async fn expand(
        &self,
        ctx: &StoreCtx<'_>,
        req: graph_storage_sdk::plugin_api::ExpandRequest,
    ) -> Result<
        graph_storage_sdk::plugin_api::ExpandResponse,
        graph_storage_sdk::plugin_api::GraphEngineError,
    > {
        use graph_storage_sdk::models::Direction;

        let tenants = self.store.tenants.lock().map_err(|_| {
            graph_storage_sdk::plugin_api::GraphEngineError::Internal("poisoned".into())
        })?;
        let Some(tenant) = tenants.get(&ctx.tenant) else {
            return Ok(graph_storage_sdk::plugin_api::ExpandResponse {
                reached: Vec::new(),
                edges: Vec::new(),
                truncated: None,
            });
        };
        let (nodes, edges, _) = visible(tenant, ctx);
        let live: BTreeMap<i64, &FakeNode> = nodes
            .iter()
            .filter(|n| !n.deleted)
            .map(|n| (n.id, n))
            .collect();
        let frontier: std::collections::BTreeSet<i64> = req.frontier.iter().copied().collect();

        let mut reached = Vec::new();
        let mut out_edges = Vec::new();
        for edge in edges.iter().filter(|e| !e.deleted) {
            if let Some(set) = &req.edge_types
                && !set.contains(&edge.type_id)
            {
                continue;
            }
            let (Some(src), Some(dst)) = (live.get(&edge.src), live.get(&edge.dst)) else {
                continue;
            };
            let forward = frontier.contains(&edge.src)
                && matches!(req.direction, Direction::Outgoing | Direction::Either);
            let backward = frontier.contains(&edge.dst)
                && matches!(req.direction, Direction::Incoming | Direction::Either);
            if !forward && !backward {
                continue;
            }
            if forward {
                reached.push(edge.dst);
            }
            if backward {
                reached.push(edge.src);
            }
            out_edges.push(graph_storage_sdk::models::EdgeRef {
                edge_key: edge.key.clone(),
                edge_type_id: edge.type_id.clone(),
                src: src.key.clone(),
                dst: dst.key.clone(),
            });
        }
        reached.sort_unstable();
        reached.dedup();

        Ok(graph_storage_sdk::plugin_api::ExpandResponse {
            reached,
            edges: out_edges,
            truncated: None,
        })
    }

    async fn shortest_path(
        &self,
        _ctx: &StoreCtx<'_>,
        _req: graph_storage_sdk::plugin_api::ShortestPathRequest,
    ) -> Result<
        graph_storage_sdk::plugin_api::PathResponse,
        graph_storage_sdk::plugin_api::GraphEngineError,
    > {
        Err(
            graph_storage_sdk::plugin_api::GraphEngineError::Unsupported {
                what: "shortest_path",
            },
        )
    }

    async fn match_pattern(
        &self,
        _ctx: &StoreCtx<'_>,
        _req: graph_storage_sdk::plugin_api::PatternRequest,
    ) -> Result<
        graph_storage_sdk::plugin_api::PatternResponse,
        graph_storage_sdk::plugin_api::GraphEngineError,
    > {
        Err(
            graph_storage_sdk::plugin_api::GraphEngineError::Unsupported {
                what: "match_pattern",
            },
        )
    }
}

fn poisoned() -> GraphStoreError {
    GraphStoreError::Internal("fake store lock is poisoned".into())
}

fn validation(index: usize, family: ItemFamily, type_id: &str, message: &str) -> GraphStoreError {
    GraphStoreError::Validation {
        items: vec![ItemError {
            index,
            family,
            gts_type: Some(type_id.to_owned()),
            pointer: None,
            message: message.to_owned(),
        }],
    }
}

fn view_of(node: &FakeNode, adjacency: Vec<AdjacencyEntry>, truncated: bool) -> NodeView {
    NodeView {
        node_key: node.key.clone(),
        type_id: node.type_id.clone(),
        name: node.name.clone(),
        payload: node.payload.clone(),
        has_embedding: node.has_embedding,
        labels: Vec::new(),
        adjacency,
        adjacency_truncated: truncated,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

/// Generation fencing on a scope replacement, before anything is written.
fn fence(
    tenant: &Tenant,
    replace: &graph_storage_sdk::models::ReplaceScope,
    request_hash: &str,
) -> Result<(), GraphStoreError> {
    let key = (replace.attribute.clone(), replace.value.clone());
    let Some((generation, hash)) = tenant.scopes.get(&key) else {
        return Ok(());
    };
    if replace.generation < *generation {
        return Err(GraphStoreError::StaleGeneration {
            recorded: *generation,
            offered: replace.generation,
        });
    }
    if replace.generation == *generation && hash != request_hash {
        return Err(GraphStoreError::Conflict {
            reason: "same source generation with different content".into(),
        });
    }
    Ok(())
}

/// Apply one node spec to the working copy. Returns whether it changed state.
fn apply_node(
    tenant: &Tenant,
    nodes: &mut Vec<FakeNode>,
    edges: &[FakeEdge],
    next_id: &mut i64,
    counts: &mut IngestCounts,
    index: usize,
    spec: &graph_storage_sdk::models::NodeSpec,
) -> Result<bool, GraphStoreError> {
    let record = tenant.types.get(&spec.type_id).ok_or_else(|| {
        validation(
            index,
            ItemFamily::Node,
            &spec.type_id,
            "type is not registered",
        )
    })?;
    if record.is_abstract {
        return Err(validation(
            index,
            ItemFamily::Node,
            &spec.type_id,
            "abstract types cannot be instantiated",
        ));
    }

    let Some(existing) = nodes.iter_mut().find(|n| n.key == spec.node_key) else {
        *next_id += 1;
        nodes.push(FakeNode {
            id: *next_id,
            key: spec.node_key.clone(),
            type_id: spec.type_id.clone(),
            name: spec.name.clone(),
            payload: spec.payload.clone(),
            has_embedding: spec.embedding.is_some(),
            version: 1,
            deleted: false,
        });
        counts.nodes_inserted += 1;
        return Ok(true);
    };

    if existing.deleted {
        return Err(GraphStoreError::Conflict {
            reason: format!(
                "node key `{}` is tombstoned and cannot be re-ingested",
                spec.node_key
            ),
        });
    }
    if let Some(expected) = spec.expected_version
        && expected != existing.version
    {
        return Err(GraphStoreError::Conflict {
            reason: format!(
                "expected version {expected}, stored version is {}",
                existing.version
            ),
        });
    }

    let same_type = existing.type_id == spec.type_id;
    if !same_type {
        let was_phantom = tenant
            .types
            .get(&existing.type_id)
            .and_then(|t| t.effective_traits.family.clone())
            .as_deref()
            == Some("phantom");
        if !was_phantom {
            return Err(validation(
                index,
                ItemFamily::Node,
                &spec.type_id,
                "a same-key ingest may not change a node's type",
            ));
        }
    }

    let unchanged = same_type
        && existing.name == spec.name
        && existing.payload == spec.payload
        && existing.has_embedding == spec.embedding.is_some();
    if unchanged {
        counts.nodes_unchanged += 1;
        return Ok(false);
    }

    if !same_type {
        // Rule 3 of the Phantom Materialization Contract: the endpoint check
        // that could not run while this node had no concrete type runs now,
        // against every edge the phantom accumulated meanwhile.
        revalidate_incident_edges(tenant, edges, existing.id, &spec.type_id, index)?;
    }

    existing.type_id.clone_from(&spec.type_id);
    existing.name.clone_from(&spec.name);
    existing.payload.clone_from(&spec.payload);
    existing.has_embedding = spec.embedding.is_some();
    existing.version += 1;
    if same_type {
        counts.nodes_updated += 1;
    } else {
        counts.phantoms_materialized += 1;
    }
    Ok(true)
}

/// Every edge already incident to a node becoming concrete must still be
/// admissible under the concrete type.
fn revalidate_incident_edges(
    tenant: &Tenant,
    edges: &[FakeEdge],
    node_id: i64,
    concrete_type: &str,
    index: usize,
) -> Result<(), GraphStoreError> {
    for edge in edges.iter().filter(|e| !e.deleted) {
        let Some(record) = tenant.types.get(&edge.type_id) else {
            continue;
        };
        for (is_end, patterns, which) in [
            (
                edge.src == node_id,
                &record.effective_traits.src_types,
                "source",
            ),
            (
                edge.dst == node_id,
                &record.effective_traits.dst_types,
                "destination",
            ),
        ] {
            if !is_end || patterns.is_empty() {
                continue;
            }
            if !ontology::matches_any_pattern(concrete_type, patterns).unwrap_or(false) {
                return Err(GraphStoreError::Validation {
                    items: vec![ItemError {
                        index,
                        family: ItemFamily::Node,
                        gts_type: Some(concrete_type.to_owned()),
                        pointer: Some("/type".to_owned()),
                        message: format!(
                            "materializing this node as `{concrete_type}` would leave edge \
                             `{}` invalid: `{}` does not admit it as a {which} (accepts {})",
                            edge.key,
                            edge.type_id,
                            patterns.join(", ")
                        ),
                    }],
                });
            }
        }
    }
    Ok(())
}

/// Resolve one endpoint in the working copy, creating a phantom when allowed.
fn apply_endpoint(
    tenant: &Tenant,
    nodes: &mut Vec<FakeNode>,
    next_id: &mut i64,
    counts: &mut IngestCounts,
    index: usize,
    type_id: &str,
    key: &str,
    create_phantoms: bool,
) -> Result<i64, GraphStoreError> {
    if let Some(existing) = nodes.iter().find(|n| n.key == key && !n.deleted) {
        return Ok(existing.id);
    }
    if !create_phantoms {
        return Err(validation(
            index,
            ItemFamily::Edge,
            type_id,
            "an endpoint does not exist and phantom creation is disabled",
        ));
    }
    let phantom_type = tenant
        .types
        .values()
        .find(|t| t.effective_traits.family.as_deref() == Some("phantom"))
        .ok_or_else(|| {
            validation(
                index,
                ItemFamily::Edge,
                type_id,
                "an endpoint does not exist and no phantom type is registered",
            )
        })?;
    *next_id += 1;
    nodes.push(FakeNode {
        id: *next_id,
        key: key.to_owned(),
        type_id: phantom_type.type_id.clone(),
        name: None,
        payload: None,
        has_embedding: false,
        version: 1,
        deleted: false,
    });
    counts.phantoms_created += 1;
    Ok(*next_id)
}

/// Apply one edge spec to the working copy. Returns whether it changed state.
#[expect(
    clippy::too_many_arguments,
    reason = "the working copies are threaded explicitly so the batch stays \
              atomic; bundling them into a struct would only rename them"
)]
fn apply_edge(
    tenant: &Tenant,
    nodes: &mut Vec<FakeNode>,
    edges: &mut Vec<FakeEdge>,
    next_id: &mut i64,
    counts: &mut IngestCounts,
    index: usize,
    spec: &graph_storage_sdk::models::EdgeSpec,
    create_phantoms: bool,
) -> Result<bool, GraphStoreError> {
    let record = tenant.types.get(&spec.type_id).cloned().ok_or_else(|| {
        validation(
            index,
            ItemFamily::Edge,
            &spec.type_id,
            "type is not registered",
        )
    })?;

    let before = counts.phantoms_created;
    let src = apply_endpoint(
        tenant,
        nodes,
        next_id,
        counts,
        index,
        &spec.type_id,
        &spec.src_node_key,
        create_phantoms,
    )?;
    let dst = apply_endpoint(
        tenant,
        nodes,
        next_id,
        counts,
        index,
        &spec.type_id,
        &spec.dst_node_key,
        create_phantoms,
    )?;
    let mut changed = counts.phantoms_created > before;

    // Endpoint constraints. A phantom endpoint is skipped: its concrete type
    // is not known yet, and the materialization path revalidates then.
    for (id, patterns, key, pointer) in [
        (
            src,
            &record.effective_traits.src_types,
            &spec.src_node_key,
            "/src_node_key",
        ),
        (
            dst,
            &record.effective_traits.dst_types,
            &spec.dst_node_key,
            "/dst_node_key",
        ),
    ] {
        let Some(endpoint) = nodes.iter().find(|n| n.id == id) else {
            continue;
        };
        let is_phantom = tenant
            .types
            .get(&endpoint.type_id)
            .and_then(|t| t.effective_traits.family.as_deref())
            == Some("phantom");
        if is_phantom || patterns.is_empty() {
            continue;
        }
        let admitted = ontology::matches_any_pattern(&endpoint.type_id, patterns).unwrap_or(false);
        if !admitted {
            return Err(GraphStoreError::Validation {
                items: vec![ItemError {
                    index,
                    family: ItemFamily::Edge,
                    gts_type: Some(spec.type_id.clone()),
                    pointer: Some(pointer.to_owned()),
                    message: format!(
                        "endpoint `{key}` is a `{}`, which `{}` does not admit; this edge \
                         type accepts {}",
                        endpoint.type_id,
                        spec.type_id,
                        patterns.join(", ")
                    ),
                }],
            });
        }
    }

    let edge_key = identity::derive_edge_key(record.type_uuid, spec);
    match edges.iter_mut().find(|e| e.key == edge_key) {
        Some(existing) if existing.payload == spec.payload && !existing.deleted => {
            counts.edges_unchanged += 1;
        }
        Some(existing) => {
            existing.payload.clone_from(&spec.payload);
            existing.deleted = false;
            counts.edges_updated += 1;
            changed = true;
        }
        None => {
            edges.push(FakeEdge {
                key: edge_key,
                type_id: spec.type_id.clone(),
                src,
                dst,
                payload: spec.payload.clone(),
                deleted: false,
            });
            counts.edges_inserted += 1;
            changed = true;
        }
    }
    Ok(changed)
}

/// A single-page envelope: the fake never paginates, so neither cursor is set.
fn empty_page_info() -> toolkit_odata::page::PageInfo {
    toolkit_odata::page::PageInfo {
        next_cursor: None,
        prev_cursor: None,
        limit: 0,
    }
}
