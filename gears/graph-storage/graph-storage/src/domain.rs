//! Domain layer. No entity, statement or connection is reachable from here:
//! the only way to data is through the ports in
//! `graph_storage_sdk::plugin_api` (`DOMAIN --> PORT`).

pub mod admission;
pub mod authz;
pub mod embedding;
pub mod error;
pub mod evolution;
pub mod identity;
pub mod local_client;
pub mod ontology;
pub mod projection;
pub mod service;
pub mod traversal;
