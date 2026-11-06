//! The direct producer: stamps each event's producer metadata from a local
//! chain head per `(producer_id, topic)` and publishes it straight to the
//! broker.
//!
//! The broker accepts a single writer per `producer_id` (ADR-0004 "Producer
//! Concurrency"), and a `Producer` is that writer, so it serializes its own
//! concurrent publishes per topic. Each topic has a `tokio::sync::Mutex`,
//! held from stamping through the broker's answer to the advance of the head.
//! Without it, two concurrent publishes would read the same head, carry the
//! same numbers, and the broker would store one and answer the other
//! `Duplicate`. A failed publish drops the lock without advancing, so the
//! next publish re-stamps the same numbers. If the failed publish was stored
//! after all, the re-stamped event is answered `Duplicate` and lost (tracked
//! in #5660). Topics do not share a lock, so they still publish in parallel.
//!
//! Lock order: the topic locks first, then the `sequence_state` `RwLock`. A
//! batch takes every topic lock it needs in ascending topic order, so two
//! batches cannot deadlock. The `RwLock` is a `std` lock, taken only for a
//! synchronous read or write: it is never held across an await, and never
//! while waiting for a topic lock.

use std::collections::{BTreeSet, HashMap};
use std::marker::PhantomData;
use std::sync::Arc;

use toolkit_security::SecurityContext;

use crate::api::{EventBrokerApi, IngestOutcome, ProducerMode};
use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::models::{ProducerMeta, ResetScope};
use crate::sdk::EventBrokerSdk;
use crate::typed_event::TypedEvent;

use super::event_factory::prepare_event;
use super::schema_cache::ProducerSchemaCache;
use super::types::{DirectDeduplication, ProducerIdentity, ValidationTiming};

pub struct Missing;
pub struct Has;

type ProducerBuilderState<Broker, Ctx, Identity, Dedup, Topics, Patterns> =
    PhantomData<(Broker, Ctx, Identity, Dedup, Topics, Patterns)>;
/// A producer's chain is kept per `(producer_id, topic)` (ADR-0004 "Chain Scope").
type ProducerSequenceKey = (ProducerId, String);
pub(super) type ProducerSequenceCursor = HashMap<ProducerSequenceKey, i64>;
type SharedProducerSequenceCursor = Arc<std::sync::RwLock<ProducerSequenceCursor>>;
/// Keyed by topic alone: a `Producer`'s `producer_id` is fixed, so the topic
/// names its chain.
type TopicPublishLocks = HashMap<String, tokio::sync::Mutex<()>>;

pub struct ProducerBuilder<
    Broker = Missing,
    Ctx = Missing,
    Identity = Missing,
    Dedup = Missing,
    Topics = Missing,
    Patterns = Missing,
> {
    broker: Option<Arc<dyn EventBrokerApi>>,
    ctx: Option<SecurityContext>,
    identity: Option<ProducerIdentity>,
    deduplication: Option<DirectDeduplication>,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    validation_timing: ValidationTiming,
    _state: ProducerBuilderState<Broker, Ctx, Identity, Dedup, Topics, Patterns>,
}

impl ProducerBuilder {
    /// Creates a builder with no broker, context or identity set.
    pub(crate) fn new() -> Self {
        Self {
            broker: None,
            ctx: None,
            identity: None,
            deduplication: None,
            topics: Vec::new(),
            event_type_patterns: Vec::new(),
            validation_timing: ValidationTiming::Eager,
            _state: PhantomData,
        }
    }
}

impl<B, C, I, D, T, P> ProducerBuilder<B, C, I, D, T, P> {
    #[must_use]
    pub fn broker(self, broker: Arc<dyn EventBrokerApi>) -> ProducerBuilder<Has, C, I, D, T, P> {
        ProducerBuilder {
            broker: Some(broker),
            ctx: self.ctx,
            identity: self.identity,
            deduplication: self.deduplication,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            _state: PhantomData,
        }
    }

