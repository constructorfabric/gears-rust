//! Payment post sidecars (DB-touching, so under `infra`, never `domain` —
//! dylint DE0301). The settle / allocate orchestrators thread these as
//! `Arc<dyn PostSidecar>` into [`crate::infra::posting::service::PostingService`]:
//! each runs inside the SAME serializable posting transaction, AFTER balance
//! projection and BEFORE the dedup row finalizes, so the payment counter writes
//! (`payment_settlement`, `payment_allocation`, `payment_allocation_refund`)
//! commit atomically with the journal entry or roll back with it.
//!
//! [`credit`] is the reusable-credit (wallet) orchestrator — grant parks pool
//! cash into the wallet, apply spends the wallet against open receivables. It is
//! sidecar-less: the wallet balance is a projector grain (the reusable-credit
//! sub-balance cache), not a payment counter table, and idempotency is the
//! engine's `(tenant, CREDIT_APPLY, credit_application_id)` dedup — so there is
//! no in-txn counter write to thread.

pub mod allocate;
pub mod chargeback;
pub mod credit;
/// [`queue_apply`] is the deferred-apply driver (Group D): it drains queued
/// allocations (allocate-before-settlement, §4.7), re-deriving each split against
/// then-current state and posting it through the engine's queued-apply path while
/// flipping the queue row `→APPLIED`.
pub mod queue_apply;
pub mod settle;
pub mod settlement_return;
pub mod sidecar;
