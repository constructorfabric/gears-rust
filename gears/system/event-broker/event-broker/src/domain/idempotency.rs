//! Idempotency key computation and checking (`DESIGN.md:597`'s
//! `domain/idempotency.rs`; realized against `evbk_producer_state` per
//! `ADR/0004-idempotent-producer-protocol.md`).
//!
//! The module/trait names stay general ("idempotency", not "producer
//! idempotency") because `DESIGN.md:597` already fixes this file's path and
//! `IdempotencyGuard`'s name; the outcome type isn't pinned by DESIGN.md, so
//! it's named precisely for what it actually checks - the producer chain
//! protocol, not idempotency in general (there's no other idempotency
//! concern in this domain yet, e.g. REST-level idempotency keys).

use async_trait::async_trait;
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Outcome of a producer chain-sequencing check for one incoming event
/// (`meta.producer_id`, `meta.previous`, `meta.sequence`) against
/// `ADR-0004`'s idempotent producer protocol. `SequenceViolation` carries
/// the broker's current `last_sequence` so the caller can build the
/// `docs/openapi.yaml` `412` (mapped to `400` - `gears-rust#4464`) response
/// body without a second lookup.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProducerIdempotencyOutcome {
    Accept,
    DuplicateIgnore,
    SequenceViolation { last_sequence: i64 },
}

/// The producer-chain identity/sequence a publish call checks against
/// `evbk_producer_state` - `None` on `IdempotencyGuard::check_and_enqueue`
/// when the publishing event carries no `meta` (stateless mode), in which
/// case there is no chain to check and the call always accepts.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProducerChainCheck {
    pub producer_id: Uuid,
    pub topic: GtsInstanceId,
    pub partition: i32,
    pub previous: i64,
    pub sequence: i64,
}

#[async_trait]
pub trait IdempotencyGuard: Send + Sync {
    /// Checks `chain` (when present) against stored
    /// `evbk_producer_state.last_sequence`, and - on `Accept` - inserts
    /// `payload` as one `toolkit-db` outbox row (design.md D5), all within
    /// **one DB transaction**: lock/check `producer_state` -> insert the
    /// outbox row -> update `producer_state` -> commit. This closes the gap
    /// `gears-rust#4346` used to track (check-and-record and the durable
    /// append had no shared transaction boundary) - the boundary is now
    /// this method's own transaction, implemented by `infra::storage::
    /// Storage`. The backend `persist()` call itself stays deliberately
    /// **outside** this transaction (design.md D5's component-boundary
    /// rule: "we always work with backend out-of-tx") - it happens later,
    /// out-of-process from this call, when the background outbox processor
    /// (`infra::outbox::IngestOutboxHandler`) leases and drains the row.
    ///
    /// When `chain` is `None` (the publishing event carries no producer
    /// `meta`), no `producer_state` row is touched and the outcome is
    /// always `Accept` - `payload` is still enqueued.
    async fn check_and_enqueue(
        &self,
        chain: Option<ProducerChainCheck>,
        payload: Vec<u8>,
        payload_type: &str,
    ) -> Result<ProducerIdempotencyOutcome, DomainError>;
}