    #[must_use]
    pub fn security_context(self, ctx: SecurityContext) -> ProducerBuilder<B, Has, I, D, T, P> {
        ProducerBuilder {
            broker: self.broker,
            ctx: Some(ctx),
            identity: self.identity,
            deduplication: self.deduplication,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            _state: PhantomData,
        }
    }

    #[must_use]
    pub fn identity(self, identity: ProducerIdentity) -> ProducerBuilder<B, C, Has, D, T, P> {
        ProducerBuilder {
            broker: self.broker,
            ctx: self.ctx,
            identity: Some(identity),
            deduplication: self.deduplication,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            _state: PhantomData,
        }
    }

    #[must_use]
    pub fn deduplication(
        self,
        deduplication: DirectDeduplication,
    ) -> ProducerBuilder<B, C, I, Has, T, P> {
        ProducerBuilder {
            broker: self.broker,
            ctx: self.ctx,
            identity: self.identity,
            deduplication: Some(deduplication),
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            _state: PhantomData,
        }
    }

    #[must_use]
    pub fn topics<S, It>(self, topics: It) -> ProducerBuilder<B, C, I, D, Has, P>
    where
        S: Into<String>,
        It: IntoIterator<Item = S>,
    {
        ProducerBuilder {
            broker: self.broker,
            ctx: self.ctx,
            identity: self.identity,
            deduplication: self.deduplication,
            topics: topics.into_iter().map(Into::into).collect(),
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            _state: PhantomData,
        }
    }

    #[must_use]
    pub fn event_type_patterns<S, It>(self, patterns: It) -> ProducerBuilder<B, C, I, D, T, Has>
    where
        S: Into<String>,
        It: IntoIterator<Item = S>,
    {
        ProducerBuilder {
            broker: self.broker,
            ctx: self.ctx,
            identity: self.identity,
            deduplication: self.deduplication,
            topics: self.topics,
            event_type_patterns: patterns.into_iter().map(Into::into).collect(),
            validation_timing: self.validation_timing,
            _state: PhantomData,
        }
    }

    #[must_use]
    pub fn lazy_validation(mut self) -> Self {
        self.validation_timing = ValidationTiming::Lazy;
        self
    }
}

impl ProducerBuilder<Has, Has, Has, Has, Has, Has> {
    /// Builds the producer and prepares every declared topic and event type
    /// now, whatever the validation timing.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidProducerOptions`] for an empty or blank topic
    /// or event-type pattern list, a blank identity source, or a registering
    /// deduplication in stateless mode; otherwise whatever preparing the
    /// topics and event types, registering the producer, or reading its
    /// cursors fails with.
    pub async fn prepare_all(self) -> Result<Producer, EventBrokerError> {
        self.build_inner(true).await
    }

    /// Builds the producer. Unless [`lazy_validation`](Self::lazy_validation)
    /// was set, every declared topic and event type is prepared now.
    ///
    /// # Errors
    ///
    /// As [`prepare_all`](Self::prepare_all), except that a lazy builder
    /// reports a preparation error on the first publish of the event type.
    pub async fn build(self) -> Result<Producer, EventBrokerError> {
        let prepare_all = matches!(self.validation_timing, ValidationTiming::Eager);
        self.build_inner(prepare_all).await
    }

