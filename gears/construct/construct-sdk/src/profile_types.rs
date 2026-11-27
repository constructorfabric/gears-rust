//! The two graph types that tie a subject's facts together: the profile's root
//! node and the edge from the root to each fact. Both are registered with
//! graph storage beside the node and link kinds of each person. The schemas are embedded from the
//! crate's `schemas/`, so the registration and the published files cannot
//! drift apart.

use crate::gts::{HAS_FACT_TYPE, PROFILE_ROOT_TYPE};

/// One graph type Construct registers with graph storage: its type id and its
/// JSON schema. A person type, the profile root or the has-fact edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphType {
    pub type_id: &'static str,
    pub schema: &'static str,
}

/// The profile's root node type. It derives from graph storage's owned node:
/// the root is Construct's own node, not a copy of the platform user.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-root:p1
pub const PROFILE_ROOT: GraphType = GraphType {
    type_id: PROFILE_ROOT_TYPE,
    schema: include_str!(
        "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.profile.root.v1~.schema.json"
    ),
};

/// The edge type from the root to a fact, a static edge with no payload.
pub const HAS_FACT: GraphType = GraphType {
    type_id: HAS_FACT_TYPE,
    schema: include_str!(
        "../schemas/gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~cf.construct.profile.has_fact.v1~.schema.json"
    ),
};

/// The graph types every tenant's profiles share, in registration order: the root and the edge. A person's node
/// and link kinds come after them: [`crate::linked_schema::Schema::graph_types`].
#[must_use]
pub fn graph_types() -> Vec<GraphType> {
    vec![PROFILE_ROOT, HAS_FACT]
}

#[cfg(test)]
#[path = "profile_types_tests.rs"]
mod profile_types_tests;
