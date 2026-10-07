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
use crate::domain::ingest::ProducerMode;

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
    /// The chained-mode link to the prior sequence. Collapsed to `0` when the
    /// producer omitted it - a monotonic producer never reaches here carrying
    /// it (the ingest path rejects that shape first), and monotonic checking
    /// ignores this field entirely.
    pub previous: i64,
    pub sequence: i64,
    /// The producer's registered dedup mode, which decides how `previous`/
    /// `sequence` are checked against stored state (`ADR-0004`).
    pub mode: ProducerMode,
}

impl ProducerChainCheck {
    /// The chain-sequencing outcome for this publish against the stored head.
    ///
    /// `stored_last` is the producer's current `last_sequence`, or `None` when no
    /// `producer_state` row exists yet (the first publish for this
    /// `(producer, topic, partition)`, which always seeds and admits).
    ///
    /// This is the domain's decision, kept pure so it reads no state and performs
    /// no I/O: infra supplies the stored value and performs whatever write the
    /// outcome dictates. The sequence space **tolerates gaps** - what matters is
    /// log/chain consistency, not contiguity:
    ///
    /// - Chained advances when `previous` links to the current head and
    ///   `sequence` moves forward (`previous == last && sequence > last`); the new
    ///   `sequence` need not be `last + 1`, so a gap over skipped values is legal.
    /// - Monotonic advances on any strictly greater `sequence`, ignoring `previous`.
    /// - A non-advancing chained publish whose `sequence` equals the head is an
    ///   idempotent retry of the step that established it (`DuplicateIgnore`); any
    ///   other non-advancing publish - a `sequence` below the head, or above it
    ///   with a `previous` that does not link to the head - is a chain desync
    ///   (`SequenceViolation`) and the producer must re-read the cursor.
    /// - A non-advancing monotonic publish (`sequence <= last`) is a re-send,
    ///   deduplicated, never a violation.
    #[must_use]
    pub fn decide(&self, stored_last: Option<i64>) -> ProducerIdempotencyOutcome {
        let Some(last) = stored_last else {
            return ProducerIdempotencyOutcome::Accept;
        };
        let advances = match self.mode {
            ProducerMode::Chained => self.previous == last && self.sequence > last,
            ProducerMode::Monotonic => self.sequence > last,
        };
        if advances {
            return ProducerIdempotencyOutcome::Accept;
        }
        match self.mode {
            ProducerMode::Chained if self.sequence == last => {
                ProducerIdempotencyOutcome::DuplicateIgnore
            }
            ProducerMode::Chained => ProducerIdempotencyOutcome::SequenceViolation {
                last_sequence: last,
            },
            ProducerMode::Monotonic => ProducerIdempotencyOutcome::DuplicateIgnore,
        }
    }
}

/// Everything one publish hands to [`IdempotencyGuard::check_and_enqueue`]:
/// the producer chain to check (absent in stateless mode), the topic
/// partition the event was already stamped with, and the payload to enqueue.
///
/// `topic_partition` is here because the enqueue needs it and the chain does
/// not always carry it: a stateless publish has no chain at all, yet its event
/// still belongs to one topic partition whose order has to be preserved
/// through the outbox (`crate::domain::outbox::outbox_partition_for`).
///
/// Built through [`PublishEnqueue::builder`] so the optional chain is named at
/// the call site rather than passed as an `Option` argument.
#[domain_model]
#[derive(Debug, Clone)]
pub struct PublishEnqueue {
    /// The producer chain to check, or `None` for a stateless publish.
    pub chain: Option<ProducerChainCheck>,
    /// The partition of the *topic*, as `IngestService` stamped it - not an
    /// outbox partition.
    pub topic_partition: i32,
    pub payload: Vec<u8>,
    pub payload_type: String,
}

impl PublishEnqueue {
    /// Three arguments, each of a different type, so none can be transposed;
    /// the producer chain is the one optional part and is set on the builder.
    #[must_use]
    pub fn builder(
        topic_partition: i32,
        payload: Vec<u8>,
        payload_type: &str,
    ) -> PublishEnqueueBuilder {
        PublishEnqueueBuilder {
            chain: None,
            topic_partition,
            payload,
            payload_type: payload_type.to_owned(),
        }
    }
}

pub struct PublishEnqueueBuilder {
    chain: Option<ProducerChainCheck>,
    topic_partition: i32,
    payload: Vec<u8>,
    payload_type: String,
}

impl PublishEnqueueBuilder {
    /// Sets the producer chain to check. Left unset, the publish is stateless
    /// and no `evbk_producer_state` row is touched.
    #[must_use]
    pub fn chain(mut self, chain: ProducerChainCheck) -> Self {
        self.chain = Some(chain);
        self
    }

    #[must_use]
    pub fn build(self) -> PublishEnqueue {
        PublishEnqueue {
            chain: self.chain,
            topic_partition: self.topic_partition,
            payload: self.payload,
            payload_type: self.payload_type,
        }
    }
}

#[async_trait]
pub trait IdempotencyGuard: Send + Sync {
    /// Checks `request.chain` (when present) against stored
    /// `evbk_producer_state.last_sequence`, and - on `Accept` - inserts
    /// `request.payload` as one `toolkit-db` outbox row (design.md D5), all within
    /// **one DB transaction**: lock/check `producer_state` -> insert the
    /// outbox row -> update `producer_state` -> commit. That shared boundary is
    /// what makes check-and-record and the durable append atomic, and it is
    /// this method's own transaction, implemented by `infra::storage::
    /// Storage`. The backend `persist()` call itself stays deliberately
    /// **outside** this transaction (design.md D5's component-boundary
    /// rule: "we always work with backend out-of-tx") - it happens later,
    /// out-of-process from this call, when the background outbox processor
    /// (`infra::outbox::IngestOutboxHandler`) leases and drains the row.
    ///
    /// When `request.chain` is `None` (the publishing event carries no
    /// producer `meta`), no `producer_state` row is touched and the outcome
    /// is always `Accept` - the payload is still enqueued.
    ///
    /// # Errors
    /// Returns a `DomainError` if the check or the enqueue fails, including
    /// when the ingest outbox pipeline is not running yet.
    async fn check_and_enqueue(
        &self,
        request: PublishEnqueue,
    ) -> Result<ProducerIdempotencyOutcome, DomainError>;
}
