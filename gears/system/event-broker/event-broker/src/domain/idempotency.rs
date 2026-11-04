//! Idempotency key computation and checking (`domain/idempotency.rs` in
//! `DESIGN.md` "Module Structure"; realized against
//! `event_broker_producer_sequence` per `ADR/0004-idempotent-producer-protocol.md`).
//!
//! The module/trait names stay general ("idempotency", not "producer
//! idempotency") because `DESIGN.md` "Module Structure" already fixes this
//! file's path and `IdempotencyGuard`'s name; the outcome type isn't pinned by
//! DESIGN.md, so it's named precisely for what it actually checks - the
//! producer chain protocol, not idempotency in general.

use async_trait::async_trait;
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ingest::ProducerMode;

/// Outcome of a producer chain-sequencing check for one incoming event
/// (`meta.producer_id`, `meta.previous`, `meta.sequence`) against
/// `ADR-0004`'s idempotent producer protocol. `SequenceViolation` carries
/// the run's producer and the broker's current `last_sequence` so the caller
/// can build the `docs/openapi.yaml` `412` response body without a second
/// lookup.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProducerIdempotencyOutcome {
    Accept,
    DuplicateIgnore,
    SequenceViolation {
        producer_id: Uuid,
        last_sequence: i64,
    },
}

/// The producer-chain identity/sequence a publish call checks against
/// `event_broker_producer_sequence` - `None` on `IdempotencyGuard::check_and_enqueue`
/// when the publishing event carries no `meta` (stateless mode), in which
/// case there is no chain to check and the call always accepts.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProducerChainCheck {
    pub producer_id: Uuid,
    pub topic: GtsInstanceId,
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

/// The stored head of one `(producer_id, topic)` chain, as a run is checked
/// against it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainHead {
    /// No `event_broker_producer_sequence` row: the first event of the run
    /// takes its numbers (feature 0001 §6, state `Absent`).
    Absent,
    /// The committed `last_sequence`.
    At(i64),
}

/// One event of a [`ChainRun`]: where it sits in the request, and its chain
/// numbers.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunEvent {
    /// The event's position in the request, which every batch error names.
    pub index: usize,
    pub previous: i64,
    pub sequence: i64,
}

/// The chained or monotonic events of one `(producer_id, topic)` in a request,
/// in request order (feature 0001 §5.2). Stateless events belong to no run, and
/// a single-event publish is a run of one.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRun {
    pub producer_id: Uuid,
    pub topic: GtsInstanceId,
    pub mode: ProducerMode,
    pub events: Vec<RunEvent>,
}

/// How a [`ChainRun`] stands against its chain's committed head (feature 0001
/// §5.2 R4 and R5).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunDecision {
    /// The first `duplicates` events are a duplicate prefix and are not
    /// enqueued; the rest are, and the head moves to `head`, the run's last
    /// `sequence`.
    Advance { duplicates: usize, head: i64 },
    /// Every event of the run is a duplicate: nothing is enqueued and the head
    /// stays.
    Duplicate,
    /// The run neither advances nor duplicates the head; the whole request is
    /// refused with the committed `last_sequence`.
    SequenceViolation { last_sequence: i64 },
}

impl ChainRun {
    /// Splits `requests` into one run per `(producer_id, topic)`, each keeping
    /// request order (feature 0001 §5.2). Runs come in the order of their first
    /// event, which is the order a batch locks their heads in.
    #[must_use]
    pub fn split(requests: &[PublishEnqueue]) -> Vec<ChainRun> {
        let mut runs: Vec<ChainRun> = Vec::new();
        for (index, chain) in requests
            .iter()
            .enumerate()
            .filter_map(|(index, request)| Some((index, request.chain.as_ref()?)))
        {
            let event = RunEvent {
                index,
                previous: chain.previous,
                sequence: chain.sequence,
            };
            match runs
                .iter_mut()
                .find(|run| run.producer_id == chain.producer_id && run.topic == chain.topic)
            {
                Some(run) => run.events.push(event),
                None => runs.push(ChainRun {
                    producer_id: chain.producer_id,
                    topic: chain.topic.clone(),
                    mode: chain.mode,
                    events: vec![event],
                }),
            }
        }
        runs
    }

