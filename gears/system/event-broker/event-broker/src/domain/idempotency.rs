//! Idempotency key computation and checking (`DESIGN.md:597`'s
//! `domain/idempotency.rs`; realized against `evbk_producer_state` per
//! `ADR/0004-idempotent-producer-protocol.md`). Signatures only.

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Outcome of an idempotency check for one incoming event's producer
/// chain (`meta.producer_id`, `meta.previous`, `meta.sequence`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyOutcome {
    Accept,
    DuplicateIgnore,
    SequenceViolation,
}

#[async_trait]
pub trait IdempotencyGuard: Send + Sync {
    /// Checks the incoming `(producer_id, topic, partition)` chain against
    /// stored `evbk_producer_state.last_sequence` and records the new
    /// sequence on acceptance.
    ///
    /// **Known gap (tracked, not resolved by this signature):** this trait
    /// and `EventRepo::append` are independent operations with no shared
    /// transaction/reservation boundary. An implementation that calls both
    /// without atomicity can durably record `Accept` while the matching
    /// event append fails (lost event on retry returning
    /// `DuplicateIgnore`), or durably append before recording (duplicate
    /// event on retry). Whoever implements the ingest flow (#4346/#4347)
    /// must provide that boundary (e.g. a shared DB transaction) - it is
    /// not something a structure-only trait signature can enforce.
    async fn check_and_record(
        &self,
        producer_id: Uuid,
        topic: &str,
        partition: i32,
        previous: i64,
        sequence: i64,
    ) -> Result<IdempotencyOutcome, DomainError>;
}
