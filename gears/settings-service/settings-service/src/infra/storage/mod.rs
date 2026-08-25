// Created: 2026-08-12 by Constructor Tech
//! Persistence.
//!
//! Holds the migration harness today. Entities and repositories arrive with the
//! features that own their tables; each will be generic over
//! [`DBRunner`](toolkit_db::secure::DBRunner) so the same repository code runs
//! against a plain connection and inside a transaction, and will reach the
//! database through the `SecureConn` the gear acquires at init rather than a raw
//! pool.

pub mod category_repo;
pub mod entity;
pub mod migrations;
pub mod odata_mapper;
