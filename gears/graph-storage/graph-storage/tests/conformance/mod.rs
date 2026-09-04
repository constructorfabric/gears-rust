#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The `GraphStoreV1` conformance suite.
//!
//! Per ADR-0001 point 5 the *suite* is the deliverable, not the trait: a
//! contract term nobody checks is a comment. Every case here runs against
//! **both** implementations — the built-in `PostgreSQL` store and the in-memory
//! fake — so a change only one of them can satisfy fails rather than passes
//! quietly. That is also why the fake exists at all.
//!
//! The five obligations, in the order DESIGN § 3.3 lists them, and what this
//! suite does about each:
//!
//! 1. batch atomicity across nodes, edges and the idempotency record —
//!    asserted;
//! 2. single-writer serialization per scope identity — **not asserted**. Two
//!    concurrent replacements of one scope are never made to race here, so
//!    this obligation rests on inspection alone. Listed rather than omitted
//!    so the gap is visible from the suite that is supposed to close it;
//! 3. monotonic generation fencing under that serialization — asserted;
//! 4. a node with a live incident edge is never removed alone — asserted;
//! 5. one snapshot across every arm of one read — asserted on the fake, which
//!    honours it, and asserted as *declined* on the built-in store, which
//!    does not (see `the_built_in_store_declines_the_snapshot_obligation`).

use std::time::Duration;

use graph_storage::domain::embedding::{EmbeddingCoordinator, SpaceState};
use graph_storage::infra::embedding::fake::FakeEmbeddingProvider;
use graph_storage_sdk::models::{
    DeleteRequest, EdgeSpec, IngestOptions, IngestRequest, ItemFamily, NodeSpec, ProjectionRequest,
    ReadSnapshot, RemainingBudget, ReplaceScope, SearchMode, SearchRequest, Subject,
    TypeRegistration,
};
use graph_storage_sdk::plugin_api::{EmbeddingPlan, GraphStoreError, GraphStoreV1, StoreCtx};
use tokio_util::sync::CancellationToken;
use toolkit_security::AccessScope;
use uuid::Uuid;

/// Producer types the suite registers on top of the base ontology.
pub const OWNED: &str = "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~test.gs._.thing.v1~";
/// An edge type that admits only owned nodes at either end — the constraint
/// that gives the endpoint check something to refuse.
pub const OWNED_ONLY: &str =
    "gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~test.gs._.owned_link.v1~";
pub const OWNED_FAMILY: &str = "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~";
/// A node type from a different family, which `OWNED_ONLY` must refuse.
pub const REFERENCE: &str =
    "gts.cf.core.graph.node.v1~cf.core.graph.reference_node.v1~test.gs._.mirror.v1~";
pub const LINK: &str = "gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~test.gs._.link.v1~";

/// Ingest through the real Embedding Coordinator, as the domain service does.
///
/// The suite calls this rather than `GraphStoreV1::ingest` directly so every
/// case exercises the composed-and-embedded path: the store's vector
/// bookkeeping is then covered by cases that were never written about vectors
/// at all, which is exactly where a divergence between two implementations
/// hides.
pub async fn ingest_batch(
    store: &(impl GraphStoreV1 + ?Sized),
    ctx: &StoreCtx<'_>,
    request: IngestRequest,
) -> Result<graph_storage_sdk::models::IngestOutcome, GraphStoreError> {
    let plan = plan_for(store, ctx, &request).await;
    store.ingest(ctx, request, plan).await
}

/// The provider every conformance run uses: deterministic, so a document's
/// own text retrieves it at distance zero on either implementation.
pub fn provider() -> std::sync::Arc<dyn graph_storage_sdk::plugin_api::EmbeddingProviderV1> {
    std::sync::Arc::new(FakeEmbeddingProvider::new(DIMENSION))
}

pub fn coordinator() -> EmbeddingCoordinator {
    EmbeddingCoordinator::new(provider(), SpaceState::Active { epoch: EPOCH }, 8 * 1024)
}

/// Vector width the suite runs under. Not a free choice: the built-in store's
/// column is `VECTOR(n)` for the width the schema was migrated with, and it
/// refuses anything else. The fake accepts any width, so a suite that picked
/// its own would pass there and fail on the first real server.
pub const DIMENSION: u32 = graph_storage::infra::store::ingest::migrated_embedding_dimension();

/// The epoch the suite writes and reads under. Arbitrary but non-default on
/// purpose: a store that ignored the plan and stamped, say, 1 would still
/// satisfy a suite that used 1.
pub const EPOCH: i64 = 42;

async fn plan_for(
    store: &(impl GraphStoreV1 + ?Sized),
    ctx: &StoreCtx<'_>,
    request: &IngestRequest,
) -> EmbeddingPlan {
    // The type records, resolved as the domain service resolves them, and read
    // through the same shared function: two copies of this is how the
    // service's own wiring came to be covered by nothing.
    let mut records: std::collections::BTreeMap<String, graph_storage_sdk::models::TypeRecord> =
        std::collections::BTreeMap::new();
    for node in &request.nodes {
        if !records.contains_key(&node.type_id)
            && let Ok(record) = store.get_type(ctx, &node.type_id).await
        {
            records.insert(node.type_id.clone(), record);
        }
    }
    plan_with(store, ctx, request, &coordinator()).await
}

