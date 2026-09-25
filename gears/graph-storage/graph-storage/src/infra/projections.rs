//! Narrow column lists for the lookups that want one or two fields.
//!
//! A `gts_type` row carries a whole JSON schema and its resolved traits; a
//! `node` row carries a payload, a search text and a 384-lane embedding. Most
//! of the reads in this gear want an identifier, a key or a pair of endpoint
//! ids, and `Entity::find(..).all(..)` brings the rest back to be dropped on
//! the next line — on hot paths, at a cost that scales with how wide the
//! tenant's data happens to be rather than with the work being done.
//!
//! They live together for the reason the canonical JSON rendering does: the
//! first two of these were written separately, and the third was about to be
//! a third copy. One home also means one place for the rendered-SQL tests
//! that keep them narrow, since widening a projection compiles, runs and
//! answers correctly — the regression is invisible without a test that reads
//! the statement.

use sea_orm::{QuerySelect, Select};

use crate::infra::storage::entity::{edge, gts_type, node};

/// A node's identity: the key a producer addresses it by, and the id
/// everything internal joins on.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct NodeIdent {
    pub id: i64,
    pub node_key: String,
}

pub fn node_ident_columns(query: Select<node::Entity>) -> Select<node::Entity> {
    query
        .select_only()
        .column(node::Column::Id)
        .column(node::Column::NodeKey)
}

/// A node's identity and its type: what an adjacency entry says about the
/// node at the far end of an edge.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct NodeTyped {
    pub id: i64,
    pub node_key: String,
    pub gts_node_type_id: i32,
}

pub fn node_typed_columns(query: Select<node::Entity>) -> Select<node::Entity> {
    query
        .select_only()
        .column(node::Column::Id)
        .column(node::Column::NodeKey)
        .column(node::Column::GtsNodeTypeId)
}

/// A registered type's interned id, when the name it was looked up by is
/// already in hand.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct TypeId {
    pub id: i32,
}

pub fn type_id_columns(query: Select<gts_type::Entity>) -> Select<gts_type::Entity> {
    query.select_only().column(gts_type::Column::Id)
}

/// Both halves of the interned-type map, for answers that name types.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct TypeName {
    pub id: i32,
    pub gts_type_id: String,
}

pub fn type_name_columns(query: Select<gts_type::Entity>) -> Select<gts_type::Entity> {
    query
        .select_only()
        .column(gts_type::Column::Id)
        .column(gts_type::Column::GtsTypeId)
}

/// A type's interned id and its resolved traits, without the schema.
///
/// The projection read needs the traits to decide which payload paths a
/// `$filter` may name, and the id to narrow the statement; the schema the
/// type was registered with is the widest column on the row and plays no
/// part in either.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct TypeTraits {
    pub id: i32,
    pub effective_traits: serde_json::Value,
}

pub fn type_traits_columns(query: Select<gts_type::Entity>) -> Select<gts_type::Entity> {
    query
        .select_only()
        .column(gts_type::Column::Id)
        .column(gts_type::Column::EffectiveTraits)
}

/// An edge's own id and the two nodes it holds: what a delete needs to
/// tombstone it and to check its endpoints are visible.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct EdgeEnds {
    pub id: i64,
    pub src_node_id: i64,
    pub dst_node_id: i64,
}

pub fn edge_ends_columns(query: Select<edge::Entity>) -> Select<edge::Entity> {
    query
        .select_only()
        .column(edge::Column::Id)
        .column(edge::Column::SrcNodeId)
        .column(edge::Column::DstNodeId)
}

/// Whether a node is tombstoned: what a conflict re-read asks, to say which
/// of its causes the caller hit.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct NodeState {
    pub deleted_at: Option<time::OffsetDateTime>,
}

pub fn node_state_columns(query: Select<node::Entity>) -> Select<node::Entity> {
    query.select_only().column(node::Column::DeletedAt)
}

/// A registered type without its schema.
///
/// The schema is the one wide column on `gts_type`, and every read that is
/// not about the schema itself -- matching type patterns, resolving a
/// family, deciding which types a scope manages, validating an endpoint --
/// wants the identifiers and the resolved traits and nothing else. Only the
/// reads that answer with a schema, or walk one, read it.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct TypeMeta {
    pub id: i32,
    pub gts_type_uuid: uuid::Uuid,
    pub gts_type_id: String,
    pub kind: String,
    pub effective_traits: serde_json::Value,
}

pub fn type_meta_columns(query: Select<gts_type::Entity>) -> Select<gts_type::Entity> {
    query
        .select_only()
        .column(gts_type::Column::Id)
        .column(gts_type::Column::GtsTypeUuid)
        .column(gts_type::Column::GtsTypeId)
        .column(gts_type::Column::Kind)
        .column(gts_type::Column::EffectiveTraits)
}

