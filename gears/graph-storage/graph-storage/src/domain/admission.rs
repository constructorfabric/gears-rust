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
//! ceilings bound each element and said nothing about their sum.
//!
//! Bytes are now bounded from both ends, and deliberately twice over. A
//! request is bounded by `ingest_max_bytes` and each element by
//! `item_max_bytes`. Every hydrated read measures what it assembled against
//! `response_max_bytes`. And `GraphStorageConfig::validate` refuses a
//! configuration whose count ceiling multiplied by `item_max_bytes` could
//! exceed that budget -- a thousand-row page of four-megabyte items is four
//! gigabytes, and every individual number in it is inside its own range, so
//! no range check on its own could ever see it.
//!
//! The startup check is a promise about the deployment; the runtime measure
//! is what happens if the promise is wrong, and neither is a substitute for
//! the other.
//!
//! What a read does when it reaches the budget differs by what its contract
//! allows. Traversal and search cut and report `ResponseBytes`. The tabular
//! projection refuses instead: its continuation token is minted for the rows
//! the statement returned, so trimming behind it and handing it back would
//! make the caller resume past rows it never saw.

use graph_storage_sdk::models::{
    EdgeSpec, IngestOptions, IngestRequest, ItemFamily, MigrationSpec, MigrationStep,
    NeighborhoodRequest, NodeSpec, ReplaceScope, SearchRequest, TraverseRequest, TypeQuery,
    TypeRegistration,
};

use crate::config::GraphStorageConfig;
use crate::domain::error::DomainError;

fn exceeded(what: impl Into<String>) -> DomainError {
    DomainError::LimitExceeded { what: what.into() }
}

/// `PostgreSQL` stores U+0000 in neither `text` nor `jsonb`: a string that
/// carries one is valid UTF-8 and valid JSON, reaches the statement, and fails
/// there (SQLSTATE `22P05`) -- which surfaced as `unknown: internal error` and
/// lost the batch, with the cause only in the database server's log. Every
/// caller-supplied string bound for the store is checked here instead, and
/// the refusal names where the character is.
fn refuse_nul(what: &str, value: &str) -> Result<(), DomainError> {
    if value.contains('\0') {
        return Err(DomainError::invalid(format!(
            "{what} carries a NUL character (U+0000), which the store cannot hold"
        )));
    }
    Ok(())
}

/// The JSON-pointer path of the first NUL in a document -- in a string value
/// or in an object key -- or `None`.
fn nul_in_json(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => text.contains('\0').then(String::new),
        serde_json::Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, item)| nul_in_json(item).map(|rest| format!("/{index}{rest}"))),
        serde_json::Value::Object(map) => map.iter().find_map(|(key, item)| {
            if key.contains('\0') {
                return Some(format!("/{}", key.replace('\0', "\\u0000")));
            }
            nul_in_json(item).map(|rest| format!("/{key}{rest}"))
        }),
        _ => None,
    }
}

/// [`refuse_nul`] for a JSON document: `what` names the document, the
/// refusal adds the path inside it.
fn refuse_nul_in_json(what: &str, value: &serde_json::Value) -> Result<(), DomainError> {
    match nul_in_json(value) {
        Some(path) => Err(DomainError::invalid(format!(
            "{what}{path} carries a NUL character (U+0000), which the store cannot hold"
        ))),
        None => Ok(()),
    }
}

/// One caller-supplied identifier on a read or a single-row write: a key,
/// a type id or pattern, a namespace.
///
/// Ingest bounds every identifier it stores, so a longer one names nothing
/// the gear can hold, and there is no reason to carry it into a statement,
/// an index probe or a pattern match to find that out. Refused with the same
/// bound and the same rejection an ingest gets. An edge key is a SHA-256 in
/// hex, 64 bytes, and 64 is the smallest `identifier_max_bytes` the
/// configuration admits, so one bound serves every kind.
pub fn admit_identifier(
    cfg: &GraphStorageConfig,
    what: &str,
    value: &str,
) -> Result<(), DomainError> {
    if value.len() > cfg.identifier_max_bytes as usize {
        return Err(exceeded(format!(
            "{what} is {} bytes; identifier_max_bytes is {}",
            value.len(),
            cfg.identifier_max_bytes
        )));
    }
    refuse_nul(what, value)
}

