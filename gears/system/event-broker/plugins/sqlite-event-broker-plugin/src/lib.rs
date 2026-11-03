//! # `SQLite` event-broker backend
//!
//! A storage backend for the event-broker gear over one `SQLite` database file
//! the plugin owns (`docs/DESIGN.md`). It implements
//! `event_broker_sdk::backend::Backend` and ships a
//! `event_broker_sdk::backend::Provider` for the backend type
//! [`BACKEND_TYPE`]:
//!
//! - one table holds every event, keyed by topic, partition and sequence; no
//!   partitioning of the table - the plugin is for small volumes;
//! - an append and its fence row commit in one write transaction
//!   (`Dedup::Fenced`), synced to disk before it returns;
//! - heads travel through a hub shared by every instance over the file in
//!   this process (`Watch::Native`);
//! - a sweep removes expired events, oldest first, ageing them by when they
//!   were stored.
//!
//! One process owns the file; sharing it between processes is unsupported.
//!
//! ## Why `sqlx` directly, not `libs/toolkit-db`
//!
//! `BEGIN IMMEDIATE` write transactions, connect-time pragmas and dynamic
//! per-topic state have no `SeaORM` equivalent. This is the same deliberate
//! exception to the platform's DB-access rule the `PostgreSQL` plugin makes -
//! hence the crate-level `allow` below. `unknown_lints` is needed alongside:
//! `de0706_no_direct_sqlx` only exists under the `cargo gears lint --dylint`
//! driver.

#![forbid(unsafe_code)]
#![allow(unknown_lints)]
#![allow(de0706_no_direct_sqlx)]

mod append;
mod backend;
mod body;
mod clock;
mod error;
mod hub;
mod provider;
mod provision;
mod read;
mod settings;
mod shared;
mod startup;
mod sweep;

#[cfg(test)]
mod append_tests;
#[cfg(test)]
mod body_tests;
#[cfg(test)]
mod clock_tests;
#[cfg(test)]
mod error_tests;
#[cfg(test)]
mod hub_tests;
#[cfg(test)]
mod settings_tests;
#[cfg(test)]
mod sweep_tests;

pub use backend::SqliteBackend;
pub use provider::{BACKEND_TYPE, SqliteBackendProvider};