/// `plan_for` with a caller-supplied coordinator, so a case can watch what its
/// provider is asked to embed.
async fn plan_with(
    store: &(impl GraphStoreV1 + ?Sized),
    ctx: &StoreCtx<'_>,
    request: &IngestRequest,
    coordinator: &EmbeddingCoordinator,
) -> EmbeddingPlan {
    let mut records: std::collections::BTreeMap<String, graph_storage_sdk::models::TypeRecord> =
        std::collections::BTreeMap::new();
    for node in &request.nodes {
        if !records.contains_key(&node.type_id)
            && let Ok(record) = store.get_type(ctx, &node.type_id).await
        {
            records.insert(node.type_id.clone(), record);
        }
    }
    // What the store already holds, read as the domain service reads it, so
    // the skip decision is exercised against both implementations.
    let keys: Vec<String> = request.nodes.iter().map(|n| n.node_key.clone()).collect();
    let current = store
        .embedding_state(ctx, &keys)
        .await
        .expect("embedding state is readable");
    let nodes = coordinator
        .plan(
            &request.nodes,
            request.options.embed.unwrap_or(true),
            |node| graph_storage::domain::embedding::declared_paths(&records, node),
            &current,
            RemainingBudget::starting_now(Duration::from_secs(30)),
            CancellationToken::new(),
        )
        .await
        .expect("the deterministic provider always embeds");
    EmbeddingPlan {
        epoch: Some(EPOCH),
        nodes,
    }
}

/// The deterministic provider, counting what it is asked to embed.
pub struct CountingProvider {
    inner: FakeEmbeddingProvider,
    calls: std::sync::Mutex<Vec<Vec<String>>>,
}

impl CountingProvider {
    pub fn new() -> Self {
        Self {
            inner: FakeEmbeddingProvider::new(DIMENSION),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Every batch of inputs the provider was handed, in order.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

impl Default for CountingProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl graph_storage_sdk::plugin_api::EmbeddingProviderV1 for CountingProvider {
    fn embedding_space(&self) -> &graph_storage_sdk::models::EmbeddingSpaceId {
        self.inner.embedding_space()
    }

    fn dimension(&self) -> u32 {
        self.inner.dimension()
    }

    async fn embed(
        &self,
        req: graph_storage_sdk::plugin_api::EmbedRequest,
    ) -> Result<
        graph_storage_sdk::plugin_api::EmbedResponse,
        graph_storage_sdk::plugin_api::EmbeddingProviderError,
    > {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(req.inputs.clone());
        }
        self.inner.embed(req).await
    }

    async fn health(&self) -> Result<(), graph_storage_sdk::plugin_api::EmbeddingProviderError> {
        self.inner.health().await
    }
}

/// D-027: a re-ingest embeds only what changed. The first batch embeds every
/// node; an identical second batch reaches the provider with nothing; a third
/// batch that changes one node's text embeds that node alone — and the
/// untouched node still ranks, because the store preserved its vector.
///
/// Asserted through the store's own `embedding_state`, so a store that
/// reported the wrong hash or epoch would show up here as extra provider
/// calls rather than as a silently slower sync.
pub async fn an_unchanged_re_ingest_embeds_nothing(store: &impl GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");
    let provider = std::sync::Arc::new(CountingProvider::new());
    let coordinator = EmbeddingCoordinator::new(
        std::sync::Arc::clone(&provider)
            as std::sync::Arc<dyn graph_storage_sdk::plugin_api::EmbeddingProviderV1>,
        SpaceState::Active { epoch: EPOCH },
        8 * 1024,
    );
    let run = |nodes: Vec<NodeSpec>| async {
        let request = batch(nodes, Vec::new());
        let plan = plan_with(store, &ctx, &request, &coordinator).await;
        store
            .ingest(&ctx, request, plan)
            .await
            .expect("the batch commits")
    };

    let first = run(vec![
        summarized("same", "Same", "text that stays"),
        summarized("moves", "Moves", "text before the change"),
    ])
    .await;
    assert_eq!(first.counts.nodes_inserted, 2);
    assert_eq!(provider.calls().len(), 1, "one call for the first batch");
    assert_eq!(provider.calls()[0].len(), 2, "both nodes embedded");

    let second = run(vec![
        summarized("same", "Same", "text that stays"),
        summarized("moves", "Moves", "text before the change"),
    ])
    .await;
    assert_eq!(
        second.counts.nodes_unchanged, 2,
        "an identical batch converges"
    );
    assert_eq!(
        provider.calls().len(),
        1,
        "an unchanged batch must not reach the provider: {:?}",
        provider.calls()
    );

    let third = run(vec![
        summarized("same", "Same", "text that stays"),
        summarized("moves", "Moves", "text after the change"),
    ])
    .await;
    assert_eq!(third.counts.nodes_updated, 1);
    assert_eq!(third.counts.nodes_unchanged, 1);
    let calls = provider.calls();
    assert_eq!(calls.len(), 2, "only the changed node embeds: {calls:?}");
    assert_eq!(calls[1].len(), 1, "one input, not the batch: {calls:?}");
    assert!(
        calls[1][0].contains("text after the change"),
        "the changed text is what embeds: {calls:?}"
    );

    // The preserved vector still ranks, and the new one describes the new text.
    let hits = search_vector(store, &ctx, "Same text that stays", EPOCH).await;
    assert_eq!(hits.first().map(String::as_str), Some("same"), "{hits:?}");
    let hits = search_vector(store, &ctx, "Moves text after the change", EPOCH).await;
    assert_eq!(hits.first().map(String::as_str), Some("moves"), "{hits:?}");
}

/// The registration batch every case starts from: the base ontology plus one
/// producer node type and one producer edge type.
pub fn ontology_batch() -> Vec<TypeRegistration> {
    let mut batch: Vec<TypeRegistration> = graph_storage::domain::ontology::BASE_SCHEMAS
        .iter()
        .map(|(type_id, raw)| TypeRegistration {
            type_id: (*type_id).to_owned(),
            schema: serde_json::from_str(raw).expect("base schema parses"),
        })
        .collect();

    batch.push(TypeRegistration {
        type_id: OWNED.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{OWNED}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": {
                "full_text_search": ["/name"],
                "vector_search": ["/payload/summary"]
            },
            "type": "object",
            "allOf": [
                { "$ref": "gts://gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~" }
            ]
        }),
    });
    batch.push(TypeRegistration {
        type_id: LINK.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{LINK}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [
                { "$ref": "gts://gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~" }
            ]
        }),
    });
    batch
}

