//! The gateway's application services: authorization, admission, chain
//! validation, traversal orchestration — everything the gear itself owns.
//! Data lives behind the ports; no service here sees an entity or a
//! statement.

use std::collections::BTreeMap;
use std::sync::Arc;

use authz_resolver_sdk::pep::PolicyEnforcer;
use graph_storage_sdk::models::{
    DeleteOutcome, DeleteRequest, EdgeKey, GraphRevision, GtsTypeId, IngestOutcome, IngestRequest,
    ItemError, ItemFamily, NeighborhoodRequest, NodeKey, NodeRow, NodeView, OnExisting, Page,
    ProjectionRequest, RegisteredType, RemainingBudget, SearchMode, SearchRequest, SearchResponse,
    TraversalResponse, TraverseRequest, TypeIdSet, TypeKind, TypeQuery, TypeRecord,
    TypeRegistration, TypeRegistrationOptions,
};
use graph_storage_sdk::plugin_api::{GraphEngineV1, GraphStoreV1, StoreCtx};
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};

use crate::config::GraphStorageConfig;
use crate::domain::embedding;
use crate::domain::embedding::EmbeddingCoordinator;
use crate::domain::error::DomainError;
use crate::domain::traversal::{WalkPlan, walk};
use crate::domain::{admission, authz, identity, ontology};

#[domain_model]
pub struct GraphServices {
    config: GraphStorageConfig,
    store: Arc<dyn GraphStoreV1>,
    engine: Arc<dyn GraphEngineV1>,
    enforcer: PolicyEnforcer,
    embedding: EmbeddingCoordinator,
}

/// One authorized call: the compiled scope plus the derived per-call context
/// pieces, kept together so `StoreCtx` construction cannot drift.
struct Authorized {
    tenant: uuid::Uuid,
    scope: AccessScope,
    /// Resolved once per request beside the scope, so every stage of that
    /// request stamps the same subject on the elements it writes.
    subject: graph_storage_sdk::models::Subject,
}

impl GraphServices {
    pub fn new(
        config: GraphStorageConfig,
        store: Arc<dyn GraphStoreV1>,
        engine: Arc<dyn GraphEngineV1>,
        enforcer: PolicyEnforcer,
        embedding: EmbeddingCoordinator,
    ) -> Self {
        Self {
            config,
            store,
            engine,
            enforcer,
            embedding,
        }
    }

    #[must_use]
    pub fn config(&self) -> &GraphStorageConfig {
        &self.config
    }

    async fn authorize(
        &self,
        ctx: &SecurityContext,
        resource: &authz_resolver_sdk::pep::ResourceType,
        action: &str,
    ) -> Result<Authorized, DomainError> {
        let scope = authz::scope_for(&self.enforcer, ctx, resource, action).await?;
        Ok(Authorized {
            tenant: ctx.subject_tenant_id(),
            scope,
            subject: graph_storage_sdk::models::Subject::from_security_context(ctx),
        })
    }

