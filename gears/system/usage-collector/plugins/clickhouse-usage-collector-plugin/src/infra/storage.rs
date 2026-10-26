//! `ClickHouse`-backed storage implementations.
//!
//! - `pool`: connection-pool bootstrap and schema reconciliation.
//! - `ddl`: the `CREATE TABLE` builder the migrator renders tables with.
//! - `migrator`: versioned schema migrations; one step list for every topology.
//! - `entity`, `mapper`, `error`, `query`, `record_store`: record persistence.
//! - `catalog_store`: usage-type catalog CRUD.
//! - `topology`: single-node vs sharded-cluster table layout.

pub mod catalog_store;
pub mod ddl;
pub mod entity;
pub mod error;
pub mod mapper;
pub mod migrator;
pub mod pool;
pub mod query;
pub mod record_store;
pub mod topology;

/// The shared `ClickHouse` test server; also compiled into `tests/common`.
#[cfg(all(test, feature = "clickhouse"))]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod test_ch_server;
