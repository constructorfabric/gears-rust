//! Domain layer: repo-facing value types and errors.

pub mod adjustment;
pub mod allocate;
pub mod approval;
pub mod audit_chain;
pub(crate) mod canonical;
pub mod chain;
pub mod error;
pub mod exception;
pub mod fx;
pub mod invoice;
pub mod model;
pub mod money;
pub mod money_math;
pub mod payment;
pub mod period;
pub mod ports;
pub mod posting;
pub mod provisioning;
pub mod recognition;
pub mod scale;
pub mod status;
