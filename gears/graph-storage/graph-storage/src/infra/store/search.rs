//! Search: two independent arms fused by Reciprocal Rank Fusion.
//!
//! The arms run independently and each reports its own rank, because RRF
//! needs ranks rather than scores and a caller must be able to see which arm
//! matched. Both arms are scoped inside the statement — before ranking and
//! before `LIMIT` — never filtered afterwards.
//!
//! The lexical predicate uses the very expression the GIN index was built on
//! (`to_tsvector(FTS_CONFIG, search_text)`, materialized as the generated
//! `search` column); a different configuration name would silently stop using
//! the index rather than fail.

use std::collections::BTreeMap;

use graph_storage_sdk::models::{
    ArmHit, SearchArm, SearchHit, SearchMode, SearchRequest, SearchResponse,
};
use graph_storage_sdk::plugin_api::{GraphStoreError, StoreCtx, VectorArm};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, QueryOrder, QuerySelect};
use toolkit_db::secure::{DBRunner, SecureEntityExt};

use crate::infra::projections::{TypeId, type_id_columns};
use crate::infra::storage::entity::{gts_type, node};
use crate::infra::storage::migrations::FTS_CONFIG;
use crate::infra::store::{PgGraphStore, map_db_error, map_scope_err};

/// RRF constant. 60 is the value the original publication uses and what every
/// comparison in the design's evaluation assumed.
const RRF_K: f64 = 60.0;

pub async fn search(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    request: SearchRequest,
    arm: Option<VectorArm>,
) -> Result<SearchResponse, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;

    let type_ids = if request.type_patterns.is_empty() {
        None
    } else {
        let set = super::types::resolve_type_set(store, ctx, &request.type_patterns).await?;
        let names: Vec<String> = set.0.into_iter().collect();
        let rows = gts_type::Entity::find()
            .secure()
            .scope_with(ctx.scope)
            .filter(Condition::all().add(gts_type::Column::GtsTypeId.is_in(names)))
            .project_all(&conn, |query| type_id_columns(query).into_model::<TypeId>())
            .await
            .map_err(map_scope_err)?;
        Some(rows.into_iter().map(|r| r.id).collect::<Vec<_>>())
    };

    let mut lexical: Vec<Ranked> = Vec::new();
    let mut vector: Vec<Ranked> = Vec::new();

    if matches!(request.mode, SearchMode::Lexical | SearchMode::Hybrid) {
        lexical = lexical_arm(ctx, &conn, &request, type_ids.as_deref()).await?;
    }
    // An absent arm is not an empty one by accident: the coordinator resolves
    // the query vector and the epoch together, and hands over neither when no
    // comparable space is in force.
    if let Some(arm) = &arm
        && matches!(request.mode, SearchMode::Vector | SearchMode::Hybrid)
    {
        vector = vector_arm(ctx, &conn, &request, type_ids.as_deref(), arm).await?;
    }

    let mut type_name_ids: Vec<i32> = lexical
        .iter()
        .chain(vector.iter())
        .map(|m| m.gts_node_type_id)
        .collect();
    type_name_ids.sort_unstable();
    type_name_ids.dedup();
    let names = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(gts_type::Column::Id.is_in(type_name_ids)))
        .all(&conn)
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .map(|t| (t.id, t.gts_type_id))
        .collect::<BTreeMap<_, _>>();

    let hits = fuse(&lexical, &vector, &names, request.limit);
    let revision = super::reads::read_revision(ctx, &conn).await?;
    // The byte budget is applied by the domain service, which is where the
    // configuration and the same rule for every store live.
    Ok(SearchResponse {
        hits,
        revision,
        truncated: None,
    })
}

/// What ranking needs of a candidate, and nothing else.
///
/// Fusion reads three fields: the key it groups by, the interned type id it
/// resolves to a name, and the name. Reading `node::Model` instead brought
/// back the payload, the search text and the stored 384-lane vector for every
/// candidate of every arm -- megabytes of transfer and deserialization per
/// request to produce a list of keys, and no chance of an index-only plan,
/// since projecting after `SELECT *` does not un-read the heap.
#[derive(Debug, sea_orm::FromQueryResult)]
struct Ranked {
    node_key: String,
    gts_node_type_id: i32,
    name: String,
}