pub fn node(key: &str, name: &str) -> NodeSpec {
    NodeSpec {
        node_key: key.to_owned(),
        type_id: OWNED.to_owned(),
        name: Some(name.to_owned()),
        ..NodeSpec::default()
    }
}

pub fn edge(src: &str, dst: &str) -> EdgeSpec {
    EdgeSpec {
        type_id: LINK.to_owned(),
        src_node_key: src.to_owned(),
        dst_node_key: dst.to_owned(),
        ..EdgeSpec::default()
    }
}

pub fn batch_of(nodes: Vec<NodeSpec>, edges: Vec<EdgeSpec>) -> IngestRequest {
    batch(nodes, edges)
}

pub fn batch(nodes: Vec<NodeSpec>, edges: Vec<EdgeSpec>) -> IngestRequest {
    IngestRequest {
        nodes,
        edges,
        options: IngestOptions::default(),
        replace_scope: None,
        idempotency_key: None,
    }
}

/// The subject every obligation writes as, unless it deliberately writes as
/// someone else. Fixed rather than random so an envelope assertion can name
/// the value it expects.
pub const WRITER: Uuid = uuid::uuid!("11111111-1111-1111-1111-111111111111");

/// The subject type that subject carries, so the optional half of the pair is
/// exercised rather than left `None` on every path.
pub const WRITER_TYPE: &str = "gts.cf.core.security.subject_user.v1~";

#[must_use]
pub fn writer() -> Subject {
    Subject {
        subject_id: WRITER,
        subject_type: Some(WRITER_TYPE.to_owned()),
    }
}

/// Build a per-call context. Tests own the scope explicitly so an assertion
/// about isolation is an assertion about the store, not about a PDP.
pub fn ctx<'a>(
    tenant: Uuid,
    scope: &'a AccessScope,
    snapshot: Option<&'a ReadSnapshot>,
) -> StoreCtx<'a> {
    ctx_as(tenant, scope, snapshot, writer())
}

/// The same, writing as a named subject -- what an envelope obligation needs
/// to tell one writer's mark from another's.
pub fn ctx_as<'a>(
    tenant: Uuid,
    scope: &'a AccessScope,
    snapshot: Option<&'a ReadSnapshot>,
    subject: Subject,
) -> StoreCtx<'a> {
    StoreCtx {
        tenant,
        scope,
        subject,
        snapshot,
        budget: RemainingBudget::starting_now(Duration::from_secs(30)),
        cancel: CancellationToken::new(),
    }
}

// ---------------------------------------------------------------------------
// The obligations
// ---------------------------------------------------------------------------

/// Obligation 1. A batch that fails partway leaves no node, no edge and no
/// idempotency record — the failure is injected by an edge naming a type that
/// is not registered, *after* several valid nodes.
pub async fn batch_atomicity(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);

    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    let before = store.revision(&ctx).await.expect("revision reads");

    let mut doomed = batch(
        vec![node("atomic-1", "one"), node("atomic-2", "two")],
        vec![edge("atomic-1", "atomic-2")],
    );
    "gts.test.unregistered._.nope.v1~".clone_into(&mut doomed.edges[0].type_id);
    doomed.idempotency_key = Some("atomic-key".to_owned());

    let error = ingest_batch(store, &ctx, doomed)
        .await
        .expect_err("an unregistered edge type must fail the batch");
    assert!(
        matches!(error, GraphStoreError::Validation { .. }),
        "expected a validation failure, got {error}"
    );

    // Nothing from the batch survived — not the nodes that were valid, and
    // not the receipt, which would otherwise make the retry a replay of a
    // batch that never committed.
    for key in ["atomic-1", "atomic-2"] {
        let found = store.get_node(&ctx, &key.to_owned(), 10).await;
        assert!(
            matches!(found, Err(GraphStoreError::NotFound)),
            "`{key}` must not exist after a failed batch"
        );
    }
    let after = store.revision(&ctx).await.expect("revision reads");
    assert_eq!(before, after, "a failed batch must not move the revision");

    let retry = {
        let mut request = batch(vec![node("atomic-1", "one")], Vec::new());
        request.idempotency_key = Some("atomic-key".to_owned());
        request
    };
    let outcome = ingest_batch(store, &ctx, retry)
        .await
        .expect("the retry commits");
    assert!(
        !outcome.replayed,
        "the failed batch must not have left a receipt to replay"
    );
}

