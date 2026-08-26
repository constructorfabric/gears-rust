//! BFS orchestration over the engine's one-hop primitive.
//!
//! The engine never expands beyond one hop; this loop owns the visited set,
//! the per-hop dedup, and the budgets, so authorization and budgets are
//! re-evaluated between hops rather than inside an opaque traversal. Edges
//! are treated as undirected for reachability (`Direction::Either` — the
//! union of two directed scans, never the undirected pattern).

use std::collections::BTreeSet;

use graph_storage_sdk::models::{
    Direction, EdgeRef, HopBudget, NodeId, TruncationReason, TypeIdSet,
};
use graph_storage_sdk::plugin_api::{ExpandRequest, GraphEngineV1, StoreCtx};

use crate::domain::error::DomainError;

pub struct WalkPlan {
    pub depth: u8,
    /// Total node budget, seeds included. Seeds always survive truncation —
    /// admission already rejected a seed set exceeding this.
    pub max_nodes: u32,
    pub max_frontier: u32,
    pub max_edges_scanned: u64,
    pub edge_types: Option<TypeIdSet>,
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
                },
            )
            .await?;

        if response.truncated.is_some() {
            truncated = response.truncated;
        }
        edges.extend(response.edges);

        let mut next: Vec<NodeId> = Vec::new();
        let mut reached = response.reached;
        reached.sort_unstable();
        reached.dedup();
        for node in reached {
            if visited.insert(node) {
                if ordered.len() >= plan.max_nodes as usize {
                    truncated = Some(TruncationReason::NodeBudget);
                    break;
                }
                ordered.push(node);
                next.push(node);
            }
        }
        frontier = next;
    }

    Ok(WalkResult {
        nodes: ordered,
        edges,
        truncated,
    })
}
