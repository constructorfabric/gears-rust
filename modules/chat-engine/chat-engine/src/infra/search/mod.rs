//! SeaORM-backed implementations of the [`SearchBackend`] trait
//! (`crate::domain::service::search_service::SearchBackend`).
//!
//! The domain layer owns the trait + an in-memory backend used by unit
//! tests. The two SeaORM impls below carry a `DatabaseConnection` and so
//! intentionally live in `infra/` per the DDD-light boundary: the
//! `#[domain_model]` enforcement rejects infrastructure types inside
//! `domain/`.
//
// @cpt-cf-chat-engine-infra-search-root:p11

pub mod backend;

pub use backend::{PgSearchBackend, SqliteSearchBackend};
