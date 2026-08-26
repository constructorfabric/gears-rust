//! `SeaORM` specifics of the built-in store: entities, the property-graph
//! declaration, migrations, and shared storage helpers. Nothing above
//! `infra/` sees any of it.

pub mod entity;
pub mod graph;
pub mod migrations;
pub mod odata_mapper;