    /// The request index of the first event that breaks the run's own shape,
    /// checked before any chain state is read (feature 0001 §5.2 R3): in a
    /// chained run every event has `sequence > previous` and every event after
    /// the first links to the one before it (`previous` is its `sequence`); a
    /// monotonic run's sequences strictly increase.
    #[must_use]
    pub fn first_malformed(&self) -> Option<usize> {
        let mut prior: Option<&RunEvent> = None;
        for event in &self.events {
            let well_formed = match self.mode {
                ProducerMode::Chained => {
                    let moves_forward = event.sequence > event.previous;
                    let links = prior.is_none_or(|prior| prior.sequence == event.previous);
                    moves_forward && links
                }
                ProducerMode::Monotonic => {
                    prior.is_none_or(|prior| event.sequence > prior.sequence)
                }
            };
            if !well_formed {
                return Some(event.index);
            }
            prior = Some(event);
        }
        None
    }

    /// The run against its chain's committed head (feature 0001 §5.2 R4, R5).
    ///
    /// This is the domain's decision, kept pure: infra reads the head and
    /// performs the writes the decision dictates. The sequence space tolerates
    /// gaps; what matters is that the chain links, not that it is contiguous.
    ///
    /// - An absent head: the first event takes its numbers, and the run
    ///   advances.
    /// - Chained (R4): the run advances when its first event links to the head
    ///   and moves past it (`previous == last && sequence > last`). When some
    ///   event `e[k]` has `sequence == last`, events through `e[k]` are a
    ///   duplicate prefix and the rest advance; when that is the last event, the
    ///   whole run is a duplicate - a lost-ack resend. Anything else is a chain
    ///   desync, and the producer re-reads its cursor.
    /// - Monotonic (R5): the leading events with `sequence <= last` are a
    ///   duplicate prefix and the rest advance; a monotonic run is never a
    ///   violation.
    ///
    /// The decision relies on R3 only for the events after the first, so a run
    /// of one - a single publish, which R3 does not gate - is decided on its
    /// own numbers.
    #[must_use]
    pub fn decide(&self, head: ChainHead) -> RunDecision {
        let (Some(first), Some(last_event)) = (self.events.first(), self.events.last()) else {
            return RunDecision::Duplicate;
        };
        let advance = |duplicates| RunDecision::Advance {
            duplicates,
            head: last_event.sequence,
        };
        let ChainHead::At(last) = head else {
            return advance(0);
        };
        let duplicates = match self.mode {
            ProducerMode::Chained => {
                if first.previous == last && first.sequence > last {
                    return advance(0);
                }
                match self.events.iter().position(|event| event.sequence == last) {
                    Some(k) => k + 1,
                    None => {
                        return RunDecision::SequenceViolation {
                            last_sequence: last,
                        };
                    }
                }
            }
            ProducerMode::Monotonic => self
                .events
                .iter()
                .take_while(|event| event.sequence <= last)
                .count(),
        };
        if duplicates == self.events.len() {
            RunDecision::Duplicate
        } else {
            advance(duplicates)
        }
    }
}

/// Everything one publish hands to [`IdempotencyGuard::check_and_enqueue`]:
/// the producer chain to check (absent in stateless mode), the ingest outbox
/// partition the event drains through, and the payload to enqueue.
///
/// `ingest_partition` is derived from the topic and the event's partition key
/// (`crate::domain::outbox::outbox_partition_for`), so every event of one key
/// drains in publish order. Ingest never knows the topic partition: the
/// storage backend assigns it.
///
/// Built through [`PublishEnqueue::builder`] so the optional chain is named at
/// the call site rather than passed as an `Option` argument.
#[domain_model]
#[derive(Debug, Clone)]
pub struct PublishEnqueue {
    /// The producer chain to check, or `None` for a stateless publish.
    pub chain: Option<ProducerChainCheck>,
    /// The ingest outbox partition, not a topic partition.
    pub ingest_partition: u32,
    pub payload: Vec<u8>,
    pub payload_type: String,
}

