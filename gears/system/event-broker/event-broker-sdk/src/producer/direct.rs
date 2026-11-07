//! The direct producer: stamps each event's source and producer metadata, from
//! a local chain head per `(producer_id, topic)`, and publishes it straight to
//! the broker.
//!
//! The broker accepts a single writer per `producer_id` (ADR-0004 "Producer
//! Concurrency"), and a `DirectProducer` is that writer, so it serializes its
//! own concurrent publishes per topic. Each topic has a `tokio::sync::Mutex`,
//! held from stamping through the broker's answer to the advance of the head,
//! and across a chain reset of that topic, so no publish is stamped from the
//! head the reset is clearing. Without it, two concurrent publishes would read
//! the same head, carry the same numbers, and the broker would store one and
//! answer the other `Duplicate`. Topics do not share a lock, so they still
//! publish in parallel.
//!
//! A publish whose outcome the producer does not see (an error, or a future
//! dropped while the broker call is in flight) leaves the topic's head
//! uncertain, because the broker may have stored the events, and the producer
//! remembers which events it sent with which numbers. Deduplication is the
//! broker's, on `(producer_id, sequence)`; the event id is used only locally,
//! to recognise a re-publish:
//! - Re-publishing exactly those events (the same ids, in the same order)
//!   before any other publish on the topic resends the numbers they carried,
//!   with no extra call: if the first attempt was stored, the broker answers
//!   `Duplicate` instead of storing them twice.
//! - Any other publish on the topic re-reads the head from the broker under
//!   the lock before it stamps, so it never carries numbers the broker held
//!   when it re-read.
//! - Fresh numbers answered `Duplicate` were taken by an earlier request
//!   whose outcome the producer did not see, for example one the broker
//!   committed after the re-read; the events were not stored. The producer
//!   moves past those numbers and resends the events once with the next ones,
//!   under the same lock. A second `Duplicate` fails the publish with
//!   [`EventBrokerError::SequenceTaken`], and the next publish re-reads.
//!
//! A `SequenceViolation` answer means the numbers were stale: the producer
//! forgets them, and the next publish, the same event included, re-reads the
//! head. Other definite rejections (a field error, `Unauthorized`, an unknown
//! event type or producer, a schema violation, `Unimplemented`) store nothing
//! and take no numbers: the producer keeps them for a re-publish, and the head
//! stays uncertain, as after any error.
//!
//! Not covered: if an earlier request of another event, whose outcome was not
//! seen, commits the numbers this event was last sent with, re-publishing this
//! event is answered `Duplicate` although it was not stored. The broker
//! deduplicates on the numbers, not on the event id (#5660).
//!
//! Also not covered, in chained mode: a re-publish whose earlier attempt was stored is
//! answered `SequenceViolation` when an earlier unseen request reaches the broker later
//! and moves the head past it; the next publish then stores it again.
//!
//! A publish whose outcome was seen costs no extra call. The producer does not
//! retry an error by itself.
//!
//! Each topic also has a `tokio::sync::Semaphore` with `max_waiting_per_topic`
//! permits, one per publish queued for or holding its lock. A publish takes
//! its permits with `try_acquire`, never waiting for one: when a topic has
//! none left it fails at once with [`EventBrokerError::ProducerBusy`], and
//! nothing is sent, locked or stamped. It holds them until it returns or its
//! future is dropped, across the lock wait, the head re-read, the send and the
//! resend. A stalled topic exhausts only its own permits.
//!
//! Order: the input checks and the schema steps (a lazy producer's registry
//! lookups included) first, holding nothing; then the topic permits; then the
//! topic locks; then the `sequence_state` `RwLock`. A batch takes one permit
//! per topic it touches, then those topics' locks, both in ascending topic
//! order, and on a missing permit gives back the ones it took. A chain reset
//! takes no permit, and takes every topic lock it needs in ascending topic
//! order too, so no two of them can deadlock. The `RwLock` is a `std` lock,
//! taken only for a synchronous read or write: it is never held across an
//! await, and never while waiting for a topic lock.

use std::collections::{BTreeSet, HashMap};
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::sync::Arc;

use gts::GtsTypeId;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::{Ack, EventBrokerApi, IngestOutcome, ProducerMode};
use crate::error::EventBrokerError;
use crate::gts::{EventMeta, EventPayload, EventV1};
use crate::ids::ProducerId;
use crate::models::ResetScope;
use crate::sdk::EventBrokerSdk;

use super::event_factory::PreparedEvent;
use super::schema_cache::ProducerSchemaCache;

pub struct Missing;
pub struct Has;

type DirectProducerBuilderState<Broker, Ctx, Source, Topics, Patterns> =
    PhantomData<(Broker, Ctx, Source, Topics, Patterns)>;
/// A producer's chain is kept per `(producer_id, topic)` (ADR-0004 "Chain Scope").
type ProducerSequenceKey = (ProducerId, String);
pub(super) type ProducerSequenceCursor = HashMap<ProducerSequenceKey, i64>;
type SharedProducerSequenceCursor = Arc<std::sync::RwLock<ProducerSequenceCursor>>;
/// Keyed by topic alone: a `DirectProducer`'s `producer_id` is fixed, so the
/// topic names its chain.
type TopicPublishLocks = HashMap<String, TopicPublishLock>;