/// Bounds every ingest batch must clear before any validation work is spent.
///
/// Every field of the request, and of each node and edge in it, is named in
/// the patterns below, and none of the patterns ends in `..`. That is the
/// guard this module rests on: a field added to a request type that this
/// function does not mention is a compile error here, not a string the
/// database meets unbounded. The bound on `type_id` was missing for as long
/// as this was a list of fields someone remembered to write down, while
/// every field beside it was checked.
pub fn admit_ingest(cfg: &GraphStorageConfig, request: &IngestRequest) -> Result<(), DomainError> {
    let IngestRequest {
        nodes,
        edges,
        options:
            IngestOptions {
                create_phantoms: _,
                report_per_item: _,
                embed: _,
            },
        replace_scope,
        idempotency_key,
    } = request;
    if nodes.len() > cfg.ingest_max_nodes as usize {
        return Err(exceeded(format!(
            "batch carries {} nodes; ingest_max_nodes is {}",
            nodes.len(),
            cfg.ingest_max_nodes
        )));
    }
    if edges.len() > cfg.ingest_max_edges as usize {
        return Err(exceeded(format!(
            "batch carries {} edges; ingest_max_edges is {}",
            edges.len(),
            cfg.ingest_max_edges
        )));
    }
    // The idempotency key is the one caller-controlled string the batch
    // carries on its own, and it is the most durable of all of them: it is the
    // TEXT primary key of `ingest_idempotency`, kept until the expiry protocol
    // lands (#4874; nothing reads `idempotency_retention_days` yet) and read
    // on every retry.
    if let Some(key) = idempotency_key {
        admit_identifier(cfg, "idempotency_key", key)?;
    }
    // Every string a node or an edge carries outside its payload is an
    // identifier: `node_key` and `name` are indexed columns and come back on
    // every read of the row, `type_id` is resolved against the catalogue and
    // interned, an endpoint key names a row and a discriminator is part of
    // the edge key. The payload has its own ceiling.
    for (index, node) in nodes.iter().enumerate() {
        let NodeSpec {
            node_key,
            type_id,
            name,
            payload,
            expected_version: _,
        } = node;
        admit_identifier(cfg, &format!("node[{index}] node_key"), node_key)?;
        admit_identifier(cfg, &format!("node[{index}] type_id"), type_id)?;
        if let Some(name) = name {
            admit_identifier(cfg, &format!("node[{index}] name"), name)?;
        }
        if let Some(payload) = payload {
            let bytes = serde_json::to_vec(payload).map_or(usize::MAX, |v| v.len());
            if bytes > cfg.payload_max_bytes as usize {
                return Err(exceeded(format!(
                    "node[{index}] payload is {bytes} bytes; payload_max_bytes is {}",
                    cfg.payload_max_bytes
                )));
            }
            refuse_nul_in_json(&format!("node[{index}] payload"), payload)?;
        }
    }
    for (index, edge) in edges.iter().enumerate() {
        let EdgeSpec {
            type_id,
            src_node_key,
            dst_node_key,
            discriminator,
            payload,
        } = edge;
        admit_identifier(cfg, &format!("edge[{index}] type_id"), type_id)?;
        admit_identifier(cfg, &format!("edge[{index}] src_node_key"), src_node_key)?;
        admit_identifier(cfg, &format!("edge[{index}] dst_node_key"), dst_node_key)?;
        if let Some(discriminator) = discriminator {
            admit_identifier(cfg, &format!("edge[{index}] discriminator"), discriminator)?;
        }
        if let Some(payload) = payload {
            let bytes = serde_json::to_vec(payload).map_or(usize::MAX, |v| v.len());
            if bytes > cfg.payload_max_bytes as usize {
                return Err(exceeded(format!(
                    "edge[{index}] payload is {bytes} bytes; payload_max_bytes is {}",
                    cfg.payload_max_bytes
                )));
            }
            refuse_nul_in_json(&format!("edge[{index}] payload"), payload)?;
        }
    }

    // The scope a replacement declares is caller-controlled too, and it is
    // the one that outlives the request: `attribute` and `value` become the
    // primary key of `scope_registry` and two indexed columns on every edge
    // the scope owns.
    if let Some(ReplaceScope {
        attribute,
        value,
        generation: _,
    }) = replace_scope
    {
        admit_identifier(cfg, "replace_scope.attribute", attribute)?;
        admit_identifier(cfg, "replace_scope.value", value)?;
    }

    // Counts and per-field ceilings are not a size bound on the batch, and
    // this is where that stopped being a theoretical point: every check above
    // passes for fifty thousand items that are each just under their own
    // ceiling, and the sum of them is a request the process does not survive.
    // Measured on the serialized form, because that is what is read, parsed,
    // held and written.
    // The scope's own bytes travel with the batch and are charged to it.
    let mut total: u64 = replace_scope.as_ref().map_or(0, |replace| {
        (replace.attribute.len() as u64).saturating_add(replace.value.len() as u64)
    });
    for (family, index, bytes) in nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (ItemFamily::Node, index, node_bytes(node)))
        .chain(
            edges
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
    let SearchRequest {
        mode: _,
        query,
        arm_limit,
        limit,
        type_patterns,
    } = request;
    let (arm_limit, limit) = (*arm_limit, *limit);
    if arm_limit == 0 || arm_limit > cfg.search_max_arm_limit {
        return Err(exceeded(format!(
            "arm_limit {arm_limit} is outside 1..={}",
            cfg.search_max_arm_limit
        )));
    }
    if limit == 0 || limit > cfg.search_max_arm_limit * 2 {
        return Err(exceeded(format!(
            "limit {limit} is outside 1..={}",
            cfg.search_max_arm_limit * 2
        )));
    }
    // Every arm now starts from text: the vector arm embeds the same `query`
    // through the same provider ingest used, which is what makes a hit
    // comparable at all (`fr-vector-search`).
    if query.as_deref().is_none_or(str::is_empty) {
        return Err(DomainError::limit_combination(
            "this search mode requires `query`",
        ));
    }
    if let Some(query) = query
        && query.len() > cfg.search_query_max_bytes as usize
    {
        return Err(exceeded(format!(
            "query is {} bytes; search_query_max_bytes is {}",
            query.len(),
            cfg.search_query_max_bytes
        )));
    }
    if let Some(query) = query {
        refuse_nul("query", query)?;
    }
    for (index, pattern) in type_patterns.iter().enumerate() {
        admit_identifier(cfg, &format!("type_patterns[{index}]"), pattern)?;
    }
    Ok(())
}

