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

use graph_storage_sdk::models::{
    DeleteRequest, EdgeSpec, IngestOptions, IngestRequest, ItemFamily, NodeSpec, ProjectionRequest,
    ReadSnapshot, RemainingBudget, ReplaceScope, SearchMode, SearchRequest, TypeRegistration,
};
use graph_storage::domain::embedding::{EmbeddingCoordinator, SpaceState};
use graph_storage::infra::embedding::fake::FakeEmbeddingProvider;
use graph_storage_sdk::plugin_api::{EmbeddingPlan, GraphStoreError, GraphStoreV1, StoreCtx};
use tokio_util::sync::CancellationToken;
use toolkit_security::AccessScope;
use uuid::Uuid;

/// Producer types the suite registers on top of the base ontology.
pub const OWNED: &str =
    "gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.owned_node.v1~test.gs._.thing.v1~";
pub const PHANTOM: &str =
    "gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.phantom_node.v1~";
/// An edge type that admits only owned nodes at either end — the constraint
/// that gives the endpoint check something to refuse.
pub const OWNED_ONLY: &str = "gts.cf.core.graph_storage.edge.v1~cf.core.graph_storage.static_edge.v1~test.gs._.owned_link.v1~";
pub const OWNED_FAMILY: &str =
    "gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.owned_node.v1~";
/// A node type from a different family, which `OWNED_ONLY` must refuse.
pub const REFERENCE: &str = "gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.reference_node.v1~test.gs._.mirror.v1~";
pub const LINK: &str =
    "gts.cf.core.graph_storage.edge.v1~cf.core.graph_storage.static_edge.v1~test.gs._.link.v1~";

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

/// Vector width and epoch the suite runs under. The epoch is arbitrary but
/// non-default on purpose: a store that ignored it and stamped, say, 1 would
/// still satisfy a suite that used 1.
pub const DIMENSION: u32 = 8;
pub const EPOCH: i64 = 42;

async fn plan_for(
    store: &(impl GraphStoreV1 + ?Sized),
    ctx: &StoreCtx<'_>,
    request: &IngestRequest,
) -> EmbeddingPlan {
    // The `vector_search` trait of each node's type, resolved the way the
    // domain service resolves it.
    let mut paths: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for node in &request.nodes {
        if !paths.contains_key(&node.type_id)
            && let Ok(record) = store.get_type(ctx, &node.type_id).await
        {
            paths.insert(node.type_id.clone(), record.effective_traits.vector_search);
        }
    }
    let empty: Vec<String> = Vec::new();
    let nodes = coordinator()
        .plan(
            &request.nodes,
            request.options.embed.unwrap_or(true),
            |node| {
                paths
                    .get(&node.type_id)
                    .map_or(empty.as_slice(), Vec::as_slice)
            },
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

fn schema_of(base: &str) -> serde_json::Value {
    graph_storage::domain::ontology::BASE_SCHEMAS
        .iter()
        .find(|(id, _)| *id == base)
        .map_or_else(
            || panic!("no base schema for {base}"),
            |(_, raw)| serde_json::from_str(raw).expect("base schema parses"),
        )
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
            "x-gts-traits": { "full_text_search": ["/name"] },
            "type": "object",
            "allOf": [
                { "$ref": "gts://gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.owned_node.v1~" }
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
                { "$ref": "gts://gts.cf.core.graph_storage.edge.v1~cf.core.graph_storage.static_edge.v1~" }
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

/// Build a per-call context. Tests own the scope explicitly so an assertion
/// about isolation is an assertion about the store, not about a PDP.
pub fn ctx<'a>(
    tenant: Uuid,
    scope: &'a AccessScope,
    snapshot: Option<&'a ReadSnapshot>,
) -> StoreCtx<'a> {
    StoreCtx {
        tenant,
        scope,
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

    let error = ingest_batch(
            store,&ctx, doomed)
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
    let outcome = ingest_batch(store, &ctx, retry).await.expect("the retry commits");
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

    ingest_batch(
            store,&ctx, replace(5, "at five"))
        .await
        .expect("generation 5 commits");

    let stale = ingest_batch(
            store,&ctx, replace(4, "at four"))
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

    let divergent = ingest_batch(
            store,&ctx, replace(5, "different at five"))
        .await
        .expect_err("an equal generation with different content must conflict");
    assert!(
        matches!(divergent, GraphStoreError::Conflict { .. }),
        "expected a conflict, got {divergent}"
    );

    ingest_batch(
            store,&ctx, replace(6, "at six"))
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
    let outcome = ingest_batch(
            store,&ctx, batch(vec![node("first", "first")], Vec::new()))
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

    let first = ingest_batch(store, &ctx, request()).await.expect("first commits");
    assert!(!first.replayed);
    assert_eq!(first.counts.nodes_inserted, 1);

    let second = ingest_batch(store, &ctx, request()).await.expect("retry replays");
    assert!(second.replayed, "a recorded key must replay");
    assert_eq!(
        second.revision, first.revision,
        "a replay reports the revision the original committed"
    );

    let mut divergent = batch(vec![node("idem-1", "different")], Vec::new());
    divergent.idempotency_key = Some("idem-key".to_owned());
    let error = ingest_batch(
            store,&ctx, divergent)
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

    let first = ingest_batch(store, &ctx, request()).await.expect("first commits");
    assert_eq!(first.counts.nodes_inserted, 2);
    assert_eq!(first.counts.edges_inserted, 1);

    let second = ingest_batch(store, &ctx, request()).await.expect("second commits");
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
            "allOf": [{ "$ref": "gts://gts.cf.core.graph_storage.edge.v1~cf.core.graph_storage.static_edge.v1~" }]
        }),
    });
    batch.push(TypeRegistration {
        type_id: REFERENCE.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{REFERENCE}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.reference_node.v1~" }]
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
            "allOf": [{ "$ref": "gts://gts.cf.core.graph_storage.edge.v1~cf.core.graph_storage.static_edge.v1~" }]
        }),
    });
    batch.push(TypeRegistration {
        type_id: REFERENCE.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{REFERENCE}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.cf.core.graph_storage.reference_node.v1~" }]
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
            store,&ctx, batch_of(vec![node("sys:repo:9", "late")], Vec::new()))
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
                store,&ctx, batch(vec![node("colliding-key", name)], Vec::new()))
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
    ingest_batch(
            store,&ctx, batch(vec![node("gone", "here")], Vec::new()))
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

    let error = ingest_batch(
            store,&ctx, batch(vec![node("gone", "back")], Vec::new()))
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
            store,&ctx, batch(vec![node("private", "secret")], Vec::new()))
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
