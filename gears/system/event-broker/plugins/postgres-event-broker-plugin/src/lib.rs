//! # `PostgreSQL` event-broker backend
//!
//! A storage backend for the event-broker gear over a `PostgreSQL` database the
//! plugin owns (`docs/DESIGN.md`). It implements
//! `event_broker_sdk::backend::Backend` and ships a
//! `event_broker_sdk::backend::Provider` for the backend type
//! [`BACKEND_TYPE`]:
//!
//! - each `(topic, partition)` is a partitioned table, each of its partitions
//!   a *segment* covering a contiguous sequence range;
//! - an append and its fence row commit in one transaction (`Dedup::Fenced`);
//! - heads travel over `LISTEN/NOTIFY` (`Watch::Native`);
//! - the plugin's own workers keep segments ahead of the head and remove
//!   whole expired segments, ageing events by when they were stored.
//!
//! ## Why `sqlx` directly, not `libs/toolkit-db`
//!
//! Dynamic per-topic tables, transactional DDL, `LISTEN/NOTIFY` on a
//! session-pinned connection, and pool connect hooks have no `SeaORM`
//! equivalent. This is a deliberate exception to the platform's DB-access
//! rule, the same one the cluster gear's `PostgreSQL` plugin makes - hence the
//! crate-level `allow` below. `unknown_lints` is needed alongside:
//! `de0706_no_direct_sqlx` only exists under the `cargo gears lint --dylint`
//! driver.

#![forbid(unsafe_code)]
#![allow(unknown_lints)]
#![allow(de0706_no_direct_sqlx)]

mod append;
mod backend;
mod body;
mod error;
mod listener;
mod maintenance;
mod names;
mod notify;
mod provider;
mod provision;
mod read;
mod segment;
mod settings;
mod shared;
mod startup;

#[cfg(test)]
mod body_tests;
#[cfg(test)]
mod error_tests;
#[cfg(test)]
mod names_tests;
#[cfg(test)]
mod segment_tests;
#[cfg(test)]
mod settings_tests;

pub use backend::PostgresBackend;
pub use provider::{BACKEND_TYPE, PostgresBackendProvider};