/// The page bound on the type catalogue, and the strings it is asked with.
///
/// Every other paged read is bounded and this one was not: a caller could ask
/// for the whole catalogue in one response, which is a tenant's entire
/// ontology in one allocation. Bounded by the same page size the projection
/// uses, since it is the same question asked of a different collection.
pub fn admit_type_query(cfg: &GraphStorageConfig, query: &TypeQuery) -> Result<(), DomainError> {
    let TypeQuery {
        kind: _,
        pattern,
        top,
        cursor,
    } = query;
    if let Some(top) = top
        && (*top == 0 || *top > cfg.projection_max_page)
    {
        return Err(exceeded(format!(
            "limit {top} is outside 1..={}",
            cfg.projection_max_page
        )));
    }
    if let Some(pattern) = pattern {
        admit_identifier(cfg, "pattern", pattern)?;
    }
    // The catalogue's cursor is the type id the previous page ended on, so
    // it is an identifier by the same rule: a longer one names no type.
    if let Some(cursor) = cursor {
        admit_identifier(cfg, "cursor", cursor)?;
    }
    Ok(())
}

pub fn admit_traverse(
    cfg: &GraphStorageConfig,
    request: &TraverseRequest,
) -> Result<(), DomainError> {
    let TraverseRequest {
        seeds,
        depth,
        edge_type_patterns,
        node_type_patterns,
        max_nodes,
    } = request;
    let depth = *depth;
    if seeds.is_empty() {
        return Err(DomainError::limit_combination(
            "traversal requires at least one seed",
        ));
    }
    if depth == 0 || depth > cfg.traversal_max_depth {
        return Err(exceeded(format!(
            "depth {depth} is outside 1..={}",
            cfg.traversal_max_depth
        )));
    }
    let max_nodes = max_nodes.unwrap_or(cfg.traversal_max_nodes);
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
    let distinct: std::collections::BTreeSet<&str> = seeds.iter().map(String::as_str).collect();
    if distinct.len() > max_nodes as usize {
        return Err(exceeded(format!(
            "{} distinct seeds exceed the node budget {max_nodes}; seeds always survive \
             truncation",
            distinct.len()
        )));
    }
    // The count bounds how many seeds, not how long each is; every one of
    // them is bound into the resolution statement.
    for (index, seed) in seeds.iter().enumerate() {
        admit_identifier(cfg, &format!("seeds[{index}]"), seed)?;
    }
    for (index, pattern) in edge_type_patterns.iter().enumerate() {
        admit_identifier(cfg, &format!("edge_type_patterns[{index}]"), pattern)?;
    }
    for (index, pattern) in node_type_patterns.iter().enumerate() {
        admit_identifier(cfg, &format!("node_type_patterns[{index}]"), pattern)?;
    }
    Ok(())
}

pub fn admit_neighborhood(
    cfg: &GraphStorageConfig,
    request: &NeighborhoodRequest,
) -> Result<(), DomainError> {
    let NeighborhoodRequest {
        root,
        depth,
        node_budget,
        include_phantoms: _,
    } = request;
    let depth = *depth;
    // The same ceiling bounded walks use, and for the same reason: both drive
    // one BFS, differing only in which nodes truncation keeps. A literal `3`
    // here made the operator's knob mean less than it says -- raising
    // `traversal_max_depth` loosened traversal and left neighborhood where it
    // was, and lowering it did not tighten neighborhood at all, which is the
    // direction that matters. The node budget beside it was already read from
    // configuration, so the two halves of this same function disagreed about
    // whether the deployment gets a say.
    //
    // The PRD's depth-3 reference scenario is a performance target, not a
    // cap: the NFR is that depth 3 answers within a second, and nothing in it
    // says depth 4 is refused.
    if depth == 0 || depth > cfg.traversal_max_depth {
        return Err(exceeded(format!(
            "neighborhood depth {depth} is outside 1..={}",
            cfg.traversal_max_depth
        )));
    }
    let budget = node_budget.unwrap_or(cfg.traversal_max_nodes);
    if budget == 0 || budget > cfg.traversal_max_nodes {
        return Err(exceeded(format!(
            "node_budget {budget} is outside 1..={}",
            cfg.traversal_max_nodes
        )));
    }
    admit_identifier(cfg, "root", root)
}

/// Bind a continuation cursor to the `$filter` of the listing that minted it.
///
/// What a cursor is bound to: the `$filter` and the selected types.
///
/// A continuation token carries the ordering it was minted under (`s`) and
/// one more field, `f`, that the platform pager compares against the query's
/// `filter_hash` and otherwise leaves alone. Two things decide which rows a
/// listing walks, and the token has to name both: the filter, and the type
/// set the statement is narrowed to (`ProjectionRequest::type_set`, resolved
/// from `type_pattern`). A cursor minted under type `A` and replayed under
/// type `B` with the same filter used to pass both checks, and resumed `B`'s
/// listing at `A`'s last key — skipping every `B` row that sorts before it.
///
/// So `f` is the identity of the listing, not of the filter alone: the
/// filter's hash as the platform computes it, joined to a hash of the sorted
/// type ids when a type set is selected. A listing with neither has no
/// identity (`None`), which keeps a token minted before this rule readable
/// where it is still correct. One function mints and checks it on both
/// sides — the service before the store round trip, and the store for a
/// caller that reaches it directly — so the two cannot drift.
#[must_use]
pub fn listing_identity(
    filter: Option<&toolkit_odata::ast::Expr>,
    type_set: Option<&graph_storage_sdk::models::TypeIdSet>,
) -> Option<String> {
    let filter = toolkit_odata::short_filter_hash(filter);
    let types = type_set.map(|set| {
        let mut text = String::new();
        for type_id in &set.0 {
            text.push_str(type_id);
            text.push('\n');
        }
        format!("{:016x}", fnv1a_64(text.as_bytes()))
    });
    match (filter, types) {
        (None, None) => None,
        (Some(filter), None) => Some(filter),
        (None, Some(types)) => Some(format!("-:{types}")),
        (Some(filter), Some(types)) => Some(format!("{filter}:{types}")),
    }
}

