//! GTS identifiers owned by the graph-storage gear.
//!
//! The base ontology (three abstract bases, six family types) lives in this
//! crate's `schemas/` directory: the gear registers it in each tenant's type
//! catalogue at boot, and this crate publishes it, with the schema of the type
//! catalogue's authorization resource, to the platform types-registry through
//! the link-time `toolkit-gts` inventory. Published here rather than from the
//! gear, so a process that links only this SDK publishes the contract its
//! producers derive from. These constants name the types and the resource
//! types authorization decisions use.

use toolkit_gts::{InventoryTypeSchema, gts_id, inventory};

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

// --- published schemas -------------------------------------------------------

/// The nine base-ontology schemas, embedded from this crate's `schemas/`, so
/// the gear's boot registration, the types-registry and the published schema
/// files cannot drift apart. Inside the crate because `cargo package` ships
/// nothing outside it.
pub const BASE_SCHEMAS: [(&str, &str); 9] = [
    (
        NODE_BASE_TYPE,
        include_str!("../schemas/gts.cf.core.graph.node.v1~.schema.json"),
    ),
    (
        EDGE_BASE_TYPE,
        include_str!("../schemas/gts.cf.core.graph.edge.v1~.schema.json"),
    ),
    (
        ATTRIBUTE_BASE_TYPE,
        include_str!("../schemas/gts.cf.core.graph.attribute.v1~.schema.json"),
    ),
    (
        OWNED_NODE_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~.schema.json"
        ),
    ),
    (
        REFERENCE_NODE_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.reference_node.v1~.schema.json"
        ),
    ),
    (
        PHANTOM_NODE_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.node.v1~cf.core.graph.phantom_node.v1~.schema.json"
        ),
    ),
    (
        STATIC_EDGE_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~.schema.json"
        ),
    ),
    (
        ANALYSIS_EDGE_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.edge.v1~cf.core.graph.analysis_edge.v1~.schema.json"
        ),
    ),
    (
        PROVENANCE_ATTRIBUTE_TYPE,
        include_str!(
            "../schemas/gts.cf.core.graph.attribute.v1~cf.core.graph.provenance.v1~.schema.json"
        ),
    ),
];

/// Schema of [`TYPE_RESOURCE`], the resource the type-catalogue permissions
/// name. Not part of the ontology, so not in [`BASE_SCHEMAS`] and never
/// registered as a graph type: it is published only so the policy-engine,
/// which refuses a policy naming a resource type the registry does not know,
/// accepts one naming the type catalogue without a wildcard.
pub const TYPE_RESOURCE_SCHEMA: &str =
    include_str!("../schemas/gts.cf.core.graph.type.v1~.schema.json");

/// Every schema this crate publishes: the ontology and the type resource.
pub fn published_schemas() -> impl Iterator<Item = (&'static str, &'static str)> {
    BASE_SCHEMAS
        .into_iter()
        .chain([(TYPE_RESOURCE, TYPE_RESOURCE_SCHEMA)])
}

macro_rules! publish_base_schema {
    ($index:literal) => {
        inventory::submit! {
            InventoryTypeSchema {
                type_id: BASE_SCHEMAS[$index].0,
                schema_fn: || BASE_SCHEMAS[$index].1.to_owned(),
            }
        }
    };
}

publish_base_schema!(0);
publish_base_schema!(1);
publish_base_schema!(2);
publish_base_schema!(3);
publish_base_schema!(4);
publish_base_schema!(5);
publish_base_schema!(6);
publish_base_schema!(7);
publish_base_schema!(8);

// Nine entries above for nine schemas; a tenth schema without a tenth entry
// fails to compile here rather than going unpublished.
const _: () = assert!(BASE_SCHEMAS.len() == 9);

inventory::submit! {
    InventoryTypeSchema {
        type_id: TYPE_RESOURCE,
        schema_fn: || TYPE_RESOURCE_SCHEMA.to_owned(),
    }
}
