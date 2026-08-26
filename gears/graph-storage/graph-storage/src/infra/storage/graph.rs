//! The property-graph declaration — the single source for both the
//! `CREATE PROPERTY GRAPH` DDL and every `MATCH` pattern (secure-orm
//! ADR-0002, Policy 3). The `PROPERTIES` lists are derived from each
//! element's key and scope columns, so an element that resolves no scope
//! column is a build error rather than a silent deny-all.
//!
//! Columns outside key ∪ scope (`deleted_at`, the interned type references)
//! are therefore invisible inside a pattern. The hop treats the pattern as a
//! candidate producer (gear ADR-0006): the pattern carries the scope and
//! proposes ids, and an ordinary scoped query re-authorizes them, applying
//! tombstone and type filters the pattern cannot express.

use toolkit_db::secure::ScopeError;
use toolkit_db::secure::pgq::{Endpoint, GraphDeclaration, PropertyGraph, VertexOf};

use super::entity::{edge, node};

/// The knowledge graph over `node` and `edge`.
pub struct KnowledgeGraph;

/// SQL/PGQ element labels.
pub const NODE_LABEL: &str = "node";
pub const EDGE_LABEL: &str = "edge";

impl PropertyGraph for KnowledgeGraph {
    const GRAPH_NAME: &'static str = "kb";

    fn declaration() -> Result<GraphDeclaration, ScopeError> {
        GraphDeclaration::new::<Self>()
            .vertex::<Self, node::Entity>(&["tenant_id", "id"])?
            .edge::<Self, edge::Entity>(
                &["tenant_id", "id"],
                Endpoint {
                    key: vec!["tenant_id".into(), "src_node_id".into()],
                    table: "node".into(),
                    references: vec!["tenant_id".into(), "id".into()],
                },
                Endpoint {
                    key: vec!["tenant_id".into(), "dst_node_id".into()],
                    table: "node".into(),
                    references: vec!["tenant_id".into(), "id".into()],
                },
            )
    }
}

impl VertexOf<KnowledgeGraph> for node::Entity {
    const LABEL: &'static str = NODE_LABEL;
}

impl toolkit_db::secure::pgq::EdgeOf<KnowledgeGraph> for edge::Entity {
    const LABEL: &'static str = EDGE_LABEL;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declaration_builds() {
        let declaration = match KnowledgeGraph::declaration() {
            Ok(d) => d,
            Err(e) => panic!("declaration must build: {e}"),
        };
        let ddl = match declaration.create_statement() {
            Ok(s) => s,
            Err(e) => panic!("DDL must render: {e}"),
        };
        assert!(ddl.contains("CREATE PROPERTY GRAPH"), "{ddl}");
        assert!(ddl.contains("\"kb\""), "{ddl}");
        // Composite element keys fence tenants structurally: an edge cannot
        // join a node of another tenant even before any predicate.
        assert!(ddl.contains("\"tenant_id\""), "{ddl}");
    }
}
