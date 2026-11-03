//! The built-in traversal engine: a directed one-hop primitive over two
//! scoped statements, the incident edges and then the authorized endpoints.
//!
//! It needs no server capability beyond the schema.
//!
//! Both directions are collected as one union rather than two membership
//! tests under a disjunction — `id IN (out) OR id IN (inc)` cannot drive an
//! index from two hashed subplans.

use std::sync::Arc;

use async_trait::async_trait;
use graph_storage_sdk::models::{
    Direction, EdgeRef, EngineCapabilities, GraphRevision, TruncationReason, TypeIdSet,
};
use graph_storage_sdk::plugin_api::{
    EngineCursor, ExpandRequest, ExpandResponse, GraphEngineError, GraphEngineV1, HopBackend,
    PathResponse, PatternRequest, PatternResponse, ShortestPathRequest, StoreCtx,
};
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{DBRunner, ScopeError, SecureEntityExt};
use tracing::warn;

use crate::infra::logged;
use crate::infra::projections::{
    EdgeHop, EndpointPair, NodeIdent, TypeId, TypeName, edge_hop_columns, endpoint_pair_columns,
    node_ident_columns, type_id_columns, type_name_columns,
};
use crate::infra::storage::entity::{edge, gts_type, node};
use crate::infra::store::PgGraphStore;

/// What a hop says when a dependency failed it. The dependency's own text is
/// logged where it failed and does not travel: an error message is read by
/// the caller and logged again at the edge, and a server's diagnostic is
/// neither bounded nor free of control characters.
const NO_CONNECTION: &str = "the database gave no connection; the reason is in the gear's log";

pub struct PgGraphEngine {
    store: Arc<PgGraphStore>,
}

impl PgGraphEngine {
    #[must_use]
    pub fn new(store: Arc<PgGraphStore>) -> Self {
        Self { store }
    }
}

fn engine_error(error: graph_storage_sdk::plugin_api::GraphStoreError) -> GraphEngineError {
    use graph_storage_sdk::plugin_api::GraphStoreError as E;
    match error {
        E::ScopeUnservable { reason } => GraphEngineError::ScopeNotEnforceable { reason },
        E::Unavailable { reason } => GraphEngineError::Unavailable { reason },
        E::Deadline => GraphEngineError::Deadline,
        E::Cancelled => GraphEngineError::Cancelled,
        other => GraphEngineError::Internal(other.to_string()),
    }
}

fn scope_error(error: ScopeError) -> GraphEngineError {
    match error {
        ScopeError::UnresolvedScopeProperty { element, property } => {
            GraphEngineError::ScopeNotEnforceable {
                reason: format!(
                    "scope does not resolve on element `{element}` property `{property}`"
                ),
            }
        }
        // A syntax refusal is this gear's declaration being wrong, not the
        // caller's scope being unservable. The same pattern is malformed on
        // every request, so reporting it as a scope problem would send the
        // caller after their own permissions for a bug that is ours.
        ScopeError::GraphSyntax(inner) => {
            GraphEngineError::Internal(format!("graph pattern is malformed: {inner}"))
        }
        other => GraphEngineError::Internal(other.to_string()),
    }
}

#[async_trait]
impl GraphEngineV1 for PgGraphEngine {
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            shortest_path: false,
            match_pattern: false,
        }
    }

    async fn cursor(&self, ctx: &StoreCtx<'_>) -> Result<EngineCursor, GraphEngineError> {
        let revision: GraphRevision = crate::infra::store::reads::revision(&self.store, ctx)
            .await
            .map_err(engine_error)?;
        Ok(EngineCursor { revision })
    }

    async fn expand(
        &self,
        ctx: &StoreCtx<'_>,
        req: ExpandRequest,
    ) -> Result<ExpandResponse, GraphEngineError> {
        if req.labels.is_some() {
            return Err(GraphEngineError::Unsupported {
                what: "per-hop label filters",
            });
        }
        if req.frontier.is_empty() {
            return Ok(ExpandResponse {
                reached: Vec::new(),
                degrees: Vec::new(),
                edges: Vec::new(),
                truncated: None,
                served_by: HopBackend::TwoQuery,
            });
        }
        if req.frontier.len() as u64 > u64::from(req.budget.max_frontier) {
            return Ok(ExpandResponse {
                reached: Vec::new(),
                degrees: Vec::new(),
                edges: Vec::new(),
                truncated: Some(TruncationReason::FrontierCap),
                served_by: HopBackend::TwoQuery,
            });
        }

        expand_two_query(&self.store, ctx, &req).await
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

/// Interned ids of the requested edge types, or `None` for "any type".
async fn edge_type_ids(
    ctx: &StoreCtx<'_>,
    runner: &impl DBRunner,
    types: Option<&TypeIdSet>,
) -> Result<Option<Vec<i32>>, GraphEngineError> {
    let Some(set) = types else {
        return Ok(None);
    };
    let names: Vec<String> = set.0.iter().cloned().collect();
    let rows = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(gts_type::Column::GtsTypeId.is_in(names)))
        .project_all(runner, |query| {
            type_id_columns(query).into_model::<TypeId>()
        })
        .await
        .map_err(scope_error)?;
    Ok(Some(rows.into_iter().map(|r| r.id).collect()))
}

