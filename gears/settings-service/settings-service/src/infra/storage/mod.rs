// Created: 2026-08-12 by Virtuozzo International GmbH
//! Persistence.
//!
//! Holds the migration harness today. Entities and repositories arrive with the
//! features that own their tables; each will be generic over
//! [`DBRunner`](toolkit_db::secure::DBRunner) so the same repository code runs
//! against a plain connection and inside a transaction, and will reach the
//! database through the `SecureConn` the gear acquires at init rather than a raw
//! pool.

pub mod access_repo;
pub mod audit_store;
pub mod category_repo;
pub mod clock;
pub mod declaration_odata_mapper;
pub mod declaration_repo;
// The tables' entities stay inside the crate: each store is the one way its
// table is written, and the audit store the one way `audit_records` is — an
// entity reachable from outside would be a second, unguarded path.
pub(crate) mod entity;
pub mod migrations;
pub mod odata_mapper;
pub mod pending_secret_repo;
pub mod search_repo;
pub mod value_repo;