/// Obligation 3. An older source generation is rejected; an equal generation
/// with different content conflicts; an equal generation with identical
/// content is a replay.
pub async fn generation_fencing(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    let replace = |generation: i64, name: &str| IngestRequest {
        nodes: vec![node("fenced", name)],
        replace_scope: Some(ReplaceScope {
            attribute: "repository".to_owned(),
            value: "acme/thing".to_owned(),
            generation,
        }),
        ..IngestRequest::default()
    };

    ingest_batch(store, &ctx, replace(5, "at five"))
        .await
        .expect("generation 5 commits");

    let stale = ingest_batch(store, &ctx, replace(4, "at four"))
        .await
        .expect_err("an older generation must be refused");
    assert!(
        matches!(
            stale,
            GraphStoreError::StaleGeneration {
                recorded: 5,
                offered: 4
            }
        ),
        "expected stale-generation fencing, got {stale}"
    );

    let divergent = ingest_batch(store, &ctx, replace(5, "different at five"))
        .await
        .expect_err("an equal generation with different content must conflict");
    assert!(
        matches!(divergent, GraphStoreError::Conflict { .. }),
        "expected a conflict, got {divergent}"
    );

    ingest_batch(store, &ctx, replace(6, "at six"))
        .await
        .expect("a newer generation commits");
}

/// Obligation 4. Deleting a node tombstones its incident edges in the same
/// transaction — a node never disappears while an edge still points at it.
pub async fn no_orphan_edges(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    ingest_batch(
        store,
        &ctx,
        batch(
            vec![node("orphan-a", "a"), node("orphan-b", "b")],
            vec![edge("orphan-a", "orphan-b")],
        ),
    )
    .await
    .expect("the batch commits");

    let outcome = store
        .soft_delete(&ctx, DeleteRequest::Node("orphan-a".to_owned()))
        .await
        .expect("the delete succeeds");
    assert_eq!(outcome.tombstoned_nodes, 1);
    assert_eq!(
        outcome.tombstoned_edges, 1,
        "the incident edge must be tombstoned with its node"
    );

    // The surviving endpoint no longer reports the edge.
    let survivor = store
        .get_node(&ctx, &"orphan-b".to_owned(), 10)
        .await
        .expect("the other endpoint still exists");
    assert!(
        survivor.adjacency.is_empty(),
        "a tombstoned edge must not appear in adjacency: {:?}",
        survivor.adjacency
    );
}

/// The revision a fresh tenant reports agrees with the one its first write
/// records: an epoch of zero on the read side would make every receipt read as
/// belonging to a previous epoch, and therefore expired, from the first retry.
pub async fn a_fresh_tenant_reports_a_usable_revision(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);

    let before = store.revision(&ctx).await.expect("revision reads");
    assert_eq!(before.revision, 0, "a fresh tenant has committed nothing");
    assert!(
        before.source_epoch > 0,
        "the epoch must be a real timeline identifier, not a default zero"
    );

    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");
    let outcome = ingest_batch(store, &ctx, batch(vec![node("first", "first")], Vec::new()))
        .await
        .expect("the batch commits");
    assert_eq!(
        outcome.revision.source_epoch, before.source_epoch,
        "the write path and the read path must agree on the epoch"
    );
    assert_eq!(outcome.revision.revision, before.revision + 1);
}

/// Idempotency: a recorded key replays without touching state, and the same
/// key with different content is refused rather than silently re-executed.
pub async fn idempotency(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    let request = || {
        let mut request = batch(vec![node("idem-1", "one")], Vec::new());
        request.idempotency_key = Some("idem-key".to_owned());
        request
    };

    let first = ingest_batch(store, &ctx, request())
        .await
        .expect("first commits");
    assert!(!first.replayed);
    assert_eq!(first.counts.nodes_inserted, 1);

    let second = ingest_batch(store, &ctx, request())
        .await
        .expect("retry replays");
    assert!(second.replayed, "a recorded key must replay");
    assert_eq!(
        second.revision, first.revision,
        "a replay reports the revision the original committed"
    );

    let mut divergent = batch(vec![node("idem-1", "different")], Vec::new());
    divergent.idempotency_key = Some("idem-key".to_owned());
    let error = ingest_batch(store, &ctx, divergent)
        .await
        .expect_err("the same key with different content must be refused");
    assert!(
        matches!(error, GraphStoreError::IdempotencyMismatch),
        "expected an idempotency mismatch, got {error}"
    );
}

/// Convergence: re-ingesting an identical batch changes nothing, and the
/// revision advances **if and only if** stored state actually changed.
pub async fn convergent_replay(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    let request = || {
        batch(
            vec![node("conv-a", "a"), node("conv-b", "b")],
            vec![edge("conv-a", "conv-b")],
        )
    };

    let first = ingest_batch(store, &ctx, request())
        .await
        .expect("first commits");
    assert_eq!(first.counts.nodes_inserted, 2);
    assert_eq!(first.counts.edges_inserted, 1);

    let second = ingest_batch(store, &ctx, request())
        .await
        .expect("second commits");
    assert_eq!(second.counts.nodes_unchanged, 2, "nothing changed");
    assert_eq!(second.counts.edges_unchanged, 1);
    assert_eq!(
        second.revision, first.revision,
        "a convergent replay must not move the revision"
    );
}

