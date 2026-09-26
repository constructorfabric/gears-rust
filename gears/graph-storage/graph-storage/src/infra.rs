//! Infrastructure: the built-in `PostgreSQL` implementations of the plugin
//! contracts, the `SeaORM` entities behind them, and the migrations.
//!
//! The built-in store and engine are plugins like any external one — they are
//! packaged here for convenience, not privilege. No domain service reaches an
//! entity, a statement or a connection: the only way out of `domain/` is
//! through the ports in `graph_storage_sdk::plugin_api`.

pub mod embedding;
pub mod engine;
/// The in-memory double, behind `test-support` so it never reaches a release
/// artifact. It exists to make the conformance suite run against a second
/// implementation -- a change only the built-in store can satisfy fails on it
/// -- which is a test concern, not a shipped one.
#[cfg(any(test, feature = "test-support"))]
pub mod fake_store;
pub(crate) mod projections;
pub mod storage;
pub mod store;
