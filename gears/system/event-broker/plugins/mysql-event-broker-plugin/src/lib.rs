//! # `MySQL` / `MariaDB` event-broker backend
//!
//! A storage backend for the event-broker gear over a `MySQL` 8.0+ or
//! `MariaDB` 10.6+ database the plugin owns. It implements
//! `event_broker_sdk::backend::Backend` and ships a
//! `event_broker_sdk::backend::Provider` for the backend type
//! [`BACKEND_TYPE`]; the engine is detected at start (`docs/DESIGN.md`):
//!
//! - each `(topic, partition)` is a table `PARTITION BY RANGE` on its
//!   sequence, each range partition a *segment*;
//! - an append and its fence row commit in one transaction (`Dedup::Fenced`);
//! - heads of watched partitions are polled in one batched query
//!   (`Watch::Native` over the instance's own heads);
//! - the plugin's own maintenance adds segments ahead of the head and drops
//!   whole expired ones, ageing events by when they were stored; every DDL
//!   is bracketed by catalog rows a later run settles.
//!
//! ## Why `sqlx` directly, not `libs/toolkit-db`
//!
//! Dynamic per-topic partitioned tables, partition DDL under named locks and
//! per-session isolation settings have no `SeaORM` equivalent. This is the same
//! deliberate exception to the platform's DB-access rule the `PostgreSQL`
//! plugin makes - hence the crate-level `allow` below. `unknown_lints` is needed
//! alongside: `de0706_no_direct_sqlx` only exists under the
//! `cargo gears lint --dylint` driver.

#![forbid(unsafe_code)]
#![allow(unknown_lints)]
#![allow(de0706_no_direct_sqlx)]

mod append;
mod backend;
mod body;
mod dialect;
mod error;
mod heads;
mod maintenance;
mod names;
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
mod dialect_tests;
#[cfg(test)]
mod error_tests;
#[cfg(test)]
mod names_tests;
#[cfg(test)]
mod segment_tests;
#[cfg(test)]
mod settings_tests;

pub use backend::MysqlBackend;
pub use provider::{BACKEND_TYPE, MysqlBackendProvider};