/// FNV-1a, 64-bit: the fingerprint `toolkit_odata::short_filter_hash` uses
/// for the filter, applied here to the type ids so the two halves of the
/// identity are the same kind of thing. A fingerprint, not a security hash:
/// nothing is authenticated by it, and a collision costs one wrong refusal.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    bytes.iter().fold(BASIS, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

/// A page is the continuation of the listing that minted its cursor, and
/// only of that listing: same `$filter`, same selected types.
///
/// The comparison is on the whole `Option`: a cursor minted under a filter
/// and replayed without one would otherwise resume an *unfiltered* walk at a
/// filtered position, which answered an empty page with `200` -- a caller
/// forwarding only the cursor concluded the listing had ended. The reverse,
/// a filter or a type set added to a cursor minted without one, is refused
/// for the same reason.
///
/// The REST extractor stamps a filter hash; an in-process query built with
/// `ODataQuery::with_filter` carries none. Neither is what the cursor is
/// bound to, so the identity is computed here from what the query holds
/// ([`listing_identity`]) and stamped on it, and the store computes the same
/// value from the same inputs.
///
/// # Errors
///
/// [`DomainError::InvalidArgument`] when the cursor's identity is not the
/// query's.
pub fn bind_listing_to_cursor(
    mut query: toolkit_odata::ODataQuery,
    type_set: Option<&graph_storage_sdk::models::TypeIdSet>,
) -> Result<toolkit_odata::ODataQuery, DomainError> {
    let identity = listing_identity(query.filter.as_deref(), type_set);
    if let Some(cursor) = &query.cursor
        && cursor.f != identity
    {
        return Err(DomainError::invalid(match (&cursor.f, &identity) {
            (Some(_), None) => {
                "the cursor was minted under a $filter or a type_pattern; send the same \
                 ones with it"
                    .to_owned()
            }
            (None, Some(_)) => {
                "the cursor was minted without a $filter or a type_pattern; neither can be \
                 added mid-listing"
                    .to_owned()
            }
            _ => "the cursor was minted under a different $filter or type_pattern".to_owned(),
        }));
    }
    query.filter_hash = identity;
    Ok(query)
}

/// The tabular projection: the type patterns it is narrowed to, and the
/// `OData` query it is shaped by.
///
/// The REST extractor bounds the text of `$filter`, `$orderby` and `$select`
/// before it parses them (`toolkit::api::odata`), and a query built
/// in-process arrives already parsed, so those budgets never saw it. The same
/// budgets are applied here to what the tree carries -- identifiers, function
/// names and string values, and the node and field counts -- which is a lower
/// bound on the text that would have spelled it, so nothing REST admits is
/// refused.
pub fn admit_projection(
    cfg: &GraphStorageConfig,
    type_patterns: &[String],
    query: &toolkit_odata::ODataQuery,
) -> Result<(), DomainError> {
    for (index, pattern) in type_patterns.iter().enumerate() {
        admit_identifier(cfg, &format!("type_patterns[{index}]"), pattern)?;
    }
    let toolkit_odata::ODataQuery {
        filter,
        order,
        limit,
        cursor: _,
        filter_hash: _,
        select,
    } = query;
    // The platform parser already rejected unknown options and the
    // cursor-with-orderby combination; what remains is this gear's page
    // ceiling, which the parser cannot know.
    if let Some(limit) = limit
        && (*limit == 0 || *limit > u64::from(cfg.projection_max_page))
    {
        return Err(exceeded(format!(
            "$top {limit} is outside 1..={}",
            cfg.projection_max_page
        )));
    }
    // The REST extractor bounds each option twice, by text length and by
    // count (`MAX_NODES`, `MAX_ORDER_FIELDS`, `MAX_SELECT_FIELDS`), and the
    // counts are the bound a tree of one-character names would slip past
    // on bytes alone. Both are applied here; the six constants are the
    // extractor's own.
    //
    // The byte sums are a lower bound on the text REST would have measured,
    // and deliberately so: the tree carries no separators, parentheses,
    // quotes or keywords, `ast::Expr` has no serializer to recover them, and
    // an estimate that overshot would refuse a query REST admits. The
    // overshoot the other way is bounded by the counts: a node spells at
    // most eight bytes of syntax (` and `, a pair of parentheses, a pair of
    // quotes), an order key at most six (`,`, ` desc`), a select field one
    // (`,`), so an in-process query admitted here is within
    // `8 * MAX_NODES` bytes of `MAX_FILTER_LEN`, `6 * MAX_ORDER_FIELDS` of
    // `MAX_ORDERBY_LEN` and `MAX_SELECT_FIELDS` of `MAX_SELECT_LEN` -- an
    // accepted asymmetry, not an unbounded one.
    if let Some(filter) = filter {
        let (bytes, nodes) = filter_shape(filter);
        if bytes > toolkit::api::odata::MAX_FILTER_LEN {
            return Err(exceeded(format!(
                "$filter carries {bytes} bytes of names and values; the bound is {}",
                toolkit::api::odata::MAX_FILTER_LEN
            )));
        }
        if nodes > toolkit::api::odata::MAX_NODES {
            return Err(exceeded(format!(
                "$filter has {nodes} nodes; the bound is {}",
                toolkit::api::odata::MAX_NODES
            )));
        }
    }
    if order.0.len() > toolkit::api::odata::MAX_ORDER_FIELDS {
        return Err(exceeded(format!(
            "$orderby names {} fields; the bound is {}",
            order.0.len(),
            toolkit::api::odata::MAX_ORDER_FIELDS
        )));
    }
    let order_bytes: usize = order.0.iter().map(|key| key.field.len()).sum();
    if order_bytes > toolkit::api::odata::MAX_ORDERBY_LEN {
        return Err(exceeded(format!(
            "$orderby names {order_bytes} bytes of fields; the bound is {}",
            toolkit::api::odata::MAX_ORDERBY_LEN
        )));
    }
    if let Some(select) = select {
        if select.len() > toolkit::api::odata::MAX_SELECT_FIELDS {
            return Err(exceeded(format!(
                "$select names {} fields; the bound is {}",
                select.len(),
                toolkit::api::odata::MAX_SELECT_FIELDS
            )));
        }
        let select_bytes: usize = select.iter().map(String::len).sum();
        if select_bytes > toolkit::api::odata::MAX_SELECT_LEN {
            return Err(exceeded(format!(
                "$select names {select_bytes} bytes of fields; the bound is {}",
                toolkit::api::odata::MAX_SELECT_LEN
            )));
        }
    }
    Ok(())
}