/// The publishes that may wait on one topic, as permits, and the lock that
/// serializes them.
#[derive(Debug)]
struct TopicPublishLock {
    /// One permit per publish queued for or holding `chain`.
    permits: tokio::sync::Semaphore,
    /// Says whether the topic's head may differ from the broker's, and what
    /// the last unseen publish sent.
    chain: tokio::sync::Mutex<TopicChain>,
}

/// What only the holder of a topic's publish lock reads or writes.
#[derive(Debug, Default)]
struct TopicChain {
    /// The last publish on the topic ended in an error or a dropped future, or
    /// a reset of the topic's chain was attempted since the head was last read,
    /// so the broker's head may differ from the local one.
    head_uncertain: bool,
    /// The events of the last publish on the topic whose outcome was not seen,
    /// in send order, with the numbers each carried. A re-publish of exactly
    /// these events resends these numbers; any other publish replaces them
    /// with its own before it sends. Empty once an outcome is seen, the chain
    /// is reset, a send is answered [`EventBrokerError::SequenceViolation`], or
    /// a resend ends in [`EventBrokerError::SequenceTaken`], so never set while
    /// the head is certain.
    unseen: Vec<(Uuid, EventMeta)>,
}

impl TopicChain {
    /// The numbers the last unseen publish gave `ids`, if it sent exactly
    /// these events in this order.
    fn unseen_numbers(&self, ids: &[Uuid]) -> Option<Vec<EventMeta>> {
        let same_events = self.unseen.len() == ids.len()
            && self
                .unseen
                .iter()
                .zip(ids)
                .all(|((sent, _), id)| sent == id);
        same_events.then(|| self.unseen.iter().map(|(_, meta)| meta.clone()).collect())
    }

    /// Records `sent` as about to go out: until its outcome is seen, the head
    /// is uncertain, and until then or a `SequenceViolation` a re-publish of
    /// these events resends these numbers.
    fn mark_unseen(&mut self, sent: Vec<(Uuid, EventMeta)>) {
        self.head_uncertain = true;
        self.unseen = sent;
    }

    fn mark_seen(&mut self) {
        self.head_uncertain = false;
        self.unseen.clear();
    }

    /// Forgets the numbers of the last unseen publish, so no re-publish
    /// resends them. The head stays uncertain, so the next publish re-reads it.
    fn forget_unseen(&mut self) {
        self.unseen.clear();
    }
}

/// How stamped events go to the broker.
#[derive(Clone, Copy)]
enum PublishCall {
    /// [`EventBrokerApi::publish`], for exactly one event.
    Single,
    /// [`EventBrokerApi::publish_batch`].
    Batch,
}

/// Builds a [`DirectProducer`]. `broker`, `security_context`, `source`, `topics`
/// and `event_type_patterns` are required; `build` exists only once all five
/// are set.
pub struct DirectProducerBuilder<
    Broker = Missing,
    Ctx = Missing,
    Source = Missing,
    Topics = Missing,
    Patterns = Missing,
> {
    broker: Option<Arc<dyn EventBrokerApi>>,
    ctx: Option<SecurityContext>,
    source: Option<String>,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    mode: ProducerMode,
    client_agent: Option<String>,
    lazy_schema_loading: bool,
    max_waiting_per_topic: usize,
    _state: DirectProducerBuilderState<Broker, Ctx, Source, Topics, Patterns>,
}

impl DirectProducerBuilder {
    /// Creates a builder with nothing set, in the default chained mode.
    fn new() -> Self {
        Self {
            broker: None,
            ctx: None,
            source: None,
            topics: Vec::new(),
            event_type_patterns: Vec::new(),
            mode: ProducerMode::default(),
            client_agent: None,
            lazy_schema_loading: false,
            max_waiting_per_topic: 64,
            _state: PhantomData,
        }
    }
}

impl<B, C, S, T, P> DirectProducerBuilder<B, C, S, T, P> {
    #[must_use]
    pub fn broker(
        mut self,
        broker: Arc<dyn EventBrokerApi>,
    ) -> DirectProducerBuilder<Has, C, S, T, P> {
        self.broker = Some(broker);
        self.into_state()
    }

    #[must_use]
    pub fn security_context(
        mut self,
        ctx: SecurityContext,
    ) -> DirectProducerBuilder<B, Has, S, T, P> {
        self.ctx = Some(ctx);
        self.into_state()
    }

    /// The `source` stamped on every event this producer sends.
    #[must_use]
    pub fn source(mut self, source: impl Into<String>) -> DirectProducerBuilder<B, C, Has, T, P> {
        self.source = Some(source.into());
        self.into_state()
    }

    #[must_use]
    pub fn topics<I, It>(mut self, topics: It) -> DirectProducerBuilder<B, C, S, Has, P>
    where
        I: Into<String>,
        It: IntoIterator<Item = I>,
    {
        self.topics = topics.into_iter().map(Into::into).collect();
        self.into_state()
    }

