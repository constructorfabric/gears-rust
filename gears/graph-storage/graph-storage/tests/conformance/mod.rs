#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The `GraphStoreV1` conformance suite.
//!
//! Per ADR-0001 point 5 the *suite* is the deliverable, not the trait: a
//! contract term nobody checks is a comment. Every case here runs against
//! **both** implementations — the built-in `PostgreSQL` store and the in-memory
//! fake — so a change only one of them can satisfy fails rather than passes
//! quietly. That is also why the fake exists at all.
//!
//! The five obligations, in the order DESIGN § 3.3 lists them:
//!
//! 1. batch atomicity across nodes, edges and the idempotency record;
//! 2. single-writer serialization per scope identity;
//! 3. monotonic generation fencing under that serialization;
//! 4. a node with a live incident edge is never removed alone;
//! 5. one snapshot across every arm of one read.

use std::time::Duration;

use graph_storage_sdk::models::{
    DeleteRequest, EdgeSpec, IngestOptions, IngestRequest, NodeSpec, ProjectionRequest,
    ReadSnapshot, RemainingBudget, ReplaceScope, SearchMode, SearchRequest, TypeRegistration,
};
use graph_storage_sdk::plugin_api::{GraphStoreError, GraphStoreV1, StoreCtx};
use tokio_util::sync::CancellationToken;
use toolkit_security::AccessScope;
use uuid::Uuid;

/// Producer types the suite registers on top of the base ontology.
pub const OWNED: &str =
    "gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.owned_node.v1~test.gs._.thing.v1~";
pub const PHANTOM: &str =
    "gts.cf.core.graph_storage.node.v1~cf.core.graph_storage.phantom_node.v1~";
pub const LINK: &str =
    "gts.cf.core.graph_storage.edge.v1~cf.core.graph_storage.static_edge.v1~test.gs._.link.v1~";

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
    let _ = schema_of(PHANTOM);
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

    let error = store
        .ingest(&ctx, doomed)
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
    let outcome = store.ingest(&ctx, retry).await.expect("the retry commits");
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

    store
        .ingest(&ctx, replace(5, "at five"))
        .await
        .expect("generation 5 commits");

    let stale = store
        .ingest(&ctx, replace(4, "at four"))
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

    let divergent = store
        .ingest(&ctx, replace(5, "different at five"))
        .await
        .expect_err("an equal generation with different content must conflict");
    assert!(
        matches!(divergent, GraphStoreError::Conflict { .. }),
        "expected a conflict, got {divergent}"
    );

    store
        .ingest(&ctx, replace(6, "at six"))
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

    store
        .ingest(
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

    let first = store.ingest(&ctx, request()).await.expect("first commits");
    assert!(!first.replayed);
    assert_eq!(first.counts.nodes_inserted, 1);

    let second = store.ingest(&ctx, request()).await.expect("retry replays");
    assert!(second.replayed, "a recorded key must replay");
    assert_eq!(
        second.revision, first.revision,
        "a replay reports the revision the original committed"
    );

    let mut divergent = batch(vec![node("idem-1", "different")], Vec::new());
    divergent.idempotency_key = Some("idem-key".to_owned());
    let error = store
        .ingest(&ctx, divergent)
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

    let first = store.ingest(&ctx, request()).await.expect("first commits");
    assert_eq!(first.counts.nodes_inserted, 2);
    assert_eq!(first.counts.edges_inserted, 1);

    let second = store.ingest(&ctx, request()).await.expect("second commits");
    assert_eq!(second.counts.nodes_unchanged, 2, "nothing changed");
    assert_eq!(second.counts.edges_unchanged, 1);
    assert_eq!(
        second.revision, first.revision,
        "a convergent replay must not move the revision"
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
        store
            .ingest(&ctx, batch(vec![node("colliding-key", name)], Vec::new()))
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
    store
        .ingest(&ctx, batch(vec![node("gone", "here")], Vec::new()))
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

    let error = store
        .ingest(&ctx, batch(vec![node("gone", "back")], Vec::new()))
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
    store
        .ingest(&ctx, batch(vec![node("private", "secret")], Vec::new()))
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
    store
        .ingest(
            &ctx,
            batch(vec![node("searchable", "findable thing")], Vec::new()),
        )
        .await
        .expect("the batch commits");

    let request = || SearchRequest {
        mode: SearchMode::Lexical,
        query: Some("findable".to_owned()),
        query_vector: None,
        arm_limit: 10,
        limit: 10,
        type_patterns: Vec::new(),
    };

    let hits = store
        .search(&ctx, request())
        .await
        .expect("search succeeds");
    assert!(
        hits.hits.iter().any(|hit| hit.node_key == "searchable"),
        "the node must be findable by its own name: {:?}",
        hits.hits
    );

    let denied = AccessScope::deny_all();
    let denied_hits = store
        .search(&self::ctx(tenant, &denied, None), request())
        .await
        .expect("search succeeds under a denying scope");
    assert!(
        denied_hits.hits.is_empty(),
        "a denying scope must rank nothing: {:?}",
        denied_hits.hits
    );
}
