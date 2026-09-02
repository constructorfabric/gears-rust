//! The built-in traversal engine: a directed one-hop primitive with two
//! interchangeable backends that must return byte-identical answers.
//!
//! **SQL/PGQ** (`GRAPH_TABLE`, `PostgreSQL` 19+) expands the frontier in one
//! statement, with the caller's scope embedded in every pattern element.
//! **Two-query** is the universal fallback that needs no server capability.
//!
//! A scope the pattern cannot carry is never served with a weaker predicate:
//! the pattern refuses, and the request falls back with a logged reason.
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
use sea_orm::sea_query::{Alias, Expr, ExprTrait as _};
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult};
use toolkit_db::secure::{DBRunner, ScopeError, SecureEntityExt};
use toolkit_security::AccessScope;
use tracing::warn;

use crate::config::HopStrategy;
use crate::infra::storage::entity::{edge, gts_type, node};
use crate::infra::storage::graph::KnowledgeGraph;
use crate::infra::store::PgGraphStore;

/// Whether this server can actually serve `GRAPH_TABLE` over the declared
/// property graph.
///
/// A gear cannot ask the catalog — the secure ORM's runner is sealed, on
/// purpose — but it does not need to: the capability that matters is not "what
/// major is this" but "will a pattern over `kb` execute here", and that is
/// answered by attempting one. The probe runs the same builder every hop uses,
/// under a scope that matches no rows, so it costs one empty result set and
/// proves exactly the thing the hop depends on.
///
/// Called once at init. On a server without the property graph — `PostgreSQL` 16,
/// or 19 where the conditional migration skipped the DDL — this returns
/// `false` and every hop is served by the fallback backend, which is what
/// ADR-0001 promises. Without it the gear would attempt a pattern per request
/// and answer `500` on a configuration the specification calls supported.
pub async fn probe_pgq(db: &toolkit_db::secure::Db) -> bool {
    let Ok(conn) = db.conn() else {
        warn!("cannot probe SQL/PGQ: no connection; assuming it is unavailable");
        return false;
    };
    // A tenant that owns nothing: the statement plans and runs, and returns
    // no rows, so this observes the server's ability to parse and execute the
    // pattern rather than any tenant's data.
    let scope = AccessScope::for_tenant(uuid::Uuid::nil());
    let probe: Result<Vec<Reached>, _> = node::Entity::find()
        .secure()
        .scope_with(&scope)
        .with_graph::<KnowledgeGraph>()
        .match_path(|p| {
            p.vertex::<node::Entity>("a")
                .edge_to::<edge::Entity>("e")
                .to::<node::Entity>("b")
        })
        .column("b", "id", "neighbour")
        .limit(1)
        .all_as(&conn)
        .await;

    match probe {
        Ok(_) => true,
        Err(error) => {
            warn!(
                %error,
                "this server does not serve SQL/PGQ over the declared property graph; \
                 every hop will use the two-query backend"
            );
            false
        }
    }
}

/// What one attempt at the pattern hop produced.
enum PatternOutcome {
    Answered(ExpandResponse),
    /// The pattern did not execute here. The two-query hop answers the same
    /// question, so the request falls back rather than failing — with the
    /// reason logged, never silently.
    Unavailable(String),
}

pub struct PgGraphEngine {
    store: Arc<PgGraphStore>,
}

impl PgGraphEngine {
    #[must_use]
    pub fn new(store: Arc<PgGraphStore>) -> Self {
        Self { store }
    }

