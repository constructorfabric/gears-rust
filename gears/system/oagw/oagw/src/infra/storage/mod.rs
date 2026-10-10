//! Upstream and route storage.
//!
//! The database-backed repositories (`*_db_repo`, ADR-0018) are selected only
//! when `gears.oagw.database` is configured; otherwise the gear keeps the
//! in-memory repositories (`*_in_memory_repo`).

pub(crate) mod entity;
mod error;
mod json;
mod load;
mod mapper;
pub(crate) mod migrations;
pub(crate) mod route_db_repo;
/// In-memory route repository.
pub(crate) mod route_in_memory_repo;
pub(crate) mod upstream_db_repo;
/// In-memory upstream repository.
pub(crate) mod upstream_in_memory_repo;

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod query_count_tests;
#[cfg(test)]
pub(crate) mod testing;

pub(crate) use migrations::Migrator;
pub(crate) use route_db_repo::DbRouteRepo;
pub(crate) use route_in_memory_repo::InMemoryRouteRepo;
pub(crate) use upstream_db_repo::DbUpstreamRepo;
pub(crate) use upstream_in_memory_repo::InMemoryUpstreamRepo;
