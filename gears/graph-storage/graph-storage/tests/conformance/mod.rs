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
//! 2. single-writer serialization per scope identity — asserted by
//!    `two_replacements_of_one_scope_serialize`, which races two replacements
//!    of one scope on a multi-threaded runtime. It was listed here as *not*
//!    asserted for as long as it was unmet: writing the case found the fence
//!    was a read-decide-write and let the loser's lower generation win;
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
/// An analysis edge: a conclusion, which a re-import must never remove.
pub const ANALYSIS: &str =
    "gts.cf.core.graph.edge.v1~cf.core.graph.analysis_edge.v1~test.gs._.introduced_by.v1~";

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
            "allOf": [{
                "$ref": "gts://gts.cf.core.graph.node.v1~cf.core.graph.reference_node.v1~"
            }]
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

// ---------------------------------------------------------------------------
// Payload projection: the `index` trait reaching `$filter` / `$orderby`
// ---------------------------------------------------------------------------

/// A producer type declaring three payload paths -- a string, a number and a
/// nested integer -- as filterable and orderable.
pub const INDEXED: &str =
    "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~test.gs._.ticket.v1~";

fn indexed_type() -> TypeRegistration {
    TypeRegistration {
        type_id: INDEXED.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{INDEXED}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": {
                "index": ["/payload/severity", "/payload/score", "/payload/loc/line"],
                "full_text_search": ["/name"]
            },
            "type": "object",
            "allOf": [
                { "$ref": "gts://gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~" },
                { "type": "object", "properties": { "payload": {
                    "type": "object",
                    "properties": {
                        "severity": { "type": "string", "enum": ["low", "high"] },
                        "score": { "type": "number" },
                        "loc": { "type": "object", "properties": {
                            "line": { "type": "integer" } } },
                        "meta": { "type": "object" }
                    }
                } } }
            ]
        }),
    }
}

fn ticket(key: &str, severity: &str, score: Option<f64>, line: i64) -> NodeSpec {
    let mut payload = serde_json::json!({
        "severity": severity,
        "loc": { "line": line }
    });
    if let Some(score) = score {
        payload["score"] = serde_json::json!(score);
    }
    NodeSpec {
        node_key: key.to_owned(),
        type_id: INDEXED.to_owned(),
        name: Some(key.to_owned()),
        payload: Some(payload),
        ..NodeSpec::default()
    }
}

/// The five tickets every payload case starts from.
async fn seed_tickets(store: &dyn GraphStoreV1, ctx: &StoreCtx<'_>) {
    let mut batch_types = ontology_batch();
    batch_types.push(indexed_type());
    store
        .register_types(ctx, batch_types)
        .await
        .expect("ontology registers");
    ingest_batch(
        store,
        ctx,
        batch(
            vec![
                ticket("t1", "high", Some(9.5), 10),
                ticket("t2", "low", Some(3.0), 20),
                ticket("t3", "high", Some(1.0), 30),
                ticket("t4", "high", None, 40),
                ticket("t5", "low", Some(7.0), 50),
            ],
            Vec::new(),
        ),
    )
    .await
    .expect("the batch commits");
}

pub fn projection(
    types: &[&str],
    filter: &str,
    order: &[(&str, toolkit_odata::SortDir)],
) -> ProjectionRequest {
    let mut query = toolkit_odata::ODataQuery::new();
    if !filter.is_empty() {
        let parsed = toolkit_odata::parse_filter_string(filter).expect("filter parses");
        query = query.with_filter(parsed.into_expr());
    }
    query = query.with_order(toolkit_odata::ODataOrderBy(
        order
            .iter()
            .map(|(field, dir)| toolkit_odata::OrderKey {
                field: (*field).to_owned(),
                dir: *dir,
            })
            .collect(),
    ));
    ProjectionRequest {
        type_set: (!types.is_empty()).then(|| {
            graph_storage_sdk::models::TypeIdSet(types.iter().map(|t| (*t).to_owned()).collect())
        }),
        query,
    }
}

fn keys(page: &toolkit_odata::Page<graph_storage_sdk::models::NodeRow>) -> Vec<String> {
    page.items.iter().map(|row| row.node_key.clone()).collect()
}

/// A path the selected type declares in its `index` trait filters and orders
/// the projection, with the kind its schema gives it: strings compare as
/// text, numbers as numbers, a nested pointer reaches its leaf, and a row
/// missing the ordered attribute sorts last in either direction.
pub async fn a_declared_payload_path_filters_and_orders_the_projection(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    use toolkit_odata::SortDir;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_tickets(store, &ctx).await;

    let page = store
        .project_table(
            &ctx,
            projection(
                &[INDEXED],
                "payload/severity eq 'high'",
                &[("payload/score", SortDir::Desc)],
            ),
        )
        .await
        .expect("projection succeeds");
    assert_eq!(
        keys(&page),
        vec!["t1", "t3", "t4"],
        "high tickets by score descending, the scoreless one last"
    );

    let page = store
        .project_table(
            &ctx,
            projection(
                &[INDEXED],
                "payload/score gt 2",
                &[("payload/score", SortDir::Asc)],
            ),
        )
        .await
        .expect("projection succeeds");
    assert_eq!(
        keys(&page),
        vec!["t2", "t5", "t1"],
        "a numeric comparison, not a textual one ('9.5' < '3.0' as text)"
    );

    let page = store
        .project_table(
            &ctx,
            projection(
                &[INDEXED],
                "payload/loc/line ge 30 and payload/severity in ('low', 'high')",
                &[("payload/loc/line", SortDir::Desc)],
            ),
        )
        .await
        .expect("projection succeeds");
    assert_eq!(
        keys(&page),
        vec!["t5", "t4", "t3"],
        "a nested path, ordered"
    );

    let page = store
        .project_table(
            &ctx,
            projection(&[INDEXED], "", &[("payload/score", SortDir::Asc)]),
        )
        .await
        .expect("projection succeeds");
    assert_eq!(
        keys(&page),
        vec!["t3", "t2", "t5", "t1", "t4"],
        "ascending too puts the missing attribute last"
    );
}

/// A payload path nobody declared is refused with the alternatives named,
/// and a payload path without a type set is refused because there is nothing
/// to read the declarations from.
pub async fn an_undeclared_payload_path_is_refused_naming_the_alternatives(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_tickets(store, &ctx).await;

    let error = store
        .project_table(&ctx, projection(&[INDEXED], "payload/nope eq 'x'", &[]))
        .await
        .expect_err("an undeclared path is refused");
    let GraphStoreError::InvalidQuery { what } = &error else {
        panic!("expected InvalidQuery, got {error:?}");
    };
    assert!(what.contains("payload/nope"), "{what}");
    assert!(
        what.contains("payload/severity"),
        "names the alternatives: {what}"
    );

    let error = store
        .project_table(&ctx, projection(&[], "payload/severity eq 'high'", &[]))
        .await
        .expect_err("a payload path without a type set is refused");
    let GraphStoreError::InvalidQuery { what } = &error else {
        panic!("expected InvalidQuery, got {error:?}");
    };
    assert!(what.contains("type_pattern"), "{what}");

    let error = store
        .project_table(&ctx, projection(&[INDEXED], "payload/score eq 'high'", &[]))
        .await
        .expect_err("a literal of the wrong kind is refused");
    assert!(
        matches!(error, GraphStoreError::InvalidQuery { .. }),
        "{error:?}"
    );
}

/// An `index` path that lands on an object, or nowhere, fails registration:
/// there is no scalar to compare, so an index over it would serve no query.
pub async fn an_index_path_onto_a_non_scalar_is_refused_at_registration(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("ontology registers");

    let mut doomed = indexed_type();
    doomed.schema["x-gts-traits"]["index"] = serde_json::json!(["/payload/meta"]);
    let error = store
        .register_types(&ctx, vec![doomed])
        .await
        .expect_err("an object path is refused");
    let GraphStoreError::Validation { items } = &error else {
        panic!("expected Validation, got {error:?}");
    };
    assert!(
        items[0].message.contains("not a scalar"),
        "{}",
        items[0].message
    );
}

