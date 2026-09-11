#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The domain service, end to end over the in-memory store.
//!
//! The conformance suite calls `GraphStoreV1` directly, which is the point of
//! it: the store contract must hold whoever calls it. But everything between
//! the API edge and that trait -- authorization, admission bounds, ontology
//! resolution, the embedding coordinator's wiring, error mapping -- was
//! reached by no test at all, and it is the layer a REST request actually
//! goes through.

/// The suite's fixtures, reused here: the same ontology and the same node
/// and edge helpers, so a service case and a store case describe the same
/// graph. Only part of it is used from this binary, hence the allowance.
#[allow(dead_code)]
mod conformance;
mod support;

use graph_storage::domain::error::DomainError;
use graph_storage_sdk::models::{
    NeighborhoodRequest, SearchMode, SearchRequest, TraverseRequest, TypeQuery,
};
use support::Harness;

// ---------------------------------------------------------------------------
// Ontology
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_ontology_registers_and_reads_back_through_the_service() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    let registered = harness
        .services
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    assert!(
        registered.len() >= 2,
        "every submitted type is reported: {}",
        registered.len()
    );

    let record = harness
        .services
        .get_type(&ctx, &conformance::OWNED.to_owned())
        .await
        .expect("the producer type reads back");
    assert_eq!(record.type_id, conformance::OWNED);
    assert_eq!(
        record.effective_traits.family.as_deref(),
        Some("owned"),
        "traits are merged down the chain, not read off the leaf"
    );

    let page = harness
        .services
        .list_types(&ctx, TypeQuery::default())
        .await
        .expect("types list");
    assert!(
        page.items
            .iter()
            .any(|item| item.type_id == conformance::OWNED),
        "the list carries what was registered"
    );
}

#[tokio::test]
async fn a_schema_outside_its_declared_chain_is_refused_by_the_service() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let orphan = graph_storage_sdk::models::TypeRegistration {
        type_id: "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~test.gs._.orphan.v1~"
            .to_owned(),
        schema: serde_json::json!({
            "$id": "gts://gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~test.gs._.orphan.v1~",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.nobody.registered.this.v1~" }]
        }),
    };
    let error = harness
        .services
        .register_types(&ctx, vec![orphan])
        .await
        .expect_err("a schema that cannot compile is refused at registration");
    let rendered = error.to_string();
    assert!(
        matches!(
            error,
            DomainError::Validation { .. } | DomainError::InvalidArgument { .. }
        ),
        "expected a validation failure, got {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Ingest, and the bounds in front of it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_ingest_goes_through_authorization_admission_and_the_coordinator() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let outcome = harness
        .services
        .ingest(
            &ctx,
            conformance::batch(
                vec![
                    conformance::node("a", "first"),
                    conformance::node("b", "second"),
                ],
                vec![conformance::edge("a", "b")],
            ),
        )
        .await
        .expect("the batch commits");
    assert_eq!(outcome.counts.nodes_inserted, 2);
    assert_eq!(outcome.counts.edges_inserted, 1);
    assert!(outcome.revision.revision > 0, "the revision advanced");

    let view = harness
        .services
        .get_node(&ctx, &"a".to_owned(), None)
        .await
        .expect("the node reads back");
    assert!(
        view.has_embedding,
        "the service ran the batch through the embedding coordinator, not around it"
    );
}

#[tokio::test]
async fn a_batch_over_the_node_bound_is_refused_before_any_validation() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;
    let too_many = (0..=harness.services.config().ingest_max_nodes)
        .map(|i| conformance::node(&format!("n{i}"), "x"))
        .collect();

    let error = harness
        .services
        .ingest(&ctx, conformance::batch(too_many, Vec::new()))
        .await
        .expect_err("the bound is enforced");
    match error {
        DomainError::LimitExceeded { what } => assert!(
            what.contains("ingest_max_nodes"),
            "the refusal names the bound it enforced: {what}"
        ),
        other => panic!("expected a limit refusal, got {other}"),
    }
}

