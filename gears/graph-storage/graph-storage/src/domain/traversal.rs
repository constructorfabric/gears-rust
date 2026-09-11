//! BFS orchestration over the engine's one-hop primitive.
//!
//! The engine never expands beyond one hop; this loop owns the visited set,
//! the per-hop dedup, and the budgets, so authorization and budgets are
//! re-evaluated between hops rather than inside an opaque traversal. Edges
//! are treated as undirected for reachability (`Direction::Either` — the
//! union of two directed scans, never the undirected pattern).

use std::collections::{BTreeMap, BTreeSet};

use graph_storage_sdk::models::{
    Direction, EdgeRef, HopBudget, NodeId, TruncationReason, TypeIdSet,
};
use graph_storage_sdk::plugin_api::{ExpandRequest, GraphEngineV1, StoreCtx};

use crate::domain::error::DomainError;

/// Which nodes survive when a hop reaches more of them than the budget
/// allows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Retention {
    /// The order the hop reached them in, by internal id. What a traversal
    /// does: its caller asked for a region and post-processes it, so no node
    /// of the region is privileged.
    #[default]
    Reached,
    /// The most connected first, counting only the edges of this hop —
    /// "degree ordering, budgets and truncation are computed on authorized
    /// rows only", so the degree is the one visible inside the authorized
    /// subgraph, never a global count the caller cannot see.
    ///
    /// What the neighborhood projection does: a UI that can draw 200 of a
    /// hub's 5 000 neighbours wants the structural core, and arrival order
    /// gives it 200 arbitrary leaves instead
    /// (`fr-neighborhood-projection`).
    Degree,
}

pub struct WalkPlan {
    pub depth: u8,
    /// Total node budget, seeds included. Seeds always survive truncation —
    /// admission already rejected a seed set exceeding this.
    pub max_nodes: u32,
    pub max_frontier: u32,
    pub max_edges_scanned: u64,
    pub edge_types: Option<TypeIdSet>,
    pub retention: Retention,
}

pub struct WalkResult {
    /// Every reached node, seeds first, in deterministic order.
    pub nodes: Vec<NodeId>,
    pub edges: Vec<EdgeRef>,
    pub truncated: Option<TruncationReason>,
}

