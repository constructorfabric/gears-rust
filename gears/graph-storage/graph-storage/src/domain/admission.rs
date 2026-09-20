//! The domain admission layer — the authoritative enforcement of the
//! Capacity and Admission Contract, identical for REST and `ClientHub` (the
//! REST edge only fast-fails; it is never the only guard).
//!
//! Every bound rejected here answers `out_of_range` / `LIMIT_EXCEEDED` (or
//! `invalid_argument` / `LIMIT_COMBINATION` for inconsistent combinations)
//! **before hydration**.
//!
//! That used to be written as "so no oversized response is ever assembled",
//! which was true of one item and false of a batch: counts and per-field
//! ceilings bound each element and said nothing about their sum. What holds
//! now is narrower and checkable. A request is bounded by `ingest_max_bytes`
//! as well as by its counts. A read is bounded by bytes in one of two ways:
//! the projection page and the search arms have count ceilings small enough
//! that the count times `item_max_bytes` fits inside `response_max_bytes`,
//! which `GraphStorageConfig::validate` refuses to start without; traversal's
//! does not fit, so it measures as it hydrates and reports the cut.

use graph_storage_sdk::models::{
    IngestRequest, ItemFamily, NeighborhoodRequest, SearchRequest, TraverseRequest,
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
    // The idempotency key is the one caller-controlled string the batch
    // carries on its own, and it is the most durable of all of them: it is the
    // TEXT primary key of `ingest_idempotency`, kept for the retention window
    // and read on every retry. Bounding the node keys and not this one leaves
    // the cheapest oversized field unbounded, and an index entry is where it
    // lands.
    if let Some(key) = &request.idempotency_key
        && key.len() > cfg.identifier_max_bytes as usize
    {
        return Err(exceeded(format!(
            "idempotency_key is {} bytes; identifier_max_bytes is {}",
            key.len(),
            cfg.identifier_max_bytes
        )));
    }
    // The two caller-controlled strings a node carries outside its payload.
    // Bounding the payload and not these leaves the cheapest oversized field
    // unbounded: `node_key` and `name` are indexed columns, and every read of
    // the row carries them back to every consumer.
    for (index, node) in request.nodes.iter().enumerate() {
        for (what, value) in [("node_key", &node.node_key)]
            .into_iter()
            .chain(node.name.as_ref().map(|name| ("name", name)))
        {
            if value.len() > cfg.identifier_max_bytes as usize {
                return Err(exceeded(format!(
                    "node[{index}] {what} is {} bytes; identifier_max_bytes is {}",
                    value.len(),
                    cfg.identifier_max_bytes
                )));
            }
        }
    }
    for (index, edge) in request.edges.iter().enumerate() {
        for (what, value) in [
            ("src_node_key", &edge.src_node_key),
            ("dst_node_key", &edge.dst_node_key),
        ]
        .into_iter()
        .chain(edge.discriminator.as_ref().map(|d| ("discriminator", d)))
        {
            if value.len() > cfg.identifier_max_bytes as usize {
                return Err(exceeded(format!(
                    "edge[{index}] {what} is {} bytes; identifier_max_bytes is {}",
                    value.len(),
                    cfg.identifier_max_bytes
                )));
            }
        }
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

    // Counts and per-field ceilings are not a size bound on the batch, and
    // this is where that stopped being a theoretical point: every check above
    // passes for fifty thousand items that are each just under their own
    // ceiling, and the sum of them is a request the process does not survive.
    // Measured on the serialized form, because that is what is read, parsed,
    // held and written.
    let mut total: u64 = 0;
    for (family, index, bytes) in request
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (ItemFamily::Node, index, node_bytes(node)))
        .chain(
            request
                .edges
                .iter()
                .enumerate()
                .map(|(index, edge)| (ItemFamily::Edge, index, edge_bytes(edge))),
        )
    {
        if bytes > u64::from(cfg.item_max_bytes) {
            return Err(exceeded(format!(
                "{family:?}[{index}] is {bytes} bytes; item_max_bytes is {}",
                cfg.item_max_bytes
            )));
        }
        total = total.saturating_add(bytes);
        if total > cfg.ingest_max_bytes {
            return Err(exceeded(format!(
                "the batch is at least {total} bytes; ingest_max_bytes is {}",
                cfg.ingest_max_bytes
            )));
        }
    }
    Ok(())
}

/// What a node costs to carry: every caller-supplied field of it, not the one
/// that happens to be largest.
///
/// Summed rather than serialized because the models are transport-agnostic and
/// carry no `Serialize`, which is deliberate -- the wire shape belongs to the
/// DTOs. The sum is a lower bound on the encoded size and an accurate one for
/// deciding admission: it counts every byte the caller controls.
fn node_bytes(node: &graph_storage_sdk::models::NodeSpec) -> u64 {
    let payload = node.payload.as_ref().map_or(0, json_bytes);
    payload
        .saturating_add(node.node_key.len() as u64)
        .saturating_add(node.type_id.len() as u64)
        .saturating_add(node.name.as_ref().map_or(0, |name| name.len() as u64))
}

/// The same for an edge: two endpoint keys, a type, an optional discriminator
/// and the payload.
fn edge_bytes(edge: &graph_storage_sdk::models::EdgeSpec) -> u64 {
    let payload = edge.payload.as_ref().map_or(0, json_bytes);
    payload
        .saturating_add(edge.src_node_key.len() as u64)
        .saturating_add(edge.dst_node_key.len() as u64)
        .saturating_add(edge.type_id.len() as u64)
        .saturating_add(
            edge.discriminator
                .as_ref()
                .map_or(0, |value| value.len() as u64),
        )
}

fn json_bytes(value: &serde_json::Value) -> u64 {
    serde_json::to_vec(value).map_or(u64::MAX, |bytes| bytes.len() as u64)
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
    if let Some(query) = &request.query
        && query.len() > cfg.search_query_max_bytes as usize
    {
        return Err(exceeded(format!(
            "query is {} bytes; search_query_max_bytes is {}",
            query.len(),
            cfg.search_query_max_bytes
        )));
    }
    Ok(())
}

/// The page bound on the type catalogue.
///
/// Every other paged read is bounded and this one was not: a caller could ask
/// for the whole catalogue in one response, which is a tenant's entire
/// ontology in one allocation. Bounded by the same page size the projection
/// uses, since it is the same question asked of a different collection.
pub fn admit_type_query(
    cfg: &GraphStorageConfig,
    query: &graph_storage_sdk::models::TypeQuery,
) -> Result<(), DomainError> {
    if let Some(top) = query.top
        && (top == 0 || top > cfg.projection_max_page)
    {
        return Err(exceeded(format!(
            "limit {top} is outside 1..={}",
            cfg.projection_max_page
        )));
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
    // The seed set is bounded before expansion, because seeds always survive
    // truncation. Counted *distinct*: a caller that names one key twice has
    // asked for one seed, and rejecting them for a budget they did not spend
    // would be a refusal they cannot act on. The contract also says
    // "authorized", which cannot be known before a store read — admission
    // runs before any — so this bound is on what was asked for, and the
    // authorized set can only be smaller.
    let distinct: std::collections::BTreeSet<&str> =
        request.seeds.iter().map(String::as_str).collect();
    if distinct.len() > max_nodes as usize {
        return Err(exceeded(format!(
            "{} distinct seeds exceed the node budget {max_nodes}; seeds always survive \
             truncation",
            distinct.len()
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