#[tokio::test]
async fn an_unregistered_type_fails_the_item_not_the_request() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let mut node = conformance::node("unknown-type", "x");
    node.type_id =
        "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~test.gs._.nope.v1~".to_owned();
    let error = harness
        .services
        .ingest(&ctx, conformance::batch(vec![node], Vec::new()))
        .await
        .expect_err("an unregistered type is refused");
    match error {
        DomainError::Validation { items } => {
            assert_eq!(items.len(), 1, "one item, one error: {items:?}");
        }
        other => panic!("expected per-item validation errors, got {other}"),
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_read_path_answers_through_the_service() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;
    harness
        .services
        .ingest(
            &ctx,
            conformance::batch(
                vec![
                    conformance::node("read-a", "findable alpha"),
                    conformance::node("read-b", "findable beta"),
                ],
                vec![conformance::edge("read-a", "read-b")],
            ),
        )
        .await
        .expect("the batch commits");

    // Node read, with the adjacency the edge created.
    let node = harness
        .services
        .get_node(&ctx, &"read-a".to_owned(), Some(5))
        .await
        .expect("the node reads");
    assert_eq!(node.adjacency.len(), 1);

    // Edge read, addressed by the key the node read handed out.
    let edge = harness
        .services
        .get_edge(&ctx, &node.adjacency[0].edge_key)
        .await
        .expect("the edge reads");
    assert_eq!((edge.src.as_str(), edge.dst.as_str()), ("read-a", "read-b"));

    // Projection.
    let page = harness
        .services
        .project_nodes(&ctx, &[], toolkit_odata::ODataQuery::default())
        .await
        .expect("the projection answers");
    assert_eq!(page.items.len(), 2, "both rows: {:?}", page.items);

    // Search.
    let hits = harness
        .services
        .search(
            &ctx,
            SearchRequest {
                mode: SearchMode::Lexical,
                query: Some("findable".to_owned()),
                arm_limit: 10,
                limit: 10,
                type_patterns: Vec::new(),
            },
        )
        .await
        .expect("search answers");
    assert_eq!(hits.hits.len(), 2, "both documents rank: {:?}", hits.hits);

    // Revision.
    let revision = harness
        .services
        .revision(&ctx)
        .await
        .expect("revision reads");
    assert!(revision.revision > 0);
}

#[tokio::test]
async fn a_read_bound_is_refused_rather_than_clamped() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let over = harness.services.config().node_read_max_adjacency + 1;
    let error = harness
        .services
        .get_node(&ctx, &"anything".to_owned(), Some(over))
        .await
        .expect_err("an adjacency limit above the ceiling is refused");
    assert!(
        matches!(error, DomainError::LimitExceeded { .. }),
        "expected a limit refusal, got {error}"
    );

    // A search mode that needs text and was given none is an inconsistent
    // combination (`LIMIT_COMBINATION`), not a breached bound: the two carry
    // different canonical reasons, and a client matches on the reason.
    let error = harness
        .services
        .search(
            &ctx,
            SearchRequest {
                mode: SearchMode::Hybrid,
                query: None,
                arm_limit: 10,
                limit: 10,
                type_patterns: Vec::new(),
            },
        )
        .await
        .expect_err("a search without text is refused");
    assert!(
        matches!(error, DomainError::LimitCombination { .. }),
        "expected a limit-combination refusal, got {error}"
    );
}

