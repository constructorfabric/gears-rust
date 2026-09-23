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

use std::sync::Arc;

use graph_storage::config::GraphStorageConfig;
use graph_storage::domain::error::DomainError;
use graph_storage::infra::fake_store::FakeGraphStore;
use graph_storage_sdk::models::{
    NeighborhoodRequest, NodeSpec, SearchMode, SearchRequest, TraverseRequest, TruncationReason,
    TypeQuery,
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
        type_id: "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~acme.gs._.orphan.v1~"
            .to_owned(),
        schema: serde_json::json!({
            "$id": "gts://gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~acme.gs._.orphan.v1~",
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
        "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~acme.gs._.nope.v1~".to_owned();
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
    assert_eq!(
        walked.seeds,
        vec!["hop-a".to_owned()],
        "the answer says which seeds it actually started from"
    );

    // A seed the caller cannot see is absent from the echo, exactly as it is
    // absent from every other read: the walk answers about the seeds that
    // survived authorization, and says which those were.
    let partly = harness
        .services
        .traverse(
            &ctx,
            TraverseRequest {
                seeds: vec![
                    "hop-a".to_owned(),
                    "hop-a".to_owned(),
                    "never-existed".to_owned(),
                ],
                depth: 1,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                max_nodes: Some(10),
            },
        )
        .await
        .expect("the traversal answers");
    assert_eq!(
        partly.seeds,
        vec!["hop-a".to_owned()],
        "duplicates collapse and an unknown seed is absent, not an error"
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

    // A principal is compared to the writer's exactly, and a writer's never
    // carries surrounding whitespace or a control character. Accepting one
    // would report a successful transfer and leave the namespace writable by
    // nobody, so it is refused -- and the owner on record is untouched.
    for stranded in ["other-gear ", " other-gear", "other-gear\n"] {
        let error = harness
            .services
            .transfer_source_namespace(&ctx, "unclaimed", stranded)
            .await
            .expect_err("a principal no writer could equal is not a transfer");
        assert!(
            matches!(error, DomainError::InvalidQuery { .. }),
            "{stranded:?} must be refused as invalid, got {error:?}"
        );
    }

    let after = harness
        .services
        .list_source_namespaces(&ctx)
        .await
        .expect("the list answers");
    assert_eq!(
        after[0].owner_principal, "other-gear",
        "a refused transfer leaves the owner where it was"
    );
}

#[tokio::test]
async fn an_unauthorized_transfer_answers_the_same_whatever_it_was_given() {
    // The shape checks on this method used to run before `authorize`, so a
    // caller with no ADMIN could tell a malformed namespace (`400
    // invalid_argument`) from a well-formed one (`404` via the denial) and
    // read the format rule out of the difference. Authorization decides
    // first now, and the answer no longer varies with an input this caller was
    // never entitled to submit.
    let harness = Harness::denied();
    let ctx = harness.ctx();

    let mut refusals = Vec::new();
    for (namespace, principal) in [
        ("github", "mirror-gear"),     // both well-formed
        ("git\nhub", "mirror-gear"),   // a namespace the shape check rejects
        ("  github  ", "mirror-gear"), // and another
        ("github", "mirror-gear "),    // a principal the shape check rejects
    ] {
        let refused = harness
            .services
            .transfer_source_namespace(&ctx, namespace, principal)
            .await
            .expect_err("a denied caller never transfers anything");
        refusals.push(std::mem::discriminant(&refused));
    }

    assert!(
        refusals.windows(2).all(|pair| pair[0] == pair[1]),
        "every refusal must be the same variant, whatever the input looked like"
    );
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

/// No new work starts once the deadline is gone.
///
/// Every request opens with an absolute budget and, until now, only the
/// embedding and type-evolution loops ever looked at it. A traversal whose
/// ten seconds elapsed on its first hop went on issuing the rest; a
/// producer-sized ingest went on issuing tens of thousands of statements
/// inside one transaction, holding one connection from a pool the whole
/// tenant shares. The client had stopped waiting either way.
///
/// A zero budget is the deadline already gone, which is the state every
/// expired request passes through -- and the only one a test can be in
/// deterministically, since waiting for a real ten seconds to elapse is a
/// test that measures the machine.
#[tokio::test]
async fn no_new_work_starts_after_the_budget_is_spent() {
    let expired = GraphStorageConfig {
        deadline_interactive_secs: 0,
        ..GraphStorageConfig::default()
    };
    // The bound is one second, so the config has to be built past its own
    // validation -- which is the point: this state is not configurable, it is
    // what every request becomes on its way out.
    assert!(
        expired.validate().is_err(),
        "a zero deadline is not a configuration anyone can set"
    );

    // A PDP that counts, so the claim in this test's name is one the test can
    // fail on. Asserting the returned variant alone cannot: a deadline check
    // moved below `authorize` still answers `Deadline`, and the round trip it
    // wasted would be invisible.
    let pdp = Arc::new(support::CountingPdp::default());
    let harness = Harness::configured(
        Arc::clone(&pdp) as Arc<dyn authz_resolver_sdk::api::AuthZResolverApi>,
        expired,
    );
    let ctx = harness.ctx();

    // No fixture, and that is the assertion: the refusal happens before the
    // policy call and therefore before anything looks at data, so a read of a
    // key that was never written answers `deadline_exceeded` rather than
    // `not_found`. Seeding would not be possible under this config anyway --
    // a registration is an operation too, and it is refused for the same
    // reason.
    let refused = harness
        .services
        .ingest(
            &ctx,
            conformance::batch(vec![conformance::node("late-a", "a")], Vec::new()),
        )
        .await
        .expect_err("an ingest does not start under a spent budget");
    assert!(
        matches!(refused, DomainError::Deadline),
        "expected a deadline refusal, got {refused}"
    );

    let refused = harness
        .services
        .traverse(
            &ctx,
            TraverseRequest {
                seeds: vec!["late-a".to_owned()],
                depth: 2,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                max_nodes: Some(10),
            },
        )
        .await
        .expect_err("a compound read does not start under a spent budget");
    assert!(
        matches!(refused, DomainError::Deadline),
        "expected a deadline refusal, got {refused}"
    );

    let search = harness.services.search(
        &ctx,
        SearchRequest {
            mode: SearchMode::Lexical,
            query: Some("anything".to_owned()),
            arm_limit: 10,
            limit: 10,
            type_patterns: Vec::new(),
        },
    );
    let reads: Vec<(&str, DomainError)> = vec![
        (
            "get_node",
            harness
                .services
                .get_node(&ctx, &"late-a".to_owned(), Some(10))
                .await
                .expect_err("get_node does not start"),
        ),
        ("search", search.await.expect_err("search does not start")),
        (
            "list_types",
            harness
                .services
                .list_types(&ctx, TypeQuery::default())
                .await
                .expect_err("the catalogue does not start"),
        ),
        (
            "project_nodes",
            harness
                .services
                .project_nodes(&ctx, &[], toolkit_odata::ODataQuery::default())
                .await
                .expect_err("the projection does not start"),
        ),
        (
            "revision",
            harness
                .services
                .revision(&ctx)
                .await
                .expect_err("the revision read does not start"),
        ),
        (
            "neighborhood",
            harness
                .services
                .neighborhood(
                    &ctx,
                    NeighborhoodRequest {
                        root: "late-a".to_owned(),
                        depth: 1,
                        node_budget: Some(10),
                        include_phantoms: false,
                    },
                )
                .await
                .expect_err("the neighborhood does not start"),
        ),
    ];
    for (what, error) in reads {
        assert!(
            matches!(error, DomainError::Deadline),
            "{what} must refuse a spent deadline, got {error}"
        );
    }

    // The claim this case is named for. Every call above returned `Deadline`,
    // which it would have done just as well with the check below the policy
    // call -- so the variant is not the assertion. This is: the PDP was never
    // asked, because no work starts once the budget is spent.
    assert_eq!(
        pdp.calls(),
        0,
        "the policy decision point must not be reached by a request that is already over \
         its deadline"
    );
}

/// A store that declares it has no snapshots is taken at its word.
///
/// The declaration existed and the service ignored it: it opened a handle
/// anyway, threaded it through every arm, and stamped the answer with the
/// handle's revision. So `snapshots = false` protected nobody, and the
/// response named a graph state it had never existed at -- which no consumer
/// can detect and every revision-keyed cache would believe.
///
/// The fake honours the obligation, which is exactly why it could not catch
/// this before: its handle worked, so nothing downstream noticed it was being
/// asked for under false pretences. `declining_snapshots` panics if asked.
#[tokio::test]
async fn a_store_without_snapshots_is_not_asked_for_one() {
    let store = Arc::new(FakeGraphStore::declining_snapshots(0));
    let harness = Harness::over(store, Arc::new(support::AllowInOwnTenant));
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;
    harness
        .services
        .ingest(
            &ctx,
            conformance::batch(
                vec![conformance::node("s-a", "a"), conformance::node("s-b", "b")],
                vec![conformance::edge("s-a", "s-b")],
            ),
        )
        .await
        .expect("the batch commits");

    let walked = harness
        .services
        .traverse(
            &ctx,
            TraverseRequest {
                seeds: vec!["s-a".to_owned()],
                depth: 1,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                max_nodes: Some(10),
            },
        )
        .await
        .expect("the traversal answers without a snapshot");

    assert_eq!(walked.nodes.len(), 2, "the walk still works: {walked:?}");
    assert!(
        walked.consistent_snapshot,
        "nothing was written while it ran, so the arms agree even without a snapshot"
    );
}

/// A walk that spanned a commit says so rather than stamping a revision it
/// never existed at.
#[tokio::test]
async fn a_walk_that_spans_a_commit_is_reported_as_inconsistent() {
    // One revision per read, which is what a tenant being written to by
    // somebody else looks like from here. Forcing that interleaving against a
    // real store is a race, and a race asserts nothing on the run where it
    // does not happen.
    let store = Arc::new(FakeGraphStore::declining_snapshots(1));
    let harness = Harness::over(store, Arc::new(support::AllowInOwnTenant));
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;
    harness
        .services
        .ingest(
            &ctx,
            conformance::batch(vec![conformance::node("d-a", "a")], Vec::new()),
        )
        .await
        .expect("the batch commits");

    let walked = harness
        .services
        .traverse(
            &ctx,
            TraverseRequest {
                seeds: vec!["d-a".to_owned()],
                depth: 1,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                max_nodes: Some(10),
            },
        )
        .await
        .expect("the traversal answers");

    assert!(
        !walked.consistent_snapshot,
        "the revision moved under the walk, and the answer says so"
    );
}

/// A traversal stops at the byte budget and says so.
///
/// Traversal is the one read whose count ceiling cannot stand in for a size
/// ceiling: `traversal_max_nodes` elements of `item_max_bytes` each is
/// gigabytes at the hard limits, and every number in that is legal. So it
/// measures while it hydrates, and the cut is reported rather than silent --
/// a short answer that claimed to be complete is the failure mode worth
/// avoiding, because the caller cannot tell it from a small graph.
#[tokio::test]
async fn a_traversal_stops_at_the_byte_budget_and_reports_it() {
    let small = GraphStorageConfig {
        response_max_bytes: 4 * 1024,
        ..GraphStorageConfig::default()
    };
    let harness = Harness::configured(Arc::new(support::AllowInOwnTenant), small);
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    // Four nodes in a chain, each carrying more than a quarter of the budget.
    let filler = "z".repeat(1_500);
    let fat = |key: &str| NodeSpec {
        payload: Some(serde_json::json!({ "note": filler })),
        ..conformance::node(key, key)
    };
    harness
        .services
        .ingest(
            &ctx,
            conformance::batch(
                vec![fat("b-a"), fat("b-b"), fat("b-c"), fat("b-d")],
                vec![
                    conformance::edge("b-a", "b-b"),
                    conformance::edge("b-b", "b-c"),
                    conformance::edge("b-c", "b-d"),
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
                seeds: vec!["b-a".to_owned()],
                depth: 3,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                // Well inside the node budget: the count is not what stops it.
                max_nodes: Some(100),
            },
        )
        .await
        .expect("the traversal answers");

    assert!(
        walked.nodes.len() < 4,
        "the budget cut the answer short: {} nodes",
        walked.nodes.len()
    );
    assert_eq!(
        walked.truncated,
        Some(TruncationReason::ResponseBytes),
        "and the cut is reported as a byte budget rather than a node budget"
    );

    // Every edge still names two nodes that came back, so a caller drawing
    // the result has no line to nothing.
    let returned: std::collections::BTreeSet<&str> =
        walked.nodes.iter().map(|n| n.node_key.as_str()).collect();
    for edge in &walked.edges {
        assert!(
            returned.contains(edge.src.as_str()) && returned.contains(edge.dst.as_str()),
            "the edge filter follows the byte cut: {edge:?} against {returned:?}"
        );
    }
}

/// A projection page is refused rather than trimmed when it hydrates past the
/// budget.
///
/// The opposite of what traversal does, and the cursor is why. The
/// continuation token is minted for the rows the statement returned, so
/// dropping rows behind it and handing it back would make the client resume
/// past rows it never saw. Losing rows silently is worse than a refusal the
/// caller can act on by asking for fewer.
#[tokio::test]
async fn an_oversized_projection_page_is_refused_rather_than_trimmed() {
    let small = GraphStorageConfig {
        response_max_bytes: 4 * 1024,
        ..GraphStorageConfig::default()
    };
    let harness = Harness::configured(Arc::new(support::AllowInOwnTenant), small);
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let filler = "p".repeat(1_500);
    let fat = |key: &str| NodeSpec {
        payload: Some(serde_json::json!({ "note": filler })),
        ..conformance::node(key, key)
    };
    harness
        .services
        .ingest(
            &ctx,
            conformance::batch(
                vec![fat("p-a"), fat("p-b"), fat("p-c"), fat("p-d")],
                Vec::new(),
            ),
        )
        .await
        .expect("the batch commits");

    let refused = harness
        .services
        .project_nodes(&ctx, &[], toolkit_odata::ODataQuery::default())
        .await
        .expect_err("the page hydrates past the budget");
    assert!(
        matches!(refused, DomainError::LimitExceeded { .. }),
        "expected a bound refusal, got {refused}"
    );
    assert!(
        refused.to_string().contains("response_max_bytes") && refused.to_string().contains("$top"),
        "the refusal names the bound and what to do about it: {refused}"
    );
}

/// A search hit list is cut at the budget and says so.
#[tokio::test]
async fn an_oversized_hit_list_is_cut_and_reported() {
    let small = GraphStorageConfig {
        // Smaller than the names below add up to, larger than one of them.
        response_max_bytes: 1_024,
        ..GraphStorageConfig::default()
    };
    let harness = Harness::configured(Arc::new(support::AllowInOwnTenant), small);
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    // Names are caller-controlled text and travel on every hit, so they are
    // what a hit costs once the ranking stopped reading payloads.
    let long = "s".repeat(400);
    let nodes: Vec<NodeSpec> = (0..6)
        .map(|i| conformance::node(&format!("hit-{i}"), &format!("{long}-{i}")))
        .collect();
    harness
        .services
        .ingest(&ctx, conformance::batch(nodes, Vec::new()))
        .await
        .expect("the batch commits");

    let found = harness
        .services
        .search(
            &ctx,
            SearchRequest {
                mode: SearchMode::Lexical,
                query: Some(long.clone()),
                arm_limit: 50,
                limit: 50,
                type_patterns: Vec::new(),
            },
        )
        .await
        .expect("the search answers");

    assert!(
        found.hits.len() < 6,
        "the budget cut the list: {} hits",
        found.hits.len()
    );
    assert_eq!(
        found.truncated,
        Some(TruncationReason::ResponseBytes),
        "and a short list says why, since a small graph looks the same"
    );
}

/// The edges are charged to the budget rather than carried for free.
///
/// The first version measured the nodes and appended the edges afterwards,
/// which is a budget on half the answer -- and on the half that grows
/// fastest, since a dense neighbourhood has many more edges than nodes and
/// each carries four caller-controlled strings.
///
/// The seed invariant is asserted here too, but it is not what this case
/// proves. Seeds are hydrated at the front of the list, so a cut reaches one
/// only when the seeds do not fit at all -- which is
/// `seeds_that_cannot_fit_the_budget_are_refused`, and that case does fail
/// without the exemption.
#[tokio::test]
async fn the_byte_budget_charges_the_edges_too() {
    /// The star's leaves. Enough of them that the edges outweigh the nodes,
    /// which is the whole point of the fixture.
    const LEAVES: usize = 8;

    // A star with small nodes: every node fits comfortably, so whatever the
    // budget cuts is the edges and only the edges. A chain of fat nodes
    // cannot show this -- there the node cut removes endpoints and the edge
    // filter drops their edges as a consequence, which looks identical from
    // the outside whether or not the edges were ever charged.
    // Nine small nodes are a few hundred bytes; eight edges are more, because
    // an edge carries two keys and two GTS identifiers while a node carries
    // one of each and no adjacency (traversal hydration does not fill it).
    let small = GraphStorageConfig {
        response_max_bytes: 1_536,
        ..GraphStorageConfig::default()
    };
    let harness = Harness::configured(Arc::new(support::AllowInOwnTenant), small);
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let mut nodes = vec![conformance::node("hub", "hub")];
    let mut edges = Vec::new();
    for index in 0..LEAVES {
        let leaf = format!("leaf-{index}");
        edges.push(conformance::edge("hub", &leaf));
        nodes.push(conformance::node(&leaf, &leaf));
    }
    harness
        .services
        .ingest(&ctx, conformance::batch(nodes, edges))
        .await
        .expect("the batch commits");

    let walked = harness
        .services
        .traverse(
            &ctx,
            TraverseRequest {
                seeds: vec!["hub".to_owned()],
                depth: 1,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                max_nodes: Some(100),
            },
        )
        .await
        .expect("the traversal answers");

    let returned: std::collections::BTreeSet<&str> =
        walked.nodes.iter().map(|n| n.node_key.as_str()).collect();
    for seed in &walked.seeds {
        assert!(
            returned.contains(seed.as_str()),
            "a seed the answer names is a seed the answer contains: {seed} not in {returned:?}"
        );
    }
    assert_eq!(
        walked.nodes.len(),
        LEAVES + 1,
        "every node fits, so the nodes are not what the budget cut: {returned:?}"
    );
    assert!(
        walked.edges.len() < LEAVES,
        "the edges are what it cut, which is only possible if they were \
         charged: {} of {LEAVES}",
        walked.edges.len()
    );
    assert_eq!(
        walked.truncated,
        Some(TruncationReason::ResponseBytes),
        "and the cut is reported"
    );
    for edge in &walked.edges {
        assert!(
            returned.contains(edge.src.as_str()) && returned.contains(edge.dst.as_str()),
            "an edge names two returned nodes: {edge:?}"
        );
    }
}

/// Seeds that do not fit are a refusal, not a silent short answer.
///
/// Seeds are exempt from the cut, so when they alone exceed the budget there
/// is no honest truncation left to make: answering would break the promise
/// the exemption exists to keep.
#[tokio::test]
async fn seeds_that_cannot_fit_the_budget_are_refused() {
    let tiny = GraphStorageConfig {
        response_max_bytes: 2 * 1024,
        ..GraphStorageConfig::default()
    };
    let harness = Harness::configured(Arc::new(support::AllowInOwnTenant), tiny);
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let filler = "w".repeat(1_500);
    let fat = |key: &str| NodeSpec {
        payload: Some(serde_json::json!({ "note": filler })),
        ..conformance::node(key, key)
    };
    harness
        .services
        .ingest(
            &ctx,
            conformance::batch(vec![fat("s-1"), fat("s-2")], Vec::new()),
        )
        .await
        .expect("the batch commits");

    let refused = harness
        .services
        .traverse(
            &ctx,
            TraverseRequest {
                seeds: vec!["s-1".to_owned(), "s-2".to_owned()],
                depth: 1,
                edge_type_patterns: Vec::new(),
                node_type_patterns: Vec::new(),
                max_nodes: Some(10),
            },
        )
        .await
        .expect_err("two seeds larger than the budget cannot be answered");
    assert!(
        matches!(refused, DomainError::LimitExceeded { .. }),
        "expected a bound refusal, got {refused}"
    );
    assert!(
        refused.to_string().contains("the seeds alone"),
        "the refusal says which part did not fit: {refused}"
    );
}

/// A batch is bounded by its total size, not only by its counts.
///
/// Every per-item check can pass for a request no process survives: fifty
/// thousand elements each just under the item ceiling is gigabytes, and the
/// count limit, the payload limit and the identifier limit all say yes. The
/// admission comment claimed no oversized work was ever started; for one item
/// that was true and for a batch it was not.
#[tokio::test]
async fn a_batch_is_bounded_by_its_total_size_and_not_only_its_counts() {
    let small = GraphStorageConfig {
        // Room for a handful of the payloads below, not for all of them.
        ingest_max_bytes: 16 * 1024,
        ..GraphStorageConfig::default()
    };
    let harness = Harness::configured(Arc::new(support::AllowInOwnTenant), small);
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let filler = "x".repeat(2 * 1024);
    let fat = |key: &str| NodeSpec {
        payload: Some(serde_json::json!({ "note": filler })),
        ..conformance::node(key, key)
    };

    // Each one of these is far inside every per-item bound.
    harness
        .services
        .ingest(&ctx, conformance::batch(vec![fat("one")], Vec::new()))
        .await
        .expect("one item of this size is ordinary");

    let many: Vec<NodeSpec> = (0..32).map(|i| fat(&format!("many-{i}"))).collect();
    let refused = harness
        .services
        .ingest(&ctx, conformance::batch(many, Vec::new()))
        .await
        .expect_err("the sum of them is not");
    assert!(
        matches!(refused, DomainError::LimitExceeded { .. }),
        "expected a bound refusal, got {refused}"
    );
    assert!(
        refused.to_string().contains("ingest_max_bytes"),
        "the refusal names the bound it hit: {refused}"
    );
}

/// An element is bounded as a whole, not only field by field.
#[tokio::test]
async fn an_element_is_bounded_as_a_whole() {
    let small = GraphStorageConfig {
        item_max_bytes: 4_096,
        payload_max_bytes: 4_096,
        ..GraphStorageConfig::default()
    };
    let harness = Harness::configured(Arc::new(support::AllowInOwnTenant), small);
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    // A payload just inside `payload_max_bytes`, plus a name and keys, is an
    // item outside `item_max_bytes` -- which is the gap between bounding the
    // largest field and bounding the element.
    let node = NodeSpec {
        payload: Some(serde_json::json!({ "note": "y".repeat(4_000) })),
        ..conformance::node("whole", &"n".repeat(200))
    };
    let refused = harness
        .services
        .ingest(&ctx, conformance::batch(vec![node], Vec::new()))
        .await
        .expect_err("the element as a whole is over the ceiling");
    assert!(
        refused.to_string().contains("item_max_bytes"),
        "the refusal names the bound it hit: {refused}"
    );
}

/// The strings a caller controls are bounded like everything else.
///
/// `payload_max_bytes` bounded the payload and nothing bounded `node_key` or
/// `name`, which are the cheaper fields to abuse: both are stored in indexed
/// columns and both come back on every later read of the row, so the cost of
/// an oversized one is paid by every consumer for as long as the node lives.
/// The same for a search query, which the lexical arm parses and the vector
/// arm embeds.
#[tokio::test]
async fn the_caller_controlled_strings_are_bounded() {
    let harness = Harness::allowed();
    let ctx = harness.ctx();
    harness.seed_ontology(&ctx).await;

    let bound = harness.services.config().identifier_max_bytes as usize;
    let long = "k".repeat(bound + 1);

    let refused = harness
        .services
        .ingest(
            &ctx,
            conformance::batch(vec![conformance::node(&long, "fine")], vec![]),
        )
        .await
        .expect_err("an oversized node_key is refused");
    assert!(
        matches!(refused, DomainError::LimitExceeded { .. }),
        "expected a bound refusal, got {refused}"
    );

    let refused = harness
        .services
        .ingest(
            &ctx,
            conformance::batch(vec![conformance::node("fine", &long)], vec![]),
        )
        .await
        .expect_err("an oversized name is refused");
    assert!(matches!(refused, DomainError::LimitExceeded { .. }));

    // The idempotency key is the same kind of string and the most durable of
    // them: it is the primary key of the retry record, kept for the retention
    // window and read on every retry.
    let mut batch = conformance::batch(vec![conformance::node("fine", "fine")], vec![]);
    batch.idempotency_key = Some(long.clone());
    let refused = harness
        .services
        .ingest(&ctx, batch)
        .await
        .expect_err("an oversized idempotency_key is refused");
    assert!(matches!(refused, DomainError::LimitExceeded { .. }));

    // The catalogue page bound is enforced in the same place, which is what
    // makes it enforced for every caller: REST passes its `limit` query
    // parameter straight through, so a guard at the edge would be a guard the
    // in-process client does not have.
    let over = harness.services.config().projection_max_page + 1;
    let refused = harness
        .services
        .list_types(
            &ctx,
            TypeQuery {
                top: Some(over),
                ..TypeQuery::default()
            },
        )
        .await
        .expect_err("an oversized catalogue page is refused");
    assert!(matches!(refused, DomainError::LimitExceeded { .. }));

    let query = "q".repeat(harness.services.config().search_query_max_bytes as usize + 1);
    let refused = harness
        .services
        .search(
            &ctx,
            SearchRequest {
                mode: SearchMode::Lexical,
                query: Some(query),
                arm_limit: 10,
                limit: 10,
                type_patterns: Vec::new(),
            },
        )
        .await
        .expect_err("an oversized query is refused");
    assert!(matches!(refused, DomainError::LimitExceeded { .. }));

    // And the ordinary sizes still pass, so the bound is a bound and not a
    // wall.
    harness
        .services
        .ingest(
            &ctx,
            conformance::batch(
                vec![conformance::node("ordinary", "an ordinary name")],
                vec![],
            ),
        )
        .await
        .expect("an ordinary node commits");
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
