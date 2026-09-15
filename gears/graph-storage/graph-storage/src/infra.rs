//! Infrastructure: the built-in `PostgreSQL` implementations of the plugin
//! contracts, the `SeaORM` entities behind them, and the migrations.
//!
//! The built-in store and engine are plugins like any external one — they are
//! packaged here for convenience, not privilege. No domain service reaches an
//! entity, a statement or a connection: the only way out of `domain/` is
//! through the ports in `graph_storage_sdk::plugin_api`.

pub mod embedding;
pub mod engine;
pub mod fake_store;
pub mod storage;
pub mod store;