    #[must_use]
    pub fn event_type_patterns<I, It>(
        mut self,
        patterns: It,
    ) -> DirectProducerBuilder<B, C, S, T, Has>
    where
        I: Into<String>,
        It: IntoIterator<Item = I>,
    {
        self.event_type_patterns = patterns.into_iter().map(Into::into).collect();
        self.into_state()
    }

    /// The mode the producer registers with; chained unless set.
    #[must_use]
    pub fn mode(mut self, mode: ProducerMode) -> Self {
        self.mode = mode;
        self
    }

    /// The client agent the producer registers with; the SDK's default unless set.
    #[must_use]
    pub fn client_agent(mut self, client_agent: impl Into<String>) -> Self {
        self.client_agent = Some(client_agent.into());
        self
    }

    /// Prepares each event type on its first publish instead of at build.
    #[must_use]
    pub fn lazy_schema_loading(mut self) -> Self {
        self.lazy_schema_loading = true;
        self
    }

    /// Publishes queued for or holding one topic's lock (one semaphore per
    /// topic); 64 unless set. Applies to every `Ack`, since every publish
    /// queues on the same lock. A publish past it fails at once with
    /// [`EventBrokerError::ProducerBusy`].
    #[must_use]
    pub fn max_waiting_per_topic(mut self, n: NonZeroUsize) -> Self {
        self.max_waiting_per_topic = n.get();
        self
    }

    fn into_state<B2, C2, S2, T2, P2>(self) -> DirectProducerBuilder<B2, C2, S2, T2, P2> {
        DirectProducerBuilder {
            broker: self.broker,
            ctx: self.ctx,
            source: self.source,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            mode: self.mode,
            client_agent: self.client_agent,
            lazy_schema_loading: self.lazy_schema_loading,
            max_waiting_per_topic: self.max_waiting_per_topic,
            _state: PhantomData,
        }
    }
}

impl DirectProducerBuilder<Has, Has, Has, Has, Has> {
    /// Unless [`lazy_schema_loading`](Self::lazy_schema_loading) was set,
    /// prepares every declared topic and event type; then registers a fresh
    /// producer id with the broker and reads its cursors.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidProducerOptions`] for an empty or blank topic
    /// or event-type pattern list, a blank `source`, a `source` that breaks
    /// [`crate::validate::source`], [`ProducerMode::Stateless`] (a direct
    /// producer always deduplicates), or a
    /// [`max_waiting_per_topic`](Self::max_waiting_per_topic) above
    /// `tokio::sync::Semaphore::MAX_PERMITS`; otherwise whatever preparing the
    /// topics and event types, registering the producer, or reading its
    /// cursors fails with.
    ///
    /// # Panics
    ///
    /// Panics if the producer's sequence-state lock is poisoned.
    pub async fn build(self) -> Result<DirectProducer, EventBrokerError> {
        let broker = self.broker.expect("typestate requires broker");
        let ctx = self.ctx.expect("typestate requires security context");
        let source = self.source.expect("typestate requires source");
        validate_non_empty("topics", &self.topics)?;
        validate_non_empty("event_type_patterns", &self.event_type_patterns)?;
        if source.trim().is_empty() {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: "source is required".to_owned(),
            });
        }
        crate::validate::source(&source).map_err(|err| {
            EventBrokerError::InvalidProducerOptions {
                detail: format!("source {}", err.detail()),
            }
        })?;
        if self.mode == ProducerMode::Stateless {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: "a direct producer deduplicates: mode must be chained or monotonic"
                    .to_owned(),
            });
        }
        // `Semaphore::new` panics above this.
        if self.max_waiting_per_topic > tokio::sync::Semaphore::MAX_PERMITS {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: format!(
                    "max_waiting_per_topic must be at most {}",
                    tokio::sync::Semaphore::MAX_PERMITS
                ),
            });
        }

        let cache = Arc::new(ProducerSchemaCache::new());
        if !self.lazy_schema_loading {
            cache
                .prepare_all(&broker, &ctx, &self.topics, &self.event_type_patterns)
                .await?;
        }

        let client_agent = self
            .client_agent
            .unwrap_or_else(|| EventBrokerSdk::default_client_agent().to_owned());
        let producer_id = broker
            .register_producer(&ctx, self.mode, &client_agent)
            .await?;

        let publish_locks = self
            .topics
            .iter()
            .map(|topic| {
                let lock = TopicPublishLock {
                    permits: tokio::sync::Semaphore::new(self.max_waiting_per_topic),
                    chain: tokio::sync::Mutex::default(),
                };
                (topic.clone(), lock)
            })
            .collect();
        let producer = DirectProducer {
            broker,
            ctx,
            source,
            mode: self.mode,
            producer_id,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            lazy_schema_loading: self.lazy_schema_loading,
            cache,
            sequence_state: Arc::new(std::sync::RwLock::new(HashMap::new())),
            publish_locks,
            max_waiting_per_topic: self.max_waiting_per_topic,
        };
        producer.prime_sequence_state().await?;
        Ok(producer)
    }
}

