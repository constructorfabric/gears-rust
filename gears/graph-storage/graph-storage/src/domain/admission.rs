//! The domain admission layer — the authoritative enforcement of the
//! Capacity and Admission Contract, identical for REST and `ClientHub` (the
//! REST edge only fast-fails; it is never the only guard).
//!
//! Every bound rejected here answers `out_of_range` / `LIMIT_EXCEEDED` (or
//! `invalid_argument` / `LIMIT_COMBINATION` for inconsistent combinations)
//! **before hydration**, so no oversized response is ever assembled.

use graph_storage_sdk::models::{
    IngestRequest, NeighborhoodRequest, SearchRequest, TraverseRequest,
};

use crate::config::GraphStorageConfig;
use crate::domain::error::DomainError;

fn exceeded(what: impl Into<String>) -> DomainError {
    DomainError::LimitExceeded { what: what.into() }
}

/// Bounds every ingest batch must clear before any validation work is spent.
pub fn admit_ingest(cfg: &GraphStorageConfig, request: &IngestRequest) -> Result<(), DomainError> {
    if request.nodes.len() > cfg.ingest_max_nodes as usize {
        return Err(exceeded(format!(
            "batch carries {} nodes; ingest_max_nodes is {}",
            request.nodes.len(),
            cfg.ingest_max_nodes
        )));
    }
    if request.edges.len() > cfg.ingest_max_edges as usize {
        return Err(exceeded(format!(
            "batch carries {} edges; ingest_max_edges is {}",
            request.edges.len(),
            cfg.ingest_max_edges
        )));
    }
    for (index, node) in request.nodes.iter().enumerate() {
        if let Some(payload) = &node.payload {
            let bytes = serde_json::to_vec(payload).map_or(usize::MAX, |v| v.len());
            if bytes > cfg.payload_max_bytes as usize {
                return Err(exceeded(format!(
                    "node[{index}] payload is {bytes} bytes; payload_max_bytes is {}",
                    cfg.payload_max_bytes
                )));
            }
        }
    }
    for (index, edge) in request.edges.iter().enumerate() {
        if let Some(payload) = &edge.payload {
            let bytes = serde_json::to_vec(payload).map_or(usize::MAX, |v| v.len());
            if bytes > cfg.payload_max_bytes as usize {
                return Err(exceeded(format!(
                    "edge[{index}] payload is {bytes} bytes; payload_max_bytes is {}",
                    cfg.payload_max_bytes
                )));
            }
        }
    }
    Ok(())
}

pub fn admit_search(cfg: &GraphStorageConfig, request: &SearchRequest) -> Result<(), DomainError> {
    if request.arm_limit == 0 || request.arm_limit > cfg.search_max_arm_limit {
        return Err(exceeded(format!(
            "arm_limit {} is outside 1..={}",
            request.arm_limit, cfg.search_max_arm_limit
        )));
    }
    if request.limit == 0 || request.limit > cfg.search_max_arm_limit * 2 {
        return Err(exceeded(format!(
            "limit {} is outside 1..={}",
            request.limit,
            cfg.search_max_arm_limit * 2
        )));
    }
    // Every arm now starts from text: the vector arm embeds the same `query`
    // through the same provider ingest used, which is what makes a hit
    // comparable at all (`fr-vector-search`).
    if request.query.as_deref().is_none_or(str::is_empty) {
        return Err(DomainError::limit_combination(
            "this search mode requires `query`",
        ));
    }
    Ok(())
}

pub fn admit_traverse(
    cfg: &GraphStorageConfig,
    request: &TraverseRequest,
) -> Result<(), DomainError> {
    if request.seeds.is_empty() {
        return Err(DomainError::limit_combination(
            "traversal requires at least one seed",
        ));
    }
    if request.depth == 0 || request.depth > cfg.traversal_max_depth {
        return Err(exceeded(format!(
            "depth {} is outside 1..={}",
            request.depth, cfg.traversal_max_depth
        )));
    }
    let max_nodes = request.max_nodes.unwrap_or(cfg.traversal_max_nodes);
    if max_nodes == 0 || max_nodes > cfg.traversal_max_nodes {
        return Err(exceeded(format!(
            "max_nodes {max_nodes} is outside 1..={}",
            cfg.traversal_max_nodes
        )));
    }
    // The seed set is bounded before expansion: a request whose distinct
    // authorized seeds exceed the node budget is rejected, because seeds
    // always survive truncation.
    if request.seeds.len() > max_nodes as usize {
        return Err(exceeded(format!(
            "{} seeds exceed the node budget {max_nodes}; seeds always survive truncation",
            request.seeds.len()
        )));
    }
    Ok(())
}

pub fn admit_neighborhood(
    cfg: &GraphStorageConfig,
    request: &NeighborhoodRequest,
) -> Result<(), DomainError> {
    if request.depth == 0 || request.depth > 3 {
        return Err(exceeded(format!(
            "neighborhood depth {} is outside 1..=3",
            request.depth
        )));
    }
    let budget = request.node_budget.unwrap_or(cfg.traversal_max_nodes);
    if budget == 0 || budget > cfg.traversal_max_nodes {
        return Err(exceeded(format!(
            "node_budget {budget} is outside 1..={}",
            cfg.traversal_max_nodes
        )));
    }
    Ok(())
}

pub fn admit_projection(
    cfg: &GraphStorageConfig,
    query: &toolkit_odata::ODataQuery,
) -> Result<(), DomainError> {
    // The platform parser already rejected unknown options and the
    // cursor-with-orderby combination; what remains is this gear's page
    // ceiling, which the parser cannot know.
    if let Some(limit) = query.limit
        && (limit == 0 || limit > u64::from(cfg.projection_max_page))
    {
        return Err(exceeded(format!(
            "$top {limit} is outside 1..={}",
            cfg.projection_max_page
        )));
    }
    Ok(())
}

pub fn admit_adjacency_limit(
    cfg: &GraphStorageConfig,
    requested: Option<u32>,
) -> Result<u32, DomainError> {
    let limit = requested.unwrap_or(cfg.node_read_max_adjacency);
    if limit == 0 || limit > cfg.node_read_max_adjacency {
        return Err(exceeded(format!(
            "adjacency_limit {limit} is outside 1..={}",
            cfg.node_read_max_adjacency
        )));
    }
    Ok(limit)
}
