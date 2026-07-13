//! GTS resource type identifiers for the nodes registry.
//!
//! Single source of truth for the nodes-registry resource-type string
//! used as the `resource_type` field on the canonical envelope (and the
//! projected [`crate::NodesRegistryError::NotFound`] variant) when a node
//! lookup misses.
//!
//! The string follows the gear's GTS namespace convention,
//! `gts.cf.nodes_registry.registry.node.v1~`. The trailing `~` is the
//! GTS terminator and is part of the identifier.
//!
//! # Note on `#[resource_error]` macro arguments
//!
//! The `toolkit_canonical_errors::resource_error` proc-macro takes a
//! literal string at expansion time and cannot resolve constants — the
//! impl-crate site (`api/rest/error.rs`) therefore duplicates this
//! literal. The round-trip tests in [`crate::error`] assert the wire
//! `resource_type` equals this constant, so a divergence trips at test
//! time, not in production.

use toolkit_gts::gts_id;
/// Nodes-registry node resource. Surfaces as the `resource_type` on the
/// canonical `NotFound` raised when a node id does not resolve (404).
pub const NODE_RESOURCE_TYPE: &str = gts_id!("cf.nodes_registry.registry.node.v1~");
