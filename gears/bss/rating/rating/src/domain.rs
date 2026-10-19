//! Domain layer of the Rating foundation: the inbox, the exception register, the operation
//! idempotency store, the operator audit, advisory leadership and the ports they depend on.

pub mod audit;
pub mod digest;
pub mod error;
pub mod exception;
pub mod inbox;
pub mod leadership;
pub mod operation;
pub mod ports;