    /// The backend a request will actually use, given configuration and what
    /// the server can parse.
    fn effective_strategy(&self) -> HopStrategy {
        match self.store.config().traversal_hop {
            HopStrategy::Pgq if !self.store.pgq_available() => {
                warn!(
                    reason = "server does not provide SQL/PGQ",
                    "falling back to the two-query hop"
                );
                HopStrategy::TwoQuery
            }
            other => other,
        }
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
        ScopeError::Pgq(inner) => GraphEngineError::ScopeNotEnforceable {
            reason: format!("pattern cannot carry this scope: {inner}"),
        },
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
        // Nothing is walked on either of these paths, so the backend named is
        // the one that would have walked it.
        let would_serve = match self.effective_strategy() {
            HopStrategy::Pgq => HopBackend::Pattern,
            HopStrategy::TwoQuery => HopBackend::TwoQuery,
        };
        if req.frontier.is_empty() {
            return Ok(ExpandResponse {
                reached: Vec::new(),
                edges: Vec::new(),
                truncated: None,
                served_by: would_serve,
            });
        }
        if req.frontier.len() as u64 > u64::from(req.budget.max_frontier) {
            return Ok(ExpandResponse {
                reached: Vec::new(),
                edges: Vec::new(),
                truncated: Some(TruncationReason::FrontierCap),
                served_by: would_serve,
            });
        }

        match self.effective_strategy() {
            HopStrategy::Pgq => match expand_pgq(&self.store, ctx, &req).await {
                Ok(PatternOutcome::Answered(response)) => Ok(response),
                // Two different reasons, one response: the pattern could not
                // serve this request, and the two-query hop answers the same
                // question. Falling back is never silent — the reason is
                // logged either way.
                Ok(PatternOutcome::Unavailable(reason)) => {
                    warn!(reason = %reason, "graph pattern did not execute; serving the two-query hop");
                    expand_two_query(&self.store, ctx, &req).await
                }
                Err(GraphEngineError::ScopeNotEnforceable { reason }) => {
                    warn!(reason = %reason, "graph pattern refused this scope; serving the two-query hop");
                    expand_two_query(&self.store, ctx, &req).await
                }
                Err(other) => Err(other),
            },
            HopStrategy::TwoQuery => expand_two_query(&self.store, ctx, &req).await,
        }
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

#[derive(Debug, FromQueryResult)]
struct Reached {
    neighbour: i64,
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
        .all(runner)
        .await
        .map_err(scope_error)?;
    Ok(Some(rows.into_iter().map(|r| r.id).collect()))
}

/// One-statement hop through `GRAPH_TABLE`, anchored on the frontier.
///
/// The pattern is a candidate producer (ADR-0006): it carries the caller's
/// scope on every element, and the edge rows are then read back through an
/// ordinary scoped query, which is where the tombstone and edge-type filters
/// live — a column outside the element's `PROPERTIES` is invisible to
/// `MATCH`, so `deleted_at` cannot be expressed there.
async fn expand_pgq(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    req: &ExpandRequest,
) -> Result<PatternOutcome, GraphEngineError> {
    let conn = store
        .db()
        .conn()
        .map_err(|e| GraphEngineError::Unavailable {
            reason: e.to_string(),
        })?;

    let anchor_correlation = |variable: &'static str| {
        Condition::all()
            .add(
                Expr::col((Alias::new(variable), Alias::new("tenant_id")))
                    .eq(Expr::col((Alias::new("node"), Alias::new("tenant_id")))),
            )
            .add(
                Expr::col((Alias::new(variable), Alias::new("id")))
                    .eq(Expr::col((Alias::new("node"), Alias::new("id")))),
            )
    };

    let mut reached: Vec<i64> = Vec::new();

    // Outgoing and incoming are two directed patterns; their results are
    // unioned here rather than expressed as a disjunction in one statement.
    for direction in directions_of(req.direction) {
        let rows: Result<Vec<Reached>, ScopeError> = {
            let select = node::Entity::find()
                .secure()
                .scope_with(ctx.scope)
                .with_graph::<KnowledgeGraph>();
            let select = match direction {
                Direction::Outgoing => select.match_path(|p| {
                    p.vertex::<node::Entity>("a")
                        .where_(anchor_correlation("a"))
                        .edge_to::<edge::Entity>("e")
                        .to::<node::Entity>("b")
                }),
                _ => select.match_path(|p| {
                    p.vertex::<node::Entity>("a")
                        .where_(anchor_correlation("a"))
                        .edge_from::<edge::Entity>("e")
                        .to::<node::Entity>("b")
                }),
            };
            select
                .column("b", "id", "neighbour")
                .filter(Condition::all().add(node::Column::Id.is_in(req.frontier.clone())))
                .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
                .limit(u64::from(req.budget.max_frontier) + 1)
                .all_as(&conn)
                .await
        };
        let rows = match rows {
            Ok(rows) => rows,
            // The scope refusals stay distinguishable: those are about *this*
            // caller and are re-raised so the caller-facing reason survives.
            Err(error @ (ScopeError::UnresolvedScopeProperty { .. } | ScopeError::Pgq(_))) => {
                return Err(scope_error(error));
            }
            // Anything else the pattern statement did — most often that this
            // server has no property graph at all, because the conditional
            // migration skipped the DDL on a major below 19 — means the
            // pattern cannot serve the request here.
            Err(error) => return Ok(PatternOutcome::Unavailable(error.to_string())),
        };
        reached.extend(rows.into_iter().map(|r| r.neighbour));
    }

    reached.sort_unstable();
    reached.dedup();

    // The pattern embedded the caller's scope on every element, so its
    // candidates are authorized. What it could not express are the columns
    // outside the elements' `PROPERTIES` — `deleted_at` and the interned edge
    // type — so an ordinary scoped read applies those and produces the edges.
    let (edges, live) = live_edges(ctx, &conn, req, Some(&reached)).await?;

    let truncated = (live.len() as u64 > u64::from(req.budget.max_frontier))
        .then_some(TruncationReason::FrontierCap);

    Ok(PatternOutcome::Answered(ExpandResponse {
        reached: live,
        edges,
        truncated,
        served_by: HopBackend::Pattern,
    }))
}

fn directions_of(direction: Direction) -> Vec<Direction> {
    match direction {
        Direction::Outgoing => vec![Direction::Outgoing],
        Direction::Incoming => vec![Direction::Incoming],
        Direction::Either => vec![Direction::Outgoing, Direction::Incoming],
    }
}

/// The scoped, tombstone-free edge rows incident to the frontier, and the far
/// endpoints reached through them.
///
/// `candidates`, when present, restricts the far side to a set a pattern
/// already authorized; when absent every far endpoint is authorized here by
/// the scoped node read, which is the two-query hop's second query.
async fn live_edges(
    ctx: &StoreCtx<'_>,
    runner: &impl DBRunner,
    req: &ExpandRequest,
    candidates: Option<&[i64]>,
) -> Result<(Vec<EdgeRef>, Vec<i64>), GraphEngineError> {
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
    if let Some(candidates) = candidates {
        if candidates.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        // Only edges whose far endpoint survived the pattern's scope.
        let far = Condition::any()
            .add(edge::Column::DstNodeId.is_in(candidates.to_vec()))
            .add(edge::Column::SrcNodeId.is_in(candidates.to_vec()));
        select = select.filter(far);
    }

    let rows = select
        .limit(req.budget.max_edges_scanned)
        .all(runner)
        .await
        .map_err(scope_error)?;

    // Endpoints the caller may not see are not reachable: a scoped node read
    // decides which endpoints exist for this caller.
    let mut endpoint_ids: Vec<i64> = rows
        .iter()
        .flat_map(|e| [e.src_node_id, e.dst_node_id])
        .collect();
    endpoint_ids.sort_unstable();
    endpoint_ids.dedup();

    let visible = node::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(node::Column::Id.is_in(endpoint_ids)))
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .all(runner)
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
        .all(runner)
        .await
        .map_err(scope_error)?
        .into_iter()
        .map(|t| (t.id, t.gts_type_id))
        .collect::<std::collections::BTreeMap<_, _>>();

