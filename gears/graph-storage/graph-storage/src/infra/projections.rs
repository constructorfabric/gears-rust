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
        edge, edge_hop_columns, endpoint_pair_columns, gts_type, node, node_ident_columns,
        type_id_columns, type_name_columns, type_traits_columns,
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
}