/// The three columns a candidate is ranked by, named once.
///
/// Shared by both arms so the projection cannot drift between them, and
/// reachable from a test so the claim that the wide columns are not read is
/// checked against the rendered statement rather than asserted in a comment.
fn ranked_columns(query: sea_orm::Select<node::Entity>) -> sea_orm::Select<node::Entity> {
    query
        .select_only()
        .column(node::Column::NodeKey)
        .column(node::Column::GtsNodeTypeId)
        .column(node::Column::Name)
}

async fn lexical_arm(
    ctx: &StoreCtx<'_>,
    runner: &impl DBRunner,
    request: &SearchRequest,
    type_ids: Option<&[i32]>,
) -> Result<Vec<Ranked>, GraphStoreError> {
    let Some(query) = request.query.as_ref() else {
        return Ok(Vec::new());
    };

    // `websearch_to_tsquery` gives producers the query syntax users already
    // know (quoted phrases, `-` for exclusion) without a parser of our own.
    // `search` is a generated tsvector column: SeaORM does not map it, and it
    // must not be mapped — writing to it is a server error. Both the predicate
    // and the ranking name it in raw SQL, on the very expression the GIN index
    // was built on.
    // The configuration name is a compile-time constant shared with the index
    // migration, and `websearch_to_tsquery` takes it as a `regconfig` rather
    // than text — a bound parameter there does not resolve. It is inlined, the
    // caller's text is bound, and both halves name the same expression the GIN
    // index was built on.
    let matches = Expr::cust_with_values(
        format!("\"node\".\"search\" @@ websearch_to_tsquery('{FTS_CONFIG}', $1)"),
        [sea_orm::Value::from(query.clone())],
    );
    let rank = Expr::cust_with_values(
        format!("ts_rank(\"node\".\"search\", websearch_to_tsquery('{FTS_CONFIG}', $1))"),
        [sea_orm::Value::from(query.clone())],
    );

    let mut select = node::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .filter(Condition::all().add(matches));
    if let Some(ids) = type_ids {
        select =
            select.filter(Condition::all().add(node::Column::GtsNodeTypeId.is_in(ids.to_vec())));
    }
    let limit = u64::from(request.arm_limit);
    select
        .project_all(runner, move |query| {
            ranked_columns(query)
                .order_by(rank, Order::Desc)
                .order_by(node::Column::Id, Order::Asc)
                .limit(limit)
                .into_model::<Ranked>()
        })
        .await
        .map_err(map_scope_err)
}