/// The scoped, tombstone-free edge rows incident to the frontier, and the far
/// endpoints reached through them.
///
/// Every far endpoint is authorized by the scoped node read, which is the
/// two-query hop's second query.
/// One hop's incident edges and the authorized far endpoints they reach,
/// plus whether the edge-scan budget cut the scan short.
struct Incidence {
    edges: Vec<EdgeRef>,
    reached: Vec<i64>,
    /// Index-aligned with `reached`: how many of this hop's edges touch it.
    degrees: Vec<u32>,
    /// True when the scan found more incident edges than the budget allows.
    /// The budget still bounds the work — the extra row is read to know, and
    /// discarded — but the answer now says it is partial. A hop that trims
    /// silently returns a subgraph a caller cannot tell from a complete one.
    over_edge_budget: bool,
}

async fn live_edges(
    ctx: &StoreCtx<'_>,
    runner: &impl DBRunner,
    req: &ExpandRequest,
) -> Result<Incidence, GraphEngineError> {
    let type_ids = edge_type_ids(ctx, runner, req.edge_types.as_ref()).await?;

    let mut incidence = Condition::any();
    match req.direction {
        Direction::Outgoing => {
            incidence = incidence.add(edge::Column::SrcNodeId.is_in(req.frontier.clone()));
        }
        Direction::Incoming => {
            incidence = incidence.add(edge::Column::DstNodeId.is_in(req.frontier.clone()));
        }
        Direction::Either => {
            incidence = incidence
                .add(edge::Column::SrcNodeId.is_in(req.frontier.clone()))
                .add(edge::Column::DstNodeId.is_in(req.frontier.clone()));
        }
    }

    let mut select = edge::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(incidence)
        .filter(Condition::all().add(edge::Column::DeletedAt.is_null()));
    if let Some(ids) = &type_ids {
        select =
            select.filter(Condition::all().add(edge::Column::GtsEdgeTypeId.is_in(ids.clone())));
    }
    // One row past the budget, so the difference between "exactly the budget"
    // and "more than the budget" is observable; the extra row is then dropped.
    let mut rows: Vec<EdgeHop> = select
        .limit(req.budget.max_edges_scanned.saturating_add(1))
        .project_all(runner, |query| {
            edge_hop_columns(query).into_model::<EdgeHop>()
        })
        .await
        .map_err(scope_error)?;
    let over_edge_budget = rows.len() as u64 > req.budget.max_edges_scanned;
    rows.truncate(usize::try_from(req.budget.max_edges_scanned).unwrap_or(usize::MAX));
    let rows_scanned = rows.len();

    // Endpoints the caller may not see are not reachable: a scoped node read
    // decides which endpoints exist for this caller.
    let mut endpoint_ids: Vec<i64> = rows
        .iter()
        .flat_map(|e| [e.src_node_id, e.dst_node_id])
        .collect();
    endpoint_ids.sort_unstable();
    endpoint_ids.dedup();

    // The key and the id, not the node. This runs per hop over every distinct
    // endpoint the hop touched, and reading whole rows meant dragging back a
    // payload and a 384-lane embedding per endpoint to build a map of two
    // small fields -- on the latency path, where the cost scales with how wide
    // the tenant's payloads happen to be rather than with the work.
    let visible: Vec<NodeIdent> = node::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(node::Column::Id.is_in(endpoint_ids)))
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .project_all(runner, |query| {
            node_ident_columns(query).into_model::<NodeIdent>()
        })
        .await
        .map_err(scope_error)?;
    let keys: std::collections::BTreeMap<i64, String> =
        visible.into_iter().map(|n| (n.id, n.node_key)).collect();

    let mut type_names: Vec<i32> = rows.iter().map(|e| e.gts_edge_type_id).collect();
    type_names.sort_unstable();
    type_names.dedup();
    let names = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(gts_type::Column::Id.is_in(type_names)))
        .project_all(runner, |query| {
            type_name_columns(query).into_model::<TypeName>()
        })
        .await
        .map_err(scope_error)?
        .into_iter()
        .map(|t| (t.id, t.gts_type_id))
        .collect::<std::collections::BTreeMap<_, _>>();

    let frontier: std::collections::BTreeSet<i64> = req.frontier.iter().copied().collect();
    let mut reached: Vec<i64> = Vec::new();
    let mut edges = Vec::new();
    for e in &rows {
        let (Some(src), Some(dst)) = (keys.get(&e.src_node_id), keys.get(&e.dst_node_id)) else {
            // An endpoint the caller cannot see makes the edge unreachable;
            // denied and nonexistent are indistinguishable.
            continue;
        };
        if frontier.contains(&e.src_node_id) {
            reached.push(e.dst_node_id);
        }
        if frontier.contains(&e.dst_node_id) {
            reached.push(e.src_node_id);
        }
        edges.push(EdgeRef {
            edge_key: e.edge_key.clone(),
            edge_type_id: names.get(&e.gts_edge_type_id).cloned().unwrap_or_default(),
            src: src.clone(),
            dst: dst.clone(),
        });
    }
    reached.sort_unstable();
    reached.dedup();

    // The degree that decides which neighbours of a hub survive a node
    // budget is the neighbour's *own* connectivity, not how many edges tie it
    // to this frontier — at depth one those are all exactly one, so the
    // within-hop count would rank a hub's neighbours arbitrarily, which is
    // the whole failure the requirement names. So it is a second scoped read
    // over the edges incident to the reached set, taken only when the caller
    // asked (a traversal does not pay for it).
    // The budget is per hop, not per scan. This second read used to take a
    // fresh `max_edges_scanned` of its own, so asking for degrees quietly
    // doubled what a hop could read -- and a ceiling that means one number
    // when you ask for degrees and another when you do not is not a number a
    // deployment can plan around. It gets what the first scan left.
    let remaining = req
        .budget
        .max_edges_scanned
        .saturating_sub(rows_scanned as u64);
    let (degrees, degree_scan_over_budget) = if req.with_degrees && !reached.is_empty() {
        degrees_of(ctx, runner, &reached, remaining).await?
    } else {
        (Vec::new(), false)
    };

    Ok(Incidence {
        edges,
        reached,
        degrees,
        over_edge_budget: over_edge_budget || degree_scan_over_budget,
    })
}