impl PublishEnqueue {
    /// Three arguments, each of a different type, so none can be transposed;
    /// the producer chain is the one optional part and is set on the builder.
    #[must_use]
    pub fn builder(
        ingest_partition: u32,
        payload: Vec<u8>,
        payload_type: &str,
    ) -> PublishEnqueueBuilder {
        PublishEnqueueBuilder {
            chain: None,
            ingest_partition,
            payload,
            payload_type: payload_type.to_owned(),
        }
    }
}

#[domain_model]
pub struct PublishEnqueueBuilder {
    chain: Option<ProducerChainCheck>,
    ingest_partition: u32,
    payload: Vec<u8>,
    payload_type: String,
}

impl PublishEnqueueBuilder {
    /// Sets the producer chain to check. Left unset, the publish is stateless
    /// and no `event_broker_producer_sequence` row is touched.
    #[must_use]
    pub fn chain(mut self, chain: ProducerChainCheck) -> Self {
        self.chain = Some(chain);
        self
    }

    #[must_use]
    pub fn build(self) -> PublishEnqueue {
        PublishEnqueue {
            chain: self.chain,
            ingest_partition: self.ingest_partition,
            payload: self.payload,
            payload_type: self.payload_type,
        }
    }
}

#[async_trait]
pub trait IdempotencyGuard: Send + Sync {
    /// Checks `request.chain` (when present) against stored
    /// `event_broker_producer_sequence.last_sequence`, and - on `Accept` - inserts
    /// `request.payload` as one `toolkit-db` outbox row, all within
    /// **one DB transaction**: read the head (locking it where the database
    /// locks rows) and decide -> insert the outbox row -> move the head with a
    /// compare-and-set against the head read -> commit. A writer that moved
    /// the head first - possible only where there was no row to lock - makes
    /// the head write change nothing; the transaction then rolls back and runs
    /// again against the new head, as it does on database contention (bounded,
    /// so a persistent failure is still reported). That shared boundary is
    /// what makes check-and-record and the durable append atomic, and it is
    /// this method's own transaction, implemented by `infra::storage::
    /// Storage`. The backend `append()` call stays deliberately **outside**
    /// this transaction - the broker never works with a backend in-tx - and
    /// happens later, when the background outbox processor
    /// (`infra::workers::IngestOutboxHandler`) leases and drains the row.
    ///
    /// When `request.chain` is `None` (the publishing event carries no
    /// producer `meta`), no `event_broker_producer_sequence` row is touched and
    /// the outcome is always `Accept` - the payload is still enqueued.
    ///
    /// # Errors
    ///
    /// Returns a `DomainError` if the check or the enqueue fails, including
    /// when the ingest outbox pipeline is not running yet.
    async fn check_and_enqueue(
        &self,
        request: PublishEnqueue,
    ) -> Result<ProducerIdempotencyOutcome, DomainError>;

    /// [`check_and_enqueue`](Self::check_and_enqueue) for every request of a
    /// batch, all within **one DB transaction**. The chained and monotonic
    /// requests are checked as [`ChainRun`]s, one per `(producer, topic)`, each
    /// against its committed head (feature 0001 §5.2 R4, R5); the caller has
    /// already checked each run's shape (R3).
    ///
    /// All-or-nothing: the first run that breaks its chain rolls the whole
    /// transaction back, so nothing of the batch is enqueued and no head
    /// moves. A duplicate enqueues nothing and the rest of the batch goes on.
    /// The outcomes, one per request in request order, are each `Accept` or
    /// `DuplicateIgnore`; a stateless request is always `Accept`.
    ///
    /// # Errors
    ///
    /// `DomainError::SequenceViolation` naming the first breaking run - the
    /// request index of its first event, its producer and topic - and the
    /// committed `last_sequence`; otherwise as
    /// [`check_and_enqueue`](Self::check_and_enqueue).
    async fn check_and_enqueue_batch(
        &self,
        requests: Vec<PublishEnqueue>,
    ) -> Result<Vec<ProducerIdempotencyOutcome>, DomainError>;
}

#[cfg(test)]
#[path = "idempotency_tests.rs"]
mod idempotency_tests;