    let frontier: std::collections::BTreeSet<i64> = req.frontier.iter().copied().collect();
    let mut reached: Vec<i64> = Vec::new();
    let mut edges = Vec::new();
    for e in rows {
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
            edge_key: e.edge_key,
            edge_type_id: names.get(&e.gts_edge_type_id).cloned().unwrap_or_default(),
            src: src.clone(),
            dst: dst.clone(),
        });
    }
    reached.sort_unstable();
    reached.dedup();
    Ok((edges, reached))
}

/// Two scoped queries: the incident live edges, then the authorized far
/// endpoints. Needs no server capability and serves every scope shape.
///
/// It shares its second query with the pattern hop — the two backends differ
/// only in where the candidate set comes from, which is what lets the parity
/// tests hold them to byte-identical answers.
async fn expand_two_query(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    req: &ExpandRequest,
) -> Result<ExpandResponse, GraphEngineError> {
    let conn = store
        .db()
        .conn()
        .map_err(|e| GraphEngineError::Unavailable {
            reason: e.to_string(),
        })?;

    let (edges, reached) = live_edges(ctx, &conn, req, None).await?;
    let truncated = (reached.len() as u64 > u64::from(req.budget.max_frontier))
        .then_some(TruncationReason::FrontierCap);

    Ok(ExpandResponse {
        reached,
        edges,
        truncated,
        served_by: HopBackend::TwoQuery,
    })
}