// ---------------------------------------------------------------------------
// Traversal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_traversal_walks_hops_and_a_neighborhood_answers_from_a_root() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;
    harness
        .services
        .ingest(
            &ctx,
            conformance::batch(
                vec![
                    conformance::node("hop-a", "a"),
                    conformance::node("hop-b", "b"),
                    conformance::node("hop-c", "c"),
                ],
                vec![
                    conformance::edge("hop-a", "hop-b"),
                    conformance::edge("hop-b", "hop-c"),
                ],
            ),
        )
        .await
        .expect("the batch commits");

    let walked = harness
        .services
        .traverse(
            &ctx,
            TraverseRequest {
                seeds: vec!["hop-a".to_owned()],
                depth: 2,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                max_nodes: Some(10),
            },
        )
        .await
        .expect("the traversal answers");
    let mut keys: Vec<String> = walked.nodes.iter().map(|n| n.node_key.clone()).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["hop-a".to_owned(), "hop-b".to_owned(), "hop-c".to_owned()],
        "two hops reach the whole chain"
    );

    let around = harness
        .services
        .neighborhood(
            &ctx,
            NeighborhoodRequest {
                root: "hop-b".to_owned(),
                depth: 1,
                node_budget: Some(10),
                include_phantoms: false,
            },
        )
        .await
        .expect("the neighborhood answers");
    assert_eq!(
        around.nodes.len(),
        3,
        "one hop around the middle reaches both sides: {:?}",
        around.nodes.iter().map(|n| &n.node_key).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_traversal_outside_its_bounds_is_refused() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let over_depth = harness.services.config().traversal_max_depth + 1;
    for (request, what) in [
        (
            TraverseRequest {
                seeds: Vec::new(),
                depth: 1,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                max_nodes: None,
            },
            "a traversal with no seed",
        ),
        (
            TraverseRequest {
                seeds: vec!["x".to_owned()],
                depth: over_depth,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                max_nodes: None,
            },
            "a depth above the ceiling",
        ),
    ] {
        assert!(
            harness.services.traverse(&ctx, request).await.is_err(),
            "{what} must be refused"
        );
    }
}

// ---------------------------------------------------------------------------
// Deletes, operations, and the denying PDP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_delete_tombstones_the_node_and_its_edge_together() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;
    harness
        .services
        .ingest(
            &ctx,
            conformance::batch(
                vec![
                    conformance::node("del-a", "a"),
                    conformance::node("del-b", "b"),
                ],
                vec![conformance::edge("del-a", "del-b")],
            ),
        )
        .await
        .expect("the batch commits");

    let outcome = harness
        .services
        .delete_node(&ctx, &"del-a".to_owned())
        .await
        .expect("the delete succeeds");
    assert_eq!(outcome.tombstoned_nodes, 1);
    assert_eq!(
        outcome.tombstoned_edges, 1,
        "an incident edge follows the node in the same transaction"
    );
    assert!(
        harness
            .services
            .get_node(&ctx, &"del-a".to_owned(), None)
            .await
            .is_err(),
        "a tombstoned node is absent from the read"
    );
}

#[tokio::test]
async fn readiness_answers_without_a_caller() {
    let harness = Harness::allowed();
    let readiness = harness.services.readiness().await;
    assert!(
        !readiness.components.is_empty(),
        "readiness names its components"
    );
}

#[tokio::test]
async fn the_namespace_surface_lists_and_transfers() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    assert!(
        harness
            .services
            .list_source_namespaces(&ctx)
            .await
            .expect("the list answers")
            .is_empty(),
        "nothing is claimed before anything is written"
    );

    // Assigning a namespace nobody has written under yet is a claim made on
    // someone's behalf -- the same administrative act, recorded the same way,
    // which is what lets an operator reserve a namespace before its producer
    // first runs.
    let assigned = harness
        .services
        .transfer_source_namespace(&ctx, "unclaimed", "mirror-gear")
        .await
        .expect("an unclaimed namespace can be pre-assigned");
    assert_eq!(assigned.owner_principal, "mirror-gear");
    assert_eq!(assigned.previous_owner, None, "there was no previous owner");
    assert!(
        assigned.transferred_by.is_some(),
        "the administrative act records who performed it"
    );

    let moved = harness
        .services
        .transfer_source_namespace(&ctx, "unclaimed", "other-gear")
        .await
        .expect("and then handed on");
    assert_eq!(moved.previous_owner.as_deref(), Some("mirror-gear"));

    let listed = harness
        .services
        .list_source_namespaces(&ctx)
        .await
        .expect("the list answers");
    assert_eq!(listed.len(), 1, "the boundary is visible: {listed:?}");
    assert_eq!(listed[0].owner_principal, "other-gear");
}