/// An edge type constrains what its endpoints may be, and the constraint is
/// enforced where DESIGN says it is: inside the ingest transaction.
///
/// `fr-type-constraints` and PRD § 9 both name this rejection. The constraint
/// is a GTS *pattern*, resolved by the platform matcher — a base identifier
/// admits every type derived from it, which is why the default
/// (`…node.v1~`) constrains nothing, and a family identifier admits only its
/// own descendants, which is what gives the check teeth.
pub async fn endpoint_constraints_are_enforced(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);

    let mut batch = ontology_batch();
    batch.push(TypeRegistration {
        type_id: OWNED_ONLY.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{OWNED_ONLY}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": { "src_types": [OWNED_FAMILY], "dst_types": [OWNED_FAMILY] },
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~" }]
        }),
    });
    batch.push(TypeRegistration {
        type_id: REFERENCE.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{REFERENCE}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.cf.core.graph.node.v1~cf.core.graph.reference_node.v1~" }]
        }),
    });
    store
        .register_types(&ctx, batch)
        .await
        .expect("ontology registers");

    let mirror = NodeSpec {
        node_key: "sys:repo:42".to_owned(),
        type_id: REFERENCE.to_owned(),
        payload: Some(serde_json::json!({
            "source": { "system": "sys", "kind": "repo", "native_id": "42" }
        })),
        ..NodeSpec::default()
    };
    ingest_batch(
        store,
        &ctx,
        batch_of(vec![node("owned-1", "one"), mirror], Vec::new()),
    )
    .await
    .expect("both nodes commit");

    // Owned -> owned is admitted.
    ingest_batch(
        store,
        &ctx,
        batch_of(
            Vec::new(),
            vec![EdgeSpec {
                type_id: OWNED_ONLY.to_owned(),
                src_node_key: "owned-1".to_owned(),
                dst_node_key: "owned-1".to_owned(),
                ..EdgeSpec::default()
            }],
        ),
    )
    .await
    .expect("an edge between admitted endpoints commits");

    // Owned -> reference is not.
    let error = ingest_batch(
        store,
        &ctx,
        batch_of(
            Vec::new(),
            vec![EdgeSpec {
                type_id: OWNED_ONLY.to_owned(),
                src_node_key: "owned-1".to_owned(),
                dst_node_key: "sys:repo:42".to_owned(),
                ..EdgeSpec::default()
            }],
        ),
    )
    .await
    .expect_err("an endpoint the edge type does not admit must be refused");

    let GraphStoreError::Validation { items } = error else {
        panic!("expected a per-item validation failure, got {error}");
    };
    let item = items.first().expect("one item error");
    assert_eq!(item.pointer.as_deref(), Some("/dst_node_key"));
    assert!(
        item.message.contains("does not admit"),
        "the error names what was refused: {}",
        item.message
    );
}

/// A phantom endpoint is admitted at edge time and checked when it becomes
/// concrete — the Phantom Materialization Contract, rule 3.
///
/// An edge may name a node the producer has not sent yet; the store stands a
/// phantom in its place. A phantom has no concrete type, so the endpoint
/// constraint cannot be evaluated then — which is exactly why materialization
/// must evaluate it, or the constraint would be trivially evadable by sending
/// the edge first.
pub async fn materializing_a_phantom_revalidates_its_edges(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);

    let mut batch = ontology_batch();
    batch.push(TypeRegistration {
        type_id: OWNED_ONLY.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{OWNED_ONLY}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": { "src_types": [OWNED_FAMILY], "dst_types": [OWNED_FAMILY] },
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~" }]
        }),
    });
    batch.push(TypeRegistration {
        type_id: REFERENCE.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{REFERENCE}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.cf.core.graph.reference_node.v1~" }]
        }),
    });
    store
        .register_types(&ctx, batch)
        .await
        .expect("ontology registers");

    // The destination does not exist yet: a phantom stands in, and the edge
    // commits because a phantom carries no type to check.
    let outcome = ingest_batch(
        store,
        &ctx,
        batch_of(
            vec![node("owned-1", "one")],
            vec![EdgeSpec {
                type_id: OWNED_ONLY.to_owned(),
                src_node_key: "owned-1".to_owned(),
                dst_node_key: "sys:repo:9".to_owned(),
                ..EdgeSpec::default()
            }],
        ),
    )
    .await
    .expect("an edge to an absent node stands up a phantom");
    assert_eq!(
        outcome.counts.phantoms_created, 1,
        "the absent endpoint became a phantom"
    );

    // Materializing it as a type the edge does not admit is refused: the check
    // deferred at edge time comes due here.
    let error = ingest_batch(
        store,
        &ctx,
        batch_of(
            vec![NodeSpec {
                node_key: "sys:repo:9".to_owned(),
                type_id: REFERENCE.to_owned(),
                payload: Some(serde_json::json!({
                    "source": { "system": "sys", "kind": "repo", "native_id": "9" }
                })),
                ..NodeSpec::default()
            }],
            Vec::new(),
        ),
    )
    .await
    .expect_err("materialization must revalidate the edges the phantom accumulated");
    let GraphStoreError::Validation { items } = error else {
        panic!("expected a per-item validation failure, got {error}");
    };
    let item = items.first().expect("one item error");
    assert_eq!(item.family, ItemFamily::Node);
    // The reference-node identity rule would refuse a wrong key here too, and
    // it is a node-family violation just the same. Name the edge, or this case
    // passes without the revalidation ever running — the key above satisfies
    // the identity rule precisely so that it cannot.
    assert!(
        item.message.contains("would leave edge"),
        "the refusal must come from the incident edge: {}",
        item.message
    );

    // Materializing it as an admitted type is accepted.
    let outcome = ingest_batch(
        store,
        &ctx,
        batch_of(vec![node("sys:repo:9", "late")], Vec::new()),
    )
    .await
    .expect("an admitted concrete type materializes the phantom");
    assert_eq!(
        outcome.counts.phantoms_materialized, 1,
        "the phantom became concrete"
    );
}

