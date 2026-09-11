#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The domain service, end to end over the in-memory store.
//!
//! The conformance suite calls `GraphStoreV1` directly, which is the point of
//! it: the store contract must hold whoever calls it. But everything between
//! the API edge and that trait -- authorization, admission bounds, ontology
//! resolution, the embedding coordinator's wiring, error mapping -- was
//! reached by no test at all, and it is the layer a REST request actually
//! goes through. These cases drive the real `GraphServices` with a real
//! `PolicyEnforcer` in front of a stub PDP.

/// The suite's fixtures, reused here: the same ontology and the same node
/// and edge helpers, so a service case and a store case describe the same
/// graph. Only part of it is used from this binary, hence the allowance.
#[allow(dead_code)]
mod conformance;

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::api::AuthZResolverApi;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::pep::PolicyEnforcer;
use graph_storage::config::GraphStorageConfig;
use graph_storage::domain::error::DomainError;
use graph_storage::domain::service::GraphServices;
use graph_storage::infra::fake_store::FakeGraphStore;
use graph_storage_sdk::models::{
    Direction, EdgeRef, GraphRevision, NeighborhoodRequest, NodeId, SearchMode, SearchRequest,
    TraverseRequest, TypeQuery,
};
use graph_storage_sdk::plugin_api::{
    EngineCursor, ExpandRequest, ExpandResponse, GraphEngineError, GraphEngineV1, GraphStoreV1,
    PathResponse, PatternRequest, PatternResponse, ShortestPathRequest, StoreCtx,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A PDP that admits everything, scoped to the caller's own tenant.
///
/// Allow-all *at the action level* and tenant-bounded at the data level,
/// which is the deployment posture the gear is written for: the PDP decides
/// who may call, the compiled scope decides what they see. A denying variant
/// below covers the other branch.
struct AllowInOwnTenant;

#[async_trait]
impl AuthZResolverApi for AllowInOwnTenant {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|value| value.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap_or_else(Uuid::nil);
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tenant],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// A PDP that denies. The gear must answer `permission_denied` from the
/// service rather than reaching the store at all.
struct DenyEverything;

#[async_trait]
impl AuthZResolverApi for DenyEverything {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: None,
            },
        })
    }
}

/// A one-hop engine over the same store the service uses.
///
/// Enough to drive the traversal service for real -- seeds resolve, hops
/// expand, budgets and filters apply -- without a `PostgreSQL` server. It
/// declares neither shortest path nor pattern matching, so the service's
/// unsupported branches are reached as well.
struct HopOverStore {
    store: Arc<FakeGraphStore>,
}

#[async_trait]
impl GraphEngineV1 for HopOverStore {
    fn capabilities(&self) -> graph_storage_sdk::models::EngineCapabilities {
        graph_storage_sdk::models::EngineCapabilities::default()
    }

    async fn cursor(&self, ctx: &StoreCtx<'_>) -> Result<EngineCursor, GraphEngineError> {
        let revision = self.store.revision(ctx).await.unwrap_or(GraphRevision {
            source_epoch: 1,
            revision: 0,
        });
        Ok(EngineCursor { revision })
    }

    async fn expand(
        &self,
        ctx: &StoreCtx<'_>,
        req: ExpandRequest,
    ) -> Result<ExpandResponse, GraphEngineError> {
        // Hydration answers with the node but not its adjacency, so the
        // frontier is turned back into keys and each one is read: this stub
        // stands in for an engine, and an engine is exactly the thing that
        // knows the edges.
        let frontier = self
            .store
            .hydrate_nodes(ctx, &req.frontier)
            .await
            .map_err(|error| GraphEngineError::Unavailable {
                reason: error.to_string(),
            })?;
        let mut views = Vec::new();
        for node in frontier {
            if let Ok(view) = self.store.get_node(ctx, &node.node_key, 1000).await {
                views.push(view);
            }
        }
        let mut edges: Vec<EdgeRef> = Vec::new();
        let mut neighbours: Vec<String> = Vec::new();
        for view in views {
            for entry in view.adjacency {
                let outgoing = entry.side == graph_storage_sdk::models::AdjacencySide::Outgoing;
                let wanted = match req.direction {
                    Direction::Outgoing => outgoing,
                    Direction::Incoming => !outgoing,
                    Direction::Either => true,
                };
                let admitted = req
                    .edge_types
                    .as_ref()
                    .is_none_or(|set| set.contains(&entry.edge_type_id));
                if !wanted || !admitted {
                    continue;
                }
                let (src, dst) = if outgoing {
                    (view.node_key.clone(), entry.neighbor_key.clone())
                } else {
                    (entry.neighbor_key.clone(), view.node_key.clone())
                };
                edges.push(EdgeRef {
                    edge_key: entry.edge_key,
                    edge_type_id: entry.edge_type_id,
                    src,
                    dst,
                });
                neighbours.push(entry.neighbor_key);
            }
        }
        neighbours.sort();
        neighbours.dedup();
        let reached: Vec<NodeId> = self
            .store
            .resolve_node_ids(ctx, &neighbours)
            .await
            .map_err(|error| GraphEngineError::Unavailable {
                reason: error.to_string(),
            })?
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        Ok(ExpandResponse {
            reached,
            edges,
            truncated: None,
            served_by: graph_storage_sdk::plugin_api::HopBackend::TwoQuery,
        })
    }

    async fn shortest_path(
        &self,
        _ctx: &StoreCtx<'_>,
        _req: ShortestPathRequest,
    ) -> Result<PathResponse, GraphEngineError> {
        Err(GraphEngineError::Unsupported {
            what: "shortest_path",
        })
    }

    async fn match_pattern(
        &self,
        _ctx: &StoreCtx<'_>,
        _req: PatternRequest,
    ) -> Result<PatternResponse, GraphEngineError> {
        Err(GraphEngineError::Unsupported {
            what: "match_pattern",
        })
    }
}

struct Harness {
    services: GraphServices,
    tenant: Uuid,
}

impl Harness {
    fn with(authz: Arc<dyn AuthZResolverApi>) -> Self {
        let store = Arc::new(FakeGraphStore::new());
        let engine = Arc::new(HopOverStore {
            store: Arc::clone(&store),
        });
        Self {
            services: GraphServices::new(
                GraphStorageConfig::default(),
                store,
                engine,
                PolicyEnforcer::new(authz),
                conformance::coordinator(),
            ),
            tenant: Uuid::now_v7(),
        }
    }

    fn allowed() -> Self {
        Self::with(Arc::new(AllowInOwnTenant))
    }

    fn denied() -> Self {
        Self::with(Arc::new(DenyEverything))
    }

    fn ctx(&self) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::now_v7())
            .subject_tenant_id(self.tenant)
            .build()
            .expect("a valid security context")
    }

    /// The base ontology plus the suite's producer types, through the service.
    async fn seed_ontology(&self, ctx: &SecurityContext) {
        self.services
            .register_types(ctx, conformance::ontology_batch())
            .await
            .expect("the ontology registers");
    }
}

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

    // A search mode that needs text and was given none is an argument error,
    // not a limit: the two carry different canonical reasons, and a client
    // matches on the reason.
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
        matches!(error, DomainError::InvalidArgument { .. }),
        "expected an argument error, got {error}"
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
