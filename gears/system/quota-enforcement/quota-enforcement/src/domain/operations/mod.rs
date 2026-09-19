//! The consumption hot path: debit, credit, rollback, and preview, plus the
//! replay cache and the retention sweeper that support them.
//!
//! Every guarded operation in a consuming service becomes a debit here, and
//! every retry of one must land exactly once. The orchestration in [`service`]
//! is deliberately thin: admission and catalogue mapping happen above it, the
//! policy is selected and evaluated inside the storage transaction, and this
//! layer only decides what to hash, what to look up, and what to record.

pub mod idempotency;
pub mod retention;
pub mod service;

pub use idempotency::{IdempotencyCache, ReplayRecord};
pub use retention::{RetentionSweeper, RetentionTiming, SweepReport};
pub use service::Operations;
