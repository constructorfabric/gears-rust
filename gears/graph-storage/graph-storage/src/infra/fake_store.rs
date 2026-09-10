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
    AdjacencyEntry, AdjacencySide, AdmissionBasis, DeleteOutcome, DeleteRequest, ElementEnvelope,
    GraphRevision, GtsTypeId, IngestCounts, IngestOutcome, IngestRequest, ItemError, ItemFamily,
    LabelAssignment, LabelId, LabelRecord, LabelSpec, NodeId, NodeKey, NodeRow, NodeView,
    OnExisting, Page, ProjectionRequest, ReadSnapshot, RegisteredType, RevisionOutcome,
    SchemaDiagnostic, SearchMode, SearchRequest, SearchResponse, SourceNamespaceOwner,
    StoreCapabilities, Subject, TopologyPage, TopologyRequest, TypeChange, TypeChangeState,
    TypeIdSet, TypeOutcome, TypeQuery, TypeRecord, TypeRegistration, TypeRegistrationOptions,
};
use graph_storage_sdk::plugin_api::{
    EmbeddingPlan, EmbeddingState, GraphStoreError, GraphStoreV1, StoreCtx, VectorArm,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::embedding::{PlannedVector, StoredVector, VectorOutcome, decide_vector};
use crate::domain::{evolution, identity, ontology, ownership, projection};

#[derive(Clone)]
struct FakeNode {
    id: i64,
    key: String,
    type_id: String,
    name: Option<String>,
    payload: Option<serde_json::Value>,
    /// The real vector, not a flag: the fake serves an actual cosine arm, so
    /// the acceptance test -- a document retrieved by its own text -- runs
    /// against both implementations rather than only the one with a database.
    embedding: Option<Vec<f32>>,
    /// `None` marks a vector that must not rank: absent, or stale.
    embedding_epoch: Option<i64>,
    embedding_input_hash: Option<String>,
    version: i64,
    deleted: bool,
    /// The audit envelope's storage. The fake tracks it for real rather than
    /// reporting a constant: an envelope only one implementation fills in is
    /// an envelope the conformance suite cannot see -- the same lesson the
    /// endpoint-constraint episode taught earlier in this prototype.
    audit: FakeAudit,
}

#[derive(Clone)]
struct FakeEdge {
    key: String,
    type_id: String,
    src: i64,
    dst: i64,
    payload: Option<serde_json::Value>,
    deleted: bool,
    audit: FakeAudit,
}

/// What `fr-audit-envelope` asks a store to remember per element: the last
/// writer of each of the three verbs, and when.
#[derive(Clone)]
struct FakeAudit {
    created_at: OffsetDateTime,
    created_by: Subject,
    updated_at: OffsetDateTime,
    updated_by: Subject,
    deleted_at: Option<OffsetDateTime>,
    deleted_by: Option<Subject>,
}

impl FakeAudit {
    fn created(subject: &Subject) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            created_at: now,
            created_by: subject.clone(),
            updated_at: now,
            updated_by: subject.clone(),
            deleted_at: None,
            deleted_by: None,
        }
    }

    fn updated(&mut self, subject: &Subject) {
        self.updated_at = OffsetDateTime::now_utc();
        self.updated_by = subject.clone();
        self.deleted_at = None;
        self.deleted_by = None;
    }

    fn tombstoned(&mut self, subject: &Subject) {
        self.deleted_at = Some(OffsetDateTime::now_utc());
        self.deleted_by = Some(subject.clone());
    }

    fn envelope(&self, key: String, tenant_id: Uuid, revision: GraphRevision) -> ElementEnvelope {
        ElementEnvelope {
            tenant_id,
            key,
            created_at: self.created_at,
            created_by: self.created_by.clone(),
            updated_at: self.updated_at,
            updated_by: self.updated_by.clone(),
            deleted_at: self.deleted_at,
            deleted_by: self.deleted_by.clone(),
            graph_revision: revision,
        }
    }
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
    /// Source namespace -> the producer principal bound to it. The fake
    /// carries the ownership boundary for the same reason it carries the
    /// others: a boundary only one implementation enforces is a boundary the
    /// conformance suite cannot see.
    namespaces: BTreeMap<String, SourceNamespaceOwner>,
    /// The resolved `index` kinds per registered type, what the projection
    /// admits payload paths against (mirrors `gts_type.effective_traits`'s
    /// `index_kinds` on the built-in store).
    index_kinds: BTreeMap<String, BTreeMap<String, ontology::ScalarKind>>,
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

pub struct FakeGraphStore {
    epoch: i64,
    tenants: Mutex<BTreeMap<Uuid, Tenant>>,
    /// Longest admitted derivation chain, in segments; the platform posture
    /// (3) unless a test raises it, exactly like `ontology_max_chain_depth`.
    max_chain_depth: usize,
}

