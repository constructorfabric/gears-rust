//! What the gear publishes to the platform types-registry: the base ontology
//! and the permissions its PEP checks (DESIGN § Base Ontology Publication).
//!
//! Both go through the process-wide `toolkit-gts` inventory, which
//! types-registry seeds at its own startup, so the gear gains no runtime
//! dependency on the registry and no deploy-order constraint. A producer can
//! then derive its graph types from `gts.cf.core.graph.*` in the registry,
//! and a grant can name these permissions.
//!
//! **The registry is not yet the source of truth for graph types.** Ingest
//! still validates against the gear's per-tenant `gts_type` catalogue, and a
//! producer's own types are registered with the gear only: types-registry
//! holds tenant-owned entities from its P1 (gears-rust #4628). ADR-0007 states
//! the interim and the stages to the target model.
//!
//! One bad inventory entry stops the registry's startup, and the platform
//! with it, so `tests/types_registry.rs` seeds the real registry with
//! everything this module submits and reads every entry back.

use graph_storage_sdk::gts::{NODE_RESOURCE, TYPE_RESOURCE};
use toolkit_gts::{AuthzPermissionV1, InventoryTypeSchema, gts_instance, inventory};

use crate::domain::authz::actions;
use crate::domain::ontology::BASE_SCHEMAS;

// ---- base ontology ---------------------------------------------------------
//
// The same embedded documents the gear validates against, byte for byte: a
// copy published here and another kept there would be two schemas under one
// identifier the first time one of them is edited.

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

// ---- permissions -----------------------------------------------------------
//
// One per resource and action the PEP checks (`domain::authz`), using the
// same constants it passes, so the catalogue cannot name an action the gear
// never enforces. Edges have no permission of their own: they are fenced by
// the scope of the nodes they join.
//
// Instance id layout: `gts.cf.toolkit.authz.permission.v1~cf.core.graph.<resource>_<action>.v1`.

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.graph.node_read.v1"),
        resource_type: NODE_RESOURCE.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read graph nodes, edges and search".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.graph.node_write.v1"),
        resource_type: NODE_RESOURCE.to_owned(),
        action: actions::WRITE.to_owned(),
        display_name: "Ingest graph nodes and edges".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.graph.node_delete.v1"),
        resource_type: NODE_RESOURCE.to_owned(),
        action: actions::DELETE.to_owned(),
        display_name: "Delete graph nodes and edges".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.graph.type_read.v1"),
        resource_type: TYPE_RESOURCE.to_owned(),
        action: actions::READ.to_owned(),
        display_name: "Read the graph type catalogue".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.core.graph.type_admin.v1"),
        resource_type: TYPE_RESOURCE.to_owned(),
        action: actions::ADMIN.to_owned(),
        display_name: "Register and change graph types".to_owned(),
    }
}