/// What a parsed `$filter` carries, as `(bytes, nodes)`: every identifier,
/// function name and string value, plus one byte per operator so a tree of
/// nothing but operators is not free; and one node per expression, counted
/// the way the REST extractor counts them for `MAX_NODES`. Walked with an
/// explicit stack; a caller building the tree in-process can nest it as
/// deeply as it likes.
fn filter_shape(root: &toolkit_odata::ast::Expr) -> (usize, usize) {
    use toolkit_odata::ast::{Expr, Value};
    let (mut bytes, mut nodes) = (0usize, 0usize);
    let mut stack = vec![root];
    while let Some(expr) = stack.pop() {
        nodes = nodes.saturating_add(1);
        match expr {
            Expr::And(left, right) | Expr::Or(left, right) | Expr::Compare(left, _, right) => {
                bytes = bytes.saturating_add(1);
                stack.push(left);
                stack.push(right);
            }
            Expr::Not(inner) => {
                bytes = bytes.saturating_add(1);
                stack.push(inner);
            }
            Expr::In(subject, items) => {
                bytes = bytes.saturating_add(1);
                stack.push(subject);
                stack.extend(items.iter());
            }
            Expr::Function(name, args) => {
                bytes = bytes.saturating_add(1).saturating_add(name.len());
                stack.extend(args.iter());
            }
            Expr::Identifier(name) => bytes = bytes.saturating_add(name.len()),
            Expr::Value(Value::String(text)) => bytes = bytes.saturating_add(text.len()),
            Expr::Value(_) => bytes = bytes.saturating_add(1),
        }
    }
    (bytes, nodes)
}

/// A type registration, and the migrations filed with it.
///
/// The type id is the identifier the catalogue is keyed by, interned and
/// carried on every row of that type; a migration names the type it rewrites
/// and the payload paths it moves. Each is bounded like any other identifier.
/// The schema itself is not measured here: it is admitted by validation
/// against the GTS meta-schema, and a byte ceiling on it would be a setting
/// the configuration does not have.
pub fn admit_registration(
    cfg: &GraphStorageConfig,
    batch: &[TypeRegistration],
    migrations: &[MigrationSpec],
) -> Result<(), DomainError> {
    for (index, registration) in batch.iter().enumerate() {
        let TypeRegistration { type_id, schema } = registration;
        admit_identifier(cfg, &format!("types[{index}].type_id"), type_id)?;
        refuse_nul_in_json(&format!("types[{index}].schema"), schema)?;
        admit_json_bytes(cfg, &format!("types[{index}].schema"), schema)?;
    }
    for (index, migration) in migrations.iter().enumerate() {
        let MigrationSpec { type_id, steps } = migration;
        admit_identifier(cfg, &format!("migrations[{index}].type_id"), type_id)?;
        for (step_index, step) in steps.iter().enumerate() {
            let what = |field: &str| format!("migrations[{index}].steps[{step_index}].{field}");
            match step {
                MigrationStep::Rename { from, to } => {
                    admit_identifier(cfg, &what("from"), from)?;
                    admit_identifier(cfg, &what("to"), to)?;
                }
                MigrationStep::Default { path, value } => {
                    admit_identifier(cfg, &what("path"), path)?;
                    refuse_nul_in_json(&what("value"), value)?;
                    admit_json_bytes(cfg, &what("value"), value)?;
                }
                MigrationStep::Drop { path } => {
                    admit_identifier(cfg, &what("path"), path)?;
                }
            }
        }
    }
    Ok(())
}

