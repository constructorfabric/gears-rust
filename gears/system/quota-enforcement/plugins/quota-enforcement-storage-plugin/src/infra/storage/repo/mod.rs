//! Repositories over the plugin's tables. Every function takes a `DBRunner`,
//! so it works inside and outside a transaction, and an `AccessScope` that
//! `SecureORM` applies to every row.

pub mod allocation_counter_repo;
pub mod config_repo;
pub mod consumption_counter_repo;
pub mod idempotency_repo;
pub mod operation_log_repo;
pub mod quota_repo;
pub mod schema_repo;

pub mod policy_repo;