    async fn build_inner(self, prepare_all: bool) -> Result<Producer, EventBrokerError> {
        let broker = self.broker.expect("typestate requires broker");
        let ctx = self.ctx.expect("typestate requires security context");
        let identity = self.identity.expect("typestate requires identity");
        let deduplication = self
            .deduplication
            .expect("typestate requires deduplication");
        validate_non_empty("topics", &self.topics)?;
        validate_non_empty("event_type_patterns", &self.event_type_patterns)?;
        identity.validate()?;
        deduplication.validate()?;

        let cache = Arc::new(ProducerSchemaCache::new());
        if prepare_all {
            cache
                .prepare_all(&broker, &ctx, &self.topics, &self.event_type_patterns)
                .await?;
        }

        let (producer_id, mode) = match deduplication {
            DirectDeduplication::Stateless => (None, ProducerMode::Stateless),
            DirectDeduplication::RegisterOnStart { mode } => {
                let client_agent = identity.client_agent_ref().map_or_else(
                    || EventBrokerSdk::default_client_agent().to_owned(),
                    str::to_owned,
                );
                let producer_id = broker.register_producer(&ctx, mode, &client_agent).await?;
                (Some(producer_id), mode)
            }
            DirectDeduplication::Reuse { mode, producer_id } => (Some(producer_id), mode),
        };

        let publish_locks = self
            .topics
            .iter()
            .map(|topic| (topic.clone(), tokio::sync::Mutex::new(())))
            .collect();
        let producer = Producer {
            broker,
            ctx,
            identity,
            mode,
            producer_id,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            cache,
            sequence_state: Arc::new(std::sync::RwLock::new(HashMap::new())),
            publish_locks,
        };
        producer.prime_sequence_state().await?;
        Ok(producer)
    }
}

/// Publishes typed events straight to the broker.
///
/// Concurrent publishes on one `Producer` are serialized per topic, so each
/// one is stamped after the previous one on its topic was answered; different
/// topics publish in parallel. Stateless producers stamp nothing and take no
/// lock.
pub struct Producer {
    broker: Arc<dyn EventBrokerApi>,
    ctx: SecurityContext,
    identity: ProducerIdentity,
    mode: ProducerMode,
    producer_id: Option<ProducerId>,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    validation_timing: ValidationTiming,
    cache: Arc<ProducerSchemaCache>,
    sequence_state: SharedProducerSequenceCursor,
    publish_locks: TopicPublishLocks,
}

impl Producer {
    /// Starts building a direct producer.
    #[must_use]
    pub fn builder() -> ProducerBuilder {
        ProducerBuilder::new()
    }