/// A caller-supplied JSON document that is not a payload -- a type's schema,
/// a migration step's default -- is bounded by `payload_max_bytes` like one.
/// A schema is walked, analyzed against its ancestors and compiled into a
/// validator before anything is stored, and a step's default is written into
/// every row of the type, so both do work in proportion to their size; the
/// identifiers beside them are bounded, and an unbounded document next to
/// them was the one field admission did not measure.
fn admit_json_bytes(
    cfg: &GraphStorageConfig,
    what: &str,
    value: &serde_json::Value,
) -> Result<(), DomainError> {
    let bytes = json_bytes(value);
    if bytes > u64::from(cfg.payload_max_bytes) {
        return Err(exceeded(format!(
            "{what} is {bytes} bytes; payload_max_bytes is {}",
            cfg.payload_max_bytes
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

#[cfg(test)]
mod tests {
    use graph_storage_sdk::models::NeighborhoodRequest;

    use super::{
        GraphStorageConfig, admit_ingest, admit_neighborhood, admit_projection, admit_registration,
        admit_traverse, admit_type_query,
    };

    fn neighborhood(depth: u8) -> NeighborhoodRequest {
        NeighborhoodRequest {
            root: "root".to_owned(),
            depth,
            node_budget: None,
            include_phantoms: true,
        }
    }

    /// The scope a replacement is filed under is bounded like every other
    /// caller-supplied identifier.
    ///
    /// `attribute` and `value` are the pair that outlives the request: they
    /// become the primary key of `scope_registry` and two indexed columns on
    /// every edge the scope owns. Admission bounded every key *in* the batch
    /// and not the key the batch is filed *under*, so a write-authorized
    /// caller could hand the database a multi-megabyte primary key while
    /// every other string in the same request was checked.
    #[test]
    fn the_scope_a_replacement_declares_is_bounded_like_any_other_identifier() {
        use graph_storage_sdk::models::{IngestOptions, IngestRequest, ReplaceScope};

        let cfg = GraphStorageConfig::default();
        let batch = |attribute: String, value: String| IngestRequest {
            nodes: Vec::new(),
            edges: Vec::new(),
            options: IngestOptions::default(),
            replace_scope: Some(ReplaceScope {
                attribute,
                value,
                generation: 1,
            }),
            idempotency_key: None,
        };
        let fits = "a".repeat(cfg.identifier_max_bytes as usize);
        let over = "a".repeat(cfg.identifier_max_bytes as usize + 1);

        admit_ingest(&cfg, &batch(fits.clone(), fits.clone())).expect("at the ceiling is admitted");

        for (attribute, value, named) in [
            (over.clone(), fits.clone(), "replace_scope.attribute"),
            (fits, over, "replace_scope.value"),
        ] {
            let refused = admit_ingest(&cfg, &batch(attribute, value))
                .expect_err("a key the database will index is not unbounded");
            assert!(
                refused.to_string().contains(named),
                "the refusal names the field: {refused}"
            );
        }
    }

    /// And its bytes are charged to the batch, not carried for free.
    #[test]
    fn the_scopes_own_bytes_count_against_the_batch_budget() {
        use graph_storage_sdk::models::{IngestOptions, IngestRequest, NodeSpec, ReplaceScope};

        let cfg = GraphStorageConfig {
            // Room for the scope and almost nothing else.
            ingest_max_bytes: u64::from(GraphStorageConfig::default().identifier_max_bytes) * 2 + 8,
            ..GraphStorageConfig::default()
        };
        let filler = "a".repeat(cfg.identifier_max_bytes as usize);
        let request = IngestRequest {
            nodes: vec![NodeSpec {
                node_key: "k".repeat(16),
                type_id: "t".repeat(16),
                ..NodeSpec::default()
            }],
            edges: Vec::new(),
            options: IngestOptions::default(),
            replace_scope: Some(ReplaceScope {
                attribute: filler.clone(),
                value: filler,
                generation: 1,
            }),
            idempotency_key: None,
        };
        let refused = admit_ingest(&cfg, &request)
            .expect_err("the scope fills the budget, so the node does not fit after it");
        assert!(
            refused.to_string().contains("ingest_max_bytes"),
            "the refusal is the batch budget: {refused}"
        );
    }

    /// The operator's depth ceiling governs both bounded walks.
    ///
    /// Neighborhood and traversal drive one BFS, differing only in which
    /// nodes truncation keeps, and the knob is documented as *the* traversal
    /// depth ceiling. A literal cap in one of them made the setting mean less
    /// than it says in the direction that matters: an operator lowering it to
    /// contain load left neighborhood answering as deep as before.
    #[test]
    fn the_configured_depth_ceiling_governs_neighborhood_too() {
        let tightened = GraphStorageConfig {
            traversal_max_depth: 2,
            ..GraphStorageConfig::default()
        };
        admit_neighborhood(&tightened, &neighborhood(2)).expect("at the ceiling is admitted");
        let refused = admit_neighborhood(&tightened, &neighborhood(3))
            .expect_err("a lowered ceiling has to bind neighborhood as well");
        assert!(
            refused.to_string().contains("1..=2"),
            "the refusal names the configured ceiling: {refused}"
        );

        // And the other direction, which is the one a reader assumes works:
        // raising it loosens both.
        let loosened = GraphStorageConfig {
            traversal_max_depth: 6,
            ..GraphStorageConfig::default()
        };
        admit_neighborhood(&loosened, &neighborhood(6))
            .expect("a raised ceiling admits what it says it admits");
        admit_neighborhood(&loosened, &neighborhood(7)).expect_err("and still refuses past it");

        // Zero is not a depth, whatever the ceiling.
        admit_neighborhood(&loosened, &neighborhood(0)).expect_err("depth 0 is not a walk");
    }

    /// The two endpoints answer the same question the same way.
    #[test]
    fn neighborhood_and_traversal_refuse_the_same_depths() {
        use graph_storage_sdk::models::TraverseRequest;

        let cfg = GraphStorageConfig {
            traversal_max_depth: 4,
            ..GraphStorageConfig::default()
        };
        let walk = |depth: u8| TraverseRequest {
            seeds: vec!["root".to_owned()],
            depth,
            edge_type_patterns: Vec::new(),
            node_type_patterns: Vec::new(),
            max_nodes: None,
        };
        for depth in 0..=8u8 {
            assert_eq!(
                admit_traverse(&cfg, &walk(depth)).is_ok(),
                admit_neighborhood(&cfg, &neighborhood(depth)).is_ok(),
                "depth {depth} is admitted by one and not the other"
            );
        }
    }

    /// Every identifier a request carries is bounded, whichever request it
    /// is and whichever field carries it.
    ///
    /// Listed by field, and the list is what the exhaustive patterns in the
    /// `admit_*` functions are read against. The bound on `type_id` was
    /// missing for twelve review rounds while every neighbouring field was
    /// checked, because each round added the one field that round had named.
    #[test]
    fn every_identifier_field_of_every_request_is_bounded() {
        use graph_storage_sdk::models::{
            EdgeSpec, IngestOptions, IngestRequest, MigrationSpec, MigrationStep, NodeSpec,
            TypeQuery, TypeRegistration,
        };

        let cfg = GraphStorageConfig::default();
        let fits = "x".repeat(cfg.identifier_max_bytes as usize);
        let over = "x".repeat(cfg.identifier_max_bytes as usize + 1);
        let ingest = |nodes: Vec<NodeSpec>, edges: Vec<EdgeSpec>| IngestRequest {
            nodes,
            edges,
            options: IngestOptions::default(),
            replace_scope: None,
            idempotency_key: None,
        };
        let node = |type_id: &str| NodeSpec {
            node_key: "k".to_owned(),
            type_id: type_id.to_owned(),
            ..NodeSpec::default()
        };
        let edge = |type_id: &str| EdgeSpec {
            type_id: type_id.to_owned(),
            src_node_key: "a".to_owned(),
            dst_node_key: "b".to_owned(),
            ..EdgeSpec::default()
        };
        let registration = |type_id: &str| TypeRegistration {
            type_id: type_id.to_owned(),
            schema: serde_json::json!({}),
        };
        let migration = |type_id: &str, step: MigrationStep| MigrationSpec {
            type_id: type_id.to_owned(),
            steps: vec![step],
        };
        let rename = |from: &str, to: &str| MigrationStep::Rename {
            from: from.to_owned(),
            to: to.to_owned(),
        };
        let default = |path: &str| MigrationStep::Default {
            path: path.to_owned(),
            value: serde_json::json!(1),
        };
        let drop = |path: &str| MigrationStep::Drop {
            path: path.to_owned(),
        };
        let type_query = |pattern: Option<&str>, cursor: Option<&str>| TypeQuery {
            kind: None,
            pattern: pattern.map(str::to_owned),
            top: None,
            cursor: cursor.map(str::to_owned),
        };

        let admitted = [
            admit_ingest(&cfg, &ingest(vec![node(&fits)], vec![edge(&fits)])),
            admit_type_query(&cfg, &type_query(Some(&fits), Some(&fits))),
            admit_projection(
                &cfg,
                std::slice::from_ref(&fits),
                &toolkit_odata::ODataQuery::default(),
            ),
            admit_registration(
                &cfg,
                &[registration(&fits)],
                &[
                    migration(&fits, rename(&fits, &fits)),
                    migration(&fits, default(&fits)),
                    migration(&fits, drop(&fits)),
                ],
            ),
        ];
        for outcome in admitted {
            outcome.expect("at the ceiling every identifier is admitted");
        }

        // One case per identifier-bearing field, and the list is the
        // reviewer's checklist: a field with no over-the-ceiling case here is
        // a bound that can be dropped without a test noticing.
        let refused = [
            (
                "node[0] type_id",
                admit_ingest(&cfg, &ingest(vec![node(&over)], Vec::new())),
            ),
            (
                "edge[0] type_id",
                admit_ingest(&cfg, &ingest(Vec::new(), vec![edge(&over)])),
            ),
            (
                "cursor",
                admit_type_query(&cfg, &type_query(None, Some(&over))),
            ),
            (
                "pattern",
                admit_type_query(&cfg, &type_query(Some(&over), None)),
            ),
            (
                "type_patterns[0]",
                admit_projection(
                    &cfg,
                    std::slice::from_ref(&over),
                    &toolkit_odata::ODataQuery::default(),
                ),
            ),
            (
                "types[0].type_id",
                admit_registration(&cfg, &[registration(&over)], &[]),
            ),
            (
                "migrations[0].type_id",
                admit_registration(
                    &cfg,
                    &[],
                    &[migration(&over, rename("/payload/a", "/payload/b"))],
                ),
            ),
            (
                "migrations[0].steps[0].from",
                admit_registration(&cfg, &[], &[migration("t", rename(&over, "/payload/b"))]),
            ),
            (
                "migrations[0].steps[0].to",
                admit_registration(&cfg, &[], &[migration("t", rename("/payload/a", &over))]),
            ),
            (
                "migrations[0].steps[0].path",
                admit_registration(&cfg, &[], &[migration("t", default(&over))]),
            ),
            (
                "migrations[0].steps[0].path",
                admit_registration(&cfg, &[], &[migration("t", drop(&over))]),
            ),
        ];
        for (field, outcome) in refused {
            let refusal = outcome.expect_err("one byte over the ceiling is refused");
            assert!(
                refusal.to_string().contains(field),
                "the refusal names `{field}`: {refusal}"
            );
        }
    }

    /// A query built in-process is held to the budgets the REST extractor
    /// applies to the text it never saw.
    #[test]
    fn an_in_process_projection_query_is_bounded_like_a_rest_one() {
        use toolkit_odata::ast::Expr;

        let cfg = GraphStorageConfig::default();
        let wide = Expr::Identifier("f".repeat(toolkit::api::odata::MAX_FILTER_LEN + 1));
        let query = toolkit_odata::ODataQuery::default().with_filter(wide);
        let refused = admit_projection(&cfg, &[], &query)
            .expect_err("a filter wider than the REST budget is refused here too");
        assert!(refused.to_string().contains("$filter"), "{refused}");

        let narrow = Expr::Identifier("name".to_owned());
        admit_projection(
            &cfg,
            &[],
            &toolkit_odata::ODataQuery::default().with_filter(narrow),
        )
        .expect("a filter REST would admit is admitted");

        // And the counts, which bytes alone would not see: one-character
        // names that stay far inside the byte budgets.
        let mut deep = Expr::Identifier("a".to_owned());
        for _ in 0..toolkit::api::odata::MAX_NODES.div_euclid(2) {
            deep = deep.and(Expr::Identifier("a".to_owned()));
        }
        let refused = admit_projection(
            &cfg,
            &[],
            &toolkit_odata::ODataQuery::default().with_filter(deep),
        )
        .expect_err("a filter with more nodes than REST admits is refused");
        assert!(refused.to_string().contains("nodes"), "{refused}");

        let many_keys = toolkit_odata::ODataOrderBy(
            (0..=toolkit::api::odata::MAX_ORDER_FIELDS)
                .map(|_| toolkit_odata::OrderKey {
                    field: "f".to_owned(),
                    dir: toolkit_odata::SortDir::Asc,
                })
                .collect(),
        );
        let refused = admit_projection(
            &cfg,
            &[],
            &toolkit_odata::ODataQuery::default().with_order(many_keys),
        )
        .expect_err("more order fields than REST admits are refused");
        assert!(refused.to_string().contains("$orderby names"), "{refused}");

        let many_fields: Vec<String> = (0..=toolkit::api::odata::MAX_SELECT_FIELDS)
            .map(|_| "s".to_owned())
            .collect();
        let query = toolkit_odata::ODataQuery {
            select: Some(many_fields),
            ..toolkit_odata::ODataQuery::default()
        };
        let refused = admit_projection(&cfg, &[], &query)
            .expect_err("more select fields than REST admits are refused");
        assert!(refused.to_string().contains("$select names"), "{refused}");

        // And exactly at each count, admitted: the boundary REST accepts is
        // the boundary this accepts, so an off-by-one cannot refuse a query
        // the REST path serves.
        let mut at_limit = Expr::Identifier("a".to_owned());
        for _ in 1..toolkit::api::odata::MAX_NODES {
            at_limit = !at_limit;
        }
        admit_projection(
            &cfg,
            &[],
            &toolkit_odata::ODataQuery::default().with_filter(at_limit),
        )
        .expect("a filter with exactly MAX_NODES nodes is admitted");
        let keys_at_limit = toolkit_odata::ODataOrderBy(
            (0..toolkit::api::odata::MAX_ORDER_FIELDS)
                .map(|_| toolkit_odata::OrderKey {
                    field: "f".to_owned(),
                    dir: toolkit_odata::SortDir::Asc,
                })
                .collect(),
        );
        admit_projection(
            &cfg,
            &[],
            &toolkit_odata::ODataQuery::default().with_order(keys_at_limit),
        )
        .expect("exactly MAX_ORDER_FIELDS order keys are admitted");
        let fields_at_limit: Vec<String> = (0..toolkit::api::odata::MAX_SELECT_FIELDS)
            .map(|_| "s".to_owned())
            .collect();
        admit_projection(
            &cfg,
            &[],
            &toolkit_odata::ODataQuery {
                select: Some(fields_at_limit),
                ..toolkit_odata::ODataQuery::default()
            },
        )
        .expect("exactly MAX_SELECT_FIELDS select fields are admitted");
    }
}
