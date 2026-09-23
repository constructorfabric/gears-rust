//! The gateway's application services: authorization, admission, chain
//! validation, traversal orchestration — everything the gear itself owns.
//! Data lives behind the ports; no service here sees an entity or a
//! statement.

use std::collections::BTreeMap;
use std::sync::Arc;

use authz_resolver_sdk::pep::PolicyEnforcer;
use graph_storage_sdk::models::{
    DeleteOutcome, DeleteRequest, EdgeKey, EdgeView, GraphRevision, GtsTypeId, IngestOutcome,
    IngestRequest, ItemError, ItemFamily, NeighborhoodRequest, NodeKey, NodeRow, NodeView,
    OnExisting, Page, ProjectionRequest, RegisteredType, RemainingBudget, SearchMode,
    SearchRequest, SearchResponse, TraversalResponse, TraverseRequest, TypeIdSet, TypeKind,
    TypeQuery, TypeRecord, TypeRegistration, TypeRegistrationOptions,
};
use graph_storage_sdk::plugin_api::{GraphEngineV1, GraphStoreV1, StoreCtx};
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};

use crate::config::{GraphStorageConfig, ValidatedConfig};
use crate::domain::embedding;
use crate::domain::embedding::EmbeddingCoordinator;
use crate::domain::error::DomainError;
use crate::domain::traversal::{Retention, WalkPlan, walk};
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
    /// The one absolute deadline this operation gets, opened where the
    /// operation was admitted.
    ///
    /// It lives here because `Authorized` is built exactly once per public
    /// call, and `store_ctx` may be built several times inside one. Starting
    /// the clock in `store_ctx` -- which is what this used to do -- gave a
    /// traversal a fresh ten seconds for its seed resolution and another ten
    /// for its hops, so "one absolute deadline per logical operation" was a
    /// deadline per store call, and a walk could outlive any number of them.
    budget: RemainingBudget,
}

