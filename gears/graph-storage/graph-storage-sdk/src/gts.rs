//! GTS identifiers owned by the graph-storage gear.
//!
//! The base ontology (three abstract bases, six family types) is registered
//! by the gear at boot from the schemas in `docs/schemas/`; these constants
//! name them and the resource types authorization decisions use.

use toolkit_gts::gts_id;

// --- ontology bases (abstract; uninstantiable) ------------------------------

/// Node base: every registrable node type derives from this.
pub const NODE_BASE_TYPE: &str = gts_id!("cf.core.graph.node.v1~");
/// Edge base.
pub const EDGE_BASE_TYPE: &str = gts_id!("cf.core.graph.edge.v1~");
/// Attribute base.
pub const ATTRIBUTE_BASE_TYPE: &str = gts_id!("cf.core.graph.attribute.v1~");

// --- family types (abstract; producers derive from these) -------------------

pub const OWNED_NODE_TYPE: &str = gts_id!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~");
pub const REFERENCE_NODE_TYPE: &str =
    gts_id!("cf.core.graph.node.v1~cf.core.graph.reference_node.v1~");
pub const PHANTOM_NODE_TYPE: &str = gts_id!("cf.core.graph.node.v1~cf.core.graph.phantom_node.v1~");
pub const STATIC_EDGE_TYPE: &str = gts_id!("cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~");
pub const ANALYSIS_EDGE_TYPE: &str =
    gts_id!("cf.core.graph.edge.v1~cf.core.graph.analysis_edge.v1~");
pub const PROVENANCE_ATTRIBUTE_TYPE: &str =
    gts_id!("cf.core.graph.attribute.v1~cf.core.graph.provenance.v1~");

// --- authorization resource types --------------------------------------------

/// Resource type of a graph node, used for authorization decisions.
pub const NODE_RESOURCE: &str = gts_id!("cf.core.graph.node.v1~");
/// Resource type of a graph edge.
pub const EDGE_RESOURCE: &str = gts_id!("cf.core.graph.edge.v1~");
/// Resource type of a registered ontology type.
pub const TYPE_RESOURCE: &str = gts_id!("cf.core.graph.type.v1~");
