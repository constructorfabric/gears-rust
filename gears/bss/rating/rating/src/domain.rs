//! Domain layer of Rating: the foundation's inbox, exception register, operation idempotency
//! store, operator audit, advisory leadership and the ports they depend on, and usage capture and
//! normalization (feature 04).

pub mod audit;
pub mod backoff;
pub mod digest;
pub mod error;
pub mod exception;
pub mod inbox;
pub mod leadership;
pub mod operation;
pub mod ports;
pub mod usage;