/// A domain hierarchy mirrored into the chain, on a store that admits it: the
/// intermediate types register, a pattern on the intermediate selects the
/// leaf, and an `index` declared on the intermediate admits a filter over the
/// leaf's rows.
pub async fn a_deeper_chain_registers_and_its_ancestor_admits_the_leaf(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);

    let managed = format!("{OWNED_FAMILY}test.dm.core.managed_object.v1~");
    let document = format!("{managed}test.dm.core.document.v1~");
    let requirement = format!("{document}test.dm.sdlc.requirement.v1~");

    let intermediate = |id: &str,
                        parent: &str,
                        abstract_: bool,
                        traits: serde_json::Value,
                        props: serde_json::Value| {
        let mut schema = serde_json::json!({
            "$id": format!("gts://{id}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": traits,
            "type": "object",
            "allOf": [
                { "$ref": format!("gts://{parent}") },
                { "type": "object", "properties": { "payload": {
                    "type": "object", "properties": props } } }
            ]
        });
        if abstract_ {
            schema["x-gts-abstract"] = serde_json::json!(true);
        }
        TypeRegistration {
            type_id: id.to_owned(),
            schema,
        }
    };

    let mut batch_types = ontology_batch();
    batch_types.push(intermediate(
        &managed,
        OWNED_FAMILY,
        true,
        serde_json::json!({ "index": ["/payload/status"], "full_text_search": ["/name"] }),
        serde_json::json!({ "status": { "type": "string" } }),
    ));
    batch_types.push(intermediate(
        &document,
        &managed,
        true,
        serde_json::json!({}),
        serde_json::json!({ "url": { "type": "string" } }),
    ));
    batch_types.push(intermediate(
        &requirement,
        &document,
        false,
        serde_json::json!({}),
        serde_json::json!({ "priority": { "type": "integer" } }),
    ));
    let records = store
        .register_types(&ctx, batch_types)
        .await
        .expect("a five-segment chain registers when the deployment admits it");
    let leaf = records
        .iter()
        .find(|r| r.type_id == requirement)
        .expect("the leaf is registered");
    assert_eq!(leaf.effective_traits.family.as_deref(), Some("owned"));
    assert_eq!(leaf.effective_traits.index, vec!["/payload/status"]);

    let selected = store
        .resolve_type_set(&ctx, &[format!("{managed}*")])
        .await
        .expect("patterns resolve");
    assert!(selected.contains(&requirement), "{selected:?}");
    assert!(selected.contains(&document), "{selected:?}");

    ingest_batch(
        store,
        &ctx,
        batch(
            vec![
                NodeSpec {
                    node_key: "r1".to_owned(),
                    type_id: requirement.clone(),
                    name: Some("r1".to_owned()),
                    payload: Some(serde_json::json!({ "status": "approved", "priority": 1 })),
                    ..NodeSpec::default()
                },
                NodeSpec {
                    node_key: "r2".to_owned(),
                    type_id: requirement.clone(),
                    name: Some("r2".to_owned()),
                    payload: Some(serde_json::json!({ "status": "draft", "priority": 2 })),
                    ..NodeSpec::default()
                },
            ],
            Vec::new(),
        ),
    )
    .await
    .expect("the batch commits");

    let page = store
        .project_table(
            &ctx,
            projection(&[requirement.as_str()], "payload/status eq 'approved'", &[]),
        )
        .await
        .expect("a path inherited from the intermediate filters the leaf");
    assert_eq!(keys(&page), vec!["r1"]);
}

/// Seed the tickets and hand back a projection over them — for the cases
/// that only one implementation can run.
#[allow(dead_code)]
pub async fn projection_seeded(
    store: &dyn GraphStoreV1,
    ctx: &StoreCtx<'_>,
    order: &[(&str, toolkit_odata::SortDir)],
) -> ProjectionRequest {
    seed_tickets(store, ctx).await;
    projection(&[INDEXED], "", order)
}

// --- type evolution (registering a changed schema in place) -----------------

/// The deck's worked example, as a registrable type: the `requirement` a PM
/// edits four times in a week.
pub const EVOLVING: &str =
    "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~test.gs._.requirement.v1~";

/// One revision of that type.
///
/// The payload level is **closed**. That is the whole difference between a
/// model whose optional-field edits are provably compatible and one whose are
/// not: at an open level the previous definition already accepted any value
/// under the new property's name, so declaring it narrows the accepted set
/// (gts sec 4.4) and the checker reports `incompatible`. Measured over the
/// Studio domain model as the exporter emits it today, that is 188 of 188 node
/// types.
fn requirement_revision(
    properties: &serde_json::Value,
    required: &[&str],
    index: &[&str],
) -> TypeRegistration {
    TypeRegistration {
        type_id: EVOLVING.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{EVOLVING}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": {
                "index": index,
                "full_text_search": ["/name"],
                // Declared so the migration cases can assert what a rewritten
                // payload does to the vector composed from it.
                "vector_search": ["/payload/statement"]
            },
            "type": "object",
            "allOf": [
                { "$ref": "gts://gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~" },
                { "type": "object", "properties": { "payload": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": properties.clone(),
                    "required": required
                } } }
            ]
        }),
    }
}

fn requirement_properties(
    status_values: &[&str],
    with_owner: bool,
    urgency: bool,
) -> serde_json::Value {
    let mut properties = serde_json::json!({
        "key": { "type": "string" },
        "statement": { "type": "string" },
        "status": { "type": "string", "enum": status_values }
    });
    properties[if urgency { "urgency" } else { "priority" }] = serde_json::json!({
        "type": "string"
    });
    if with_owner {
        properties["owner"] = serde_json::json!({ "type": "string" });
    }
    properties
}

/// Model v7: what the graph already holds 10 000 of.
fn requirement_v1() -> TypeRegistration {
    requirement_revision(
        &requirement_properties(&["proposed", "approved"], false, false),
        &["key", "statement"],
        &["/payload/status"],
    )
}

fn requirement(key: &str, status: &str) -> NodeSpec {
    NodeSpec {
        node_key: key.to_owned(),
        type_id: EVOLVING.to_owned(),
        name: Some(key.to_owned()),
        payload: Some(serde_json::json!({
            "key": key,
            "statement": format!("the system shall {key}"),
            "status": status,
            "priority": "normal"
        })),
        ..NodeSpec::default()
    }
}

/// Register v1 and three requirements against it.
async fn seed_requirements(store: &dyn GraphStoreV1, ctx: &StoreCtx<'_>, statuses: &[&str]) {
    let mut types = ontology_batch();
    types.push(requirement_v1());
    store
        .register_types(ctx, types)
        .await
        .expect("the ontology registers");
    let nodes = statuses
        .iter()
        .enumerate()
        .map(|(index, status)| requirement(&format!("r{index}"), status))
        .collect();
    ingest_batch(store, ctx, batch(nodes, Vec::new()))
        .await
        .expect("the requirements commit");
}

fn update_options() -> graph_storage_sdk::models::TypeRegistrationOptions {
    graph_storage_sdk::models::TypeRegistrationOptions {
        on_existing: graph_storage_sdk::models::OnExisting::Update,
        revalidate: false,
        dry_run: false,
        migrations: Vec::new(),
    }
}

/// Edits 1 and 2 of the deck's four: a new optional property and a widened
/// enum. Both are proved compatible from the schemas alone, so the update
/// reads no row, keeps the identifier, and the stored objects stay exactly
/// where they were — which is the whole product complaint answered.
pub async fn a_backward_compatible_change_updates_the_type_in_place(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed", "approved", "proposed"]).await;

    let edited = requirement_revision(
        &requirement_properties(&["proposed", "approved", "blocked"], true, false),
        &["key", "statement"],
        &["/payload/status"],
    );
    let registered = store
        .register_types_with(&ctx, vec![edited], update_options())
        .await
        .expect("a backward-compatible change is admitted in place");

    let updated = registered
        .iter()
        .find(|item| item.record.type_id == EVOLVING)
        .expect("the edited type is reported");
    assert_eq!(
        updated.outcome,
        graph_storage_sdk::models::TypeOutcome::Updated
    );
    assert_eq!(
        updated.basis,
        Some(graph_storage_sdk::models::AdmissionBasis::SchemaProved),
        "no row may be read for a change the schemas prove"
    );
    assert_eq!(updated.record.revision, 2, "the retained revision advances");
    let change = updated.change.as_ref().expect("the verdict is reported");
    assert_eq!(change.state.as_str(), "compatible");
    assert_eq!(change.rows, None, "a proved change counts no rows");

    // The identifier is the same one, so the objects are still there.
    let stored = store
        .get_type(&ctx, &EVOLVING.to_owned())
        .await
        .expect("the type is still registered under its own id");
    assert_eq!(stored.revision, 2);
    for key in ["r0", "r1", "r2"] {
        store
            .get_node(&ctx, &key.to_owned(), 10)
            .await
            .unwrap_or_else(|error| panic!("`{key}` must survive the type update: {error}"));
    }

    // And what the new definition admits, ingest now admits.
    let mut blocked = requirement("r3", "blocked");
    blocked.payload = Some(serde_json::json!({
        "key": "r3",
        "statement": "the system shall block",
        "status": "blocked",
        "priority": "normal",
        "owner": "ada"
    }));
    ingest_batch(store, &ctx, batch(vec![blocked], Vec::new()))
        .await
        .expect("a payload the new definition admits ingests");
}

/// Edit 3, the rename. Refused — and the refusal says where.
pub async fn an_incompatible_change_is_refused_with_its_location(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed"]).await;

    let renamed = requirement_revision(
        &requirement_properties(&["proposed", "approved"], false, true),
        &["key", "statement"],
        &["/payload/status"],
    );
    let error = store
        .register_types_with(&ctx, vec![renamed], update_options())
        .await
        .expect_err("a rename cannot be admitted in place");
    let GraphStoreError::Conflict { reason } = error else {
        panic!("a refused change is a conflict, not {error:?}");
    };
    assert!(
        reason.contains("$.payload"),
        "the refusal must name the offending schema location: {reason}"
    );
    assert!(
        reason.contains("priority") || reason.contains("urgency"),
        "and the property that moved: {reason}"
    );

    // Nothing was written.
    let stored = store
        .get_type(&ctx, &EVOLVING.to_owned())
        .await
        .expect("the type is untouched");
    assert_eq!(stored.revision, 1);
}