/// Tenancy: two tenants owning the same node key see only their own row, and
/// neither can reach the other's through any read path.
pub async fn tenant_isolation(store: &dyn GraphStoreV1, one: Uuid, two: Uuid) {
    let scope_one = AccessScope::for_tenant(one);
    let scope_two = AccessScope::for_tenant(two);

    for (tenant, scope, name) in [
        (one, &scope_one, "tenant one"),
        (two, &scope_two, "tenant two"),
    ] {
        let ctx = self::ctx(tenant, scope, None);
        store
            .register_types(&ctx, ontology_batch())
            .await
            .expect("ontology registers");
        ingest_batch(
            store,
            &ctx,
            batch(vec![node("colliding-key", name)], Vec::new()),
        )
        .await
        .expect("the batch commits");
    }

    // The colliding key is the trap: a leak shows up as the *other* tenant's
    // name, which a "did I get a row back" assertion would not catch.
    let view_one = store
        .get_node(
            &self::ctx(one, &scope_one, None),
            &"colliding-key".to_owned(),
            10,
        )
        .await
        .expect("tenant one sees its node");
    assert_eq!(view_one.name.as_deref(), Some("tenant one"));

    let view_two = store
        .get_node(
            &self::ctx(two, &scope_two, None),
            &"colliding-key".to_owned(),
            10,
        )
        .await
        .expect("tenant two sees its node");
    assert_eq!(view_two.name.as_deref(), Some("tenant two"));

    // A projection under one tenant returns exactly one row for that key.
    let page = store
        .project_table(
            &self::ctx(one, &scope_one, None),
            ProjectionRequest::default(),
        )
        .await
        .expect("projection succeeds");
    let matching = page
        .items
        .iter()
        .filter(|row| row.node_key == "colliding-key")
        .count();
    assert_eq!(matching, 1, "one tenant, one row: {:?}", page.items);
}

/// Tombstone visibility: a deleted node is absent from every read path, and
/// its key cannot be reused before a purge.
pub async fn tombstones_are_invisible(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");
    ingest_batch(store, &ctx, batch(vec![node("gone", "here")], Vec::new()))
        .await
        .expect("the batch commits");
    store
        .soft_delete(&ctx, DeleteRequest::Node("gone".to_owned()))
        .await
        .expect("the delete succeeds");

    assert!(
        matches!(
            store.get_node(&ctx, &"gone".to_owned(), 10).await,
            Err(GraphStoreError::NotFound)
        ),
        "a tombstoned node must read as absent"
    );
    assert!(
        store
            .resolve_node_ids(&ctx, &["gone".to_owned()])
            .await
            .expect("resolution succeeds")
            .is_empty(),
        "a tombstoned node must not resolve"
    );
    let page = store
        .project_table(&ctx, ProjectionRequest::default())
        .await
        .expect("projection succeeds");
    assert!(
        !page.items.iter().any(|row| row.node_key == "gone"),
        "a tombstoned node must not appear in a projection"
    );

    let error = ingest_batch(store, &ctx, batch(vec![node("gone", "back")], Vec::new()))
        .await
        .expect_err("a tombstoned key is not reusable before purge");
    assert!(
        matches!(error, GraphStoreError::Conflict { .. }),
        "expected a conflict, got {error}"
    );
}

/// Anti-enumeration: an unauthorized read is indistinguishable from a
/// nonexistent one — both answer `NotFound`, never `PermissionDenied`.
pub async fn denied_is_indistinguishable_from_absent(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");
    ingest_batch(
        store,
        &ctx,
        batch(vec![node("private", "secret")], Vec::new()),
    )
    .await
    .expect("the batch commits");

    let denied = AccessScope::deny_all();
    let denied_ctx = self::ctx(tenant, &denied, None);

    let existing = store.get_node(&denied_ctx, &"private".to_owned(), 10).await;
    let absent = store
        .get_node(&denied_ctx, &"never-existed".to_owned(), 10)
        .await;
    assert!(
        matches!(existing, Err(GraphStoreError::NotFound))
            && matches!(absent, Err(GraphStoreError::NotFound)),
        "a denied row and an absent row must answer identically"
    );
}