/// Each reached node's degree in the authorized subgraph, index-aligned with
/// `reached`, and whether the scan hit the hop's edge budget.
///
/// Counted in Rust from scoped rows rather than by a `GROUP BY`: the
/// platform's secure ORM exposes `all`, `one` and `count` on a scoped select
/// and no aggregate projection, and a per-node `count()` would be one
/// statement per neighbour. The scan carries the hop's own edge budget, so a
/// dense region bounds this read exactly as it bounds the incidence read; on
/// overflow the counts are a lower bound and the hop says so.
async fn degrees_of(
    ctx: &StoreCtx<'_>,
    runner: &impl DBRunner,
    reached: &[i64],
    budget: u64,
) -> Result<(Vec<u32>, bool), GraphEngineError> {
    // Nothing left of the hop's allowance: the degrees are unknown rather
    // than zero, and the answer says the scan was cut short so retention does
    // not silently rank every neighbour the same.
    if budget == 0 {
        return Ok((vec![0; reached.len()], true));
    }
    let incidence = Condition::any()
        .add(edge::Column::SrcNodeId.is_in(reached.to_vec()))
        .add(edge::Column::DstNodeId.is_in(reached.to_vec()));
    let mut rows = edge::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(incidence)
        .filter(Condition::all().add(edge::Column::DeletedAt.is_null()))
        .limit(budget.saturating_add(1))
        .project_all(runner, |query| {
            endpoint_pair_columns(query).into_model::<EndpointPair>()
        })
        .await
        .map_err(scope_error)?;
    let over_budget = rows.len() as u64 > budget;
    rows.truncate(usize::try_from(budget).unwrap_or(usize::MAX));

    let mut incident: std::collections::BTreeMap<i64, u32> = std::collections::BTreeMap::new();
    for row in &rows {
        for id in [row.src_node_id, row.dst_node_id] {
            *incident.entry(id).or_default() += 1;
        }
    }
    let degrees = reached
        .iter()
        .map(|id| incident.get(id).copied().unwrap_or(0))
        .collect();
    Ok((degrees, over_budget))
}

/// Two scoped queries: the incident live edges, then the authorized far
/// endpoints. Needs no server capability and serves every scope shape.
///
async fn expand_two_query(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    req: &ExpandRequest,
) -> Result<ExpandResponse, GraphEngineError> {
    let conn = store.db().conn().map_err(|error| {
        warn!(error = %logged(&error), "the database gave the hop no connection");
        GraphEngineError::Unavailable {
            reason: NO_CONNECTION.to_owned(),
        }
    })?;

    let incidence = live_edges(ctx, &conn, req).await?;

    Ok(ExpandResponse {
        truncated: hop_truncation(req, &incidence),
        reached: incidence.reached,
        degrees: incidence.degrees,
        edges: incidence.edges,
        served_by: HopBackend::TwoQuery,
    })
}

/// Which bound, if either, cut this hop short.
///
/// The edge budget is reported ahead of the frontier cap when both are hit:
/// it is the earlier cut, and the frontier the caller sees was computed from
/// an already-incomplete edge set, so naming the frontier would send them to
/// raise the wrong limit.
fn hop_truncation(req: &ExpandRequest, incidence: &Incidence) -> Option<TruncationReason> {
    if incidence.over_edge_budget {
        return Some(TruncationReason::EdgeScanCap);
    }
    (incidence.reached.len() as u64 > u64::from(req.budget.max_frontier))
        .then_some(TruncationReason::FrontierCap)
}