/// Without `on_existing: update` the gear answers exactly what it always
/// answered. A caller that does not ask for the new behaviour does not get it.
pub async fn a_changed_schema_is_still_a_conflict_by_default(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed"]).await;

    let compatible = requirement_revision(
        &requirement_properties(&["proposed", "approved"], true, false),
        &["key", "statement"],
        &["/payload/status"],
    );
    let error = store
        .register_types(&ctx, vec![compatible])
        .await
        .expect_err("the default mode rejects any changed schema");
    assert!(
        matches!(error, GraphStoreError::Conflict { .. }),
        "{error:?}"
    );
}

/// The question the architect's loop actually asks: what would this edit cost?
/// The dry run answers for a whole batch at once and writes nothing.
pub async fn a_dry_run_reports_every_verdict_and_writes_nothing(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed", "approved"]).await;

    let dry = graph_storage_sdk::models::TypeRegistrationOptions {
        on_existing: graph_storage_sdk::models::OnExisting::Update,
        revalidate: true,
        dry_run: true,
        migrations: Vec::new(),
    };

    let compatible = requirement_revision(
        &requirement_properties(&["proposed", "approved"], true, false),
        &["key", "statement"],
        &["/payload/status"],
    );
    let reported = store
        .register_types_with(&ctx, vec![compatible], dry.clone())
        .await
        .expect("a dry run never fails on a refusal");
    let change = reported[0]
        .change
        .as_ref()
        .expect("a dry run always reports the verdict");
    assert_eq!(change.state.as_str(), "compatible");
    assert!(change.admissible);
    assert!(!change.migration_required);
    assert_eq!(
        change.forward, "incompatible",
        "an added property is exactly where the two directions disagree, and a \
         producer has to hear it"
    );

    let renamed = requirement_revision(
        &requirement_properties(&["proposed", "approved"], false, true),
        &["key", "statement"],
        &["/payload/status"],
    );
    let reported = store
        .register_types_with(&ctx, vec![renamed], dry)
        .await
        .expect("a dry run reports a refusal rather than raising it");
    let change = reported[0]
        .change
        .as_ref()
        .expect("a dry run always reports the verdict");
    assert_eq!(change.state.as_str(), "incompatible");
    assert!(!change.admissible);
    assert!(change.migration_required);
    assert!(
        change
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.location == "$.payload"),
        "{:?}",
        change.diagnostics
    );

    let stored = store
        .get_type(&ctx, &EVOLVING.to_owned())
        .await
        .expect("the type is untouched");
    assert_eq!(stored.revision, 1, "a dry run writes nothing");
}

/// A narrowed enum is not backward compatible — the old definition accepted
/// `approved` and the new one does not. But if no stored row ever used it, the
/// change is safe *for this graph*, and the gear holds the rows to prove it.
///
/// The two grounds are never conflated: this one reports `data_backed` with
/// the number of rows it read.
pub async fn a_change_the_schemas_cannot_prove_is_admitted_when_the_rows_fit(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed", "proposed", "proposed"]).await;

    let narrowed = requirement_revision(
        &requirement_properties(&["proposed"], false, false),
        &["key", "statement"],
        &["/payload/status"],
    );
    let options = graph_storage_sdk::models::TypeRegistrationOptions {
        on_existing: graph_storage_sdk::models::OnExisting::Update,
        revalidate: true,
        dry_run: false,
        migrations: Vec::new(),
    };
    let registered = store
        .register_types_with(&ctx, vec![narrowed.clone()], options)
        .await
        .expect("rows that all fit admit the change");
    let updated = &registered[registered.len() - 1];
    assert_eq!(
        updated.basis,
        Some(graph_storage_sdk::models::AdmissionBasis::DataBacked { rows_validated: 3 }),
        "the admission is a claim about the rows, and says how many"
    );
    let change = updated.change.as_ref().expect("the verdict is reported");
    assert_eq!(change.state.as_str(), "incompatible");
    assert!(
        change.admissible,
        "not provable from the schemas, still admitted from the rows"
    );

    // Without the flag the same change is refused: the data-backed ground is
    // opt-in, never a relaxation of the default.
    let mut types = ontology_batch();
    types.push(requirement_v1());
    store
        .register_types_with(&ctx, types, update_options())
        .await
        .expect("restoring the wider enum is itself compatible");
    let error = store
        .register_types_with(&ctx, vec![narrowed], update_options())
        .await
        .expect_err("without `revalidate` the gear fails closed");
    assert!(
        matches!(error, GraphStoreError::Conflict { .. }),
        "{error:?}"
    );
}

/// The same narrowing, with one row that contradicts it. Refused, naming the
/// row — a caller fixes the data or writes a migration, and either way knows
/// which objects are in the way.
pub async fn a_change_the_stored_rows_contradict_is_refused_naming_them(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed", "approved"]).await;

    let narrowed = requirement_revision(
        &requirement_properties(&["proposed"], false, false),
        &["key", "statement"],
        &["/payload/status"],
    );
    let options = graph_storage_sdk::models::TypeRegistrationOptions {
        on_existing: graph_storage_sdk::models::OnExisting::Update,
        revalidate: true,
        dry_run: false,
        migrations: Vec::new(),
    };
    let error = store
        .register_types_with(&ctx, vec![narrowed], options)
        .await
        .expect_err("a row that the candidate refuses refuses the candidate");
    let GraphStoreError::Validation { items } = error else {
        panic!("an offending row is a validation failure, not {error:?}");
    };
    assert!(
        items.iter().any(|item| item.message.contains("r1")),
        "the refusal must name the row in the way: {items:?}"
    );

    let stored = store
        .get_type(&ctx, &EVOLVING.to_owned())
        .await
        .expect("the type is untouched");
    assert_eq!(stored.revision, 1, "a refused update writes nothing");
}

/// An accepted update moves the tenant's graph revision.
///
/// The Read Consistency Contract's promise is that two reads at one revision
/// cannot observe different content, and an updated type changes what a read
/// answers: the projection admits a path it refused, and ingest validates
/// against a different schema. A label attach carries the same obligation for
/// the same reason. A `created` type changes no existing read, and registration
/// has never moved the counter for one — so this case pins the difference
/// rather than only the bump.
pub async fn an_accepted_type_update_advances_the_graph_revision(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed"]).await;

    let before = store
        .revision(&ctx)
        .await
        .expect("the tenant reports a revision");

    // Registering the same batch again converges: nothing moves.
    let mut types = ontology_batch();
    types.push(requirement_v1());
    store
        .register_types_with(&ctx, types, update_options())
        .await
        .expect("an identical re-registration converges");
    let unchanged = store
        .revision(&ctx)
        .await
        .expect("the tenant reports a revision");
    assert_eq!(
        unchanged.revision, before.revision,
        "a convergent re-registration must leave the revision alone"
    );

    let edited = requirement_revision(
        &requirement_properties(&["proposed", "approved", "blocked"], false, false),
        &["key", "statement"],
        &["/payload/status"],
    );
    store
        .register_types_with(&ctx, vec![edited], update_options())
        .await
        .expect("a wider enum is admitted in place");
    let after = store
        .revision(&ctx)
        .await
        .expect("the tenant reports a revision");
    assert!(
        after.revision > before.revision,
        "an accepted update changes what a read answers, so it must fence the \
         revision: {} -> {}",
        before.revision,
        after.revision
    );
}

/// Declaring a new `index` path changes no constraint on any instance, so the
/// comparison proves it compatible and the path becomes filterable at once —
/// without recreating the type, and without touching a row.
///
/// This is also the fix for a wart the prototype hit: before updates existed,
/// a re-registration converged and left the *stored* trait resolution as it
/// was, so a type registered by an older build kept a resolution without
/// `index_kinds` and the only remedy was to recreate the database.
pub async fn a_new_index_path_becomes_filterable_without_recreating_the_type(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    use toolkit_odata::SortDir;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed", "approved"]).await;

    // `priority` is declared in the schema but not in the `index` trait, so
    // the projection refuses it — the catalogue owes callers that refusal.
    store
        .project_table(
            &ctx,
            projection(
                &[EVOLVING],
                "payload/priority eq 'normal'",
                &[("node_key", SortDir::Asc)],
            ),
        )
        .await
        .expect_err("an undeclared path is refused before the update");

    let widened = requirement_revision(
        &requirement_properties(&["proposed", "approved"], false, false),
        &["key", "statement"],
        &["/payload/status", "/payload/priority"],
    );
    let registered = store
        .register_types_with(&ctx, vec![widened], update_options())
        .await
        .expect("declaring another index path constrains no instance");
    let updated = &registered[registered.len() - 1];
    assert_eq!(
        updated.outcome,
        graph_storage_sdk::models::TypeOutcome::Updated
    );
    let change = updated.change.as_ref().expect("the verdict is reported");
    assert_eq!(change.state.as_str(), "compatible");
    let index_change = change
        .traits_changed
        .iter()
        .find(|change| change.trait_name == "index")
        .expect("the moved trait is reported even though the schemas agree");
    assert_eq!(index_change.added, vec!["/payload/priority".to_owned()]);
    assert!(index_change.removed.is_empty());

    let page = store
        .project_table(
            &ctx,
            projection(
                &[EVOLVING],
                "payload/priority eq 'normal'",
                &[("node_key", SortDir::Asc)],
            ),
        )
        .await
        .expect("the newly declared path filters at once");
    assert_eq!(keys(&page), vec!["r0".to_owned(), "r1".to_owned()]);
}