    /// Prepares `E`'s event type and the declared topics, so a lazy
    /// producer's first publish of `E` looks nothing up.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::EventTypeNotDeclared`] if `E` matches no declared
    /// pattern, [`EventBrokerError::TopicNotFound`] for a declared topic the
    /// broker does not list, [`EventBrokerError::TypeNotInDeclaredTopic`] if
    /// `E` belongs to an undeclared topic, [`EventBrokerError::Internal`] if
    /// its data schema does not compile; otherwise whatever the broker's
    /// lookups fail with.
    pub async fn prepare<E: TypedEvent>(&self) -> Result<(), EventBrokerError> {
        self.cache
            .prepare_one(
                &self.broker,
                &self.ctx,
                &self.topics,
                &self.event_type_patterns,
                E::TYPE_ID,
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
    pub async fn prepare_all(&self) -> Result<(), EventBrokerError> {
        self.cache
            .prepare_all(
                &self.broker,
                &self.ctx,
                &self.topics,
                &self.event_type_patterns,
            )
            .await
    }

    /// The broker-registered id stamped on this producer's events; `None` for
    /// a stateless producer.
    pub fn producer_id(&self) -> Option<ProducerId> {
        self.producer_id
    }

    /// Publishes `event` through [`EventBrokerApi::publish`].
    ///
    /// # Errors
    ///
    /// Returns the error of preparing `event` or of the broker's publish, or
    /// [`EventBrokerError::Internal`] on a broken internal invariant (no
    /// publish lock for the event's declared topic).
    ///
    /// # Panics
    ///
    /// Panics if the producer's sequence-state lock is poisoned.
    pub async fn publish<E: TypedEvent>(
        &self,
        event: E,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.publish_inner(event, false).await
    }

    /// Publishes `event` through [`EventBrokerApi::publish_sync`], which holds
    /// the response until the backend stores the event (`Persisted`) or the
    /// broker's `Prefer: wait` ceiling elapses (`Accepted`); a duplicate is
    /// `Duplicate`. Both the REST client (via `Prefer: wait=N`) and the
    /// in-process `LocalBroker` implement it.
    ///
    /// # Errors
    ///
    /// Returns the error of preparing `event` or of the broker's publish, or
    /// [`EventBrokerError::Internal`] on a broken internal invariant (no
    /// publish lock for the event's declared topic).
    ///
    /// # Panics
    ///
    /// Panics if the producer's sequence-state lock is poisoned.
    pub async fn publish_persisted<E: TypedEvent>(
        &self,
        event: E,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.publish_inner(event, true).await
    }

    /// Publishes `events` as one all-or-nothing batch through
    /// [`EventBrokerApi::publish_batch`]. An empty `events` publishes nothing
    /// and returns no outcomes.
    ///
    /// # Errors
    ///
    /// Returns the error of preparing an event or of the broker's publish, or
    /// [`EventBrokerError::Internal`] on a broken internal invariant (no
    /// publish lock for an event's declared topic).
    ///
    /// # Panics
    ///
    /// Panics if the producer's sequence-state lock is poisoned.
    pub async fn publish_batch<E: TypedEvent>(
        &self,
        events: Vec<E>,
    ) -> Result<Vec<IngestOutcome>, EventBrokerError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let mut prepared = Vec::with_capacity(events.len());
        for event in events {
            prepared.push(self.prepare_wire_event(event).await?);
        }
        let mut publish_guards = Vec::new();
        if self.producer_id.is_some() {
            let topics = prepared
                .iter()
                .map(|prepared| prepared.topic.as_str())
                .collect::<BTreeSet<_>>();
            for topic in topics {
                publish_guards.push(self.publish_lock(topic)?.lock().await);
            }
        }
        let mut batch_sequence_state = self
            .sequence_state
            .read()
            .expect("producer sequence state lock poisoned")
            .clone();
        for prepared in &mut prepared {
            prepared.event.meta =
                self.producer_meta_from_cursor(&batch_sequence_state, &prepared.topic);
            advance_cursor(
                &mut batch_sequence_state,
                &prepared.topic,
                prepared.event.meta.as_ref(),
            );
        }
        let wire_events = prepared
            .iter()
            .map(|prepared| prepared.event.clone())
            .collect::<Vec<_>>();
        // A batch is all-or-nothing: its one outcome means the broker holds
        // every event, so each one advances the local chain.
        let outcome = self.broker.publish_batch(&self.ctx, &wire_events).await?;
        for prepared in &prepared {
            self.advance_after_acceptance(prepared, outcome).await;
        }
        Ok(vec![outcome; prepared.len()])
    }

    /// Resets this producer's chain on the broker, over `scope`.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] if called on a stateless producer, a
    /// caller error since it has no chain; otherwise whatever the broker's
    /// reset fails with.
    pub async fn reset_chain(&self, scope: ResetScope<'_>) -> Result<(), EventBrokerError> {
        let producer_id = self.producer_id.ok_or_else(|| {
            EventBrokerError::Internal("reset_chain called on a stateless producer".to_owned())
        })?;
        self.broker
            .reset_producer_chain(&self.ctx, producer_id, scope)
            .await
    }

    async fn publish_inner<E: TypedEvent>(
        &self,
        event: E,
        persisted: bool,
    ) -> Result<IngestOutcome, EventBrokerError> {
        let mut prepared = self.prepare_wire_event(event).await?;
        let _publish_guard = match self.producer_id {
            Some(_) => Some(self.publish_lock(&prepared.topic)?.lock().await),
            None => None,
        };
        prepared.event.meta = self.producer_meta(&prepared.topic);
        let outcome = if persisted {
            self.broker.publish_sync(&self.ctx, &prepared.event).await?
        } else {
            self.broker.publish(&self.ctx, &prepared.event).await?
        };
        self.advance_after_acceptance(&prepared, outcome).await;
        Ok(outcome)
    }

    /// Prepares the event unstamped: its producer metadata is set only under
    /// the topic's publish lock.
    async fn prepare_wire_event<E: TypedEvent>(
        &self,
        event: E,
    ) -> Result<super::event_factory::PreparedEvent, EventBrokerError> {
        self.cache
            .ensure_declared(&self.event_type_patterns, E::TYPE_ID)
            .await?;
        if matches!(self.validation_timing, ValidationTiming::Lazy)
            && !self.cache.is_prepared(E::TYPE_ID).await
        {
            self.prepare::<E>().await?;
        }
        prepare_event(&self.cache, &self.identity, &self.ctx, event).await
    }

    /// A miss is an internal fault: preparing an event already checked that its
    /// topic is declared, and every declared topic has a lock.
    fn publish_lock(&self, topic: &str) -> Result<&tokio::sync::Mutex<()>, EventBrokerError> {
        self.publish_locks.get(topic).ok_or_else(|| {
            EventBrokerError::Internal(format!("no publish lock for the declared topic {topic}"))
        })
    }

    fn producer_meta(&self, topic: &str) -> Option<ProducerMeta> {
        let sequence_state = self
            .sequence_state
            .read()
            .expect("producer sequence state lock poisoned");
        self.producer_meta_from_cursor(&sequence_state, topic)
    }

    fn producer_meta_from_cursor(
        &self,
        cursor: &ProducerSequenceCursor,
        topic: &str,
    ) -> Option<ProducerMeta> {
        let producer_id = self.producer_id?;
        let last = cursor
            .get(&(producer_id, topic.to_owned()))
            .copied()
            .unwrap_or(-1);
        producer_meta_for_last(self.mode, producer_id, last)
    }

    /// Moves the local chain up to `prepared`'s sequence once the broker
    /// holds the event. Every outcome means it does: a `Duplicate` says the
    /// broker already holds this sequence or a later one, so staying behind
    /// would re-stamp every following event with these numbers and have each
    /// dropped as one more duplicate. The caller holds the topic's publish
    /// lock, so `prepared` carries the head's next sequence; the state only
    /// ever moves forward.
    pub(super) async fn advance_after_acceptance(
        &self,
        prepared: &super::event_factory::PreparedEvent,
        outcome: IngestOutcome,
    ) {
        // Irrefutable, so a new outcome has to decide whether it advances.
        let (IngestOutcome::Accepted | IngestOutcome::Persisted | IngestOutcome::Duplicate) =
            outcome;
        let Some(meta) = &prepared.event.meta else {
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
        let Some(producer_id) = self.producer_id else {
            return Ok(());
        };
        let cursors = self
            .broker
            .get_producer_cursors(&self.ctx, producer_id)
            .await?;
        self.sequence_state
            .write()
            .expect("producer sequence state lock poisoned")
            .extend(cursors.topics.into_iter().map(|topic| {
                (
                    (producer_id, topic.topic.into_string()),
                    topic.last_sequence,
                )
            }));
        Ok(())
    }
}

pub(super) fn advance_cursor(
    cursor: &mut ProducerSequenceCursor,
    topic: &str,
    meta: Option<&ProducerMeta>,
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
) -> Option<ProducerMeta> {
    match mode {
        ProducerMode::Stateless => None,
        ProducerMode::Monotonic => Some(ProducerMeta {
            version: 1,
            producer_id: Some(producer_id.0),
            previous: None,
            sequence: Some(last + 1),
        }),
        ProducerMode::Chained => Some(ProducerMeta {
            version: 1,
            producer_id: Some(producer_id.0),
            previous: Some(last),
            sequence: Some(last + 1),
        }),
    }
}

fn validate_non_empty(field: &'static str, values: &[String]) -> Result<(), EventBrokerError> {
    if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
        Err(EventBrokerError::InvalidProducerOptions {
            detail: format!("{field} must contain at least one non-empty entry"),
        })
    } else {
        Ok(())
    }
}