/// Breadth-first walk from `seeds` to `plan.depth`.
pub async fn walk(
    engine: &dyn GraphEngineV1,
    ctx: &StoreCtx<'_>,
    seeds: Vec<NodeId>,
    plan: &WalkPlan,
) -> Result<WalkResult, DomainError> {
    let mut visited: BTreeSet<NodeId> = seeds.iter().copied().collect();
    let mut ordered: Vec<NodeId> = {
        // Deterministic seed ordering is part of the contract.
        let mut sorted = seeds;
        sorted.sort_unstable();
        sorted.dedup();
        sorted
    };
    let mut frontier: Vec<NodeId> = ordered.clone();
    let mut edges: Vec<EdgeRef> = Vec::new();
    // An edge is reachable from both of its endpoints, so an undirected walk
    // meets each one twice: once expanding its source, once expanding its
    // destination. Without this the same edge is reported twice, and a caller
    // drawing or counting the result is simply wrong.
    let mut seen_edges: BTreeSet<String> = BTreeSet::new();
    let mut truncated: Option<TruncationReason> = None;

    for _ in 0..plan.depth {
        if frontier.is_empty() || truncated.is_some() {
            break;
        }
        let response = engine
            .expand(
                ctx,
                ExpandRequest {
                    frontier: frontier.clone(),
                    direction: Direction::Either,
                    edge_types: plan.edge_types.clone(),
                    labels: None,
                    budget: HopBudget {
                        max_frontier: plan.max_frontier,
                        max_edges_scanned: plan.max_edges_scanned,
                    },
                    with_degrees: plan.retention == Retention::Degree,
                },
            )
            .await?;

        if response.truncated.is_some() {
            truncated = response.truncated;
        }
        // Index-aligned by the port's contract; a short list would silently
        // read as degree zero, so it is the engine's bug rather than a
        // default to paper over.
        let degree: BTreeMap<NodeId, u32> = response
            .reached
            .iter()
            .copied()
            .zip(response.degrees.iter().copied())
            .collect();
        for edge in response.edges {
            if seen_edges.insert(edge.edge_key.clone()) {
                edges.push(edge);
            }
        }

        // Candidates: what this hop reached and the walk has not seen.
        let mut reached = response.reached;
        reached.sort_unstable();
        reached.dedup();
        let mut candidates: Vec<NodeId> = reached
            .into_iter()
            .filter(|node| !visited.contains(node))
            .collect();

        if plan.retention == Retention::Degree {
            // Most connected first, ties by id so the answer is reproducible.
            candidates.sort_by_key(|node| {
                (
                    std::cmp::Reverse(degree.get(node).copied().unwrap_or(0)),
                    *node,
                )
            });
        }

        let mut next: Vec<NodeId> = Vec::new();
        for node in candidates {
            if ordered.len() >= plan.max_nodes as usize {
                // The budget is reached, not the end of the hop: what is left
                // out is what the retention rule put last.
                truncated = Some(TruncationReason::NodeBudget);
                break;
            }
            visited.insert(node);
            ordered.push(node);
            next.push(node);
        }
        frontier = next;
    }

    Ok(WalkResult {
        nodes: ordered,
        edges,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use graph_storage_sdk::models::{
        EdgeSpec, IngestRequest, NodeSpec, RemainingBudget, TypeRegistration,
    };
    use graph_storage_sdk::plugin_api::GraphStoreV1;
    use tokio_util::sync::CancellationToken;
    use toolkit_security::AccessScope;
    use uuid::Uuid;

    use super::*;
    use crate::domain::ontology::BASE_SCHEMAS;
    use crate::infra::fake_store::{FakeGraphEngine, FakeGraphStore};

    const OWNED: &str = "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~acme.walk._.n.v1~";
    const LINK: &str = "gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~acme.walk._.e.v1~";

    fn derived(type_id: &str, family: &str) -> TypeRegistration {
        TypeRegistration {
            type_id: type_id.to_owned(),
            schema: serde_json::json!({
                "$id": format!("gts://{type_id}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "allOf": [{ "$ref": format!("gts://{family}") }],
            }),
        }
    }

    /// A chain `a -> b -> c`, walked two hops from `a`.
    ///
    /// The middle node is reached by expanding `a`, and expanding it in turn
    /// meets the very edge that led there — once as an outgoing edge of `a`,
    /// once as an incoming edge of `b`. A caller drawing the result must not
    /// see that edge twice.
    #[tokio::test]
    async fn an_edge_met_from_both_ends_is_reported_once() {
        let store = Arc::new(FakeGraphStore::new());
        let engine = FakeGraphEngine::new(Arc::clone(&store));
        let tenant = Uuid::now_v7();
        let scope = AccessScope::for_tenant(tenant);
        let ctx = StoreCtx {
            tenant,
            scope: &scope,
            subject: graph_storage_sdk::models::Subject {
                subject_id: Uuid::nil(),
                subject_type: None,
            },
            snapshot: None,
            budget: RemainingBudget::starting_now(Duration::from_secs(30)),
            cancel: CancellationToken::new(),
        };

        let mut types: Vec<TypeRegistration> = BASE_SCHEMAS
            .iter()
            .map(|(type_id, raw)| TypeRegistration {
                type_id: (*type_id).to_owned(),
                schema: serde_json::from_str(raw).unwrap_or_default(),
            })
            .collect();
        types.push(derived(
            OWNED,
            "gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~",
        ));
        types.push(derived(
            LINK,
            "gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~",
        ));
        store
            .register_types(&ctx, types)
            .await
            .unwrap_or_else(|e| panic!("ontology registers: {e}"));

        let node = |key: &str| NodeSpec {
            node_key: key.to_owned(),
            type_id: OWNED.to_owned(),
            ..NodeSpec::default()
        };
        let link = |from: &str, to: &str| EdgeSpec {
            type_id: LINK.to_owned(),
            src_node_key: from.to_owned(),
            dst_node_key: to.to_owned(),
            ..EdgeSpec::default()
        };
        // This case walks edges; vectors are beside the point, so the plan
        // records three unembedded nodes rather than dragging a provider in.
        let unembedded = graph_storage_sdk::plugin_api::EmbeddingPlan {
            epoch: None,
            nodes: ["a", "b", "c"]
                .iter()
                .map(|key| {
                    graph_storage_sdk::plugin_api::NodeEmbedding::skipped(
                        crate::domain::embedding::input_hash(key),
                    )
                })
                .collect(),
        };
        store
            .ingest(
                &ctx,
                IngestRequest {
                    nodes: vec![node("a"), node("b"), node("c")],
                    edges: vec![link("a", "b"), link("b", "c")],
                    ..IngestRequest::default()
                },
                unembedded,
            )
            .await
            .unwrap_or_else(|e| panic!("the batch commits: {e}"));

        let seed = store
            .resolve_node_ids(&ctx, &["a".to_owned()])
            .await
            .unwrap_or_else(|e| panic!("resolution succeeds: {e}"))
            .first()
            .map_or_else(|| panic!("`a` resolves"), |(_, id)| *id);

        let plan = WalkPlan {
            depth: 2,
            max_nodes: 100,
            max_frontier: 100,
            max_edges_scanned: 1_000,
            edge_types: None,
            retention: Retention::Reached,
        };
        let result = walk(&engine, &ctx, vec![seed], &plan)
            .await
            .unwrap_or_else(|e| panic!("the walk runs: {e}"));

        assert_eq!(result.nodes.len(), 3, "the walk reaches every node once");
        assert_eq!(
            result.edges.len(),
            2,
            "two edges, reported once each: {:?}",
            result
                .edges
                .iter()
                .map(|e| (e.src.as_str(), e.dst.as_str()))
                .collect::<Vec<_>>()
        );
    }
}