// --- payload migrations ------------------------------------------------------

fn migration(
    type_id: &str,
    steps: Vec<graph_storage_sdk::models::MigrationStep>,
) -> graph_storage_sdk::models::MigrationSpec {
    graph_storage_sdk::models::MigrationSpec {
        type_id: type_id.to_owned(),
        steps,
    }
}

fn migrating_options(
    migrations: Vec<graph_storage_sdk::models::MigrationSpec>,
) -> graph_storage_sdk::models::TypeRegistrationOptions {
    graph_storage_sdk::models::TypeRegistrationOptions {
        on_existing: graph_storage_sdk::models::OnExisting::Update,
        revalidate: false,
        dry_run: false,
        migrations,
    }
}

/// The deck's third edit, end to end: the rename that the compatibility check
/// refuses on its own becomes one request that moves the type **and** the data.
///
/// The assertion that matters is the last one. A rename admitted without moving
/// the data leaves every query on the new name returning nothing, which is the
/// failure mode `data_backed` cannot see; here the projection over
/// `payload/urgency` finds the rows.
pub async fn a_migration_moves_the_data_with_the_type(store: &dyn GraphStoreV1, tenant: Uuid) {
    use toolkit_odata::SortDir;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed", "approved", "proposed"]).await;

    let renamed = requirement_revision(
        &requirement_properties(&["proposed", "approved"], false, true),
        &["key", "statement"],
        &["/payload/status", "/payload/urgency"],
    );
    let registered = store
        .register_types_with(
            &ctx,
            vec![renamed],
            migrating_options(vec![migration(
                EVOLVING,
                vec![graph_storage_sdk::models::MigrationStep::Rename {
                    from: "/payload/priority".to_owned(),
                    to: "/payload/urgency".to_owned(),
                }],
            )]),
        )
        .await
        .expect("a rename with a migration is admitted");

    let updated = registered
        .iter()
        .find(|item| item.record.type_id == EVOLVING)
        .expect("the migrated type is reported");
    assert_eq!(
        updated.outcome,
        graph_storage_sdk::models::TypeOutcome::Updated
    );
    assert_eq!(
        updated.basis,
        Some(graph_storage_sdk::models::AdmissionBasis::Migrated {
            rows_scanned: 3,
            rows_rewritten: 3,
        }),
        "the report says what was read and what was changed, separately"
    );

    // The data moved, not just the schema.
    let node = store
        .get_node(&ctx, &"r0".to_owned(), 10)
        .await
        .expect("the migrated node reads");
    let payload = node.payload.expect("the node has a payload");
    assert_eq!(payload.get("urgency"), Some(&serde_json::json!("normal")));
    assert!(
        payload.get("priority").is_none(),
        "the old property is gone, not duplicated: {payload}"
    );

    let page = store
        .project_table(
            &ctx,
            projection(
                &[EVOLVING],
                "payload/urgency eq 'normal'",
                &[("node_key", SortDir::Asc)],
            ),
        )
        .await
        .expect("the renamed path filters");
    assert_eq!(
        keys(&page),
        vec!["r0".to_owned(), "r1".to_owned(), "r2".to_owned()],
        "a rename that moved the data answers queries on the new name"
    );
}

/// A migration is only as good as its steps, and the gear checks them against
/// the rows rather than taking the caller's word: the plan below renames into a
/// property the candidate declares as an integer, so every row fails and
/// nothing at all is written.
pub async fn a_migration_that_leaves_rows_invalid_is_refused_naming_them(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed", "approved"]).await;

    let mut properties = requirement_properties(&["proposed", "approved"], false, true);
    properties["urgency"] = serde_json::json!({ "type": "integer" });
    let retyped = requirement_revision(&properties, &["key", "statement"], &["/payload/status"]);

    let error = store
        .register_types_with(
            &ctx,
            vec![retyped],
            migrating_options(vec![migration(
                EVOLVING,
                vec![graph_storage_sdk::models::MigrationStep::Rename {
                    from: "/payload/priority".to_owned(),
                    to: "/payload/urgency".to_owned(),
                }],
            )]),
        )
        .await
        .expect_err("a plan whose result does not validate is refused");
    let GraphStoreError::Validation { items } = error else {
        panic!("an invalid row after a migration is a validation failure, not {error:?}");
    };
    assert!(
        items.iter().any(|item| item.message.contains("r0")),
        "the refusal names the row it could not migrate: {items:?}"
    );

    // Nothing was written: not the type, and not the rows.
    let stored = store
        .get_type(&ctx, &EVOLVING.to_owned())
        .await
        .expect("the type is untouched");
    assert_eq!(stored.revision, 1);
    let node = store
        .get_node(&ctx, &"r0".to_owned(), 10)
        .await
        .expect("the node reads");
    let payload = node.payload.expect("the node has a payload");
    assert_eq!(payload.get("priority"), Some(&serde_json::json!("normal")));
}

/// A migration needs a schema change to migrate towards. Without one this
/// endpoint would be a payload-editing API wearing a type registration's
/// clothes — a different feature, with a different authorization story.
pub async fn a_migration_without_a_schema_change_is_refused(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_requirements(store, &ctx, &["proposed"]).await;

    let error = store
        .register_types_with(
            &ctx,
            vec![requirement_v1()],
            migrating_options(vec![migration(
                EVOLVING,
                vec![graph_storage_sdk::models::MigrationStep::Drop {
                    path: "/payload/priority".to_owned(),
                }],
            )]),
        )
        .await
        .expect_err("a migration against an unchanged schema is refused");
    assert!(
        matches!(error, GraphStoreError::InvalidQuery { .. }),
        "{error:?}"
    );
}

/// Every write records who made it and moves the row's compare-and-set target;
/// a migration is a write. Without the version bump a producer holding the
/// pre-migration value would overwrite the migrated row and undo the migration
/// in silence, and without the subject the row would claim its last writer was
/// the producer.
pub async fn a_migration_stamps_its_writer_and_moves_the_version(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let producer = ctx(tenant, &scope, None);
    seed_requirements(store, &producer, &["proposed"]).await;

    // The version is not on any read surface, so the property is asserted the
    // way a producer would meet it: an ingest carrying the pre-migration
    // version must be refused once the migration has moved the row.
    let stale_cas = {
        let mut spec = requirement("r0", "proposed");
        spec.expected_version = Some(1);
        spec
    };

    let migrator = ctx_as(tenant, &scope, None, editor());
    let filled = requirement_revision(
        &requirement_properties(&["proposed", "approved"], true, false),
        &["key", "statement", "owner"],
        &["/payload/status"],
    );
    store
        .register_types_with(
            &migrator,
            vec![filled],
            migrating_options(vec![migration(
                EVOLVING,
                vec![graph_storage_sdk::models::MigrationStep::Default {
                    path: "/payload/owner".to_owned(),
                    value: serde_json::json!("unassigned"),
                }],
            )]),
        )
        .await
        .expect("a default fills the newly required field");

    let after = store
        .get_node(&producer, &"r0".to_owned(), 10)
        .await
        .expect("the node reads");
    assert_eq!(
        after.payload.expect("payload")["owner"],
        serde_json::json!("unassigned")
    );
    let error = ingest_batch(store, &producer, batch(vec![stale_cas], Vec::new()))
        .await
        .expect_err("the migration moved the row, so the producer's version is stale");
    assert!(
        matches!(error, GraphStoreError::Conflict { .. }),
        "a stale expected_version after a migration is a conflict, not {error:?}"
    );
    assert_eq!(
        after.envelope.updated_by,
        editor(),
        "the row records the subject that migrated it, not the producer"
    );

    // The stored vector was made from text this row no longer has, so it must
    // stop ranking until something re-embeds it.
    let state = store
        .embedding_state(&producer, &["r0".to_owned()])
        .await
        .expect("the embedding state reads");
    let state = state
        .first()
        .and_then(Clone::clone)
        .expect("the row's embedding state is reported");
    assert!(
        state.vector_epoch.is_none(),
        "the type composes its embedding input from the payload, so a rewritten \
         payload leaves the vector stale rather than silently wrong: {state:?}"
    );
}

// --- source-namespace ownership ----------------------------------------------

/// A second producer, with its own principal.
fn producer_b() -> Subject {
    Subject {
        subject_id: uuid::uuid!("33333333-3333-3333-3333-333333333333"),
        subject_type: Some("gts.cf.core.security.subject_service.v1~".to_owned()),
    }
}

/// A reference node under `system`, keyed the way the identity rule requires.
fn mirror_node(system: &str, native_id: &str) -> NodeSpec {
    NodeSpec {
        node_key: format!("{system}:repo:{native_id}"),
        type_id: REFERENCE.to_owned(),
        payload: Some(serde_json::json!({
            "source": { "system": system, "kind": "repo", "native_id": native_id }
        })),
        ..NodeSpec::default()
    }
}