async fn vector_arm(
    ctx: &StoreCtx<'_>,
    runner: &impl DBRunner,
    request: &SearchRequest,
    type_ids: Option<&[i32]>,
    arm: &VectorArm,
) -> Result<Vec<Ranked>, GraphStoreError> {
    // The literal is the pgvector text form; the cast is what lets the HNSW
    // cosine index serve the ordering.
    let literal = format!(
        "[{}]",
        arm.query_vector
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    let distance = Expr::cust_with_values(
        "\"node\".\"embedding\" <=> $1::vector",
        [sea_orm::Value::from(literal)],
    );

    let mut select = node::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .filter(Condition::all().add(node::Column::Embedding.is_not_null()))
        // Only *current* vectors rank (`fr-embedding-pipeline`). A vector of
        // another epoch was made by another model, and a vector whose input
        // changed while embedding was skipped carries a NULL epoch: neither
        // is comparable with this query, and both would rank content that is
        // no longer stored.
        .filter(Condition::all().add(node::Column::EmbeddingEpoch.eq(arm.epoch)));
    if let Some(ids) = type_ids {
        select =
            select.filter(Condition::all().add(node::Column::GtsNodeTypeId.is_in(ids.to_vec())));
    }
    let limit = u64::from(request.arm_limit);
    select
        .project_all(runner, move |query| {
            ranked_columns(query)
                .order_by(distance, Order::Asc)
                .order_by(node::Column::Id, Order::Asc)
                .limit(limit)
                .into_model::<Ranked>()
        })
        .await
        .map_err(map_scope_err)
}

/// Reciprocal Rank Fusion over the two arms' ranks.
fn fuse(
    lexical: &[Ranked],
    vector: &[Ranked],
    names: &BTreeMap<i32, String>,
    limit: u32,
) -> Vec<SearchHit> {
    struct Accumulated {
        model_index: (bool, usize),
        score: f64,
        arms: Vec<ArmHit>,
    }

    let mut by_key: BTreeMap<String, Accumulated> = BTreeMap::new();

    for (arm, rows, from_lexical) in [
        (SearchArm::Lexical, lexical, true),
        (SearchArm::Vector, vector, false),
    ] {
        for (position, model) in rows.iter().enumerate() {
            let rank = u32::try_from(position)
                .unwrap_or(u32::MAX)
                .saturating_add(1);
            let contribution = 1.0 / (RRF_K + f64::from(rank));
            let entry = by_key
                .entry(model.node_key.clone())
                .or_insert_with(|| Accumulated {
                    model_index: (from_lexical, position),
                    score: 0.0,
                    arms: Vec::new(),
                });
            entry.score += contribution;
            entry.arms.push(ArmHit {
                arm,
                rank,
                score: contribution,
            });
        }
    }

    let mut hits: Vec<SearchHit> = by_key
        .into_iter()
        .map(|(node_key, accumulated)| {
            let (from_lexical, index) = accumulated.model_index;
            let model = if from_lexical {
                &lexical[index]
            } else {
                &vector[index]
            };
            SearchHit {
                node_key,
                type_id: names
                    .get(&model.gts_node_type_id)
                    .cloned()
                    .unwrap_or_default(),
                name: (!model.name.is_empty()).then(|| model.name.clone()),
                score: accumulated.score,
                arms: accumulated.arms,
                snippet: None,
            }
        })
        .collect();

    // Deterministic order: score first, then the key, so equal scores do not
    // reorder between calls.
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node_key.cmp(&b.node_key))
    });
    hits.truncate(limit as usize);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ranking statement does not read the wide columns.
    ///
    /// Asserted on the rendered SQL rather than trusted, because the failure
    /// this guards against is silent: `.all()` returning `node::Model` looks
    /// identical at the call site and costs a payload, a search text and a
    /// 384-lane vector per candidate. Projecting after the fact does not
    /// un-read a heap page, so the only place this can be got right is the
    /// statement.
    #[test]
    fn the_ranking_statement_reads_three_columns_and_no_more() {
        use sea_orm::QueryTrait;

        let sql = ranked_columns(node::Entity::find())
            .build(sea_orm::DatabaseBackend::Postgres)
            .to_string();
        for wide in ["payload", "search_text", "embedding"] {
            assert!(!sql.contains(wide), "ranking must not read `{wide}`: {sql}");
        }
        for needed in ["node_key", "gts_node_type_id", "name"] {
            assert!(sql.contains(needed), "ranking needs `{needed}`: {sql}");
        }
    }

    /// A candidate as the arms now return it. That this is three fields long
    /// is the point of the projection: everything the old twenty-two-field
    /// literal carried was read from the database and thrown away.
    fn model(key: &str) -> Ranked {
        Ranked {
            node_key: key.to_owned(),
            gts_node_type_id: 1,
            name: String::new(),
        }
    }

    #[test]
    fn a_hit_found_by_both_arms_outranks_one_found_by_either() {
        let lexical = vec![model("both"), model("lexical-only")];
        let vector = vec![model("vector-only"), model("both")];
        let hits = fuse(&lexical, &vector, &BTreeMap::new(), 10);
        assert_eq!(hits[0].node_key, "both");
        assert_eq!(hits[0].arms.len(), 2, "both arms are reported per hit");
    }

    #[test]
    fn each_arm_reports_its_own_rank() {
        let lexical = vec![model("a"), model("b")];
        let hits = fuse(&lexical, &[], &BTreeMap::new(), 10);
        let b = hits.iter().find(|h| h.node_key == "b").expect("b is a hit");
        assert_eq!(b.arms[0].rank, 2);
        assert_eq!(b.arms[0].arm, SearchArm::Lexical);
    }
}