/// Search scoping: the arms apply the scope inside the statement, so a denied
/// caller ranks nothing rather than ranking and then filtering.
pub async fn search_is_scoped(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");
    ingest_batch(
        store,
        &ctx,
        batch(vec![node("searchable", "findable thing")], Vec::new()),
    )
    .await
    .expect("the batch commits");

    let request = || SearchRequest {
        mode: SearchMode::Lexical,
        query: Some("findable".to_owned()),
        arm_limit: 10,
        limit: 10,
        type_patterns: Vec::new(),
    };

    let hits = store
        .search(&ctx, request(), None)
        .await
        .expect("search succeeds");
    assert!(
        hits.hits.iter().any(|hit| hit.node_key == "searchable"),
        "the node must be findable by its own name: {:?}",
        hits.hits
    );

    let denied = AccessScope::deny_all();
    let denied_hits = store
        .search(&self::ctx(tenant, &denied, None), request(), None)
        .await
        .expect("search succeeds under a denying scope");
    assert!(
        denied_hits.hits.is_empty(),
        "a denying scope must rank nothing: {:?}",
        denied_hits.hits
    );
}

// ---------------------------------------------------------------------------
// Vector search
// ---------------------------------------------------------------------------

/// Query the vector arm the way the domain service does: embed the text with
/// the same provider ingest used, and rank only the active epoch.
async fn search_vector(
    store: &(impl GraphStoreV1 + ?Sized),
    ctx: &StoreCtx<'_>,
    text: &str,
    epoch: i64,
) -> Vec<String> {
    let query_vector = coordinator()
        .embed_query(
            text,
            RemainingBudget::starting_now(Duration::from_secs(30)),
            CancellationToken::new(),
        )
        .await
        .expect("the deterministic provider always embeds");
    store
        .search(
            ctx,
            SearchRequest {
                mode: SearchMode::Vector,
                query: Some(text.to_owned()),
                arm_limit: 10,
                limit: 10,
                type_patterns: Vec::new(),
            },
            Some(graph_storage_sdk::plugin_api::VectorArm {
                query_vector,
                epoch,
            }),
        )
        .await
        .expect("search succeeds")
        .hits
        .into_iter()
        .map(|hit| hit.node_key)
        .collect()
}

fn summarized(key: &str, name: &str, summary: &str) -> NodeSpec {
    NodeSpec {
        node_key: key.to_owned(),
        type_id: OWNED.to_owned(),
        name: Some(name.to_owned()),
        payload: Some(serde_json::json!({ "summary": summary })),
        ..NodeSpec::default()
    }
}

/// ADR-0005's own acceptance test: "a document ingested and then queried with
/// its own text ranks first in the vector arm".
pub async fn a_document_is_retrieved_by_its_own_text(store: &impl GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    ingest_batch(
        store,
        &ctx,
        batch(
            vec![
                summarized("doc-1", "Hardcoded credential", "in the deploy script"),
                summarized("doc-2", "Unrelated", "something else entirely"),
            ],
            Vec::new(),
        ),
    )
    .await
    .expect("the batch commits");

    // The text the node itself composes: its name plus the payload path its
    // type declares vectorizable.
    let hits = search_vector(
        store,
        &ctx,
        "Hardcoded credential in the deploy script",
        EPOCH,
    )
    .await;
    assert_eq!(
        hits.first().map(String::as_str),
        Some("doc-1"),
        "a document must rank first for its own text: {hits:?}"
    );
}

/// The `vector_search` trait is what decides the input, so a value at a
/// declared path must reach the vector. If it did not, the two documents below
/// would embed identically and the query could not tell them apart.
pub async fn a_declared_path_reaches_the_vector(store: &impl GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    ingest_batch(
        store,
        &ctx,
        batch(
            vec![
                summarized("same-1", "Same name", "first summary"),
                summarized("same-2", "Same name", "second summary"),
            ],
            Vec::new(),
        ),
    )
    .await
    .expect("the batch commits");

    let hits = search_vector(store, &ctx, "Same name second summary", EPOCH).await;
    assert_eq!(
        hits.first().map(String::as_str),
        Some("same-2"),
        "two nodes sharing a name are told apart only by the declared path: {hits:?}"
    );
}

/// `embed = false` with unchanged content preserves the vector: a
/// metadata-only re-sync must not cost a re-embedding pass, and must not empty
/// the vector arm either.
pub async fn a_skipped_re_ingest_preserves_the_vector(store: &impl GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    let spec = || summarized("keep", "Kept", "unchanged text");
    ingest_batch(store, &ctx, batch(vec![spec()], Vec::new()))
        .await
        .expect("the first batch commits");

    let mut skipped = batch(vec![spec()], Vec::new());
    skipped.options.embed = Some(false);
    ingest_batch(store, &ctx, skipped)
        .await
        .expect("the skipped batch commits");

    let hits = search_vector(store, &ctx, "Kept unchanged text", EPOCH).await;
    assert_eq!(
        hits.first().map(String::as_str),
        Some("keep"),
        "a skipped re-ingest must keep the vector, not clear it: {hits:?}"
    );
}