async fn seed_reference_ontology(store: &dyn GraphStoreV1, ctx: &StoreCtx<'_>) {
    let mut batch = ontology_batch();
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
        .register_types(ctx, batch)
        .await
        .expect("the ontology registers");
}

/// An unclaimed namespace is claimed by the producer that first writes it, so
/// a single-producer deployment needs no setup — and the claim is visible,
/// because an ownership boundary nobody can read is one nobody can operate.
pub async fn a_source_namespace_is_claimed_by_its_first_writer(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_reference_ontology(store, &ctx).await;

    assert!(
        store
            .list_source_namespaces(&ctx)
            .await
            .expect("the registry reads")
            .is_empty(),
        "nothing is claimed before anything is written"
    );

    ingest_batch(
        store,
        &ctx,
        batch(vec![mirror_node("sys", "42")], Vec::new()),
    )
    .await
    .expect("the first writer claims the namespace");

    let claimed = store
        .list_source_namespaces(&ctx)
        .await
        .expect("the registry reads");
    assert_eq!(claimed.len(), 1, "{claimed:?}");
    assert_eq!(claimed[0].namespace, "sys");
    assert_eq!(
        claimed[0].owner_principal,
        writer().principal(),
        "the namespace is bound to the principal that wrote it"
    );
    assert!(claimed[0].previous_owner.is_none());

    // The same producer keeps writing it, including a second object.
    ingest_batch(
        store,
        &ctx,
        batch(vec![mirror_node("sys", "43")], Vec::new()),
    )
    .await
    .expect("the owner keeps writing its own namespace");
}

/// The boundary, and the reason it exists: the identity triple that makes two
/// producers converge on one object would otherwise let a generic `write`
/// permission overwrite the projection another source maintains. Refused for
/// an update exactly as for an insert — an overwrite is the case that matters.
pub async fn writing_under_another_producers_namespace_is_forbidden(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let owner = ctx(tenant, &scope, None);
    seed_reference_ontology(store, &owner).await;
    ingest_batch(
        store,
        &owner,
        batch(vec![mirror_node("sys", "42")], Vec::new()),
    )
    .await
    .expect("the owner claims the namespace");

    let intruder = ctx_as(tenant, &scope, None, producer_b());

    // A new object under someone else's namespace.
    let error = ingest_batch(
        store,
        &intruder,
        batch(vec![mirror_node("sys", "99")], Vec::new()),
    )
    .await
    .expect_err("another producer may not write this namespace");
    assert!(
        matches!(&error, GraphStoreError::SourceNamespaceForbidden { namespace } if namespace == "sys"),
        "a namespace denial is its own error, not a not-found: {error:?}"
    );

    // And an overwrite of the owner's existing object.
    let error = ingest_batch(
        store,
        &intruder,
        batch(vec![mirror_node("sys", "42")], Vec::new()),
    )
    .await
    .expect_err("an update is re-authorized against the owner, not only an insert");
    assert!(
        matches!(error, GraphStoreError::SourceNamespaceForbidden { .. }),
        "{error:?}"
    );

    // Nothing of the intruder's batch landed.
    assert!(
        store
            .get_node(&owner, &"sys:repo:99".to_owned(), 10)
            .await
            .is_err(),
        "a refused batch commits nothing"
    );
}

/// Ownership moves one way only: through the administrative flow. Afterwards
/// the new owner writes and the old one is refused, and the row says who moved
/// it and from whom — the audit trail of the one act that can move a boundary.
pub async fn a_transfer_moves_the_namespace_and_records_who_moved_it(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let first = ctx(tenant, &scope, None);
    seed_reference_ontology(store, &first).await;
    ingest_batch(
        store,
        &first,
        batch(vec![mirror_node("sys", "42")], Vec::new()),
    )
    .await
    .expect("the first producer claims the namespace");

    let admin = ctx_as(tenant, &scope, None, editor());
    let moved = store
        .transfer_source_namespace(&admin, "sys", &producer_b().principal())
        .await
        .expect("the administrative flow moves the namespace");
    assert_eq!(moved.owner_principal, producer_b().principal());
    assert_eq!(
        moved.previous_owner.as_deref(),
        Some(writer().principal().as_str()),
        "the row records whom it was taken from"
    );
    assert_eq!(
        moved.transferred_by.as_ref(),
        Some(&editor()),
        "and who took it"
    );

    // The new owner writes; the previous one no longer can.
    let second = ctx_as(tenant, &scope, None, producer_b());
    ingest_batch(
        store,
        &second,
        batch(vec![mirror_node("sys", "50")], Vec::new()),
    )
    .await
    .expect("the new owner writes the namespace");
    let error = ingest_batch(
        store,
        &first,
        batch(vec![mirror_node("sys", "51")], Vec::new()),
    )
    .await
    .expect_err("the previous owner is refused after the transfer");
    assert!(
        matches!(error, GraphStoreError::SourceNamespaceForbidden { .. }),
        "{error:?}"
    );

    // The rows the first producer created still say it created them: the
    // registry moved, provenance did not.
    store
        .get_node(&second, &"sys:repo:42".to_owned(), 10)
        .await
        .expect("the object it created is still there, and readable by the new owner");
}

/// `source` in an owned node's payload is a payload field, not a boundary: it
/// claims nothing, and it authorizes nothing.
pub async fn an_owned_nodes_source_field_claims_no_namespace(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_reference_ontology(store, &ctx).await;

    let mut owned = node("owned-1", "one");
    owned.payload = Some(serde_json::json!({
        "source": { "system": "sys", "kind": "repo", "native_id": "42" }
    }));
    ingest_batch(store, &ctx, batch(vec![owned], Vec::new()))
        .await
        .expect("an owned node with a source-shaped payload is just a payload");

    assert!(
        store
            .list_source_namespaces(&ctx)
            .await
            .expect("the registry reads")
            .is_empty(),
        "an owned node claims no namespace"
    );

    // And the namespace is still free for the producer that does own it.
    let other = ctx_as(tenant, &scope, None, producer_b());
    ingest_batch(
        store,
        &other,
        batch(vec![mirror_node("sys", "42")], Vec::new()),
    )
    .await
    .expect("the reference producer claims a namespace no owned node took");
}

// --- readiness ----------------------------------------------------------------

/// The matrix's aggregate rule, and the one row that contradicts it.
///
/// `fr-readiness` is per capability, not one boolean: a component is healthy,
/// degraded or unhealthy, and only *some* states take the gear out of service.
/// The embedding-space row is the case worth pinning — it is `unhealthy` and
/// the gear stays ready, because a mismatch blocks the vector arms and nothing
/// else. A conformance case rather than a unit test because the probe is the
/// store's, and the two stores must answer the same shape.
pub async fn readiness_reports_every_capability_and_only_some_block_service(
    store: &dyn GraphStoreV1,
) {
    use graph_storage_sdk::models::{ComponentReadiness, Readiness, ReadinessState};

    let rows = store.probe_readiness().await;
    let named: Vec<&str> = rows.iter().map(|row| row.component.as_str()).collect();
    assert!(
        named.contains(&graph_storage_sdk::models::DATABASE),
        "every store answers for its own storage: {named:?}"
    );
    assert!(
        named.contains(&graph_storage_sdk::models::SQLPGQ),
        "and for the traversal backend it actually provides: {named:?}"
    );
    for row in &rows {
        match row.state {
            graph_storage_sdk::models::ReadinessState::Healthy => assert!(
                row.problem.is_none(),
                "a healthy component names no problem: {row:?}"
            ),
            _ => assert!(
                row.problem.is_some() && row.recovery.is_some(),
                "a non-healthy component names its problem and what it waits on: {row:?}"
            ),
        }
    }

    let space_mismatch = ComponentReadiness::new(
        graph_storage_sdk::models::EMBEDDING_SPACE,
        ReadinessState::Unhealthy,
        "stored vectors belong to another space",
        "vector and hybrid search",
        "re-embed",
    );
    assert!(
        Readiness::of(vec![space_mismatch.clone()]).ready,
        "an embedding-space mismatch blocks the vector arms and leaves the gear ready"
    );
    let database_down = ComponentReadiness::new(
        graph_storage_sdk::models::DATABASE,
        ReadinessState::Unhealthy,
        "unreachable",
        "everything",
        "connectivity",
    );
    assert!(
        !Readiness::of(vec![space_mismatch, database_down]).ready,
        "an unreachable database admits no traffic at all"
    );
}

// --- scope replacement: the removal half --------------------------------------

/// A scope-managed node under `repository = acme/infra`.
fn scoped_node(key: &str, repository: &str) -> NodeSpec {
    NodeSpec {
        node_key: key.to_owned(),
        type_id: OWNED.to_owned(),
        name: Some(key.to_owned()),
        payload: Some(serde_json::json!({ "repository": repository })),
        ..NodeSpec::default()
    }
}

fn replacing(generation: i64) -> ReplaceScope {
    ReplaceScope {
        attribute: "repository".to_owned(),
        value: "acme/infra".to_owned(),
        generation,
    }
}