#[tokio::test]
async fn a_denying_pdp_stops_every_surface_before_the_store() {
    let harness = Harness::denied();
    let ctx = harness.ctx();

    let refusals = [
        harness
            .services
            .register_types(&ctx, conformance::ontology_batch())
            .await
            .err(),
        harness
            .services
            .ingest(&ctx, conformance::batch(Vec::new(), Vec::new()))
            .await
            .err(),
        harness
            .services
            .get_node(&ctx, &"x".to_owned(), None)
            .await
            .err(),
        harness.services.get_edge(&ctx, &"x".to_owned()).await.err(),
        harness
            .services
            .project_nodes(&ctx, &[], toolkit_odata::ODataQuery::default())
            .await
            .err(),
        harness.services.revision(&ctx).await.err(),
        harness
            .services
            .delete_node(&ctx, &"x".to_owned())
            .await
            .err(),
    ];
    for refusal in refusals {
        assert!(
            matches!(refusal, Some(DomainError::AccessDenied)),
            "a denied caller must be refused by the PEP, got {refusal:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The in-process client
// ---------------------------------------------------------------------------

/// The `ClientHub` client is the other entrance to the same building.
///
/// DESIGN says the in-process path is subject to the same admission and the
/// same authorization as REST because it goes through the same services --
/// a claim worth an assertion, since the cheap way to write a local client
/// is to reach past them.
#[tokio::test]
async fn the_local_client_answers_like_the_service_and_is_bounded_like_it() {
    use graph_storage::domain::local_client::GraphStorageLocalClient;
    use graph_storage_sdk::GraphStorageClientV1;

    let harness = Harness::allowed();
    let ctx = harness.ctx();
    let client = GraphStorageLocalClient::new(std::sync::Arc::clone(&harness.services));

    client
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers in-process");
    let outcome = client
        .ingest(
            &ctx,
            conformance::batch(
                vec![
                    conformance::node("local-a", "findable one"),
                    conformance::node("local-b", "findable two"),
                ],
                vec![conformance::edge("local-a", "local-b")],
            ),
        )
        .await
        .expect("the batch commits in-process");
    assert_eq!(outcome.counts.nodes_inserted, 2);

    let node = client
        .get_node(&ctx, &"local-a".to_owned(), None)
        .await
        .expect("the node reads");
    assert_eq!(node.adjacency.len(), 1);
    let edge_key = node.adjacency[0].edge_key.clone();

    assert_eq!(
        client
            .get_type(&ctx, &conformance::OWNED.to_owned())
            .await
            .expect("the type reads")
            .type_id,
        conformance::OWNED
    );
    assert!(
        !client
            .list_types(&ctx, TypeQuery::default())
            .await
            .expect("types list")
            .items
            .is_empty()
    );
    assert_eq!(
        client
            .project_nodes(&ctx, &[], toolkit_odata::ODataQuery::default())
            .await
            .expect("the projection answers")
            .items
            .len(),
        2
    );
    assert_eq!(
        client
            .search(
                &ctx,
                SearchRequest {
                    mode: SearchMode::Lexical,
                    query: Some("findable".to_owned()),
                    arm_limit: 10,
                    limit: 10,
                    type_patterns: Vec::new(),
                },
            )
            .await
            .expect("search answers")
            .hits
            .len(),
        2
    );
    assert_eq!(
        client
            .traverse(
                &ctx,
                TraverseRequest {
                    seeds: vec!["local-a".to_owned()],
                    depth: 1,
                    edge_type_patterns: Vec::new(),
                    node_type_patterns: Vec::new(),
                    max_nodes: Some(10),
                },
            )
            .await
            .expect("the traversal answers")
            .nodes
            .len(),
        2
    );
    assert!(
        !client
            .neighborhood(
                &ctx,
                NeighborhoodRequest {
                    root: "local-a".to_owned(),
                    depth: 1,
                    node_budget: Some(10),
                    include_phantoms: false,
                },
            )
            .await
            .expect("the neighborhood answers")
            .nodes
            .is_empty()
    );
    assert!(
        client
            .revision(&ctx)
            .await
            .expect("revision reads")
            .revision
            > 0,
        "the in-process path reports the same revision surface"
    );

    // The same bound, refused the same way -- and rendered as a canonical
    // error, because that is what an in-process caller receives.
    let over = harness.services.config().node_read_max_adjacency + 1;
    let refused = client
        .get_node(&ctx, &"local-a".to_owned(), Some(over))
        .await
        .expect_err("the in-process path is bounded too");
    assert_eq!(
        refused.status_code(),
        400,
        "the same refusal, rendered through the same mapping: {refused:?}"
    );

    let deleted = client
        .delete_edge(&ctx, &edge_key)
        .await
        .expect("the edge is deleted in-process");
    assert_eq!(deleted.tombstoned_edges, 1);
    assert_eq!(
        client
            .delete_node(&ctx, &"local-a".to_owned())
            .await
            .expect("the node is deleted in-process")
            .tombstoned_nodes,
        1
    );
}

/// A hub's neighbourhood keeps the structural core when the budget cuts it,
/// not whichever leaves happened to be reached first.
///
/// `fr-neighborhood-projection` asks for retained nodes to be ordered by
/// degree "so truncation keeps the structural core", and the PRD's own
/// alternative flow spells out the case: a dense hub truncates to the
/// *highest-degree* neighbours. Before this, retention was arrival order by
/// internal id — for a UI that can draw 200 of a hub's 5 000 neighbours, 200
/// arbitrary leaves.
///
/// The fixture makes the two orders disagree on purpose. `hub` has six
/// neighbours; the last three by id are the connected ones, so arrival order
/// and degree order are exact opposites and a passing assertion cannot be an
/// accident of insertion order.
#[tokio::test]
async fn a_budgeted_neighborhood_keeps_the_best_connected_neighbours() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    // Ingest order is the id order: leaves first, then the well-connected
    // three, then the far nodes that give them their degree.
    let mut nodes = vec![conformance::node("hub", "hub")];
    for leaf in ["leaf-1", "leaf-2", "leaf-3"] {
        nodes.push(conformance::node(leaf, leaf));
    }
    for core in ["core-1", "core-2", "core-3"] {
        nodes.push(conformance::node(core, core));
    }
    for far in ["far-1", "far-2", "far-3", "far-4", "far-5", "far-6"] {
        nodes.push(conformance::node(far, far));
    }

    let mut edges = Vec::new();
    for neighbour in ["leaf-1", "leaf-2", "leaf-3", "core-1", "core-2", "core-3"] {
        edges.push(conformance::edge("hub", neighbour));
    }
    // Each `core-*` carries two edges of its own; every `leaf-*` has only the
    // one that ties it to the hub.
    for (core, far) in [
        ("core-1", "far-1"),
        ("core-1", "far-2"),
        ("core-2", "far-3"),
        ("core-2", "far-4"),
        ("core-3", "far-5"),
        ("core-3", "far-6"),
    ] {
        edges.push(conformance::edge(core, far));
    }

    harness
        .services
        .ingest(&ctx, conformance::batch(nodes, edges))
        .await
        .expect("the hub commits");

    // Budget four: the root plus three of its six neighbours.
    let around = harness
        .services
        .neighborhood(
            &ctx,
            NeighborhoodRequest {
                root: "hub".to_owned(),
                depth: 1,
                node_budget: Some(4),
                include_phantoms: false,
            },
        )
        .await
        .expect("the neighborhood answers");

    let mut kept: Vec<String> = around.nodes.iter().map(|n| n.node_key.clone()).collect();
    kept.sort();
    assert_eq!(
        kept,
        vec![
            "core-1".to_owned(),
            "core-2".to_owned(),
            "core-3".to_owned(),
            "hub".to_owned(),
        ],
        "the budget keeps the root and the three connected neighbours, not the leaves"
    );
    assert!(
        around.truncated.is_some(),
        "a truncated neighborhood says so"
    );

    // And the same walk without a binding budget still answers with all of
    // them, so the ordering is a retention rule and not a filter.
    let whole = harness
        .services
        .neighborhood(
            &ctx,
            NeighborhoodRequest {
                root: "hub".to_owned(),
                depth: 1,
                node_budget: Some(50),
                include_phantoms: false,
            },
        )
        .await
        .expect("the neighborhood answers");
    assert_eq!(whole.nodes.len(), 7, "the root and all six neighbours");
    assert!(whole.truncated.is_none());
}