impl Default for FakeGraphStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeGraphStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: 1,
            tenants: Mutex::new(BTreeMap::new()),
            max_chain_depth: 3,
        }
    }

    /// Admit chains up to `depth` segments, as a deployment raising
    /// `ontology_max_chain_depth` would.
    #[must_use]
    pub fn with_max_chain_depth(mut self, depth: usize) -> Self {
        self.max_chain_depth = depth;
        self
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
            // A real cosine arm over the stored vectors, not a stub.
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
        let mut tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let tenant = tenants.entry(ctx.tenant).or_default();

        // Atomic: analyze and decide everything before storing anything.
        let mut prepared = Vec::new();
        for registration in &batch {
            let chain = ontology::ancestors(&registration.type_id);
            let mut ancestors: Vec<(String, serde_json::Value)> = Vec::new();
            for ancestor in &chain[..chain.len().saturating_sub(1)] {
                let schema = batch
                    .iter()
                    .find(|r| &r.type_id == ancestor)
                    .map(|r| r.schema.clone())
                    .or_else(|| tenant.types.get(ancestor).map(|t| t.schema.clone()))
                    .ok_or_else(|| {
                        validation(
                            0,
                            ItemFamily::Node,
                            &registration.type_id,
                            &format!("ancestor `{ancestor}` is not registered"),
                        )
                    })?;
                ancestors.push((ancestor.clone(), schema));
            }
            let refs: Vec<&serde_json::Value> =
                ancestors.iter().map(|(_, schema)| schema).collect();
            let descriptor = ontology::analyze(
                &registration.type_id,
                &registration.schema,
                &refs,
                self.max_chain_depth,
            )
            .map_err(|error| {
                validation(
                    0,
                    ItemFamily::Node,
                    &registration.type_id,
                    &error.to_string(),
                )
            })?;

            if let Some(spec) = options.migration_for(&descriptor.type_id)
                && !tenant.types.contains_key(&descriptor.type_id)
            {
                return Err(GraphStoreError::InvalidQuery {
                    what: format!(
                        "the migration for `{}` has nothing to migrate: the type is not \
                         registered yet, so it holds no rows",
                        spec.type_id
                    ),
                });
            }
            let decided = match tenant.types.get(&descriptor.type_id).cloned() {
                None => Decided {
                    outcome: TypeOutcome::Created,
                    basis: None,
                    change: fresh_change(&descriptor.type_id),
                    revision: 1,
                    created_at: OffsetDateTime::now_utc(),
                    rewrites: Vec::new(),
                },
                Some(existing) => {
                    decide_existing(tenant, &existing, &descriptor, &ancestors, &options)?
                }
            };

            let kinds: BTreeMap<String, ontology::ScalarKind> = descriptor
                .index_paths
                .iter()
                .map(|p| (p.pointer.clone(), p.kind))
                .collect();
            prepared.push((
                TypeRecord {
                    type_id: descriptor.type_id,
                    type_uuid: descriptor.type_uuid,
                    kind: descriptor.kind,
                    is_abstract: descriptor.is_abstract,
                    schema: descriptor.schema,
                    effective_traits: descriptor.effective_traits,
                    created_at: decided.created_at,
                    revision: decided.revision,
                },
                kinds,
                decided,
            ));
        }

        let mut out = Vec::with_capacity(prepared.len());
        for (record, kinds, decided) in prepared {
            if !options.dry_run && decided.outcome != TypeOutcome::Unchanged {
                tenant.types.insert(record.type_id.clone(), record.clone());
                tenant.index_kinds.insert(record.type_id.clone(), kinds);
                // Validated above, written here: a migrated payload lands with
                // the same three marks the built-in store gives it — the new
                // version, the acting subject, and a stale vector.
                for rewrite in &decided.rewrites {
                    match rewrite.family {
                        ItemFamily::Node => {
                            if let Some(node) =
                                tenant.nodes.iter_mut().find(|n| n.key == rewrite.key)
                            {
                                node.payload = Some(rewrite.payload.clone());
                                node.version += 1;
                                // Only a type that composes its embedding
                                // input from the payload can have been made
                                // stale by rewriting it; clearing the epoch
                                // otherwise would re-embed the whole type for
                                // nothing.
                                if !record.effective_traits.vector_search.is_empty() {
                                    node.embedding_epoch = None;
                                }
                                node.audit.updated(&ctx.subject);
                            }
                        }
                        ItemFamily::Edge => {
                            if let Some(edge) =
                                tenant.edges.iter_mut().find(|e| e.key == rewrite.key)
                            {
                                edge.payload = Some(rewrite.payload.clone());
                                edge.audit.updated(&ctx.subject);
                            }
                        }
                    }
                }
                if decided.outcome == TypeOutcome::Updated {
                    // What a read answers has changed, so the revision has to
                    // move — the same obligation a label attach carries
                    // (ADR-0006). A `created` type changes no existing read.
                    tenant.revision += 1;
                }
            }
            out.push(RegisteredType {
                record,
                outcome: decided.outcome,
                basis: decided.basis,
                change: Some(decided.change),
            });
        }
        Ok(out)
    }

    async fn list_source_namespaces(
        &self,
        ctx: &StoreCtx<'_>,
    ) -> Result<Vec<SourceNamespaceOwner>, GraphStoreError> {
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        Ok(tenants
            .get(&ctx.tenant)
            .map(|tenant| tenant.namespaces.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn transfer_source_namespace(
        &self,
        ctx: &StoreCtx<'_>,
        namespace: &str,
        owner_principal: &str,
    ) -> Result<SourceNamespaceOwner, GraphStoreError> {
        if owner_principal.trim().is_empty() {
            return Err(GraphStoreError::InvalidQuery {
                what: "a transfer needs the principal to transfer to".to_owned(),
            });
        }
        let mut tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let tenant = tenants.entry(ctx.tenant).or_default();
        let now = OffsetDateTime::now_utc();
        let previous = tenant
            .namespaces
            .get(namespace)
            .map(|row| row.owner_principal.clone());
        let row = SourceNamespaceOwner {
            namespace: namespace.to_owned(),
            owner_principal: owner_principal.to_owned(),
            claimed_at: tenant
                .namespaces
                .get(namespace)
                .map_or(now, |row| row.claimed_at),
            previous_owner: previous.filter(|owner| owner != owner_principal),
            transferred_at: Some(now),
            transferred_by: Some(ctx.subject.clone()),
        };
        tenant.namespaces.insert(namespace.to_owned(), row.clone());
        Ok(row)
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
        embedding: EmbeddingPlan,
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

        // The ownership boundary, before any working copy is taken: a
        // reference node names its source namespace in its own payload, and a
        // payload proves nothing about who may speak for it. Claims land in the
        // registry here, so a batch that is later refused for any other reason
        // has still not handed anyone a namespace it did not have — the claim
        // and the write commit together, as they do in the built-in store's
        // transaction.
        let writer = ctx.subject.principal();
        let mut claims: Vec<(String, SourceNamespaceOwner)> = Vec::new();
        for spec in &req.nodes {
            let family = tenant
                .types
                .get(&spec.type_id)
                .and_then(|record| record.effective_traits.family.clone());
            let namespace = match ownership::namespace_of(family.as_deref(), spec.payload.as_ref())
                .map_err(|error| {
                    validation(0, ItemFamily::Node, &spec.type_id, &error.to_string())
                })? {
                ownership::Namespaced::None => continue,
                ownership::Namespaced::Under(namespace) => namespace.to_owned(),
            };
            let held = tenant
                .namespaces
                .get(&namespace)
                .map(|row| row.owner_principal.clone())
                .or_else(|| {
                    claims
                        .iter()
                        .find(|(name, _)| name == &namespace)
                        .map(|(_, row)| row.owner_principal.clone())
                });
            match ownership::decide(held.as_deref(), &writer) {
                ownership::Claim::Allowed => {}
                ownership::Claim::Forbidden => {
                    return Err(GraphStoreError::SourceNamespaceForbidden { namespace });
                }
                ownership::Claim::Take => claims.push((
                    namespace.clone(),
                    SourceNamespaceOwner {
                        namespace,
                        owner_principal: writer.clone(),
                        claimed_at: OffsetDateTime::now_utc(),
                        previous_owner: None,
                        transferred_at: None,
                        transferred_by: None,
                    },
                )),
            }
        }

        // Working copies: written back only once the whole batch succeeded, so
        // a partway failure leaves nothing.
        let mut nodes = tenant.nodes.clone();
        let mut edges = tenant.edges.clone();
        let mut next_id = tenant.next_id;
        let mut counts = IngestCounts::default();
        let mut changed = false;

        let mut state = BatchState {
            next_id: &mut next_id,
            counts: &mut counts,
            subject: &ctx.subject,
        };
        for (index, spec) in req.nodes.iter().enumerate() {
            let decided = embedding.nodes.get(index).ok_or_else(|| {
                GraphStoreError::Internal(format!(
                    "embedding plan covers {} nodes; the batch has {}",
                    embedding.nodes.len(),
                    req.nodes.len()
                ))
            })?;
            // Resolved before the borrow `apply_node` takes, and from the same
            // shared decision the built-in store uses: a vector state that only
            // one implementation gets right is one the suite cannot see.
            let vector = plan_vector(
                nodes.iter().find(|n| n.key == spec.node_key),
                PlannedVector {
                    decided,
                    active_epoch: embedding.epoch,
                },
            );
            changed |= apply_node(tenant, &mut nodes, &edges, &mut state, index, spec, vector)?;
        }
        for (index, spec) in req.edges.iter().enumerate() {
            changed |= apply_edge(
                tenant,
                &mut nodes,
                &mut edges,
                &mut state,
                index,
                spec,
                req.options.create_phantoms.unwrap_or(true),
            )?;
        }

        tenant.nodes = nodes;
        tenant.edges = edges;
        tenant.next_id = next_id;
        for (namespace, row) in claims {
            tenant.namespaces.insert(namespace, row);
        }
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
                        edge.audit.tombstoned(&ctx.subject);
                        tombstoned += 1;
                    }
                }
                tenant.nodes[index].deleted = true;
                tenant.nodes[index].audit.tombstoned(&ctx.subject);
                (1u64, tombstoned)
            }
            DeleteRequest::Edge(key) => {
                let Some(edge) = tenant.edges.iter_mut().find(|e| e.key == key && !e.deleted)
                else {
                    return Err(GraphStoreError::NotFound);
                };
                edge.deleted = true;
                edge.audit.tombstoned(&ctx.subject);
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
        let (nodes, edges, revision) = visible(tenant, ctx);
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
        Ok(view_of(
            node,
            ctx.tenant,
            GraphRevision {
                source_epoch: self.epoch,
                revision,
            },
            adjacency,
            truncated,
        ))
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
        let (nodes, _, revision) = visible(tenant, ctx);
        let revision = GraphRevision {
            source_epoch: self.epoch,
            revision,
        };
        Ok(ids
            .iter()
            .filter_map(|id| nodes.iter().find(|n| n.id == *id && !n.deleted))
            .map(|n| view_of(n, ctx.tenant, revision, Vec::new(), false))
            .collect())
    }

    async fn search(
        &self,
        ctx: &StoreCtx<'_>,
        req: SearchRequest,
        vector: Option<VectorArm>,
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
        // scoping and revision stamping hold on every arm. The vector arm, by
        // contrast, is real cosine over the stored vectors -- the acceptance
        // test (a document retrieved by its own text) has to mean the same
        // thing here as against the database.
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let Some(tenant) = tenants.get(&ctx.tenant) else {
            return Err(GraphStoreError::NotFound);
        };
        let (nodes, _, revision) = visible(tenant, ctx);
        let live: Vec<&FakeNode> = nodes.iter().filter(|n| !n.deleted).collect();

        let lexical: Vec<&FakeNode> =
            if matches!(req.mode, SearchMode::Lexical | SearchMode::Hybrid) {
                let needle = req.query.clone().unwrap_or_default().to_lowercase();
                live.iter()
                    .copied()
                    .filter(|n| {
                        needle.is_empty()
                            || n.name
                                .as_deref()
                                .unwrap_or_default()
                                .to_lowercase()
                                .contains(&needle)
                    })
                    .take(req.arm_limit as usize)
                    .collect()
            } else {
                Vec::new()
            };

        let ranked: Vec<&FakeNode> = match &vector {
            Some(arm) if matches!(req.mode, SearchMode::Vector | SearchMode::Hybrid) => {
                // Only current vectors rank: a vector of another epoch came
                // from another model, and one whose input changed carries no
                // epoch at all.
                let mut scored: Vec<(f64, &FakeNode)> = live
                    .iter()
                    .copied()
                    .filter(|n| n.embedding_epoch == Some(arm.epoch))
                    .filter_map(|n| {
                        n.embedding
                            .as_ref()
                            .map(|stored| (cosine_distance(stored, &arm.query_vector), n))
                    })
                    .collect();
                scored.sort_by(|a, b| {
                    a.0.partial_cmp(&b.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.1.id.cmp(&b.1.id))
                });
                scored
                    .into_iter()
                    .map(|(_, n)| n)
                    .take(req.arm_limit as usize)
                    .collect()
            }
            _ => Vec::new(),
        };

        Ok(SearchResponse {
            hits: fuse_arms(&lexical, &ranked, req.limit as usize),
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
        let (nodes, _, revision) = visible(tenant, ctx);
        let limit = req.query.limit.unwrap_or(200);
        let selected: Vec<&FakeNode> = nodes
            .iter()
            .filter(|n| !n.deleted)
            .filter(|n| {
                req.type_set
                    .as_ref()
                    .is_none_or(|set| set.contains(&n.type_id))
            })
            .collect();

        // Column-only filter and ordering are the platform's on the real
        // store, and the fake answers the unfiltered page for them. A payload
        // path is this gear's own rule, so the fake evaluates the shared plan
        // -- the admissibility check and the semantics it implies are then
        // asserted against both implementations (DEVIATIONS D-104).
        let selected: Vec<&FakeNode> = if projection::mentions_payload(&req.query) {
            let admitted = req.type_set.as_ref().map(|set| {
                let kinds: Vec<BTreeMap<String, ontology::ScalarKind>> = set
                    .0
                    .iter()
                    .map(|type_id| tenant.index_kinds.get(type_id).cloned().unwrap_or_default())
                    .collect();
                projection::admitted_paths(&kinds)
            });
            let plan = projection::plan(&req.query, admitted.as_ref())
                .map_err(|error| GraphStoreError::InvalidQuery { what: error.0 })?;
            if req.query.cursor.is_some() {
                return Err(GraphStoreError::InvalidQuery {
                    what: "the in-memory store does not page a payload projection".to_owned(),
                });
            }
            let mut rows: Vec<&FakeNode> = selected
                .into_iter()
                .filter(|n| plan.filter.as_ref().is_none_or(|p| eval::holds(p, n)))
                .collect();
            rows.sort_by(|a, b| eval::order(&plan, a, b));
            rows
        } else {
            selected
        };

        let items = selected
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .map(|n| NodeRow {
                envelope: n.audit.envelope(
                    n.key.clone(),
                    ctx.tenant,
                    GraphRevision {
                        source_epoch: self.epoch,
                        revision,
                    },
                ),
                node_key: n.key.clone(),
                type_id: n.type_id.clone(),
                name: n.name.clone(),
                payload: n.payload.clone(),
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

    async fn embedding_state(
        &self,
        ctx: &StoreCtx<'_>,
        keys: &[NodeKey],
    ) -> Result<Vec<Option<EmbeddingState>>, GraphStoreError> {
        if !scope_admits(ctx.scope, ctx.tenant) {
            return Ok(keys.iter().map(|_| None).collect());
        }
        let tenants = self.tenants.lock().map_err(|_| poisoned())?;
        let Some(tenant) = tenants.get(&ctx.tenant) else {
            return Ok(keys.iter().map(|_| None).collect());
        };
        let (nodes, _, _) = visible(tenant, ctx);
        Ok(keys
            .iter()
            .map(|key| {
                nodes
                    .iter()
                    .find(|n| &n.key == key && !n.deleted)
                    .map(|n| EmbeddingState {
                        input_hash: n.embedding_input_hash.clone(),
                        vector_epoch: if n.embedding.is_some() {
                            n.embedding_epoch
                        } else {
                            None
                        },
                    })
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
                served_by: graph_storage_sdk::plugin_api::HopBackend::TwoQuery,
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
            // The fake walks in memory; it has no pattern backend to decline.
            served_by: graph_storage_sdk::plugin_api::HopBackend::TwoQuery,
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

/// What the fake decided about one identifier that is already registered.
struct Decided {
    outcome: TypeOutcome,
    basis: Option<AdmissionBasis>,
    change: TypeChange,
    revision: i32,
    created_at: OffsetDateTime,
    /// Payloads a migration produced, applied by the write loop — the fake
    /// decides with an immutable borrow of the tenant and writes afterwards,
    /// exactly as the built-in store validates before it writes.
    rewrites: Vec<Rewrite>,
}

/// One migrated row, keyed the way a producer names it.
struct Rewrite {
    family: ItemFamily,
    key: String,
    payload: serde_json::Value,
}

fn fresh_change(type_id: &str) -> TypeChange {
    TypeChange {
        type_id: type_id.to_owned(),
        state: TypeChangeState::New,
        backward: "compatible".to_owned(),
        forward: "compatible".to_owned(),
        diagnostics: Vec::new(),
        traits_changed: Vec::new(),
        rows: None,
        rows_rewritten: None,
        levels_not_evolvable_in_place: Vec::new(),
        migration_required: false,
        admissible: true,
    }
}

/// The same rule the built-in store applies, over in-memory rows.
///
/// The fake carries the data-backed ground too, and not as a stub: an
/// admission ground only the `PostgreSQL` store applies is a ground the
/// conformance suite cannot see, which is the lesson the endpoint-constraint
/// episode taught earlier in this prototype.
fn decide_existing(
    tenant: &Tenant,
    existing: &TypeRecord,
    descriptor: &ontology::TypeDescriptor,
    ancestors: &[(String, serde_json::Value)],
    options: &TypeRegistrationOptions,
) -> Result<Decided, GraphStoreError> {
    let update = options.on_existing == OnExisting::Update;
    let traits_changed =
        evolution::traits_diff(&existing.effective_traits, &descriptor.effective_traits);
    let unchanged = |traits_changed: Vec<graph_storage_sdk::models::TraitChange>| Decided {
        outcome: TypeOutcome::Unchanged,
        basis: None,
        change: TypeChange {
            type_id: descriptor.type_id.clone(),
            state: TypeChangeState::Unchanged,
            backward: "compatible".to_owned(),
            forward: "compatible".to_owned(),
            diagnostics: Vec::new(),
            traits_changed,
            rows: None,
            rows_rewritten: None,
            levels_not_evolvable_in_place: Vec::new(),
            migration_required: false,
            admissible: true,
        },
        revision: existing.revision,
        created_at: existing.created_at,
        rewrites: Vec::new(),
    };

    // Byte-identical re-registration converges. The fake stores the resolved
    // traits as a value rather than as JSON, so unlike the built-in store's
    // column it cannot go stale; the diff is still reported.
    if existing.schema == descriptor.schema {
        if options.migration_for(&descriptor.type_id).is_some() {
            return Err(GraphStoreError::InvalidQuery {
                what: format!(
                    "the migration for `{}` has nothing to migrate towards: the candidate \
                     schema is byte-identical to the registered one",
                    descriptor.type_id
                ),
            });
        }
        return Ok(unchanged(traits_changed));
    }

    let comparison = evolution::compare(
        &existing.schema,
        &descriptor.schema,
        ancestors.iter().cloned(),
    )
    .map_err(|error| validation(0, ItemFamily::Node, &descriptor.type_id, &error.to_string()))?;
    let state = comparison.state();
    let mut change = TypeChange {
        type_id: descriptor.type_id.clone(),
        state,
        backward: comparison.backward.as_str().to_owned(),
        forward: comparison.forward.as_str().to_owned(),
        diagnostics: comparison.diagnostics,
        traits_changed,
        rows: None,
        rows_rewritten: None,
        levels_not_evolvable_in_place: comparison.levels_not_evolvable_in_place,
        migration_required: !matches!(state, TypeChangeState::Compatible),
        admissible: false,
    };

    let accepted = |change: TypeChange, basis: AdmissionBasis, rewrites: Vec<Rewrite>| Decided {
        outcome: TypeOutcome::Updated,
        basis: Some(basis),
        change,
        revision: existing.revision.saturating_add(1),
        created_at: existing.created_at,
        rewrites,
    };

    let migration = options.migration_for(&descriptor.type_id);
    match evolution::decide(
        state,
        evolution::Asked {
            update,
            offered: evolution::offered(migration.is_some(), options.revalidate),
        },
    ) {
        evolution::Decision::Refuse => {
            if options.dry_run {
                return Ok(Decided {
                    outcome: TypeOutcome::Unchanged,
                    basis: None,
                    change,
                    revision: existing.revision,
                    created_at: existing.created_at,
                    rewrites: Vec::new(),
                });
            }
            Err(GraphStoreError::Conflict {
                reason: if update {
                    evolution::refusal_reason(&descriptor.type_id, state, &change.diagnostics, 5)
                } else {
                    format!(
                        "type `{}` is already registered with a different schema",
                        descriptor.type_id
                    )
                },
            })
        }
        evolution::Decision::Accept => {
            change.admissible = true;
            Ok(accepted(change, AdmissionBasis::SchemaProved, Vec::new()))
        }
        evolution::Decision::Migrate => {
            let Some(spec) = migration else {
                return Err(GraphStoreError::Internal(
                    "the rule asked for a migration where none was declared".to_owned(),
                ));
            };
            let plan = crate::domain::migration::compile(spec).map_err(|error| {
                GraphStoreError::InvalidQuery {
                    what: error.to_string(),
                }
            })?;
            let mut chain: Vec<(String, serde_json::Value)> = ancestors.to_vec();
            chain.push((descriptor.type_id.clone(), descriptor.schema.clone()));
            let validator =
                ontology::ChainValidator::compile(&descriptor.schema, chain).map_err(|error| {
                    validation(0, ItemFamily::Node, &descriptor.type_id, &error.to_string())
                })?;
            let (scanned, rewrites, failures) =
                migrate_fake(tenant, &descriptor.type_id, &plan, &validator);
            change.rows = Some(scanned);
            change.rows_rewritten = Some(rewrites.len() as u64);
            if failures.is_empty() {
                change.admissible = true;
                let basis = AdmissionBasis::Migrated {
                    rows_scanned: scanned,
                    rows_rewritten: rewrites.len() as u64,
                };
                return Ok(accepted(
                    change,
                    basis,
                    if options.dry_run {
                        Vec::new()
                    } else {
                        rewrites
                    },
                ));
            }
            if !options.dry_run {
                return Err(GraphStoreError::Validation { items: failures });
            }
            for failure in &failures {
                change.diagnostics.push(SchemaDiagnostic {
                    location: failure.pointer.clone().unwrap_or_default(),
                    finding: "row_invalid_after_migration".to_owned(),
                    message: failure.message.clone(),
                });
            }
            Ok(Decided {
                outcome: TypeOutcome::Unchanged,
                basis: None,
                change,
                revision: existing.revision,
                created_at: existing.created_at,
                rewrites: Vec::new(),
            })
        }
        evolution::Decision::Revalidate => {
            let mut chain: Vec<(String, serde_json::Value)> = ancestors.to_vec();
            chain.push((descriptor.type_id.clone(), descriptor.schema.clone()));
            let validator =
                ontology::ChainValidator::compile(&descriptor.schema, chain).map_err(|error| {
                    validation(0, ItemFamily::Node, &descriptor.type_id, &error.to_string())
                })?;
            let rows = rows_of_type(tenant, &descriptor.type_id);
            change.rows = Some(rows);
            let failures = revalidate_fake(tenant, &descriptor.type_id, &validator);
            if failures.is_empty() {
                change.admissible = true;
                return Ok(accepted(
                    change,
                    AdmissionBasis::DataBacked {
                        rows_validated: rows,
                    },
                    Vec::new(),
                ));
            }
            if !options.dry_run {
                return Err(GraphStoreError::Validation { items: failures });
            }
            for failure in &failures {
                change.diagnostics.push(SchemaDiagnostic {
                    location: failure.pointer.clone().unwrap_or_default(),
                    finding: "stored_row_invalid".to_owned(),
                    message: failure.message.clone(),
                });
            }
            Ok(Decided {
                outcome: TypeOutcome::Unchanged,
                basis: None,
                change,
                revision: existing.revision,
                created_at: existing.created_at,
                rewrites: Vec::new(),
            })
        }
    }
}

/// Live rows of one type, nodes and edges alike.
fn rows_of_type(tenant: &Tenant, type_id: &str) -> u64 {
    let nodes = tenant
        .nodes
        .iter()
        .filter(|node| !node.deleted && node.type_id == type_id)
        .count();
    let edges = tenant
        .edges
        .iter()
        .filter(|edge| !edge.deleted && edge.type_id == type_id)
        .count();
    (nodes + edges) as u64
}

/// Apply the plan to every live row of the type in memory, validate the
/// result, and report what would be written.
///
/// The fake carries the migration too, and not as a stub: a ground for
/// admission only the `PostgreSQL` store applies is a ground the conformance
/// suite cannot see.
fn migrate_fake(
    tenant: &Tenant,
    type_id: &str,
    plan: &crate::domain::migration::Plan,
    validator: &ontology::ChainValidator,
) -> (u64, Vec<Rewrite>, Vec<ItemError>) {
    let mut scanned = 0u64;
    let mut rewrites = Vec::new();
    let mut failures = Vec::new();

    for node in tenant
        .nodes
        .iter()
        .filter(|node| !node.deleted && node.type_id == type_id)
    {
        scanned += 1;
        let mut payload = node.payload.clone().unwrap_or(serde_json::Value::Null);
        if payload.is_null() {
            payload = serde_json::json!({});
        }
        let changed = plan.apply(&mut payload);
        let mut instance = serde_json::json!({ "node_key": node.key, "type": type_id });
        if let Some(name) = &node.name {
            instance["name"] = serde_json::json!(name);
        }
        instance["payload"] = payload.clone();
        let violations = validator.validate(&instance);
        if !violations.is_empty() {
            for (pointer, message) in violations {
                failures.push(ItemError {
                    index: usize::try_from(scanned - 1).unwrap_or(usize::MAX),
                    family: ItemFamily::Node,
                    gts_type: Some(type_id.to_owned()),
                    pointer: Some(pointer),
                    message: format!(
                        "node `{}` does not satisfy the candidate after the migration: {message}",
                        node.key
                    ),
                });
            }
            continue;
        }
        if changed {
            rewrites.push(Rewrite {
                family: ItemFamily::Node,
                key: node.key.clone(),
                payload,
            });
        }
    }

    for edge in tenant
        .edges
        .iter()
        .filter(|edge| !edge.deleted && edge.type_id == type_id)
    {
        scanned += 1;
        let mut payload = edge.payload.clone().unwrap_or(serde_json::json!({}));
        let changed = plan.apply(&mut payload);
        let key_of = |id: i64| {
            tenant
                .nodes
                .iter()
                .find(|node| node.id == id)
                .map_or_else(String::new, |node| node.key.clone())
        };
        let mut instance = serde_json::json!({
            "type": type_id,
            "src_node_key": key_of(edge.src),
            "dst_node_key": key_of(edge.dst),
        });
        instance["payload"] = payload.clone();
        let violations = validator.validate(&instance);
        if !violations.is_empty() {
            for (pointer, message) in violations {
                failures.push(ItemError {
                    index: usize::try_from(scanned - 1).unwrap_or(usize::MAX),
                    family: ItemFamily::Edge,
                    gts_type: Some(type_id.to_owned()),
                    pointer: Some(pointer),
                    message: format!(
                        "edge `{}` does not satisfy the candidate after the migration: {message}",
                        edge.key
                    ),
                });
            }
            continue;
        }
        if changed {
            rewrites.push(Rewrite {
                family: ItemFamily::Edge,
                key: edge.key.clone(),
                payload,
            });
        }
    }

    (scanned, rewrites, failures)
}

/// Does every live row of the type validate against the candidate?
fn revalidate_fake(
    tenant: &Tenant,
    type_id: &str,
    validator: &ontology::ChainValidator,
) -> Vec<ItemError> {
    let mut errors = Vec::new();
    for (index, node) in tenant
        .nodes
        .iter()
        .filter(|node| !node.deleted && node.type_id == type_id)
        .enumerate()
    {
        let mut instance = serde_json::json!({ "node_key": node.key, "type": type_id });
        if let Some(name) = &node.name {
            instance["name"] = serde_json::json!(name);
        }
        if let Some(payload) = &node.payload {
            instance["payload"] = payload.clone();
        }
        for (pointer, message) in validator.validate(&instance) {
            errors.push(ItemError {
                index,
                family: ItemFamily::Node,
                gts_type: Some(type_id.to_owned()),
                pointer: Some(pointer),
                message: format!("node `{}`: {message}", node.key),
            });
        }
    }
    for (index, edge) in tenant
        .edges
        .iter()
        .filter(|edge| !edge.deleted && edge.type_id == type_id)
        .enumerate()
    {
        let key_of = |id: i64| {
            tenant
                .nodes
                .iter()
                .find(|node| node.id == id)
                .map_or_else(String::new, |node| node.key.clone())
        };
        let mut instance = serde_json::json!({
            "type": type_id,
            "src_node_key": key_of(edge.src),
            "dst_node_key": key_of(edge.dst),
        });
        if let Some(payload) = &edge.payload {
            instance["payload"] = payload.clone();
        }
        for (pointer, message) in validator.validate(&instance) {
            errors.push(ItemError {
                index,
                family: ItemFamily::Edge,
                gts_type: Some(type_id.to_owned()),
                pointer: Some(pointer),
                message: format!("edge `{}`: {message}", edge.key),
            });
        }
    }
    errors
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

fn view_of(
    node: &FakeNode,
    tenant_id: Uuid,
    revision: GraphRevision,
    adjacency: Vec<AdjacencyEntry>,
    truncated: bool,
) -> NodeView {
    NodeView {
        node_key: node.key.clone(),
        type_id: node.type_id.clone(),
        name: node.name.clone(),
        payload: node.payload.clone(),
        has_embedding: node.embedding.is_some(),
        labels: Vec::new(),
        adjacency,
        adjacency_truncated: truncated,
        envelope: node.audit.envelope(node.key.clone(), tenant_id, revision),
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
/// The mutable bookkeeping one batch carries from item to item: the id
/// allocator and the running tally. They travel together because every write
/// touches both, and separately they made every `apply_*` signature two
/// parameters longer than it had any reason to be.
struct BatchState<'a> {
    next_id: &'a mut i64,
    counts: &'a mut IngestCounts,
    /// The subject stamped on every element this batch writes.
    subject: &'a Subject,
}

/// The vector columns of one fake row, spelled from the shared decision.
struct VectorWrite {
    embedding: Option<Vec<f32>>,
    epoch: Option<i64>,
    input_hash: Option<String>,
}

fn plan_vector(current: Option<&FakeNode>, planned: PlannedVector<'_>) -> VectorWrite {
    let stored = current.map(|row| StoredVector {
        has_vector: row.embedding.is_some(),
        input_hash: row.embedding_input_hash.as_deref(),
    });
    match decide_vector(stored, planned) {
        VectorOutcome::Store {
            vector,
            epoch,
            input_hash,
        } => VectorWrite {
            embedding: Some(vector),
            epoch,
            input_hash: Some(input_hash),
        },
        VectorOutcome::Absent { input_hash } => VectorWrite {
            embedding: None,
            epoch: None,
            input_hash: Some(input_hash),
        },
        VectorOutcome::Preserve => VectorWrite {
            embedding: current.and_then(|row| row.embedding.clone()),
            epoch: current.and_then(|row| row.embedding_epoch),
            input_hash: current.and_then(|row| row.embedding_input_hash.clone()),
        },
        VectorOutcome::Stale => VectorWrite {
            embedding: current.and_then(|row| row.embedding.clone()),
            epoch: None,
            input_hash: current.and_then(|row| row.embedding_input_hash.clone()),
        },
    }
}

fn apply_node(
    tenant: &Tenant,
    nodes: &mut Vec<FakeNode>,
    edges: &[FakeEdge],
    state: &mut BatchState<'_>,
    index: usize,
    spec: &graph_storage_sdk::models::NodeSpec,
    vector: VectorWrite,
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
        *state.next_id += 1;
        nodes.push(FakeNode {
            id: *state.next_id,
            key: spec.node_key.clone(),
            type_id: spec.type_id.clone(),
            name: spec.name.clone(),
            payload: spec.payload.clone(),
            embedding: vector.embedding,
            embedding_epoch: vector.epoch,
            embedding_input_hash: vector.input_hash,
            version: 1,
            deleted: false,
            audit: FakeAudit::created(state.subject),
        });
        state.counts.nodes_inserted += 1;
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

    let vector_unchanged = (
        &existing.embedding,
        existing.embedding_epoch,
        &existing.embedding_input_hash,
    ) == (&vector.embedding, vector.epoch, &vector.input_hash);
    let unchanged = same_type
        && existing.name == spec.name
        && existing.payload == spec.payload
        && vector_unchanged;
    if unchanged {
        state.counts.nodes_unchanged += 1;
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
    existing.embedding = vector.embedding;
    existing.embedding_epoch = vector.epoch;
    existing.embedding_input_hash = vector.input_hash;
    existing.version += 1;
    existing.audit.updated(state.subject);
    if same_type {
        state.counts.nodes_updated += 1;
    } else {
        state.counts.phantoms_materialized += 1;
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
    state: &mut BatchState<'_>,
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
    *state.next_id += 1;
    nodes.push(FakeNode {
        id: *state.next_id,
        key: key.to_owned(),
        type_id: phantom_type.type_id.clone(),
        name: None,
        payload: None,
        embedding: None,
        embedding_epoch: None,
        embedding_input_hash: None,
        version: 1,
        deleted: false,
        // A phantom is brought into being by the edge that named it, so the
        // subject writing that edge is the one recorded here.
        audit: FakeAudit::created(state.subject),
    });
    state.counts.phantoms_created += 1;
    Ok(*state.next_id)
}

/// Apply one edge spec to the working copy. Returns whether it changed state.
fn apply_edge(
    tenant: &Tenant,
    nodes: &mut Vec<FakeNode>,
    edges: &mut Vec<FakeEdge>,
    state: &mut BatchState<'_>,
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

    let before = state.counts.phantoms_created;
    let src = apply_endpoint(
        tenant,
        nodes,
        state,
        index,
        &spec.type_id,
        &spec.src_node_key,
        create_phantoms,
    )?;
    let dst = apply_endpoint(
        tenant,
        nodes,
        state,
        index,
        &spec.type_id,
        &spec.dst_node_key,
        create_phantoms,
    )?;
    let mut changed = state.counts.phantoms_created > before;

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
            state.counts.edges_unchanged += 1;
        }
        Some(existing) => {
            existing.payload.clone_from(&spec.payload);
            existing.deleted = false;
            existing.audit.updated(state.subject);
            state.counts.edges_updated += 1;
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
                audit: FakeAudit::created(state.subject),
            });
            state.counts.edges_inserted += 1;
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

/// Cosine distance, the same measure pgvector's `<=>` operator serves.
fn cosine_distance(one: &[f32], other: &[f32]) -> f64 {
    let dot: f64 = one
        .iter()
        .zip(other)
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum();
    let norm = |v: &[f32]| -> f64 {
        v.iter()
            .map(|x| f64::from(*x) * f64::from(*x))
            .sum::<f64>()
            .sqrt()
    };
    let (left, right) = (norm(one), norm(other));
    if left == 0.0 || right == 0.0 {
        return 1.0;
    }
    1.0 - dot / (left * right)
}

/// Reciprocal Rank Fusion over the two arms, with the same constant the
/// built-in store uses. Hits report which arms matched and at what rank, so a
/// caller can tell a lexical hit from a semantic one.
fn fuse_arms(
    lexical: &[&FakeNode],
    vector: &[&FakeNode],
    limit: usize,
) -> Vec<graph_storage_sdk::models::SearchHit> {
    use graph_storage_sdk::models::{ArmHit, SearchArm, SearchHit};
    const K: f64 = 60.0;

    let mut order: Vec<i64> = Vec::new();
    let mut fused: BTreeMap<i64, (f64, Vec<ArmHit>, &FakeNode)> = BTreeMap::new();
    for (arm, rows) in [(SearchArm::Lexical, lexical), (SearchArm::Vector, vector)] {
        for (position, node) in rows.iter().enumerate() {
            let rank = u32::try_from(position)
                .unwrap_or(u32::MAX)
                .saturating_add(1);
            let contribution = 1.0 / (K + f64::from(rank));
            let entry = fused
                .entry(node.id)
                .or_insert_with(|| (0.0, Vec::new(), node));
            if entry.1.is_empty() {
                order.push(node.id);
            }
            entry.0 += contribution;
            entry.1.push(ArmHit {
                arm,
                rank,
                score: contribution,
            });
        }
    }

    let mut hits: Vec<SearchHit> = order
        .into_iter()
        .filter_map(|id| fused.get(&id))
        .map(|(score, arms, node)| SearchHit {
            node_key: node.key.clone(),
            type_id: node.type_id.clone(),
            name: node.name.clone(),
            score: *score,
            arms: arms.clone(),
            snippet: None,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    hits
}

/// In-memory evaluation of a projection plan over fake nodes.
mod eval {
    use std::cmp::Ordering;

    use toolkit_odata::SortDir;

    use super::FakeNode;
    use crate::domain::ontology::ScalarKind;
    use crate::domain::projection::{CmpOp, FieldRef, Plan, Predicate, Scalar, TextOp};

    /// One field's value on one row, in the field's kind.
    #[derive(Clone, Debug, PartialEq)]
    enum Cell {
        Null,
        Text(String),
        Num(f64),
        Bool(bool),
    }

    fn cell(field: &FieldRef, node: &FakeNode) -> Cell {
        match field {
            FieldRef::NodeKey => Cell::Text(node.key.clone()),
            FieldRef::Name => Cell::Text(node.name.clone().unwrap_or_default()),
            FieldRef::CreatedAt => rfc3339(node.audit.created_at),
            FieldRef::UpdatedAt => rfc3339(node.audit.updated_at),
            FieldRef::Payload { pointer, kind } => {
                let inner = pointer.strip_prefix("/payload").unwrap_or(pointer);
                let Some(value) = node.payload.as_ref().and_then(|p| p.pointer(inner)) else {
                    return Cell::Null;
                };
                match (kind, value) {
                    (ScalarKind::String | ScalarKind::DateTime, serde_json::Value::String(s)) => {
                        Cell::Text(s.clone())
                    }
                    (ScalarKind::Number | ScalarKind::Integer, serde_json::Value::Number(n)) => {
                        n.as_f64().map_or(Cell::Null, Cell::Num)
                    }
                    (ScalarKind::Boolean, serde_json::Value::Bool(b)) => Cell::Bool(*b),
                    _ => Cell::Null,
                }
            }
        }
    }

    fn rfc3339(at: time::OffsetDateTime) -> Cell {
        at.format(&time::format_description::well_known::Rfc3339)
            .map_or(Cell::Null, Cell::Text)
    }

    fn literal(value: &Scalar) -> Cell {
        match value {
            Scalar::Str(s) | Scalar::DateTime(s) => Cell::Text(s.clone()),
            Scalar::Num(n) => n.parse::<f64>().map_or(Cell::Null, Cell::Num),
            Scalar::Bool(b) => Cell::Bool(*b),
        }
    }

    /// SQL three-valued comparison collapsed to "holds": a NULL never holds.
    fn compare(a: &Cell, b: &Cell) -> Option<Ordering> {
        match (a, b) {
            (Cell::Text(x), Cell::Text(y)) => Some(x.cmp(y)),
            (Cell::Num(x), Cell::Num(y)) => x.partial_cmp(y),
            (Cell::Bool(x), Cell::Bool(y)) => Some(x.cmp(y)),
            _ => None,
        }
    }

    pub(super) fn holds(predicate: &Predicate, node: &FakeNode) -> bool {
        match predicate {
            Predicate::Compare { field, op, value } => {
                let Some(ordering) = compare(&cell(field, node), &literal(value)) else {
                    return false;
                };
                match op {
                    CmpOp::Eq => ordering == Ordering::Equal,
                    CmpOp::Ne => ordering != Ordering::Equal,
                    CmpOp::Gt => ordering == Ordering::Greater,
                    CmpOp::Ge => ordering != Ordering::Less,
                    CmpOp::Lt => ordering == Ordering::Less,
                    CmpOp::Le => ordering != Ordering::Greater,
                }
            }
            Predicate::In { field, values } => {
                let actual = cell(field, node);
                values
                    .iter()
                    .any(|v| compare(&actual, &literal(v)) == Some(Ordering::Equal))
            }
            Predicate::Text { field, op, needle } => match cell(field, node) {
                Cell::Text(text) => match op {
                    TextOp::Contains => text.contains(needle.as_str()),
                    TextOp::StartsWith => text.starts_with(needle.as_str()),
                    TextOp::EndsWith => text.ends_with(needle.as_str()),
                },
                _ => false,
            },
            Predicate::And(children) => children.iter().all(|c| holds(c, node)),
            Predicate::Or(children) => children.iter().any(|c| holds(c, node)),
            Predicate::Not(inner) => !holds(inner, node),
        }
    }

    /// The plan's order, nulls last in either direction -- the same rule the
    /// built-in store renders.
    pub(super) fn order(plan: &Plan, a: &FakeNode, b: &FakeNode) -> Ordering {
        for term in &plan.order {
            let (x, y) = (cell(&term.field, a), cell(&term.field, b));
            let ordering = match (&x, &y) {
                (Cell::Null, Cell::Null) => Ordering::Equal,
                (Cell::Null, _) => Ordering::Greater,
                (_, Cell::Null) => Ordering::Less,
                _ => {
                    let natural = compare(&x, &y).unwrap_or(Ordering::Equal);
                    match term.dir {
                        SortDir::Asc => natural,
                        SortDir::Desc => natural.reverse(),
                    }
                }
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    }
}