fn batch_replacing(nodes: Vec<NodeSpec>, edges: Vec<EdgeSpec>, generation: i64) -> IngestRequest {
    IngestRequest {
        replace_scope: Some(replacing(generation)),
        ..batch(nodes, edges)
    }
}

/// PRD § 9 criterion 2, the half that was missing: a re-import is the whole of
/// its scope, so what it no longer names is gone.
///
/// Removal is a hard delete rather than a tombstone, and the second half of
/// this case is why: a tombstoned key is not reusable before purge, so
/// tombstoning here would make the *next* import of the same object a
/// conflict — the opposite of what a replacement is for.
pub async fn scope_replacement_removes_what_the_batch_no_longer_names(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    store
        .register_types(&ctx, ontology_batch())
        .await
        .expect("the ontology registers");

    ingest_batch(
        store,
        &ctx,
        batch_replacing(
            vec![
                scoped_node("in-scope-1", "acme/infra"),
                scoped_node("in-scope-2", "acme/infra"),
                scoped_node("elsewhere", "acme/web"),
            ],
            Vec::new(),
            1,
        ),
    )
    .await
    .expect("the first snapshot lands");

    // The second snapshot names only one of them.
    let outcome = ingest_batch(
        store,
        &ctx,
        batch_replacing(vec![scoped_node("in-scope-1", "acme/infra")], Vec::new(), 2),
    )
    .await
    .expect("the second snapshot lands");
    assert_eq!(
        outcome.counts.scope_removed_nodes, 1,
        "exactly the one the batch stopped naming"
    );

    store
        .get_node(&ctx, &"in-scope-1".to_owned(), 10)
        .await
        .expect("what the batch re-supplied stays");
    assert!(
        store
            .get_node(&ctx, &"in-scope-2".to_owned(), 10)
            .await
            .is_err(),
        "what it no longer names is gone"
    );
    store
        .get_node(&ctx, &"elsewhere".to_owned(), 10)
        .await
        .expect("another scope is untouched: membership is the payload attribute");

    // The removed key is reusable at once: a hard delete, not a tombstone.
    ingest_batch(
        store,
        &ctx,
        batch_replacing(
            vec![
                scoped_node("in-scope-1", "acme/infra"),
                scoped_node("in-scope-2", "acme/infra"),
            ],
            Vec::new(),
            3,
        ),
    )
    .await
    .expect("a later import re-adds the same key");
}

/// The other half of criterion 2, and the principle behind it
/// (`principle-provenance-survives-resync`): a re-import removes what it
/// re-derives and never what was concluded about it.
pub async fn scope_replacement_preserves_analysis_edges_and_their_endpoints(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    let mut types = ontology_batch();
    types.push(TypeRegistration {
        type_id: ANALYSIS.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{ANALYSIS}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.cf.core.graph.edge.v1~cf.core.graph.analysis_edge.v1~" }]
        }),
    });
    store
        .register_types(&ctx, types)
        .await
        .expect("the ontology registers");

    // Two scoped nodes, one static edge between them, and one analysis edge
    // carrying provenance.
    let analysis = EdgeSpec {
        type_id: ANALYSIS.to_owned(),
        src_node_key: "concluded-about".to_owned(),
        dst_node_key: "also-concluded".to_owned(),
        payload: Some(serde_json::json!({
            "provenance": {
                "produced_by": {
                    "subject_id": "00000000-0000-0000-0000-0000000000aa",
                    "subject_type": "gts.cf.core.security.subject_service.v1~"
                },
                "method": "static-analysis"
            }
        })),
        ..EdgeSpec::default()
    };
    ingest_batch(
        store,
        &ctx,
        batch_replacing(
            vec![
                scoped_node("concluded-about", "acme/infra"),
                scoped_node("also-concluded", "acme/infra"),
                scoped_node("plain", "acme/infra"),
            ],
            vec![edge("concluded-about", "plain"), analysis],
            1,
        ),
    )
    .await
    .expect("the first snapshot lands");

    // A snapshot that names none of them.
    let outcome = ingest_batch(
        store,
        &ctx,
        batch_replacing(vec![scoped_node("kept", "acme/infra")], Vec::new(), 2),
    )
    .await
    .expect("the second snapshot lands");

    // `plain` had only a static edge, so both it and the edge go.
    assert!(
        store.get_node(&ctx, &"plain".to_owned(), 10).await.is_err(),
        "a node held only by static content is removed with it"
    );
    assert!(
        outcome.counts.scope_removed_edges >= 1,
        "the static edge is removed: {:?}",
        outcome.counts
    );

    // The two endpoints of the analysis edge stay, and so does the edge.
    let src = store
        .get_node(&ctx, &"concluded-about".to_owned(), 10)
        .await
        .expect("an endpoint of an analysis edge survives the re-import");
    store
        .get_node(&ctx, &"also-concluded".to_owned(), 10)
        .await
        .expect("and so does the other one");
    assert!(
        src.adjacency
            .iter()
            .any(|entry| entry.edge_type_id == ANALYSIS),
        "the conclusion itself survives: {:?}",
        src.adjacency
    );
}