/// `embed = false` with *changed* content leaves the vector stale: it stays
/// stored, so re-embedding can replace it, but it stops ranking. "A stored
/// vector can never rank content that is no longer stored."
pub async fn a_stale_vector_stops_ranking_but_the_node_stays(
    store: &impl GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    ingest_batch(
        store,
        &ctx,
        batch(
            vec![summarized("drift", "Drifting", "the original text")],
            Vec::new(),
        ),
    )
    .await
    .expect("the first batch commits");
    assert_eq!(
        search_vector(store, &ctx, "Drifting the original text", EPOCH)
            .await
            .first()
            .map(String::as_str),
        Some("drift"),
        "precondition: the node ranks for its own text before it drifts"
    );

    let mut changed = batch(
        vec![summarized("drift", "Drifting", "an entirely new text")],
        Vec::new(),
    );
    changed.options.embed = Some(false);
    ingest_batch(store, &ctx, changed)
        .await
        .expect("the skipped batch commits");

    let hits = search_vector(store, &ctx, "Drifting the original text", EPOCH).await;
    assert!(
        !hits.iter().any(|key| key == "drift"),
        "a vector describing text the node no longer carries must not rank: {hits:?}"
    );

    // Only the vector arm loses it. The node is present as ever.
    let view = store
        .get_node(&ctx, &"drift".to_owned(), 10)
        .await
        .expect("the node is still readable");
    assert_eq!(view.node_key, "drift");
    assert!(
        view.has_embedding,
        "the vector is kept for re-embedding, only barred from ranking"
    );
}

/// Vectors of another epoch were produced by another model. They must not be
/// ranked against this query, however similar the numbers look.
pub async fn only_the_active_epoch_ranks(store: &impl GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    ingest_batch(
        store,
        &ctx,
        batch(
            vec![summarized("epochal", "Epochal", "written under one epoch")],
            Vec::new(),
        ),
    )
    .await
    .expect("the batch commits");

    let text = "Epochal written under one epoch";
    assert_eq!(
        search_vector(store, &ctx, text, EPOCH)
            .await
            .first()
            .map(String::as_str),
        Some("epochal"),
        "precondition: the node ranks under the epoch it was written with"
    );
    assert!(
        search_vector(store, &ctx, text, EPOCH + 1).await.is_empty(),
        "a vector of another epoch must not rank"
    );
}

/// `fr-audit-envelope`: the envelope records the subject that performed each
/// verb, not merely the subject that created the element.
///
/// Written against two different writers on purpose. A store that stamps the
/// caller on creation but forgets it on update passes every single-writer
/// assertion, and the question the envelope exists to answer -- *who touched
/// this last* -- is exactly the one it then gets wrong.
pub async fn the_envelope_records_the_subject_of_each_verb(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let author = ctx(tenant, &scope, None);
    let editor_subject = editor();
    let editor = ctx_as(tenant, &scope, None, editor_subject.clone());

    seed_one_audited_node(store, &author).await;

    let created = envelope_of(store, &author).await;
    assert_eq!(created.key, "audited", "the envelope keys the element");
    assert_eq!(created.tenant_id, tenant, "the envelope carries the tenant");
    assert_eq!(created.created_by, writer(), "the creator is recorded");
    assert_eq!(
        created.updated_by,
        writer(),
        "a fresh element's last writer is its creator"
    );
    assert!(
        created.deleted_at.is_none() && created.deleted_by.is_none(),
        "a live element carries no tombstone"
    );

    // A second subject rewrites it. Creation must not move; the update must.
    ingest_batch(
        store,
        &editor,
        batch(vec![node("audited", "second")], Vec::new()),
    )
    .await
    .expect("the second batch commits");

    let updated = envelope_of(store, &author).await;
    assert_eq!(
        updated.created_by,
        writer(),
        "an update must not rewrite who created the element"
    );
    assert_eq!(
        updated.created_at, created.created_at,
        "an update must not move the creation time"
    );
    assert_eq!(
        updated.updated_by, editor_subject,
        "the last writer is the subject that performed the update"
    );
}

/// `fr-audit-envelope` on the projection, where it is also the only carrier of
/// the observed revision: the page wrapper is the platform's
/// `toolkit_odata::Page`, which has no member for one.
pub async fn a_projection_row_carries_the_envelope(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let author = ctx(tenant, &scope, None);
    let editor_subject = editor();
    let editor = ctx_as(tenant, &scope, None, editor_subject.clone());

    seed_one_audited_node(store, &author).await;
    ingest_batch(
        store,
        &editor,
        batch(vec![node("audited", "second")], Vec::new()),
    )
    .await
    .expect("the second batch commits");

    let page = store
        .project_table(&author, ProjectionRequest::default())
        .await
        .expect("projection succeeds");
    let row = page
        .items
        .iter()
        .find(|row| row.node_key == "audited")
        .expect("the node is projected");
    assert_eq!(
        row.envelope.updated_by, editor_subject,
        "the projection reports the same last writer as the node read"
    );
    assert!(
        row.envelope.graph_revision.revision > 0,
        "the projection reports its observed revision on the element"
    );
}

/// The subject an envelope obligation writes as when it needs a *second*
/// writer. Carries no subject type, which is the case the optional half of the
/// pair exists for: an automation is not a user.
fn editor() -> Subject {
    Subject {
        subject_id: uuid::uuid!("22222222-2222-2222-2222-222222222222"),
        subject_type: None,
    }
}

async fn seed_one_audited_node(store: &dyn GraphStoreV1, author: &StoreCtx<'_>) {
    store
        .register_types(author, ontology_batch())
        .await
        .expect("ontology registers");
    ingest_batch(
        store,
        author,
        batch(vec![node("audited", "first")], Vec::new()),
    )
    .await
    .expect("the batch commits");
}

async fn envelope_of(
    store: &dyn GraphStoreV1,
    ctx: &StoreCtx<'_>,
) -> graph_storage_sdk::models::ElementEnvelope {
    store
        .get_node(ctx, &"audited".to_owned(), 10)
        .await
        .expect("the node reads")
        .envelope
}