impl GraphServices {
    /// Takes a [`ValidatedConfig`] for the same reason `PgGraphStore::new`
    /// does: the byte budgets and limits here are enforced at startup, and a
    /// construction path that skipped the check would run outside them
    /// silently.
    pub fn new(
        config: ValidatedConfig,
        store: Arc<dyn GraphStoreV1>,
        engine: Arc<dyn GraphEngineV1>,
        enforcer: PolicyEnforcer,
        embedding: EmbeddingCoordinator,
    ) -> Self {
        Self {
            config: config.into_inner(),
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
        // Before the policy call, which is itself work: a request whose
        // deadline is already gone should not spend a PDP round trip either.
        //
        // This is the only place the clock starts, and the only place an
        // operation is refused for being out of time. Everything below --
        // the hop loop, the catalogue passes, the ingest item loops -- is a
        // chunk boundary inside an operation already admitted here. None of
        // them can abort a statement already issued to the server; that needs
        // a server-side bound toolkit-db does not offer yet (gears-rust
        // #4761).
        let budget = RemainingBudget::starting_now(self.config.deadline_interactive());
        if budget.is_exhausted() {
            return Err(DomainError::Deadline);
        }
        let scope = authz::scope_for(&self.enforcer, ctx, resource, action).await?;
        Ok(Authorized {
            tenant: ctx.subject_tenant_id(),
            scope,
            subject: graph_storage_sdk::models::Subject::from_security_context(ctx),
            budget,
        })
    }

    fn store_ctx<'a>(
        auth: &'a Authorized,
        snapshot: Option<&'a graph_storage_sdk::models::ReadSnapshot>,
    ) -> StoreCtx<'a> {
        StoreCtx {
            tenant: auth.tenant,
            scope: &auth.scope,
            subject: auth.subject.clone(),
            snapshot,
            budget: auth.budget,
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
        let store_ctx = Self::store_ctx(&auth, None);

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
        // A base schema the caller sent itself is already in the batch, and
        // prepending it too would make the gear hand the store the same
        // identifier twice — one act naming a type twice, which the store
        // refuses. Producers that mirror the base ontology do send them, and
        // so does the conformance suite.
        let carried: std::collections::BTreeSet<&str> = batch
            .iter()
            .map(|registration| registration.type_id.as_str())
            .collect();
        let mut prefix: Vec<TypeRegistration> = Vec::new();
        for (type_id, raw) in ontology::BASE_SCHEMAS {
            if carried.contains(type_id) {
                continue;
            }
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

    /// Readiness, per capability, in the shape DESIGN § Readiness Matrix
    /// specifies (`fr-readiness`).
    ///
    /// Unauthenticated on purpose: the matrix leaves the health endpoints
    /// available precisely when the authorization resolver is the thing that
    /// is down, so this method takes no `SecurityContext` and touches no
    /// tenant data — it reports capabilities, never content.
    ///
    /// Rows the matrix specifies and this iteration does not ship are reported
    /// `not_implemented` with the deviation that records why. That is the
    /// difference between a readiness surface and a green light: an operator
    /// can see that dynamic indexes are not degraded but absent, and that
    /// nothing will ever flip them healthy in this build.
    pub async fn readiness(&self) -> graph_storage_sdk::models::Readiness {
        use graph_storage_sdk::models::{
            AUTHZ, ComponentReadiness as Row, DYNAMIC_INDEXES, EMBEDDING_PROVIDER, EMBEDDING_SPACE,
            GRAPH_ENGINE, METRIC_ANNOTATION, Readiness, ReadinessState as State,
            TENANT_RECONCILIATION, TYPES_REGISTRY,
        };

        let mut rows = self.store.probe_readiness().await;

        // The provider answers for itself, from what the last real exchange
        // showed rather than from a probe of its own: this endpoint is
        // anonymous and polled on a schedule, and asking a remote provider
        // costs the deployment a billable inference request every time.
        // A provider that cannot say it is healthy degrades the paths that
        // need it and nothing else.
        match self.embedding.health().await {
            Ok(()) => rows.push(Row::healthy(EMBEDDING_PROVIDER)),
            Err(error) => rows.push(Row::new(
                EMBEDDING_PROVIDER,
                State::Degraded,
                &format!("the embedding provider is unavailable: {error}"),
                "ingest with `embed=true`, and the vector arm of search",
                "automatic on provider recovery; vectors missed meanwhile are stale by input \
                 hash and re-embedded by the normal path",
            )),
        }

        // The identity row is `unhealthy` and the gear stays ready — the one
        // place the matrix's row and its aggregate rule disagree, resolved in
        // favour of the row (DESIGN § Readiness Matrix).
        rows.push(match self.embedding.active_epoch() {
            Some(_) => Row::healthy(EMBEDDING_SPACE),
            None => Row::new(
                EMBEDDING_SPACE,
                State::Unhealthy,
                "stored vectors belong to a space the active provider is not",
                "vector and hybrid search (`failed_precondition` /                  `EMBEDDING_SPACE_MISMATCH`)",
                "re-embed to the active space; the identity match restores the capability at \
                 cutover",
            ),
        });

        // The built-in engine is the only one in this deployment, and a
        // capability it declares absent is the contract working rather than a
        // fault: the matrix's degraded and unhealthy rows are about *external*
        // plugins — a stale projection, an unprovable cursor, a selector that
        // matched nothing. There is nothing here to be stale.
        rows.push(Row::healthy(GRAPH_ENGINE));

        // What the matrix specifies and this build does not have. Named, so
        // the absence is a fact an operator reads rather than one they infer.
        rows.push(Row::new(
            AUTHZ,
            State::NotImplemented,
            "the resolver is consulted per request and fails closed, but it is not probed \
             here: the platform PEP publishes no health surface",
            "nothing that is not already failing closed",
            "a platform health surface for the PEP",
        ));
        rows.push(Row::new(
            TYPES_REGISTRY,
            State::NotImplemented,
            "this iteration reaches the registry only at publication, not per request, so \
             there is no runtime dependency to probe",
            "nothing",
            "a runtime dependency on the registry, when the verdict is delegated to it",
        ));
        rows.push(Row::new(
            DYNAMIC_INDEXES,
            State::NotImplemented,
            "the index-activation lifecycle is not built; a declared path is \
             filterable as soon as it is declared, served by the static payload GIN",
            "nothing, and that is the gap: nothing rejects a filter for an index that is \
             still building, because no index is ever built",
            "the DDL surface a gear may call (gears-rust #4721) and the lifecycle above it",
        ));
        rows.push(Row::new(
            TENANT_RECONCILIATION,
            State::NotImplemented,
            "tenant offboarding is not built, so no deletion generation is tracked or \
             reconciled",
            "nothing",
            "the offboarding protocol",
        ));
        rows.push(Row::new(
            METRIC_ANNOTATION,
            State::NotImplemented,
            "metric annotation is not built; projections carry no annotations to be \
             missing",
            "nothing",
            "the analytics gear's annotation surface",
        ));

        Readiness::of(rows)
    }

    /// The source namespaces claimed in this tenant, with their owners.
    ///
    /// A read of the ownership boundary, authorized as a type-catalogue read:
    /// it says who may write a namespace, not what is stored under it.
    pub async fn list_source_namespaces(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<graph_storage_sdk::models::SourceNamespaceOwner>, DomainError> {
        let auth = self
            .authorize(ctx, &authz::type_resource(), authz::actions::READ)
            .await?;
        Ok(self
            .store
            .list_source_namespaces(&Self::store_ctx(&auth, None))
            .await?)
    }

    /// Move a namespace to another producer principal.
    ///
    /// Ontology administration, not write: transferring a namespace is
    /// deciding who may speak for a source, which is an administrative act
    /// even though its effect is felt on the write path
    /// (`fr-source-ownership`, DESIGN § Authorization Model).
    pub async fn transfer_source_namespace(
        &self,
        ctx: &SecurityContext,
        namespace: &str,
        owner_principal: &str,
    ) -> Result<graph_storage_sdk::models::SourceNamespaceOwner, DomainError> {
        // The registry is only useful because the owner it holds is the exact
        // string a writer presents, and a writer's never carries surrounding
        // whitespace or a control character. Storing one that does reports a
        // successful transfer and leaves the namespace writable by nobody,
        // until an administrator notices and transfers again. Refused here
        // rather than trimmed: the administrator asked for a specific
        // principal and is owed either that one or an error.
        //
        // Here rather than in a store, because every store answers the same
        // registry contract and the fake one must refuse what PostgreSQL
        // refuses.
        // The namespace arrives as a path segment here rather than out of a
        // payload, so `namespace_of`'s refusal never sees it -- and this is the
        // one route that writes a registry row for a namespace nobody has
        // written under yet. A row keyed by a namespace no payload can produce
        // is unreachable by design, so it would sit in the registry and in
        // every listing of it, owned by someone, matching nothing.
        if namespace.trim() != namespace
            || namespace.trim().is_empty()
            || namespace.chars().any(char::is_control)
        {
            return Err(DomainError::InvalidQuery {
                message: "the namespace is compared to a node's `payload.source.system` exactly, \
                          so it cannot carry surrounding whitespace or control characters"
                    .to_owned(),
            });
        }
        if !crate::domain::ownership::is_canonical_principal(owner_principal) {
            return Err(DomainError::InvalidQuery {
                message: "the principal is compared to the writer's exactly, so it cannot \
                          carry surrounding whitespace or control characters"
                    .to_owned(),
            });
        }
        let auth = self
            .authorize(ctx, &authz::type_resource(), authz::actions::ADMIN)
            .await?;
        Ok(self
            .store
            .transfer_source_namespace(&Self::store_ctx(&auth, None), namespace, owner_principal)
            .await?)
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
            .get_type(&Self::store_ctx(&auth, None), type_id)
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
        admission::admit_type_query(&self.config, &query)?;
        Ok(self
            .store
            .list_types(&Self::store_ctx(&auth, None), query)
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

        let store_ctx = Self::store_ctx(&auth, None);
        let records = self.validate_batch(&store_ctx, &request).await?;

        // Composed and embedded *before* the transaction, as DESIGN's ingest
        // sequence has it (step 5, ahead of step 6). It costs one extra read:
        // what the store already holds of each node's vector, so a node whose
        // text has not changed is not embedded again (`GraphStoreV1::embedding_state`). Validation
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
                // An absent or incomplete `source` is reported by the chain
                // validator below, naming the member it misses.
                if let Some(complaint) =
                    identity::reference_key_complaint(&node.node_key, node.payload.as_ref())
                {
                    push(Some("/id".into()), complaint);
                }
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
                &Self::store_ctx(&auth, None),
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
                &Self::store_ctx(&auth, None),
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
            .get_node(&Self::store_ctx(&auth, None), node_key, limit)
            .await?)
    }

    /// One edge with its payload and envelope.
    ///
    /// Authorized as a node read: the authorization table gives edges no
    /// resource of their own -- an edge is reachable through its endpoints
    /// (DESIGN § Authorization Model), and the store refuses one whose
    /// endpoints the scope does not admit.
    pub async fn get_edge(
        &self,
        ctx: &SecurityContext,
        edge_key: &EdgeKey,
    ) -> Result<EdgeView, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::READ)
            .await?;
        Ok(self
            .store
            .get_edge(&Self::store_ctx(&auth, None), edge_key)
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
        let page = self
            .store
            .project_table(
                &Self::store_ctx(&auth, None),
                ProjectionRequest { type_set, query },
            )
            .await?;

        // A page is refused rather than trimmed, which is the opposite of
        // what traversal does and for a reason the cursor forces. The
        // continuation token is minted by the platform pager for the rows the
        // *statement* returned; dropping rows behind it and handing the token
        // back would make the client resume past the rows it never saw --
        // silently losing them, which is worse than a refusal it can act on.
        // So the caller is told to ask for fewer.
        let spent: u64 = page.items.iter().map(row_bytes).sum();
        if spent > self.config.response_max_bytes {
            return Err(DomainError::LimitExceeded {
                what: format!(
                    "the page hydrates to {spent} bytes; response_max_bytes is {}. \
                     Ask for fewer rows with `$top`, or narrow `$select`",
                    self.config.response_max_bytes
                ),
            });
        }
        Ok(page)
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
        let store_ctx = Self::store_ctx(&auth, None);

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

        let mut answer = self.store.search(&store_ctx, request, arm).await?;
        // The same rule the traversal follows, in the same layer: counts are
        // not a memory bound, so the bytes are measured rather than inferred
        // from the arm limits. The startup check makes this unreachable under
        // a validated configuration, which is the point of having both -- one
        // is a promise about the deployment and the other is what happens if
        // the promise is wrong.
        let budget = self.config.response_max_bytes;
        let mut spent: u64 = 0;
        let before = answer.hits.len();
        answer.hits.retain(|hit| {
            spent = spent.saturating_add(hit_bytes(hit));
            spent <= budget
        });
        if answer.hits.len() < before {
            answer.truncated = Some(graph_storage_sdk::models::TruncationReason::ResponseBytes);
        }
        Ok(answer)
    }

    pub async fn revision(&self, ctx: &SecurityContext) -> Result<GraphRevision, DomainError> {
        let auth = self
            .authorize(ctx, &authz::node_resource(), authz::actions::READ)
            .await?;
        Ok(self.store.revision(&Self::store_ctx(&auth, None)).await?)
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
            // A traversal's caller asked for a region and post-processes it,
            // so no node of the region is privileged over another.
            retention: Retention::Reached,
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
            // A UI that can draw 200 of a hub's 5 000 neighbours wants the
            // structural core, not 200 arbitrary leaves
            // (`fr-neighborhood-projection`).
            retention: Retention::Degree,
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
            .resolve_type_set(&Self::store_ctx(auth, None), patterns)
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
        //
        // Unless the store cannot hold one. It says so -- `snapshots = false`
        // -- and this used to open a handle from it anyway and stamp the
        // answer with the handle's revision, so the declaration protected
        // nobody and the response named a state it never existed at. A store
        // that declines is now taken at its word: no handle, and the walk is
        // bracketed by a revision read instead, which turns "the arms may
        // disagree" from an invisible property into a reported one.
        let snapshot_ctx = Self::store_ctx(auth, None);
        if !self.store.capabilities().snapshots {
            let before = self.store.revision(&snapshot_ctx).await?;
            let mut result = self
                .walk_without_snapshot(auth, seeds, plan, node_types, include_phantoms)
                .await?;
            let after = self.store.revision(&snapshot_ctx).await?;
            result.consistent_snapshot = before == after;
            result.revision = after;
            return Ok(result);
        }

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

    /// The same walk with no snapshot handle, for a store that declines them.
    ///
    /// The store context carries `None` where a snapshot would go, which is
    /// what every arm already does with a store that ignores it -- the
    /// difference is that nothing now claims otherwise.
    async fn walk_without_snapshot(
        &self,
        auth: &Authorized,
        seeds: &[NodeKey],
        plan: WalkPlan,
        node_types: Option<TypeIdSet>,
        include_phantoms: bool,
    ) -> Result<TraversalResponse, DomainError> {
        let revision = self.store.revision(&Self::store_ctx(auth, None)).await?;
        let standin = graph_storage_sdk::models::ReadSnapshot {
            id: uuid::Uuid::now_v7(),
            revision,
        };
        self.walk_under_snapshot(auth, seeds, plan, node_types, include_phantoms, &standin)
            .await
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
        let ctx = Self::store_ctx(auth, Some(snapshot));

        let resolved = self.store.resolve_node_ids(&ctx, seeds).await?;
        // Deduped: the walk starts from a set, so the echo is that set and
        // not the request's spelling of it.
        let seed_keys: std::collections::BTreeSet<&str> =
            resolved.iter().map(|(key, _)| key.as_str()).collect();
        let admitted: Vec<NodeKey> = seed_keys.iter().map(|key| (*key).to_owned()).collect();
        let seed_ids: Vec<i64> = resolved.iter().map(|(_, id)| *id).collect();
        if seed_ids.is_empty() {
            // Denied and nonexistent seeds are indistinguishable; an empty
            // authorized seed set is an empty answer, not an error.
            return Ok(TraversalResponse {
                consistent_snapshot: true,
                nodes: Vec::new(),
                edges: Vec::new(),
                seeds: Vec::new(),
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

        // Counts are not a memory bound. `traversal_max_nodes` elements of
        // `item_max_bytes` each is gigabytes at the hard limits, and every
        // individual number in that is legal -- so the arm whose count
        // ceiling does not fit inside `response_max_bytes` measures instead.
        // The projection page, the search arms and a node read do fit, which
        // is checked once at startup rather than per request.
        //
        // **Seeds are exempt, exactly as they are from the filter above.**
        // The response echoes the seeds it admitted, so a seed cut here
        // leaves the answer naming a node that is not in it -- and the
        // contract says seeds always survive truncation. When the seeds alone
        // do not fit there is no honest truncation left to make, and the read
        // is refused rather than answered with something that breaks its own
        // promise.
        let budget = self.config.response_max_bytes;
        let mut spent: u64 = nodes
            .iter()
            .filter(|view| seed_keys.contains(view.node_key.as_str()))
            .map(hydrated_bytes)
            .sum();
        if spent > budget {
            return Err(DomainError::LimitExceeded {
                what: format!(
                    "the seeds alone hydrate to {spent} bytes; response_max_bytes is \
                     {budget}. Ask for fewer seeds, or for a narrower node type set"
                ),
            });
        }
        let mut over_bytes = false;
        nodes.retain(|view| {
            if over_bytes {
                return false;
            }
            spent = spent.saturating_add(hydrated_bytes(view));
            if spent > budget {
                over_bytes = true;
                return false;
            }
            true
        });

        // An edge whose endpoint the filter removed goes with it. The
        // filter's whole purpose is that those nodes are not part of the
        // answer, and an edge naming one both dangles — a caller drawing the
        // result has a line to nothing — and says the node exists, which for
        // the phantom toggle is precisely what the caller asked not to be
        // told. Same rule as the edge read: an edge is a statement about two
        // nodes, and it is returned when both are visible.
        let surviving: std::collections::BTreeSet<&str> =
            nodes.iter().map(|view| view.node_key.as_str()).collect();
        let mut edges: Vec<_> = result
            .edges
            .iter()
            .filter(|edge| {
                surviving.contains(edge.src.as_str()) && surviving.contains(edge.dst.as_str())
            })
            .cloned()
            .collect();

        // Edges pay too. Measuring the nodes and appending the edges for free
        // is a budget on half the answer, and the half it leaves out is the
        // one that grows fastest: a dense neighbourhood has many more edges
        // than nodes, and each carries four caller-controlled strings.
        //
        // Cut after the nodes rather than before, because the asymmetry runs
        // that way: an edge without its endpoints is a line drawn to nothing,
        // while a node without some of its edges is simply a node with fewer
        // edges.
        edges.retain(|edge| {
            if over_bytes {
                return false;
            }
            spent = spent.saturating_add(edge_bytes(edge));
            if spent > budget {
                over_bytes = true;
                return false;
            }
            true
        });

        let truncated = if over_bytes {
            Some(graph_storage_sdk::models::TruncationReason::ResponseBytes)
        } else {
            result.truncated
        };

        Ok(TraversalResponse {
            // Overwritten by the caller when the store declines snapshots.
            consistent_snapshot: true,
            nodes,
            edges,
            seeds: admitted,
            truncated,
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

/// What one hydrated node costs to hold and to encode.
///
/// The payload dominates and the adjacency is the other half that grows: a
/// node carries its neighbours' keys and types too, and a traversal returns
/// many such nodes. Counting only the payload would make this a payload
/// budget rather than a response budget.
fn hydrated_bytes(view: &graph_storage_sdk::models::NodeView) -> u64 {
    let payload = view.payload.as_ref().map_or(0, |value| {
        serde_json::to_vec(value).map_or(u64::MAX, |bytes| bytes.len() as u64)
    });
    let adjacency: u64 = view
        .adjacency
        .iter()
        .map(|entry| {
            (entry.edge_key.len() + entry.edge_type_id.len() + entry.neighbor_key.len()) as u64
        })
        .sum();
    payload
        .saturating_add(adjacency)
        .saturating_add(view.node_key.len() as u64)
        .saturating_add(view.type_id.len() as u64)
        .saturating_add(view.name.as_ref().map_or(0, |name| name.len() as u64))
}

/// What one search hit costs to carry.
///
/// A hit is a key, a type, a name and its per-arm ranks -- deliberately not a
/// payload, which is what the ranking projection exists to avoid reading. It
/// is still caller-controlled text, so it is still measured.
fn hit_bytes(hit: &graph_storage_sdk::models::SearchHit) -> u64 {
    (hit.node_key.len()
        + hit.type_id.len()
        + hit.name.as_ref().map_or(0, String::len)
        + hit.snippet.as_ref().map_or(0, String::len)
        + hit.arms.len() * std::mem::size_of::<graph_storage_sdk::models::ArmHit>()) as u64
}

/// What one projected row costs to carry.
fn row_bytes(row: &NodeRow) -> u64 {
    let payload = row.payload.as_ref().map_or(0, |value| {
        serde_json::to_vec(value).map_or(u64::MAX, |bytes| bytes.len() as u64)
    });
    payload
        .saturating_add(row.node_key.len() as u64)
        .saturating_add(row.type_id.len() as u64)
        .saturating_add(row.name.as_ref().map_or(0, |name| name.len() as u64))
}

/// What one traversed edge costs to carry: four caller-controlled strings.
fn edge_bytes(edge: &graph_storage_sdk::models::EdgeRef) -> u64 {
    (edge.edge_key.len() + edge.edge_type_id.len() + edge.src.len() + edge.dst.len()) as u64
}