// Prints the fields, the broker only as set or unset and the security context
// only as its subject id.
impl<B, C, S, T, P> std::fmt::Debug for DirectProducerBuilder<B, C, S, T, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let broker = if self.broker.is_some() {
            "set"
        } else {
            "unset"
        };
        f.debug_struct("DirectProducerBuilder")
            .field("broker", &format_args!("{broker}"))
            .field(
                "security_context",
                &self.ctx.as_ref().map(SecurityContext::subject_id),
            )
            .field("source", &self.source)
            .field("topics", &self.topics)
            .field("event_type_patterns", &self.event_type_patterns)
            .field("mode", &self.mode)
            .field("client_agent", &self.client_agent)
            .field("lazy_schema_loading", &self.lazy_schema_loading)
            .field("max_waiting_per_topic", &self.max_waiting_per_topic)
            .finish()
    }
}

/// Publishes events straight to the broker, deduplicated by the producer id it
/// registered at build.
///
/// Concurrent publishes on one `DirectProducer` are serialized per topic, so
/// each one is stamped after the previous one on its topic was answered;
/// different topics publish in parallel. At most
/// [`max_waiting_per_topic`](DirectProducerBuilder::max_waiting_per_topic)
/// publishes wait on one topic; the next fails at once with
/// [`EventBrokerError::ProducerBusy`].
pub struct DirectProducer {
    broker: Arc<dyn EventBrokerApi>,
    ctx: SecurityContext,
    source: String,
    mode: ProducerMode,
    producer_id: ProducerId,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    lazy_schema_loading: bool,
    cache: Arc<ProducerSchemaCache>,
    sequence_state: SharedProducerSequenceCursor,
    publish_locks: TopicPublishLocks,
    max_waiting_per_topic: usize,
}

// Prints no security context, broker or local state.
impl std::fmt::Debug for DirectProducer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectProducer")
            .field("mode", &self.mode)
            .field("producer_id", &self.producer_id)
            .field("topics", &self.topics)
            .finish_non_exhaustive()
    }
}

impl DirectProducer {
    /// Starts building a direct producer.
    #[must_use]
    pub fn builder() -> DirectProducerBuilder {
        DirectProducerBuilder::new()
    }

    /// Prepares the event type `type_id` and the declared topics, so a lazy
    /// producer's first publish of that type looks nothing up.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::EventTypeNotDeclared`] if `type_id` matches no
    /// declared pattern, [`EventBrokerError::TopicNotFound`] for a declared
    /// topic the broker does not list, [`EventBrokerError::TypeNotInDeclaredTopic`]
    /// if the type belongs to an undeclared topic, [`EventBrokerError::Internal`]
    /// if its data schema does not compile; otherwise whatever the broker's
    /// lookups fail with.
    pub async fn prepare_event_type(&self, type_id: &GtsTypeId) -> Result<(), EventBrokerError> {
        self.cache
            .prepare_one(
                &self.broker,
                &self.ctx,
                &self.topics,
                &self.event_type_patterns,
                type_id.as_ref(),
            )
            .await
    }

    /// Prepares every event type the declared patterns match, and the
    /// declared topics.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::TopicNotFound`] for a declared topic the broker
    /// does not list, [`EventBrokerError::EventTypeUnknown`] if the patterns
    /// match no event type, [`EventBrokerError::TypeNotInDeclaredTopic`] if a
    /// matched type belongs to an undeclared topic,
    /// [`EventBrokerError::Internal`] if a matched type's data schema does not
    /// compile; otherwise whatever the broker's lookups fail with.
    pub async fn prepare_all_event_types(&self) -> Result<(), EventBrokerError> {
        self.cache
            .prepare_all(
                &self.broker,
                &self.ctx,
                &self.topics,
                &self.event_type_patterns,
            )
            .await
    }

    /// The broker-registered id stamped on this producer's events.
    #[must_use]
    pub fn producer_id(&self) -> ProducerId {
        self.producer_id
    }

