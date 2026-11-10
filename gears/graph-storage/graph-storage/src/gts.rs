//! The permissions the gear's PEP checks, published to the platform
//! types-registry (DESIGN § Base Ontology Publication). The base ontology and
//! the schema of the type-catalogue resource are published by the SDK
//! (`graph_storage_sdk::gts`), as the other gears publish their type schemas;
//! the permission instances stay with the gear that enforces them.
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
//! everything this gear and its SDK submit and reads every entry back.

use graph_storage_sdk::gts::{NODE_RESOURCE, TYPE_RESOURCE};
use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::authz::actions;

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