    fn store_ctx<'a>(
        &self,
        auth: &'a Authorized,
        snapshot: Option<&'a graph_storage_sdk::models::ReadSnapshot>,
    ) -> StoreCtx<'a> {
        StoreCtx {
            tenant: auth.tenant,
            scope: &auth.scope,
            subject: auth.subject.clone(),
            snapshot,
            budget: RemainingBudget::starting_now(self.config.deadline_interactive()),
            cancel: CancellationToken::new(),
        }
    }

    // --- ontology -----------------------------------------------------------

    pub async fn register_types(
        &self,
        ctx: &SecurityContext,
        batch: Vec<TypeRegistration>,
    ) -> Result<Vec<TypeRecord>, DomainError> {
        let registered = self
            .register_types_with(ctx, batch, TypeRegistrationOptions::default())
            .await?;
        Ok(registered.into_iter().map(|item| item.record).collect())
    }

    /// What registering this batch *would* do: every verdict, no write.
    ///
    /// The most-asked question about a type is not "may I change it" but "what
    /// does this change cost me" — so this runs the identical code path with
    /// `dry_run`, which is the only way the answer cannot drift from the
    /// decision.
    pub async fn type_compatibility(
        &self,
        ctx: &SecurityContext,
        batch: Vec<TypeRegistration>,
        revalidate: bool,
        migrations: Vec<graph_storage_sdk::models::MigrationSpec>,
    ) -> Result<Vec<RegisteredType>, DomainError> {
        self.register_types_with(
            ctx,
            batch,
            TypeRegistrationOptions {
                on_existing: OnExisting::Update,
                revalidate,
                dry_run: true,
                migrations,
            },
        )
        .await
    }

    pub async fn register_types_with(
        &self,
        ctx: &SecurityContext,
        batch: Vec<TypeRegistration>,
        options: TypeRegistrationOptions,
    ) -> Result<Vec<RegisteredType>, DomainError> {
        let auth = self
            .authorize(ctx, &authz::type_resource(), authz::actions::ADMIN)
            .await?;
        // Reading the tenant's rows — and a migration *writes* them — is not
        // something ontology administration authorizes. The data decision is
        // asked for separately and the call is served under its scope: the
        // same scope ingest writes those rows with, which is what makes one
        // scope reach both the catalogue and the rows.
        let auth = if options.revalidate || !options.migrations.is_empty() {
            self.authorize(ctx, &authz::node_resource(), authz::actions::WRITE)
                .await?
        } else {
            auth
        };
        let store_ctx = self.store_ctx(&auth, None);

        // The base ontology is published per tenant on first use rather than
        // at boot: a tenant that never touches the graph gets no rows, and a
        // tenant created later still finds its ancestors. Every producer type
        // derives from a family, so without this the first registration fails
        // on an ancestor nobody registered.
        let batch = Self::with_base_ontology(&store_ctx, self.store.as_ref(), batch).await?;

        // Resolve every ancestor schema — from the batch first (a batch may
        // carry a family and its producer type together), then from the
        // registered set — and analyze before anything persists.
        let in_batch: BTreeMap<&str, &serde_json::Value> = batch
            .iter()
            .map(|r| (r.type_id.as_str(), &r.schema))
            .collect();

        for registration in &batch {
            let chain = ontology::ancestors(&registration.type_id);
            let mut ancestor_values: Vec<serde_json::Value> = Vec::new();
            for ancestor in &chain[..chain.len().saturating_sub(1)] {
                if let Some(schema) = in_batch.get(ancestor.as_str()) {
                    ancestor_values.push((*schema).clone());
                } else {
                    let record = self
                        .store
                        .get_type(&store_ctx, &ancestor.clone())
                        .await
                        .map_err(|_| {
                            DomainError::invalid(format!(
                                "type `{}`: ancestor `{ancestor}` is not registered and not in this batch",
                                registration.type_id
                            ))
                        })?;
                    ancestor_values.push(record.schema);
                }
            }
            let ancestor_refs: Vec<&serde_json::Value> = ancestor_values.iter().collect();
            ontology::analyze(
                &registration.type_id,
                &registration.schema,
                &ancestor_refs,
                usize::from(self.config.ontology_max_chain_depth),
            )?;
        }

        Ok(self
            .store
            .register_types_with(&store_ctx, batch, options)
            .await?)
    }

    /// Prepend whichever base-ontology schemas this tenant is missing.
    ///
    /// Idempotent by construction: a schema already registered byte-identical
    /// converges, and the base documents are compiled into the binary, so two
    /// gears of the same version cannot disagree about them.
    async fn with_base_ontology(
        store_ctx: &StoreCtx<'_>,
        store: &dyn GraphStoreV1,
        batch: Vec<TypeRegistration>,
    ) -> Result<Vec<TypeRegistration>, DomainError> {
        let mut prefix: Vec<TypeRegistration> = Vec::new();
        for (type_id, raw) in ontology::BASE_SCHEMAS {
            if store.get_type(store_ctx, &type_id.to_owned()).await.is_ok() {
                continue;
            }
            let schema = serde_json::from_str(raw).map_err(|error| {
                DomainError::internal(format!("base schema `{type_id}` does not parse: {error}"))
            })?;
            prefix.push(TypeRegistration {
                type_id: type_id.to_owned(),
                schema,
            });
        }
        if prefix.is_empty() {
            return Ok(batch);
        }
        // The caller's types come after their ancestors, in one batch, so the
        // whole publication is as atomic as the registration it enables.
        prefix.extend(batch);
        Ok(prefix)
    }

    pub async fn get_type(
        &self,
        ctx: &SecurityContext,
        type_id: &GtsTypeId,
    ) -> Result<TypeRecord, DomainError> {
        let auth = self
            .authorize(ctx, &authz::type_resource(), authz::actions::READ)
            .await?;
        Ok(self
            .store
            .get_type(&self.store_ctx(&auth, None), type_id)
            .await?)
    }

    pub async fn list_types(
        &self,
        ctx: &SecurityContext,
        query: TypeQuery,
    ) -> Result<Page<TypeRecord>, DomainError> {
        let auth = self
            .authorize(ctx, &authz::type_resource(), authz::actions::READ)
            .await?;
        Ok(self
            .store
            .list_types(&self.store_ctx(&auth, None), query)
            .await?)
    }

    // --- ingest --------------------------------------------------------------

    pub async fn ingest(
        &self,
        ctx: &SecurityContext,
        request: IngestRequest,
    ) -> Result<IngestOutcome, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::WRITE)
            .await?;
        admission::admit_ingest(&self.config, &request)?;

        let store_ctx = self.store_ctx(&auth, None);
        let records = self.validate_batch(&store_ctx, &request).await?;

        // Composed and embedded *before* the transaction, as DESIGN's ingest
        // sequence has it (step 5, ahead of step 6). It costs one extra read:
        // what the store already holds of each node's vector, so a node whose
        // text has not changed is not embedded again (D-027). Validation
        // already resolved every type record, so each node's `vector_search`
        // trait is in hand.
        let embed = request.options.embed.unwrap_or(true);
        let current = if embed && self.embedding.active_epoch().is_some() {
            let keys: Vec<String> = request.nodes.iter().map(|n| n.node_key.clone()).collect();
            self.store.embedding_state(&store_ctx, &keys).await?
        } else {
            Vec::new()
        };
        let plan = self
            .embedding
            .plan(
                &request.nodes,
                embed,
                |node| embedding::declared_paths(&records, node),
                &current,
                store_ctx.budget,
                store_ctx.cancel.clone(),
            )
            .await?;
        let plan = graph_storage_sdk::plugin_api::EmbeddingPlan {
            epoch: self.embedding.active_epoch(),
            nodes: plan,
        };

        Ok(self.store.ingest(&store_ctx, request, plan).await?)
    }

    /// Chain-validate every item, reporting **all** violations in one answer.
    ///
    /// Returns the resolved type records, because the caller needs the same
    /// ones the validation walked: re-fetching them to read one trait would
    /// be a second round trip per distinct type for information already held.
    async fn validate_batch(
        &self,
        store_ctx: &StoreCtx<'_>,
        request: &IngestRequest,
    ) -> Result<BTreeMap<String, TypeRecord>, DomainError> {
        let mut records: BTreeMap<String, TypeRecord> = BTreeMap::new();
        let mut validators: BTreeMap<String, ontology::ChainValidator> = BTreeMap::new();
        let mut errors: Vec<ItemError> = Vec::new();

        let mut distinct: Vec<&str> = request
            .nodes
            .iter()
            .map(|n| n.type_id.as_str())
            .chain(request.edges.iter().map(|e| e.type_id.as_str()))
            .collect();
        distinct.sort_unstable();
        distinct.dedup();

        for type_id in distinct {
            if let Ok(record) = self.store.get_type(store_ctx, &type_id.to_owned()).await {
                let mut chain: Vec<(String, serde_json::Value)> =
                    vec![(record.type_id.clone(), record.schema.clone())];
                for ancestor in ontology::ancestors(type_id) {
                    if ancestor == type_id {
                        continue;
                    }
                    if let Ok(parent) = self.store.get_type(store_ctx, &ancestor).await {
                        chain.push((parent.type_id.clone(), parent.schema));
                    }
                }
                let validator = ontology::ChainValidator::compile(&record.schema, chain)?;
                validators.insert(type_id.to_owned(), validator);
                records.insert(type_id.to_owned(), record);
            } else {
                // Reported per item below, so the producer sees which
                // items named the unknown type.
            }
        }

        for (index, node) in request.nodes.iter().enumerate() {
            let Some(record) = records.get(&node.type_id) else {
                errors.push(ItemError {
                    index,
                    family: ItemFamily::Node,
                    gts_type: Some(node.type_id.clone()),
                    pointer: None,
                    message: "type is not registered".into(),
                });
                continue;
            };
            Self::check_node_item(index, node, record, &validators, &mut errors);
        }

        for (index, edge) in request.edges.iter().enumerate() {
            let Some(record) = records.get(&edge.type_id) else {
                errors.push(ItemError {
                    index,
                    family: ItemFamily::Edge,
                    gts_type: Some(edge.type_id.clone()),
                    pointer: None,
                    message: "type is not registered".into(),
                });
                continue;
            };
            Self::check_edge_item(index, edge, record, &validators, &mut errors);
        }

        if errors.is_empty() {
            Ok(records)
        } else {
            Err(DomainError::Validation { items: errors })
        }
    }

    fn check_node_item(
        index: usize,
        node: &graph_storage_sdk::models::NodeSpec,
        record: &TypeRecord,
        validators: &BTreeMap<String, ontology::ChainValidator>,
        errors: &mut Vec<ItemError>,
    ) {
        let mut push = |pointer: Option<String>, message: String| {
            errors.push(ItemError {
                index,
                family: ItemFamily::Node,
                gts_type: Some(node.type_id.clone()),
                pointer,
                message,
            });
        };

        if record.kind != TypeKind::Node {
            push(
                None,
                format!(
                    "`{}` is a {} type, not a node type",
                    node.type_id,
                    record.kind.as_str()
                ),
            );
            return;
        }
        if record.is_abstract {
            push(None, "abstract types cannot be instantiated".into());
            return;
        }
        match record.effective_traits.family.as_deref() {
            Some("phantom") => {
                push(
                    None,
                    "phantom nodes are created by the gear, never ingested directly".into(),
                );
                return;
            }
            Some("reference") => {
                let source = node
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("source"))
                    .and_then(serde_json::Value::as_object);
                if let Some(source) = source {
                    let get = |k: &str| {
                        source
                            .get(k)
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                    };
                    let expected =
                        identity::reference_node_key(get("system"), get("kind"), get("native_id"));
                    if node.node_key != expected {
                        push(
                            Some("/id".into()),
                            format!(
                                "reference node keys derive from the full source triple; expected `{expected}`"
                            ),
                        );
                    }
                }
                // A missing `source` is reported by the chain validator below.
            }
            _ => {}
        }

        // The instance validated is the producer-authored document only. The
        // gear-assigned envelope -- tenant, timestamps, subjects, tombstone,
        // revision -- is described by the API schema and is deliberately not
        // part of the GTS type (DESIGN § API element envelope).
        let mut instance = serde_json::json!({
            "node_key": node.node_key,
            "type": node.type_id,
        });
        if let Some(name) = &node.name {
            instance["name"] = serde_json::json!(name);
        }
        if let Some(payload) = &node.payload {
            instance["payload"] = payload.clone();
        }
        if let Some(validator) = validators.get(&node.type_id) {
            for (pointer, message) in validator.validate(&instance) {
                push(Some(pointer), message);
            }
        }
    }

    fn check_edge_item(
        index: usize,
        edge: &graph_storage_sdk::models::EdgeSpec,
        record: &TypeRecord,
        validators: &BTreeMap<String, ontology::ChainValidator>,
        errors: &mut Vec<ItemError>,
    ) {
        let mut push = |pointer: Option<String>, message: String| {
            errors.push(ItemError {
                index,
                family: ItemFamily::Edge,
                gts_type: Some(edge.type_id.clone()),
                pointer,
                message,
            });
        };

        if record.kind != TypeKind::Edge {
            push(
                None,
                format!(
                    "`{}` is a {} type, not an edge type",
                    edge.type_id,
                    record.kind.as_str()
                ),
            );
            return;
        }
        if record.is_abstract {
            push(None, "abstract types cannot be instantiated".into());
            return;
        }

        // The edge base declares no key: `edge_key` is derived by the gear
        // from the type, the endpoints and the discriminator, so it is
        // envelope rather than body and is not offered for validation.
        let mut instance = serde_json::json!({
            "type": edge.type_id,
            "src_node_key": edge.src_node_key,
            "dst_node_key": edge.dst_node_key,
        });
        if let Some(discriminator) = &edge.discriminator {
            instance["discriminator"] = serde_json::json!(discriminator);
        }
        if let Some(payload) = &edge.payload {
            instance["payload"] = payload.clone();
        }
        if let Some(validator) = validators.get(&edge.type_id) {
            for (pointer, message) in validator.validate(&instance) {
                push(Some(pointer), message);
            }
        }
    }

    // --- deletes -------------------------------------------------------------

    pub async fn delete_node(
        &self,
        ctx: &SecurityContext,
        node_key: &NodeKey,
    ) -> Result<DeleteOutcome, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::DELETE)
            .await?;
        Ok(self
            .store
            .soft_delete(
                &self.store_ctx(&auth, None),
                DeleteRequest::Node(node_key.clone()),
            )
            .await?)
    }

    pub async fn delete_edge(
        &self,
        ctx: &SecurityContext,
        edge_key: &EdgeKey,
    ) -> Result<DeleteOutcome, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::DELETE)
            .await?;
        Ok(self
            .store
            .soft_delete(
                &self.store_ctx(&auth, None),
                DeleteRequest::Edge(edge_key.clone()),
            )
            .await?)
    }

    // --- reads ---------------------------------------------------------------

    pub async fn get_node(
        &self,
        ctx: &SecurityContext,
        node_key: &NodeKey,
        adjacency_limit: Option<u32>,
    ) -> Result<NodeView, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::READ)
            .await?;
        let limit = admission::admit_adjacency_limit(&self.config, adjacency_limit)?;
        Ok(self
            .store
            .get_node(&self.store_ctx(&auth, None), node_key, limit)
            .await?)
    }

    pub async fn project_nodes(
        &self,
        ctx: &SecurityContext,
        type_patterns: &[String],
        query: toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<NodeRow>, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::READ)
            .await?;
        admission::admit_projection(&self.config, &query)?;
        let type_set = self.resolve_patterns(&auth, type_patterns).await?;
        Ok(self
            .store
            .project_table(
                &self.store_ctx(&auth, None),
                ProjectionRequest { type_set, query },
            )
            .await?)
    }

    pub async fn search(
        &self,
        ctx: &SecurityContext,
        request: SearchRequest,
    ) -> Result<SearchResponse, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::READ)
            .await?;
        admission::admit_search(&self.config, &request)?;
        let store_ctx = self.store_ctx(&auth, None);

        // The query is embedded by the same provider ingest used
        // (`fr-vector-search`). No caller supplies a vector: one that came
        // from elsewhere could not be compared with anything stored.
        let arm = if matches!(request.mode, SearchMode::Vector | SearchMode::Hybrid) {
            let text = request.query.as_deref().unwrap_or_default();
            let query_vector = self
                .embedding
                .embed_query(text, store_ctx.budget, store_ctx.cancel.clone())
                .await?;
            self.embedding
                .active_epoch()
                .map(|epoch| graph_storage_sdk::plugin_api::VectorArm {
                    query_vector,
                    epoch,
                })
        } else {
            None
        };

        Ok(self.store.search(&store_ctx, request, arm).await?)
    }

    pub async fn revision(&self, ctx: &SecurityContext) -> Result<GraphRevision, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::READ)
            .await?;
        Ok(self.store.revision(&self.store_ctx(&auth, None)).await?)
    }

    // --- traversal -------------------------------------------------------------

    pub async fn traverse(
        &self,
        ctx: &SecurityContext,
        request: TraverseRequest,
    ) -> Result<TraversalResponse, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::READ)
            .await?;
        admission::admit_traverse(&self.config, &request)?;

        let plan = WalkPlan {
            depth: request.depth,
            max_nodes: request.max_nodes.unwrap_or(self.config.traversal_max_nodes),
            max_frontier: self.config.traversal_max_frontier,
            max_edges_scanned: self.config.traversal_max_edges_scanned,
            edge_types: self
                .resolve_patterns(&auth, &request.edge_type_patterns)
                .await?,
        };
        let node_types = self
            .resolve_patterns(&auth, &request.node_type_patterns)
            .await?;

        self.walk_and_hydrate(&auth, &request.seeds, plan, node_types, true)
            .await
    }

    pub async fn neighborhood(
        &self,
        ctx: &SecurityContext,
        request: NeighborhoodRequest,
    ) -> Result<TraversalResponse, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::READ)
            .await?;
        admission::admit_neighborhood(&self.config, &request)?;

        let plan = WalkPlan {
            depth: request.depth,
            max_nodes: request
                .node_budget
                .unwrap_or(self.config.traversal_max_nodes),
            max_frontier: self.config.traversal_max_frontier,
            max_edges_scanned: self.config.traversal_max_edges_scanned,
            edge_types: None,
        };
        let seeds = vec![request.root.clone()];
        self.walk_and_hydrate(&auth, &seeds, plan, None, request.include_phantoms)
            .await
    }

    /// Resolve GTS patterns to a registered-type set (never SQL text).
    async fn resolve_patterns(
        &self,
        auth: &Authorized,
        patterns: &[String],
    ) -> Result<Option<TypeIdSet>, DomainError> {
        if patterns.is_empty() {
            return Ok(None);
        }
        let set = self
            .store
            .resolve_type_set(&self.store_ctx(auth, None), patterns)
            .await?;
        Ok(Some(set))
    }

    async fn walk_and_hydrate(
        &self,
        auth: &Authorized,
        seeds: &[NodeKey],
        plan: WalkPlan,
        node_types: Option<TypeIdSet>,
        include_phantoms: bool,
    ) -> Result<TraversalResponse, DomainError> {
        // One snapshot across the whole compound read: seed resolution, every
        // hop, and the final hydration observe one graph state.
        let snapshot_ctx = self.store_ctx(auth, None);
        let snapshot = self.store.begin_read(&snapshot_ctx).await?;
        let result = self
            .walk_under_snapshot(auth, seeds, plan, node_types, include_phantoms, &snapshot)
            .await;
        // Releasing the snapshot must never mask the walk's own outcome, so a
        // failure to close is logged rather than returned.
        if let Err(error) = self.store.end_read(snapshot).await {
            tracing::warn!(%error, "could not release the read snapshot");
        }
        result
    }

    async fn walk_under_snapshot(
        &self,
        auth: &Authorized,
        seeds: &[NodeKey],
        plan: WalkPlan,
        node_types: Option<TypeIdSet>,
        include_phantoms: bool,
        snapshot: &graph_storage_sdk::models::ReadSnapshot,
    ) -> Result<TraversalResponse, DomainError> {
        let ctx = self.store_ctx(auth, Some(snapshot));

        let resolved = self.store.resolve_node_ids(&ctx, seeds).await?;
        let seed_keys: std::collections::BTreeSet<&str> =
            resolved.iter().map(|(key, _)| key.as_str()).collect();
        let seed_ids: Vec<i64> = resolved.iter().map(|(_, id)| *id).collect();
        if seed_ids.is_empty() {
            // Denied and nonexistent seeds are indistinguishable; an empty
            // authorized seed set is an empty answer, not an error.
            return Ok(TraversalResponse {
                nodes: Vec::new(),
                edges: Vec::new(),
                truncated: None,
                revision: snapshot.revision,
            });
        }

        let result = walk(self.engine.as_ref(), &ctx, seed_ids, &plan).await?;
        let mut nodes = self.store.hydrate_nodes(&ctx, &result.nodes).await?;

        // Output filtering. Seeds always survive; everything else must pass
        // the node-type filter and the phantom toggle.
        let phantom_types = self.phantom_types(&ctx, &nodes).await?;
        nodes.retain(|view| {
            if seed_keys.contains(view.node_key.as_str()) {
                return true;
            }
            if let Some(set) = &node_types
                && !set.contains(&view.type_id)
            {
                return false;
            }
            if !include_phantoms && phantom_types.contains(&view.type_id) {
                return false;
            }
            true
        });

        Ok(TraversalResponse {
            nodes,
            edges: result.edges,
            truncated: result.truncated,
            revision: snapshot.revision,
        })
    }

    /// Which of the result's types are phantom-family.
    async fn phantom_types(
        &self,
        ctx: &StoreCtx<'_>,
        nodes: &[NodeView],
    ) -> Result<std::collections::BTreeSet<GtsTypeId>, DomainError> {
        let mut distinct: Vec<&str> = nodes.iter().map(|n| n.type_id.as_str()).collect();
        distinct.sort_unstable();
        distinct.dedup();
        let mut phantom = std::collections::BTreeSet::new();
        for type_id in distinct {
            if let Ok(record) = self.store.get_type(ctx, &type_id.to_owned()).await
                && record.effective_traits.family.as_deref() == Some("phantom")
            {
                phantom.insert(type_id.to_owned());
            }
        }
        Ok(phantom)
    }
}