/// Which two nodes an edge holds — all a reference check asks.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct EndpointPair {
    pub src_node_id: i64,
    pub dst_node_id: i64,
}

pub fn endpoint_pair_columns(query: Select<edge::Entity>) -> Select<edge::Entity> {
    query
        .select_only()
        .column(edge::Column::SrcNodeId)
        .column(edge::Column::DstNodeId)
}

/// What one hop needs of an edge: which edge, of which type, between which
/// two nodes. Read per hop for every live incident edge, so its payload and
/// audit columns were the widest thing on the traversal hot path.
#[derive(Debug, sea_orm::FromQueryResult)]
pub struct EdgeHop {
    pub edge_key: String,
    pub gts_edge_type_id: i32,
    pub src_node_id: i64,
    pub dst_node_id: i64,
}

pub fn edge_hop_columns(query: Select<edge::Entity>) -> Select<edge::Entity> {
    query
        .select_only()
        .column(edge::Column::EdgeKey)
        .column(edge::Column::GtsEdgeTypeId)
        .column(edge::Column::SrcNodeId)
        .column(edge::Column::DstNodeId)
}

#[cfg(test)]
mod tests {
    use sea_orm::{DatabaseBackend, EntityTrait, QueryTrait};

    use super::{
        edge, edge_ends_columns, edge_hop_columns, endpoint_pair_columns, gts_type, node,
        node_ident_columns, node_state_columns, node_typed_columns, type_id_columns,
        type_meta_columns, type_name_columns, type_traits_columns,
    };

    /// The columns no projection here may read, by entity. Each of them is
    /// the reason the projection exists.
    const WIDE_NODE: [&str; 3] = ["payload", "search_text", "embedding"];
    const WIDE_TYPE: [&str; 2] = ["schema", "traits"];
    const WIDE_EDGE: [&str; 1] = ["payload"];

    fn rendered<E: EntityTrait>(query: &sea_orm::Select<E>) -> String {
        query.build(DatabaseBackend::Postgres).to_string()
    }

    fn holds(sql: &str, forbidden: &[&str], needed: &[&str]) {
        for wide in forbidden {
            assert!(!sql.contains(wide), "must not read `{wide}`: {sql}");
        }
        for want in needed {
            assert!(sql.contains(want), "needs `{want}`: {sql}");
        }
    }

    #[test]
    fn a_node_identity_reads_two_columns() {
        holds(
            &rendered(&node_ident_columns(node::Entity::find())),
            &WIDE_NODE,
            &["id", "node_key"],
        );
    }

    #[test]
    fn an_interned_type_id_reads_one_column() {
        let sql = rendered(&type_id_columns(gts_type::Entity::find()));
        holds(&sql, &WIDE_TYPE, &["id"]);
        assert!(
            !sql.contains("gts_type_id"),
            "the id alone is what this one is for: {sql}"
        );
    }

    #[test]
    fn a_type_name_map_reads_two_columns() {
        holds(
            &rendered(&type_name_columns(gts_type::Entity::find())),
            &WIDE_TYPE,
            &["id", "gts_type_id"],
        );
    }

    #[test]
    fn an_endpoint_pair_reads_two_columns() {
        holds(
            &rendered(&endpoint_pair_columns(edge::Entity::find())),
            &WIDE_EDGE,
            &["src_node_id", "dst_node_id"],
        );
    }

    #[test]
    fn an_edge_hop_reads_four_columns() {
        holds(
            &rendered(&edge_hop_columns(edge::Entity::find())),
            &WIDE_EDGE,
            &["edge_key", "gts_edge_type_id", "src_node_id", "dst_node_id"],
        );
    }

    #[test]
    fn a_types_traits_are_read_without_its_schema() {
        let sql = rendered(&type_traits_columns(gts_type::Entity::find()));
        holds(&sql, &["schema"], &["id", "effective_traits"]);
    }

    #[test]
    fn a_neighbour_reads_its_identity_and_type_and_no_more() {
        holds(
            &rendered(&node_typed_columns(node::Entity::find())),
            &WIDE_NODE,
            &["id", "node_key", "gts_node_type_id"],
        );
    }

    #[test]
    fn an_edges_ends_read_three_columns() {
        holds(
            &rendered(&edge_ends_columns(edge::Entity::find())),
            &WIDE_EDGE,
            &["id", "src_node_id", "dst_node_id"],
        );
    }

    #[test]
    fn a_nodes_state_reads_one_column() {
        holds(
            &rendered(&node_state_columns(node::Entity::find())),
            &WIDE_NODE,
            &["deleted_at"],
        );
    }

    #[test]
    fn a_types_meta_reads_everything_but_its_schema() {
        holds(
            &rendered(&type_meta_columns(gts_type::Entity::find())),
            &["type_schema"],
            &[
                "id",
                "gts_type_uuid",
                "gts_type_id",
                "kind",
                "effective_traits",
            ],
        );
    }
}