    /// Stamps `event` with this producer's source and the next numbers of its
    /// topic's chain, and publishes it through [`EventBrokerApi::publish`] with
    /// `ack`. No automatic retry.
    ///
    /// After a publish on the topic whose outcome was not seen, re-publishing
    /// the same event (same `id`) before any other publish on that topic, and
    /// alone, resends the numbers it carried, so the broker answers `Duplicate`
    /// if the first attempt was stored; any other event is stamped from the
    /// producer's head re-read from the broker. Fresh numbers answered
    /// `Duplicate` were taken by an earlier request: the event is resent once
    /// with the next numbers and, unless it is answered `Duplicate` too, that
    /// resend's answer is returned. A `SequenceViolation` answer means the
    /// numbers were stale: the producer forgets them, and the next publish, the
    /// same event included, re-reads the head. Re-publishing an event whose
    /// publish returned `Ok` stores it again.
    ///
    /// Not covered: if an earlier request of another event, whose outcome was
    /// not seen, commits the numbers this event was last sent with,
    /// re-publishing this event is answered `Duplicate` although it was not
    /// stored. The broker deduplicates on the numbers, not on the event id
    /// (#5660).
    ///
    /// Also not covered, in chained mode: a re-publish whose earlier attempt was stored
    /// is answered `SequenceViolation` when an earlier unseen request reaches the broker
    /// later and moves the head past it; the next publish then stores it again.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidEventField`] or
    /// [`EventBrokerError::InvalidTextField`] for an event the producer does not
    /// take ([`EventV1::check_producer_input`]) or whose envelope breaks a
    /// builder rule, before any broker call;
    /// [`EventBrokerError::ProducerBusy`] when
    /// [`max_waiting_per_topic`](DirectProducerBuilder::max_waiting_per_topic)
    /// publishes already wait on the event's topic, after preparing `event`
    /// and before sending, locking or stamping anything;
    /// [`EventBrokerError::SequenceTaken`] when the resend is answered
    /// `Duplicate` too; otherwise the error of preparing `event` or of the
    /// broker's publish, or the error of re-reading the producer's head after
    /// an earlier publish on the topic whose outcome was not seen or a reset of
    /// its chain, or [`EventBrokerError::Internal`] when `data` does not
    /// serialize or on a broken internal invariant (no publish lock for the
    /// event's declared topic, closed publish permits, or a publish that
    /// locked no topic).
    ///
    /// # Panics
    ///
    /// Panics if the producer's sequence-state lock is poisoned.
    pub async fn publish<P: EventPayload>(
        &self,
        event: EventV1<P>,
        ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError> {
        Self::check_input(&event)?;
        let prepared = self.prepare_wire_event(&event).await?;
        self.publish_prepared(vec![prepared], PublishCall::Single, ack)
            .await
    }

    /// Publishes `events` as one all-or-nothing batch through
    /// [`EventBrokerApi::publish_batch`] with `ack`, stamped as
    /// [`publish`](Self::publish) stamps one event. Every event is checked
    /// before any is prepared or sent. No automatic retry.
    ///
    /// The batch is resent with the numbers its events carried when, on every
    /// topic it touches, the last publish's outcome was not seen and that
    /// publish sent exactly this batch's events of the topic, in order; its
    /// fresh numbers answered `Duplicate` are resent once with the next ones,
    /// as for one event. Not covered: a batch that shares only some events
    /// with an earlier unseen batch is stamped fresh from a re-read head; a
    /// fresh batch whose leading events land on numbers an earlier unseen
    /// request took is answered `Accepted`, with those events dropped by the
    /// broker as duplicates; and if an earlier request of other events, whose
    /// outcome was not seen, commits the numbers this batch was last sent with,
    /// re-publishing this batch is answered `Duplicate` although it was not
    /// stored. The broker deduplicates on the numbers, not on the event ids
    /// (#5660). Also not covered, in chained mode: a re-publish of this batch whose
    /// earlier attempt was stored is answered `SequenceViolation` when an earlier unseen
    /// request reaches the broker later and moves the head past it; the next publish
    /// then stores it again.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidEventField`] for an empty `events`, and the
    /// errors of [`publish`](Self::publish) for the first event that fails, in
    /// the same order; nothing is sent then.
    /// [`EventBrokerError::ProducerBusy`] when any topic of the batch already
    /// has [`max_waiting_per_topic`](DirectProducerBuilder::max_waiting_per_topic)
    /// publishes waiting on it; the batch then gives back the permits it took
    /// and sends nothing.
    /// [`EventBrokerError::SequenceTaken`], naming the batch's first topic in
    /// ascending order, when the resend is answered `Duplicate` too.
    /// [`EventBrokerError::Internal`] on a broken internal invariant (closed
    /// publish permits, or a publish that locked no topic).
    ///
    /// # Panics
    ///
    /// Panics if the producer's sequence-state lock is poisoned.
    pub async fn publish_batch<P: EventPayload>(
        &self,
        events: Vec<EventV1<P>>,
        ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError> {
        if events.is_empty() {
            return Err(EventBrokerError::InvalidEventField {
                field: "events",
                detail: "empty batch".to_owned(),
            });
        }
        for event in &events {
            Self::check_input(event)?;
        }
        let mut prepared = Vec::with_capacity(events.len());
        for event in &events {
            prepared.push(self.prepare_wire_event(event).await?);
        }
        self.publish_prepared(prepared, PublishCall::Batch, ack)
            .await
    }

    /// Resets this producer's chain on the broker, over `scope`. It holds the
    /// publish lock of each declared topic in `scope` across the broker call,
    /// taken in ascending topic order, and the next publish on each of those
    /// topics re-reads its head from the broker.
    ///
    /// The locks are granted in FIFO order, behind every publish already waiting
    /// on them. A single publish holds its topic's lock for at most one head
    /// re-read and two publish calls (the resend after a `Duplicate`); over
    /// REST each call may take 4 attempts of 30 s and 3 waits of up to 10 s on
    /// a rate-limited broker, 150 s, so 450 s. A reset of every topic, like a
    /// batch over several topics, holds the locks it has while it queues for
    /// the later ones: with `n` holders queued ahead of it on later topics, it
    /// holds its first lock for up to `n` x 450 s, plus 150 s for a reset's own
    /// call, 450 s for a batch's. A reset takes no permit: it is not a publish, and
    /// the permits already bound the publishes queued ahead of it to
    /// [`max_waiting_per_topic`](DirectProducerBuilder::max_waiting_per_topic) per
    /// topic, so `n` is at most that many per later topic, plus any other reset
    /// queued there.
    ///
    /// # Errors
    ///
    /// Whatever the broker's reset fails with.
    pub async fn reset_chain(&self, scope: ResetScope<'_>) -> Result<(), EventBrokerError> {
        // A topic this producer does not declare has no local head to guard.
        let locks: Vec<_> = match scope {
            ResetScope::AllTopics => {
                let mut locks = self.publish_locks.iter().collect::<Vec<_>>();
                locks.sort_unstable_by_key(|(topic, _)| *topic);
                locks.into_iter().map(|(_, lock)| &lock.chain).collect()
            }
            ResetScope::Topic(topic) => self
                .publish_locks
                .get(topic)
                .map(|lock| &lock.chain)
                .into_iter()
                .collect(),
        };
        let mut chains = Vec::with_capacity(locks.len());
        for lock in locks {
            chains.push(lock.lock().await);
        }
        // Set before the call and never cleared here: a failed or dropped
        // reset may still have reset the broker, so the next publish re-reads.
        // Numbers from before the reset are never resent.
        for chain in &mut chains {
            chain.mark_unseen(Vec::new());
        }
        self.broker
            .reset_producer_chain(&self.ctx, self.producer_id, scope)
            .await
    }

    /// Stamps `prepared` and sends it under the publish lock of every topic it
    /// touches, after a permit of each, both taken in ascending topic order:
    /// with the numbers an unseen publish of exactly these events carried, or
    /// else with fresh numbers from the local heads, re-read first where
    /// uncertain. Fresh numbers answered `Duplicate` are resent once with the
    /// next ones; numbers answered `SequenceViolation` are forgotten.
    async fn publish_prepared(
        &self,
        mut prepared: Vec<PreparedEvent>,
        call: PublishCall,
        ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError> {
        let topics = prepared
            .iter()
            .map(|prepared| prepared.topic.clone())
            .collect::<BTreeSet<_>>();
        // Held until this publish returns or its future is dropped. A missing
        // permit returns at once, and the ones taken so far drop with `permits`.
        let mut permits = Vec::with_capacity(topics.len());
        for topic in &topics {
            permits.push(self.take_permit(topic)?);
        }
        let mut chains = Vec::with_capacity(topics.len());
        for topic in topics {
            let chain = self.publish_lock(&topic)?.chain.lock().await;
            chains.push((topic, chain));
        }
        let remembered = chains
            .iter()
            .map(|(topic, chain)| chain.unseen_numbers(&Self::ids_on(&prepared, topic)))
            .collect::<Option<Vec<_>>>();
        let reused = remembered.is_some();
        if let Some(remembered) = remembered {
            for ((topic, _), numbers) in chains.iter().zip(remembered) {
                let on_topic = prepared.iter_mut().filter(|p| p.topic == *topic);
                for (prepared, meta) in on_topic.zip(numbers) {
                    prepared.event.stamp_meta(meta);
                }
            }
        } else {
            let uncertain = chains
                .iter()
                .filter(|(_, chain)| chain.head_uncertain)
                .map(|(topic, _)| topic.as_str())
                .collect::<Vec<_>>();
            if !uncertain.is_empty() {
                self.refresh_heads(&uncertain).await?;
            }
            self.stamp_fresh(&mut prepared);
        }
        // Set before the call, so an error or a dropped future leaves the head
        // uncertain and, unless the error is a `SequenceViolation`, these
        // numbers for a re-publish: the broker may hold the events without the
        // answer arriving.
        Self::mark_unseen(&mut chains, &prepared);
        let outcome = self
            .send(&prepared, call, ack)
            .await
            .inspect_err(|error| Self::forget_stale(&mut chains, error))?;
        // Exhaustive, so a new outcome has to decide whether it is resent.
        let taken = match outcome {
            IngestOutcome::Accepted | IngestOutcome::Persisted => false,
            IngestOutcome::Duplicate => true,
        };
        // A batch is all-or-nothing: its one outcome covers every event, so
        // each one advances the local chain.
        self.advance_all(&prepared, outcome).await;
        if reused || !taken {
            for (_, chain) in &mut chains {
                chain.mark_seen();
            }
            return Ok(outcome);
        }
        // Fresh numbers answered `Duplicate`: an earlier request whose outcome
        // was not seen took them, and these events are not stored. The chain
        // now stands past them; resend once from there.
        self.stamp_fresh(&mut prepared);
        Self::mark_unseen(&mut chains, &prepared);
        let outcome = self
            .send(&prepared, call, ack)
            .await
            .inspect_err(|error| Self::forget_stale(&mut chains, error))?;
        if outcome == IngestOutcome::Duplicate {
            for (_, chain) in &mut chains {
                chain.forget_unseen();
            }
            let topic = chains
                .first()
                .map(|(topic, _)| topic.clone())
                .ok_or_else(|| {
                    EventBrokerError::Internal("a publish locks at least one topic".to_owned())
                })?;
            return Err(EventBrokerError::SequenceTaken { topic });
        }
        self.advance_all(&prepared, outcome).await;
        for (_, chain) in &mut chains {
            chain.mark_seen();
        }
        Ok(outcome)
    }

    async fn send(
        &self,
        prepared: &[PreparedEvent],
        call: PublishCall,
        ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError> {
        match (call, prepared) {
            (PublishCall::Single, [prepared]) => {
                self.broker.publish(&self.ctx, &prepared.event, ack).await
            }
            (PublishCall::Single, _) => Err(EventBrokerError::Internal(
                "a single publish carries exactly one event".to_owned(),
            )),
            (PublishCall::Batch, prepared) => {
                let events = prepared
                    .iter()
                    .map(|prepared| prepared.event.clone())
                    .collect::<Vec<_>>();
                self.broker.publish_batch(&self.ctx, &events, ack).await
            }
        }
    }

    /// The ids of `prepared`'s events on `topic`, in batch order.
    fn ids_on(prepared: &[PreparedEvent], topic: &str) -> Vec<Uuid> {
        prepared
            .iter()
            .filter(|prepared| prepared.topic == topic)
            .map(|prepared| prepared.event.id())
            .collect()
    }

    /// Records, per topic, the events of `prepared` and their numbers as sent
    /// with an outcome not yet seen.
    fn mark_unseen(
        chains: &mut [(String, tokio::sync::MutexGuard<'_, TopicChain>)],
        prepared: &[PreparedEvent],
    ) {
        for (topic, chain) in chains {
            let sent = prepared
                .iter()
                .filter(|prepared| prepared.topic == *topic)
                .filter_map(|prepared| {
                    prepared
                        .event
                        .meta()
                        .map(|meta| (prepared.event.id(), meta.clone()))
                })
                .collect();
            chain.mark_unseen(sent);
        }
    }

    /// Forgets the numbers `chains` remember when `error` says they were stale:
    /// a `SequenceViolation` means the broker's head is elsewhere, so resending
    /// them would be refused again. Any other error keeps them for a
    /// re-publish: a definite rejection stored nothing and took no numbers,
    /// and a lost outcome may have stored the events.
    fn forget_stale(
        chains: &mut [(String, tokio::sync::MutexGuard<'_, TopicChain>)],
        error: &EventBrokerError,
    ) {
        if matches!(error, EventBrokerError::SequenceViolation { .. }) {
            for (_, chain) in chains {
                chain.forget_unseen();
            }
        }
    }

    /// Stamps each of `prepared` with the next numbers of its topic, in order,
    /// from the local heads. The caller holds those topics' publish locks.
    fn stamp_fresh(&self, prepared: &mut [PreparedEvent]) {
        let mut cursor = self
            .sequence_state
            .read()
            .expect("producer sequence state lock poisoned")
            .clone();
        for prepared in prepared {
            if let Some(meta) = self.producer_meta_from_cursor(&cursor, &prepared.topic) {
                prepared.event.stamp_meta(meta);
            }
            advance_cursor(&mut cursor, &prepared.topic, prepared.event.meta());
        }
    }

    async fn advance_all(&self, prepared: &[PreparedEvent], outcome: IngestOutcome) {
        for prepared in prepared {
            self.advance_after_acceptance(prepared, outcome).await;
        }
    }

    /// Refuses an event the producer does not take, or whose envelope breaks a
    /// builder rule: an event read with serde never passed through the builder.
    fn check_input<P: EventPayload>(event: &EventV1<P>) -> Result<(), EventBrokerError> {
        event.check_producer_input()?;
        event.check_envelope()
    }

    /// Prepares the event unstamped, with this producer's source: its producer
    /// metadata is set only under the topic's publish lock.
    async fn prepare_wire_event<P: EventPayload>(
        &self,
        event: &EventV1<P>,
    ) -> Result<PreparedEvent, EventBrokerError> {
        let mut raw = EventV1::try_from(event)?;
        raw.stamp_source(self.source.clone());
        let type_id = event.event_type();
        self.cache
            .ensure_declared(&self.event_type_patterns, type_id.as_ref())
            .await?;
        if self.lazy_schema_loading && !self.cache.is_prepared(type_id.as_ref()).await {
            self.prepare_event_type(type_id).await?;
        }
        PreparedEvent::new(&self.cache, raw).await
    }

    /// Takes one of `topic`'s permits without waiting for it, or refuses the
    /// publish as busy when the topic has none left.
    fn take_permit(
        &self,
        topic: &str,
    ) -> Result<tokio::sync::SemaphorePermit<'_>, EventBrokerError> {
        self.publish_lock(topic)?
            .permits
            .try_acquire()
            .map_err(|error| match error {
                tokio::sync::TryAcquireError::NoPermits => EventBrokerError::ProducerBusy {
                    limit: self.max_waiting_per_topic,
                },
                // Nothing closes the permits.
                tokio::sync::TryAcquireError::Closed => EventBrokerError::Internal(format!(
                    "the publish permits of the declared topic {topic} are closed"
                )),
            })
    }

    /// A miss is an internal fault: preparing an event already checked that its
    /// topic is declared, and every declared topic has a lock.
    fn publish_lock(&self, topic: &str) -> Result<&TopicPublishLock, EventBrokerError> {
        self.publish_locks.get(topic).ok_or_else(|| {
            EventBrokerError::Internal(format!("no publish lock for the declared topic {topic}"))
        })
    }

    fn producer_meta_from_cursor(
        &self,
        cursor: &ProducerSequenceCursor,
        topic: &str,
    ) -> Option<EventMeta> {
        let last = cursor
            .get(&(self.producer_id, topic.to_owned()))
            .copied()
            .unwrap_or(-1);
        producer_meta_for_last(self.mode, self.producer_id, last)
    }

    /// Moves the local chain up to `prepared`'s sequence once the broker holds
    /// that sequence, from this event or from an earlier request. Every outcome
    /// means it does: a `Duplicate` says the broker already holds this sequence
    /// or a later one, so staying behind would re-stamp every following event
    /// with these numbers and have each dropped as one more duplicate. The
    /// caller holds the topic's publish lock, so `prepared` carries the head's
    /// next sequence; the state only ever moves forward.
    pub(super) async fn advance_after_acceptance(
        &self,
        prepared: &PreparedEvent,
        outcome: IngestOutcome,
    ) {
        // Irrefutable, so a new outcome has to decide whether it advances.
        let (IngestOutcome::Accepted | IngestOutcome::Persisted | IngestOutcome::Duplicate) =
            outcome;
        let Some(meta) = prepared.event.meta() else {
            return;
        };
        let (Some(producer_id), Some(sequence)) = (meta.producer_id, meta.sequence) else {
            return;
        };
        let mut sequence_state = self
            .sequence_state
            .write()
            .expect("producer sequence state lock poisoned");
        let last = sequence_state
            .entry((ProducerId(producer_id), prepared.topic.clone()))
            .or_insert(sequence);
        *last = (*last).max(sequence);
    }

    async fn prime_sequence_state(&self) -> Result<(), EventBrokerError> {
        let cursors = self
            .broker
            .get_producer_cursors(&self.ctx, self.producer_id)
            .await?;
        self.sequence_state
            .write()
            .expect("producer sequence state lock poisoned")
            .extend(cursors.topics.into_iter().map(|topic| {
                (
                    (self.producer_id, topic.topic.into_string()),
                    topic.last_sequence,
                )
            }));
        Ok(())
    }

    /// Replaces the local head of each of `topics` with the broker's, or `-1`
    /// where the broker lists none: after an outcome the producer did not see,
    /// or a chain reset, the broker's head is the only one that counts. The
    /// caller holds those topics' publish locks.
    async fn refresh_heads(&self, topics: &[&str]) -> Result<(), EventBrokerError> {
        let cursors = self
            .broker
            .get_producer_cursors(&self.ctx, self.producer_id)
            .await?;
        let mut sequence_state = self
            .sequence_state
            .write()
            .expect("producer sequence state lock poisoned");
        for &topic in topics {
            let last = cursors
                .topics
                .iter()
                .find(|cursor| cursor.topic == topic)
                .map_or(-1, |cursor| cursor.last_sequence);
            sequence_state.insert((self.producer_id, topic.to_owned()), last);
        }
        Ok(())
    }
}

pub(super) fn advance_cursor(
    cursor: &mut ProducerSequenceCursor,
    topic: &str,
    meta: Option<&EventMeta>,
) {
    let Some(meta) = meta else {
        return;
    };
    let (Some(producer_id), Some(sequence)) = (meta.producer_id, meta.sequence) else {
        return;
    };
    cursor.insert((ProducerId(producer_id), topic.to_owned()), sequence);
}

pub(super) fn producer_meta_for_last(
    mode: ProducerMode,
    producer_id: ProducerId,
    last: i64,
) -> Option<EventMeta> {
    match mode {
        ProducerMode::Stateless => None,
        ProducerMode::Monotonic => Some(EventMeta {
            version: 1,
            producer_id: Some(producer_id.0),
            previous: None,
            sequence: Some(last + 1),
        }),
        ProducerMode::Chained => Some(EventMeta {
            version: 1,
            producer_id: Some(producer_id.0),
            previous: Some(last),
            sequence: Some(last + 1),
        }),
    }
}

pub(super) fn validate_non_empty(
    field: &'static str,
    values: &[String],
) -> Result<(), EventBrokerError> {
    if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
        Err(EventBrokerError::InvalidProducerOptions {
            detail: format!("{field} must contain at least one non-empty entry"),
        })
    } else {
        Ok(())
    }
}
