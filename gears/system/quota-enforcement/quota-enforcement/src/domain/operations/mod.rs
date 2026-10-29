//! The consumption hot path: debit, credit, rollback, preview, and the three
//! lease operations, plus the replay cache and the two sweepers that support
//! them.
//!
//! Every guarded operation in a consuming service becomes a debit here, and
//! every retry of one must land exactly once. The orchestration in [`service`]
//! is deliberately thin: admission and catalogue mapping happen above it, the
//! policy is selected and evaluated inside the storage transaction, and this
//! layer only decides what to hash, what to look up, and what to record.

pub mod batch;
pub mod idempotency;
pub mod lease_sweeper;
pub mod leases;
pub mod retention;
pub mod service;

pub use batch::BatchLimits;
pub use idempotency::{IdempotencyCache, ReplayRecord};
pub use lease_sweeper::{LeaseSweepReport, LeaseSweepTiming, LeaseSweeper};
pub use leases::LeaseLimits;
pub use retention::{RetentionSweeper, RetentionTiming, SweepReport};
pub use service::Operations;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod harness_tests;