/// Obligation 2 of the store contract, which the suite's header has claimed
/// since the beginning with no case behind it: two concurrent replacements of
/// one scope serialize rather than union.
///
/// The assertion holds whichever of them reaches the fence first, and that is
/// the point. If the higher generation lands first, the lower one is refused
/// as stale; if the lower lands first, the higher one's removal takes what it
/// wrote. Either way the scope ends up holding exactly one snapshot — never
/// both — and the recorded generation is the higher one.
pub async fn two_replacements_of_one_scope_serialize(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let reader = ctx(tenant, &scope, None);
    let lower = ctx(tenant, &scope, None);
    let higher = ctx(tenant, &scope, None);
    store
        .register_types(&reader, ontology_batch())
        .await
        .expect("the ontology registers");
    let (first, second) = tokio::join!(
        ingest_batch(
            store,
            &higher,
            batch_replacing(
                vec![scoped_node("from-higher", "acme/infra")],
                Vec::new(),
                2
            ),
        ),
        ingest_batch(
            store,
            &lower,
            batch_replacing(vec![scoped_node("from-lower", "acme/infra")], Vec::new(), 1),
        ),
    );

    assert!(
        first.is_ok(),
        "the higher generation is never the one refused: {first:?}"
    );
    if let Err(error) = &second {
        assert!(
            matches!(error, GraphStoreError::StaleGeneration { .. }),
            "a loser is refused as stale, not as something else: {error:?}"
        );
    }

    store
        .get_node(&reader, &"from-higher".to_owned(), 10)
        .await
        .expect("the higher generation's content is what remains");
    assert!(
        store
            .get_node(&reader, &"from-lower".to_owned(), 10)
            .await
            .is_err(),
        "the two snapshots never union: the lower generation's node is not there"
    );

    // And the fence records the higher generation, so a replay of the lower
    // one is refused from now on.
    let error = ingest_batch(
        store,
        &reader,
        batch_replacing(vec![scoped_node("from-lower", "acme/infra")], Vec::new(), 1),
    )
    .await
    .expect_err("the recorded generation is the higher one");
    assert!(
        matches!(
            error,
            GraphStoreError::StaleGeneration {
                recorded: 2,
                offered: 1
            }
        ),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Both node families and both edge families, and the edge read
// ---------------------------------------------------------------------------

/// The ontology criterion 1 of PRD § 9 asks for, registered once: owned nodes,
/// reference nodes, static edges and analysis edges.
async fn seed_both_families(store: &dyn GraphStoreV1, ctx: &StoreCtx<'_>) {
    let mut batch = ontology_batch();
    for (type_id, base) in [
        (
            REFERENCE,
            "gts://gts.cf.core.graph.node.v1~cf.core.graph.reference_node.v1~",
        ),
        (
            ANALYSIS,
            "gts://gts.cf.core.graph.edge.v1~cf.core.graph.analysis_edge.v1~",
        ),
    ] {
        batch.push(TypeRegistration {
            type_id: type_id.to_owned(),
            schema: serde_json::json!({
                "$id": format!("gts://{type_id}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "allOf": [{ "$ref": base }]
            }),
        });
    }
    store
        .register_types(ctx, batch)
        .await
        .expect("the ontology registers");
}

/// An analysis edge with the provenance its family requires.
fn analysis_edge(src: &str, dst: &str, method: &str) -> EdgeSpec {
    EdgeSpec {
        type_id: ANALYSIS.to_owned(),
        src_node_key: src.to_owned(),
        dst_node_key: dst.to_owned(),
        payload: Some(serde_json::json!({
            "provenance": {
                "produced_by": {
                    "subject_id": "00000000-0000-0000-0000-0000000000aa",
                    "subject_type": "gts.cf.core.security.subject_service.v1~"
                },
                "method": method
            }
        })),
        ..EdgeSpec::default()
    }
}

/// Everything the read surfaces say about a set of nodes and every edge
/// incident to them, in a form two runs can be compared by.
///
/// Timestamps are included deliberately: "byte-identical state" is the
/// criterion, and an upsert that rewrote an unchanged row would move
/// `updated_at` while leaving every value the same.
async fn readable_state(
    store: &dyn GraphStoreV1,
    ctx: &StoreCtx<'_>,
    keys: &[&str],
) -> serde_json::Value {
    let mut nodes = Vec::new();
    let mut edge_keys: Vec<String> = Vec::new();
    for key in keys {
        let view = store
            .get_node(ctx, &(*key).to_owned(), 50)
            .await
            .unwrap_or_else(|error| panic!("`{key}` is readable: {error}"));
        edge_keys.extend(view.adjacency.iter().map(|entry| entry.edge_key.clone()));
        nodes.push(serde_json::json!({
            "key": view.node_key,
            "type": view.type_id,
            "name": view.name,
            "payload": view.payload,
            "created_at": view.envelope.created_at.to_string(),
            "updated_at": view.envelope.updated_at.to_string(),
        }));
    }
    edge_keys.sort();
    edge_keys.dedup();

    let mut edges = Vec::new();
    for key in &edge_keys {
        // The key came from an adjacency entry, so the edge read must find
        // it: the two surfaces name edges the same way or one of them is
        // unusable from the other.
        let view = store
            .get_edge(ctx, key)
            .await
            .unwrap_or_else(|error| panic!("adjacency names edge `{key}`, which reads: {error}"));
        edges.push(serde_json::json!({
            "key": view.edge_key,
            "type": view.edge_type_id,
            "src": view.src,
            "dst": view.dst,
            "discriminator": view.discriminator,
            "payload": view.payload,
            "created_at": view.envelope.created_at.to_string(),
            "updated_at": view.envelope.updated_at.to_string(),
        }));
    }
    serde_json::json!({ "nodes": nodes, "edges": edges })
}

/// Criterion 1 of PRD § 9, end to end: a producer registers an ontology,
/// ingests one batch holding owned nodes, reference nodes and both edge
/// families, and re-runs the identical batch to the same graph.
///
/// The reference node is keyed by its full source triple and the analysis
/// edge carries provenance -- the two rules that make those families what
/// they are -- and both are read back rather than assumed from the counts.
pub async fn both_node_families_and_both_edge_families_round_trip(
    store: &dyn GraphStoreV1,
    tenant: Uuid,
) {
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx(tenant, &scope, None);
    seed_both_families(store, &ctx).await;

    let mirror = mirror_node("scm", "7");
    let mirror_key = mirror.node_key.clone();
    assert_eq!(
        mirror_key, "scm:repo:7",
        "the reference key is the source triple, not the native id (ADR-0002)"
    );
    let request = || {
        batch(
            vec![node("owned-a", "a"), node("owned-b", "b"), mirror.clone()],
            vec![
                edge("owned-a", "owned-b"),
                edge("owned-b", &mirror_key),
                analysis_edge("owned-a", &mirror_key, "static-analysis"),
            ],
        )
    };

    let first = ingest_batch(store, &ctx, request())
        .await
        .expect("the mixed batch commits");
    assert_eq!(first.counts.nodes_inserted, 3, "{:?}", first.counts);
    assert_eq!(first.counts.edges_inserted, 3, "{:?}", first.counts);

    let keys = ["owned-a", "owned-b", mirror_key.as_str()];
    let after_first = readable_state(store, &ctx, &keys).await;

    // The reference node comes back with the identity it was keyed by, and
    // the analysis edge with the provenance its family requires.
    let reference = store
        .get_node(&ctx, &mirror_key, 50)
        .await
        .expect("the reference node reads");
    assert_eq!(reference.type_id, REFERENCE);
    assert_eq!(
        reference.payload.as_ref().and_then(|p| p.get("source")),
        Some(&serde_json::json!({
            "system": "scm", "kind": "repo", "native_id": "7"
        })),
        "the source triple survives the round trip intact"
    );
    let analysis_key = reference
        .adjacency
        .iter()
        .find(|entry| entry.edge_type_id == ANALYSIS)
        .map(|entry| entry.edge_key.clone())
        .expect("the analysis edge is incident to the reference node");
    let analysis = store
        .get_edge(&ctx, &analysis_key)
        .await
        .expect("the analysis edge reads as an element");
    assert_eq!(
        analysis
            .payload
            .as_ref()
            .and_then(|p| p.pointer("/provenance/method"))
            .and_then(serde_json::Value::as_str),
        Some("static-analysis"),
        "an analysis edge's provenance is readable, not merely accepted"
    );
    assert_eq!(
        (analysis.src, analysis.dst),
        ("owned-a".to_owned(), mirror_key.clone())
    );

    // The same batch again: nothing inserted, nothing rewritten, nothing
    // moved -- including the timestamps.
    let second = ingest_batch(store, &ctx, request())
        .await
        .expect("the identical batch commits again");
    assert_eq!(second.counts.nodes_unchanged, 3, "{:?}", second.counts);
    assert_eq!(second.counts.edges_unchanged, 3, "{:?}", second.counts);
    assert_eq!(
        second.revision, first.revision,
        "an identical re-run leaves the revision where it was"
    );
    assert_eq!(
        readable_state(store, &ctx, &keys).await,
        after_first,
        "the graph a re-run leaves behind is the graph the first run left"
    );
}

/// The key of the one edge incident to a node, as the node read names it.
/// Going through adjacency rather than re-deriving the hash is deliberate:
/// the two surfaces have to agree on how an edge is addressed.
async fn only_incident_edge_key(
    store: &dyn GraphStoreV1,
    ctx: &StoreCtx<'_>,
    node_key: &str,
) -> String {
    store
        .get_node(ctx, &node_key.to_owned(), 10)
        .await
        .unwrap_or_else(|error| panic!("`{node_key}` reads: {error}"))
        .adjacency
        .first()
        .unwrap_or_else(|| panic!("`{node_key}` has an incident edge"))
        .edge_key
        .clone()
}

/// `fr-audit-envelope` asks for the envelope on every node **and edge** a read
/// surface returns. Until the edge read existed the edge half was unassertable
/// (dev/DEVIATIONS.md D-022): the columns were written and nothing read them.
pub async fn an_edge_read_carries_the_envelope(store: &dyn GraphStoreV1, tenant: Uuid) {
    let scope = AccessScope::for_tenant(tenant);
    let author = ctx(tenant, &scope, None);
    let editor_subject = editor();
    let editor = ctx_as(tenant, &scope, None, editor_subject.clone());
    seed_both_families(store, &author).await;

    ingest_batch(
        store,
        &author,
        batch(
            vec![node("env-src", "src"), node("env-dst", "dst")],
            vec![analysis_edge("env-src", "env-dst", "first")],
        ),
    )
    .await
    .expect("the batch commits");

    let key = only_incident_edge_key(store, &author, "env-src").await;

    let created = store
        .get_edge(&author, &key)
        .await
        .expect("the edge reads")
        .envelope;
    assert_eq!(
        created.key, key,
        "an edge has no producer-authored key, so the envelope carries the derived one"
    );
    assert_eq!(created.tenant_id, tenant);
    assert_eq!(created.created_by, writer(), "the creator is recorded");
    assert_eq!(created.updated_by, writer());
    assert!(created.deleted_at.is_none() && created.deleted_by.is_none());
    assert!(
        created.graph_revision.revision > 0,
        "the edge read reports the revision it observed"
    );

    // A second producer re-asserts the same relationship with different
    // content. The edge is rewritten, not versioned, so `updated_by` answers
    // the question the audit trail is for: who claimed it last.
    ingest_batch(
        store,
        &editor,
        batch(
            Vec::new(),
            vec![analysis_edge("env-src", "env-dst", "second")],
        ),
    )
    .await
    .expect("the re-assertion commits");
    let updated = store
        .get_edge(&author, &key)
        .await
        .expect("the edge still reads")
        .envelope;
    assert_eq!(updated.created_by, writer(), "creation is not rewritten");
    assert_eq!(updated.created_at, created.created_at);
    assert_eq!(
        updated.updated_by, editor_subject,
        "the last producer to assert the relationship is the one recorded"
    );

    tombstoned_and_unknown_edges_read_alike(store, &author, &key).await;
}

/// A tombstoned edge is absent from the read, like every other read path
/// (Soft Delete Contract), and so is a key that never existed -- the same
/// answer, because denied and nonexistent are indistinguishable.
async fn tombstoned_and_unknown_edges_read_alike(
    store: &dyn GraphStoreV1,
    ctx: &StoreCtx<'_>,
    key: &str,
) {
    store
        .soft_delete(ctx, DeleteRequest::Edge(key.to_owned()))
        .await
        .expect("the edge is tombstoned");
    assert!(
        store.get_edge(ctx, &key.to_owned()).await.is_err(),
        "a tombstoned edge is not returned by the edge read"
    );
    assert!(
        store
            .get_edge(ctx, &"no-such-edge".to_owned())
            .await
            .is_err(),
        "an unknown key answers the same way"
    );
}

/// The induced authorized subgraph, on the edge read: an edge is a statement
/// about two nodes, so seeing it while an endpoint is hidden would leak the
/// connectivity the node read refuses to.
pub async fn an_edge_whose_endpoint_is_hidden_is_not_readable(
    store: &dyn GraphStoreV1,
    one: Uuid,
    two: Uuid,
) {
    let scope_one = AccessScope::for_tenant(one);
    let ctx_one = ctx(one, &scope_one, None);
    seed_both_families(store, &ctx_one).await;
    ingest_batch(
        store,
        &ctx_one,
        batch(
            vec![node("iso-src", "src"), node("iso-dst", "dst")],
            vec![edge("iso-src", "iso-dst")],
        ),
    )
    .await
    .expect("the batch commits under the first tenant");
    let key = only_incident_edge_key(store, &ctx_one, "iso-src").await;

    let scope_two = AccessScope::for_tenant(two);
    let ctx_two = ctx(two, &scope_two, None);
    assert!(
        store.get_edge(&ctx_two, &key).await.is_err(),
        "another tenant's edge key reads as absent"
    );

    // And the endpoint half of the rule, within one tenant: tombstone one
    // endpoint and the edge stops being readable even though its own row is
    // the one the tombstone did not touch.
    store
        .soft_delete(&ctx_one, DeleteRequest::Node("iso-dst".to_owned()))
        .await
        .expect("the endpoint is tombstoned");
    assert!(
        store.get_edge(&ctx_one, &key).await.is_err(),
        "an edge with an invisible endpoint is not an edge the caller may see"
    );
}

// ---------------------------------------------------------------------------
// One adversarial fixture, every read surface
// ---------------------------------------------------------------------------

/// Seed one tenant with the trap: a node under a key the *other* tenant also
/// owns, a node only this tenant owns, an edge between them, and text both
/// tenants' nodes share so no search arm can tell them apart by content.
async fn seed_trap(store: &dyn GraphStoreV1, ctx: &StoreCtx<'_>, only: &str) {
    store
        .register_types(ctx, ontology_batch())
        .await
        .expect("ontology registers");
    ingest_batch(
        store,
        ctx,
        batch(
            vec![
                summarized("shared-key", "findable thing", "a shared summary"),
                summarized(only, "findable thing", "a shared summary"),
            ],
            vec![edge("shared-key", only)],
        ),
    )
    .await
    .expect("the trap commits");
}

/// `nfr-tenant-zero-leak` on every read surface the store port exposes, under
/// one fixture built to expose a leak rather than to be absent from one.
///
/// Each assertion names what it would have seen had the surface leaked, and
/// the other tenant's fixture is asserted to exist first: a guard test whose
/// trap quietly stopped being seeded passes for as long as nobody looks.
pub async fn no_read_surface_answers_with_another_tenants_rows(
    store: &dyn GraphStoreV1,
    one: Uuid,
    two: Uuid,
) {
    let ours_scope = AccessScope::for_tenant(one);
    let theirs_scope = AccessScope::for_tenant(two);
    let ours = ctx(one, &ours_scope, None);
    let theirs = ctx(two, &theirs_scope, None);
    seed_trap(store, &ours, "ours-only").await;
    seed_trap(store, &theirs, "theirs-only").await;

    // Precondition. Everything below asserts an absence, and an absence is
    // only evidence when the thing being looked for exists somewhere.
    let their_node = store
        .get_node(&theirs, &"theirs-only".to_owned(), 10)
        .await
        .expect("the other tenant's fixture exists");
    assert_eq!(their_node.adjacency.len(), 1, "with its edge");
    let their_id = store
        .resolve_node_ids(&theirs, &["theirs-only".to_owned()])
        .await
        .expect("resolution succeeds")
        .first()
        .expect("their key resolves")
        .1;

    the_node_read_stays_inside(store, &ours).await;
    resolution_and_hydration_stay_inside(store, &ours, their_id).await;
    the_projection_stays_inside(store, &ours).await;
    both_search_arms_stay_inside(store, &ours).await;
    topology_and_embedding_state_stay_inside(store, &ours).await;
}

/// The colliding key is ours, the other tenant's own key is not reachable,
/// and adjacency does not cross the boundary either.
async fn the_node_read_stays_inside(store: &dyn GraphStoreV1, ours: &StoreCtx<'_>) {
    let shared = store
        .get_node(ours, &"shared-key".to_owned(), 10)
        .await
        .expect("we see our own node");
    assert!(
        shared
            .adjacency
            .iter()
            .all(|entry| entry.neighbor_key == "ours-only"),
        "adjacency crossed the tenant boundary: {:?}",
        shared.adjacency
    );
    assert!(
        store
            .get_node(ours, &"theirs-only".to_owned(), 10)
            .await
            .is_err(),
        "another tenant's key must read as absent"
    );
}

/// Key resolution, where unknown and unauthorized are alike absent, and
/// hydration by internal id, which bypasses keys entirely -- the surface
/// where a missing tenant predicate would not show up as a key collision.
async fn resolution_and_hydration_stay_inside(
    store: &dyn GraphStoreV1,
    ours: &StoreCtx<'_>,
    their_id: graph_storage_sdk::models::NodeId,
) {
    let resolved = store
        .resolve_node_ids(ours, &["shared-key".to_owned(), "theirs-only".to_owned()])
        .await
        .expect("resolution succeeds");
    assert_eq!(
        resolved
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>(),
        vec!["shared-key".to_owned()],
        "resolution admitted a key that is not ours"
    );

    let hydrated = store
        .hydrate_nodes(ours, &[their_id])
        .await
        .expect("hydration succeeds");
    assert!(
        hydrated.is_empty(),
        "another tenant's internal id hydrated: {hydrated:?}"
    );
}

async fn the_projection_stays_inside(store: &dyn GraphStoreV1, ours: &StoreCtx<'_>) {
    let page = store
        .project_table(ours, ProjectionRequest::default())
        .await
        .expect("projection succeeds");
    let projected: Vec<String> = page.items.iter().map(|row| row.node_key.clone()).collect();
    assert!(
        projected.iter().any(|key| key == "ours-only"),
        "the projection must carry our own rows: {projected:?}"
    );
    assert_no_foreign_keys(&projected, "the projection");
}

/// Both arms, under text the two tenants share: a leak cannot hide behind
/// ranking, because it shows up as the other tenant's key or as two hits for
/// the colliding one.
async fn both_search_arms_stay_inside(store: &dyn GraphStoreV1, ours: &StoreCtx<'_>) {
    let lexical = store
        .search(
            ours,
            SearchRequest {
                mode: SearchMode::Lexical,
                query: Some("findable".to_owned()),
                arm_limit: 10,
                limit: 10,
                type_patterns: Vec::new(),
            },
            None,
        )
        .await
        .expect("search succeeds")
        .hits
        .into_iter()
        .map(|hit| hit.node_key)
        .collect::<Vec<_>>();
    assert!(
        lexical.contains(&"ours-only".to_owned()),
        "the lexical arm must find our own text first: {lexical:?}"
    );
    assert_no_foreign_keys(&lexical, "the lexical arm");

    let vector = search_vector(store, ours, "a shared summary", EPOCH).await;
    assert!(
        vector.contains(&"ours-only".to_owned()),
        "the vector arm must find our own text first: {vector:?}"
    );
    assert_no_foreign_keys(&vector, "the vector arm");
}

/// Topology is the widest surface of all -- it exists to hand a whole graph
/// to the analytics gear, so a missing predicate here hands over two -- and
/// embedding state decides what gets embedded, so a foreign key reading as
/// *known* would make the coordinator skip work it owes.
async fn topology_and_embedding_state_stay_inside(store: &dyn GraphStoreV1, ours: &StoreCtx<'_>) {
    let request = || graph_storage_sdk::models::TopologyRequest {
        cursor: None,
        page_size: Some(100),
    };
    if store.capabilities().topology {
        let topology = store
            .load_topology(ours, request())
            .await
            .expect("topology loads");
        let keys: Vec<String> = topology.nodes.iter().map(|(key, _)| key.clone()).collect();
        assert!(
            keys.contains(&"ours-only".to_owned()),
            "the topology must carry our own nodes: {keys:?}"
        );
        assert_no_foreign_keys(&keys, "the topology");
        for edge in &topology.edges {
            assert!(
                edge.src != "theirs-only" && edge.dst != "theirs-only",
                "the topology leaked an edge: {edge:?}"
            );
        }
    } else {
        // A store that declares the capability absent is not excused, it is
        // held to the other half of the contract: refuse, never approximate.
        assert!(
            matches!(
                store.load_topology(ours, request()).await,
                Err(GraphStoreError::Unsupported { .. })
            ),
            "a store without the topology capability must refuse it"
        );
    }

    let states = store
        .embedding_state(ours, &["theirs-only".to_owned()])
        .await
        .expect("embedding state reads");
    assert_eq!(
        states,
        vec![None],
        "another tenant's vector state is not ours"
    );
}

fn assert_no_foreign_keys(keys: &[String], what: &str) {
    assert!(
        !keys.iter().any(|key| key == "theirs-only"),
        "{what} returned another tenant's row: {keys:?}"
    );
    assert_eq!(
        keys.iter()
            .filter(|key| key.as_str() == "shared-key")
            .count(),
        usize::from(keys.iter().any(|key| key == "shared-key")),
        "{what} returned the colliding key more than once: {keys:?}"
    );
}
