//! The outbox producer: events are written to a toolkit-db outbox queue
//! inside the caller's transaction and published to the broker when they
//! drain.
//!
//! An [`OutboxSetup`] is built once, prepares the declared event types (unless
//! lazy), and is consumed by [`OutboxSetup::start`], which starts an outbox
//! holding only its queue, or by [`OutboxSetup::register`], which adds its
//! queue to an outbox the gear starts itself. Either way one setup has one
//! drain processor. The [`OutboxProducer`] it yields enqueues inside the
//! caller's transaction and makes no broker call there.

use std::marker::PhantomData;
use std::sync::Arc;

use gts::GtsTypeId;
#[cfg(any(test, feature = "test-util"))]
use toolkit_db::outbox::MessageResult;
use toolkit_db::outbox::{
    LeasedQueueBuilder, Outbox, OutboxBuilder, OutboxError, OutboxHandle, Partitions, Record,
    Records, Wake,
};
use toolkit_db::secure::DBRunner;
use toolkit_security::SecurityContext;

use crate::api::EventBrokerApi;
use crate::error::{EventBrokerError, json_error_category};
use crate::gts::{EventPayload, EventV1};
#[cfg(any(test, feature = "test-util"))]
use crate::ids::ProducerId;
use crate::models::ResetScope;

use super::db::DbProducer;
use super::direct::{Has, Missing, validate_non_empty};
#[cfg(any(test, feature = "test-util"))]
use super::outbox::TEST_QUEUE;
use super::outbox::{PRODUCER_OUTBOX_PAYLOAD_TYPE, ProducerOutboxProcessor};
use super::registration::MAX_NAME_LEN;
use super::types::OutboxRegistration;

type OutboxSetupBuilderState<Broker, Db, Ctx, Source, Registration, Topics, Patterns, Queue> =
    PhantomData<(
        Broker,
        Db,
        Ctx,
        Source,
        Registration,
        Topics,
        Patterns,
        Queue,
    )>;

/// Builds an [`OutboxSetup`]. `broker`, `db`, `security_context`, `source`,
/// `registration`, `topics`, `event_type_patterns` and `queue` are required;
/// `build` exists only once all eight are set.
pub struct OutboxSetupBuilder<
    Broker = Missing,
    Db = Missing,
    Ctx = Missing,
    Source = Missing,
    Registration = Missing,
    Topics = Missing,
    Patterns = Missing,
    Queue = Missing,
> {
    broker: Option<Arc<dyn EventBrokerApi>>,
    db: Option<toolkit_db::Db>,
    ctx: Option<SecurityContext>,
    source: Option<String>,
    registration: Option<OutboxRegistration>,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    queue: Option<String>,
    partitions: Option<Partitions>,
    client_agent: Option<String>,
    lazy_schema_loading: bool,
    _state: OutboxSetupBuilderState<Broker, Db, Ctx, Source, Registration, Topics, Patterns, Queue>,
}

impl OutboxSetupBuilder {
    /// Creates a builder with nothing set.
    fn new() -> Self {
        Self {
            broker: None,
            db: None,
            ctx: None,
            source: None,
            registration: None,
            topics: Vec::new(),
            event_type_patterns: Vec::new(),
            queue: None,
            partitions: None,
            client_agent: None,
            lazy_schema_loading: false,
            _state: PhantomData,
        }
    }
}

impl<B, D, C, S, R, T, P, Q> OutboxSetupBuilder<B, D, C, S, R, T, P, Q> {
    /// The broker the producer prepares its types with and drains to.
    #[must_use]
    pub fn broker(
        mut self,
        broker: Arc<dyn EventBrokerApi>,
    ) -> OutboxSetupBuilder<Has, D, C, S, R, T, P, Q> {
        self.broker = Some(broker);
        self.into_state()
    }

    /// The database holding the outbox tables and the producer registrations.
    #[must_use]
    pub fn db(mut self, db: toolkit_db::Db) -> OutboxSetupBuilder<B, Has, C, S, R, T, P, Q> {
        self.db = Some(db);
        self.into_state()
    }

    /// The security context the producer's broker calls run under.
    #[must_use]
    pub fn security_context(
        mut self,
        ctx: SecurityContext,
    ) -> OutboxSetupBuilder<B, D, Has, S, R, T, P, Q> {
        self.ctx = Some(ctx);
        self.into_state()
    }

    /// The `source` stamped on every event this producer enqueues.
    #[must_use]
    pub fn source(
        mut self,
        source: impl Into<String>,
    ) -> OutboxSetupBuilder<B, D, C, Has, R, T, P, Q> {
        self.source = Some(source.into());
        self.into_state()
    }

    /// How the producer is known to the broker.
    #[must_use]
    pub fn registration(
        mut self,
        registration: OutboxRegistration,
    ) -> OutboxSetupBuilder<B, D, C, S, Has, T, P, Q> {
        self.registration = Some(registration);
        self.into_state()
    }

    /// The topics the producer may enqueue to.
    #[must_use]
    pub fn topics<I, It>(mut self, topics: It) -> OutboxSetupBuilder<B, D, C, S, R, Has, P, Q>
    where
        I: Into<String>,
        It: IntoIterator<Item = I>,
    {
        self.topics = topics.into_iter().map(Into::into).collect();
        self.into_state()
    }

    /// The GTS patterns of the event types the producer may enqueue.
    #[must_use]
    pub fn event_type_patterns<I, It>(
        mut self,
        patterns: It,
    ) -> OutboxSetupBuilder<B, D, C, S, R, T, Has, Q>
    where
        I: Into<String>,
        It: IntoIterator<Item = I>,
    {
        self.event_type_patterns = patterns.into_iter().map(Into::into).collect();
        self.into_state()
    }

    /// The outbox queue the producer's events are written to and drained
    /// from, over `partitions` outbox partitions.
    #[must_use]
    pub fn queue(
        mut self,
        queue: impl Into<String>,
        partitions: Partitions,
    ) -> OutboxSetupBuilder<B, D, C, S, R, T, P, Has> {
        self.queue = Some(queue.into());
        self.partitions = Some(partitions);
        self.into_state()
    }

    /// The client agent the producer registers with, checked by `build`; the
    /// SDK's default unless set.
    #[must_use]
    pub fn client_agent(mut self, client_agent: impl Into<String>) -> Self {
        self.client_agent = Some(client_agent.into());
        self
    }

    /// Skips preparing the event types at build. The caller then prepares
    /// each type with `prepare_event_type`, outside any transaction, before
    /// its first enqueue; an enqueue of an unprepared type fails with
    /// [`EventBrokerError::SchemaNotPrepared`].
    #[must_use]
    pub fn lazy_schema_loading(mut self) -> Self {
        self.lazy_schema_loading = true;
        self
    }

    fn into_state<B2, D2, C2, S2, R2, T2, P2, Q2>(
        self,
    ) -> OutboxSetupBuilder<B2, D2, C2, S2, R2, T2, P2, Q2> {
        OutboxSetupBuilder {
            broker: self.broker,
            db: self.db,
            ctx: self.ctx,
            source: self.source,
            registration: self.registration,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            queue: self.queue,
            partitions: self.partitions,
            client_agent: self.client_agent,
            lazy_schema_loading: self.lazy_schema_loading,
            _state: PhantomData,
        }
    }

    /// Refuses a blank queue name, one with leading or trailing whitespace,
    /// one longer than the registration row holds, and then any name the
    /// outbox refuses, with the outbox's own reason.
    ///
    /// The outbox's rule is applied through its own batch builder, so it
    /// cannot drift from the check that `OutboxBuilder::start` and every
    /// enqueue run. Its length and emptiness reasons cannot fire here, since
    /// the checks before it refuse those names first, and any other outbox
    /// error is a broken invariant; neither is tested.
    fn check_queue(queue: &str) -> Result<(), EventBrokerError> {
        let refused = |detail: String| EventBrokerError::InvalidProducerOptions { detail };
        if queue.trim().is_empty() {
            return Err(refused(
                "producer outbox queue name must not be empty".to_owned(),
            ));
        }
        if queue != queue.trim() {
            return Err(refused(
                "producer outbox queue name must not have leading or trailing whitespace"
                    .to_owned(),
            ));
        }
        if queue.len() > MAX_NAME_LEN {
            return Err(refused(format!(
                "producer outbox queue name must be at most {MAX_NAME_LEN} bytes"
            )));
        }
        // An empty untraced batch validates only the queue name.
        match Records::to(queue)
            .payload_type(PRODUCER_OUTBOX_PAYLOAD_TYPE)
            .build()
        {
            Ok(_) => Ok(()),
            Err(OutboxError::InvalidQueueName { reason }) => {
                Err(refused(format!("producer outbox queue name {reason}")))
            }
            // Only on a broken invariant, given the batch above.
            Err(other) => Err(EventBrokerError::from(other)),
        }
    }
}

impl OutboxSetupBuilder<Has, Has, Has, Has, Has, Has, Has, Has> {
    /// Checks every option, then, unless
    /// [`lazy_schema_loading`](Self::lazy_schema_loading) was set, prepares
    /// every declared topic and event type, and for a managed registration
    /// reads the key's stored registrations, which must match its mode and
    /// client agent.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidProducerOptions`] on the first failing
    /// check, in this order: an empty or blank `topics` or
    /// `event_type_patterns` list; a blank `source`, or one that breaks
    /// [`crate::validate::source`]; a set `client_agent` that breaks
    /// [`crate::validate::client_agent`] (the SDK default, used when none is
    /// set, is a constant and is not re-checked); a managed registration in
    /// `Stateless` mode, or with a blank key, a key with leading or trailing
    /// whitespace, or a key longer than 255 bytes; a blank queue name, one
    /// with leading or trailing whitespace, or one longer than 255 bytes; a
    /// queue name the outbox refuses (only ASCII alphanumerics, `.`, `_` and
    /// `-`, starting and ending with an alphanumeric), with the outbox's
    /// reason; a stored registration of the key whose mode or client agent
    /// differs. [`EventBrokerError::Internal`] only on a broken internal
    /// invariant of the queue-name check. Otherwise whatever preparing the
    /// topics and event types, or reading the stored registrations, fails
    /// with.
    pub async fn build(self) -> Result<OutboxSetup, EventBrokerError> {
        let broker = self.broker.expect("typestate requires broker");
        let db = self.db.expect("typestate requires db");
        let ctx = self.ctx.expect("typestate requires security context");
        let source = self.source.expect("typestate requires source");
        let registration = self.registration.expect("typestate requires registration");
        let queue = self.queue.expect("typestate requires queue");
        let partitions = self
            .partitions
            .expect("typestate requires queue partitions");
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
        if let Some(client_agent) = &self.client_agent {
            crate::validate::client_agent(client_agent).map_err(|err| {
                EventBrokerError::InvalidProducerOptions {
                    detail: format!("client_agent {}", err.detail()),
                }
            })?;
        }
        if let OutboxRegistration::Managed(managed) = &registration {
            managed.check()?;
        }
        Self::check_queue(&queue)?;

        let mut producer = DbProducer::builder()
            .broker(broker)
            .db(db)
            .security_context(ctx)
            .source(source)
            .registration(registration)
            .topics(self.topics)
            .event_type_patterns(self.event_type_patterns);
        if let Some(client_agent) = self.client_agent {
            producer = producer.client_agent(client_agent);
        }
        let producer = if self.lazy_schema_loading {
            producer.lazy_validation().build().await?
        } else {
            producer.build().await?
        };
        Ok(OutboxSetup {
            producer,
            queue,
            partitions,
        })
    }
}

// Prints the fields, the broker and the database only as set or unset and the
// security context only as its subject id.
impl<B, D, C, S, R, T, P, Q> std::fmt::Debug for OutboxSetupBuilder<B, D, C, S, R, T, P, Q> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = |is_set: bool| if is_set { "set" } else { "unset" };
        f.debug_struct("OutboxSetupBuilder")
            .field("broker", &format_args!("{}", set(self.broker.is_some())))
            .field("db", &format_args!("{}", set(self.db.is_some())))
            .field(
                "security_context",
                &self.ctx.as_ref().map(SecurityContext::subject_id),
            )
            .field("source", &self.source)
            .field("registration", &self.registration)
            .field("topics", &self.topics)
            .field("event_type_patterns", &self.event_type_patterns)
            .field("queue", &self.queue)
            .field("partitions", &self.partitions.map(Partitions::count))
            .field("client_agent", &self.client_agent)
            .field("lazy_schema_loading", &self.lazy_schema_loading)
            .finish()
    }
}

/// A built outbox producer before its outbox runs. Consumed by
/// [`start`](Self::start) or [`register`](Self::register), so one setup has
/// one drain processor.
pub struct OutboxSetup {
    producer: DbProducer,
    queue: String,
    partitions: Partitions,
}

// Prints no security context, broker, database or schema cache.
impl std::fmt::Debug for OutboxSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxSetup")
            .field("queue", &self.queue)
            .field("partitions", &self.partitions.count())
            .field("mode", &self.producer.registration.mode())
            .finish_non_exhaustive()
    }
}

impl OutboxSetup {
    /// Starts building an outbox setup.
    #[must_use]
    pub fn builder() -> OutboxSetupBuilder {
        OutboxSetupBuilder::new()
    }

    /// Prepares the event type `type_id` and the declared topics, so a lazy
    /// setup's producer can enqueue that type. Makes broker calls: call it
    /// outside any transaction. The producer this setup starts shares what it
    /// prepared.
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
        self.producer.prepare_event_type(type_id).await
    }

    /// Prepares every event type the declared patterns match, and the
    /// declared topics. Makes broker calls: call it outside any transaction.
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
        self.producer.prepare_all().await
    }

    /// Adds this setup's queue and drain processor to `builder`, starts it,
    /// and binds the producer to the started outbox.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] with the outbox's error text when the
    /// outbox does not start, for example when registering its queues in the
    /// database fails or a queue's stored partition count differs.
    pub async fn start(
        self,
        builder: OutboxBuilder,
    ) -> Result<OutboxProducerHandle, EventBrokerError> {
        let (queue_builder, binder) = self.register(builder);
        let handle = queue_builder.start().await?;
        let producer = binder.bind(&handle);
        Ok(OutboxProducerHandle { handle, producer })
    }

    /// Adds this setup's queue and drain processor to `builder`, for a gear
    /// that starts the outbox itself with queues of its own. Start the
    /// returned builder, then bind the producer to its handle with the
    /// returned [`OutboxBinder`].
    pub fn register(self, builder: OutboxBuilder) -> (LeasedQueueBuilder, OutboxBinder) {
        let processor = ProducerOutboxProcessor::new(self.producer.clone(), self.queue.clone());
        let queue_builder = builder
            .queue(&self.queue, self.partitions)
            .leased(processor);
        let binder = OutboxBinder {
            producer: self.producer,
            queue: self.queue,
            partitions: self.partitions,
        };
        (queue_builder, binder)
    }
}

/// Hooks the SDK's own integration tests reach the drain through, with no
/// outbox running. Not part of the API: built only for this crate's tests and
/// under the `test-util` feature.
#[cfg(any(test, feature = "test-util"))]
impl OutboxSetup {
    /// The producer id messages of `partition` of `queue` publish under,
    /// registering it now if the partition has none yet, as its first drain
    /// would.
    ///
    /// # Errors
    ///
    /// The queue-name errors of [`OutboxSetupBuilder::build`] if `queue` is
    /// not a valid outbox queue name; [`EventBrokerError::Internal`] if the
    /// registration is stateless or the registration task panics; otherwise
    /// whatever the broker or the registration store fail with.
    #[doc(hidden)]
    pub async fn partition_producer_for_test(
        &self,
        queue: &str,
        partition: i64,
    ) -> Result<ProducerId, EventBrokerError> {
        <OutboxSetupBuilder>::check_queue(queue)?;
        self.producer.partition_producer(queue, partition).await
    }

    /// Every stored partition registration of the managed registration's
    /// key, as `(queue, partition, producer id, generation)`, ordered by queue
    /// and partition.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] if the registration is stateless;
    /// whatever the registration store fails with.
    #[doc(hidden)]
    pub async fn registrations_for_test(
        &self,
    ) -> Result<Vec<(String, i64, ProducerId, i64)>, EventBrokerError> {
        self.producer.stored_partition_registrations().await
    }

    /// The outbox partition, of this setup's partitions, and the envelope
    /// [`OutboxProducer::enqueue`] would write for `event`, as JSON: its
    /// checks and conversion, up to the write.
    ///
    /// # Errors
    ///
    /// The errors of [`OutboxProducer::enqueue`] that come before its
    /// payload-limit check; [`EventBrokerError::Internal`] if the envelope
    /// does not serialize, a broken internal invariant.
    #[doc(hidden)]
    pub async fn outbox_envelope_for_test<P: EventPayload>(
        &self,
        event: EventV1<P>,
    ) -> Result<(u32, serde_json::Value), EventBrokerError> {
        OutboxProducer::check_input(&event)?;
        let (partition, envelope) = self
            .producer
            .build_outbox_envelope(&event, u32::from(self.partitions.count()))
            .await?;
        let envelope = serde_json::to_value(&envelope).map_err(|err| {
            EventBrokerError::Internal(format!(
                "serialize producer outbox envelope ({})",
                json_error_category(&err)
            ))
        })?;
        Ok((partition, envelope))
    }

    /// Drains `payload` as message `seq` of partition id 0 of [`TEST_QUEUE`]
    /// through a fresh drain processor.
    #[doc(hidden)]
    pub async fn process_outbox_payload_for_test(
        &self,
        payload: Vec<u8>,
        seq: i64,
    ) -> MessageResult {
        ProducerOutboxProcessor::new(self.producer.clone(), TEST_QUEUE.to_owned())
            .handle_test_payload(payload, seq)
            .await
    }

    /// As [`process_outbox_payload_for_test`](Self::process_outbox_payload_for_test),
    /// with the chained cursor of the envelope's topic seeded to
    /// `initial_chained_previous` for partition 0's producer id.
    #[doc(hidden)]
    pub async fn process_outbox_payload_with_cursor_for_test(
        &self,
        payload: Vec<u8>,
        seq: i64,
        initial_chained_previous: i64,
    ) -> MessageResult {
        let processor = ProducerOutboxProcessor::new(self.producer.clone(), TEST_QUEUE.to_owned());
        processor
            .seed_test_cursor(&payload, initial_chained_previous)
            .await;
        processor.handle_test_payload(payload, seq).await
    }
}

/// Binds the producer of a registered setup to the outbox the gear started.
pub struct OutboxBinder {
    producer: DbProducer,
    queue: String,
    partitions: Partitions,
}

// Prints no security context, broker, database or schema cache.
impl std::fmt::Debug for OutboxBinder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxBinder")
            .field("queue", &self.queue)
            .field("partitions", &self.partitions.count())
            .field("mode", &self.producer.registration.mode())
            .finish_non_exhaustive()
    }
}

impl OutboxBinder {
    /// Binds the producer to `handle`, which must be the handle started from
    /// the `LeasedQueueBuilder` that [`OutboxSetup::register`] returned. A
    /// handle whose outbox has no queue of this name, or has it with fewer
    /// partitions than an event's partition, fails that event's enqueue with
    /// [`EventBrokerError::OutboxQueueMismatch`] and writes nothing. A handle
    /// from another outbox that has a queue of this name with enough
    /// partitions accepts the rows into its tables, so bind only that handle.
    #[must_use]
    pub fn bind(self, handle: &OutboxHandle) -> OutboxProducer {
        OutboxProducer {
            producer: self.producer,
            outbox: Arc::clone(handle.outbox()),
            queue: self.queue,
            partitions: u32::from(self.partitions.count()),
        }
    }
}

/// The running outbox a setup started, and its producer.
pub struct OutboxProducerHandle {
    handle: OutboxHandle,
    producer: OutboxProducer,
}

// Prints no security context, broker, database, schema cache or outbox.
impl std::fmt::Debug for OutboxProducerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxProducerHandle")
            .field("queue", &self.producer.queue)
            .field("partitions", &self.producer.partitions)
            .field("mode", &self.producer.producer.registration.mode())
            .finish_non_exhaustive()
    }
}

impl OutboxProducerHandle {
    /// The producer bound to this outbox.
    #[must_use]
    pub fn producer(&self) -> &OutboxProducer {
        &self.producer
    }

    /// Stops the outbox's workers.
    pub async fn stop(self) {
        self.handle.stop().await;
    }
}

/// Enqueues events into its outbox queue inside the caller's transaction;
/// the outbox drains them to the broker. Clones share the schema cache and
/// the outbox.
#[derive(Clone)]
pub struct OutboxProducer {
    producer: DbProducer,
    outbox: Arc<Outbox>,
    queue: String,
    partitions: u32,
}

// Prints no security context, broker, database, schema cache or outbox.
impl std::fmt::Debug for OutboxProducer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxProducer")
            .field("queue", &self.queue)
            .field("partitions", &self.partitions)
            .field("mode", &self.producer.registration.mode())
            .finish_non_exhaustive()
    }
}

impl OutboxProducer {
    /// Writes `event` to the outbox queue within `tx`, stamped with the
    /// setup's source. Makes no broker call: inside `tx` it runs only CPU
    /// work (the checks, the schema check against the cached schema) and the
    /// outbox's insert statements. On a lazy setup an unprepared type fails
    /// with [`EventBrokerError::SchemaNotPrepared`]; call
    /// [`prepare_event_type`](Self::prepare_event_type) before opening the
    /// transaction.
    ///
    /// The write does not wake the outbox: call
    /// [`Wake::fire`](toolkit_db::outbox::Wake::fire) on the returned handle
    /// once `tx` has committed, and call
    /// [`Wake::discard`](toolkit_db::outbox::Wake::discard) on rollback. An
    /// unfired event is durable and drains when the outbox's reconciler finds
    /// it.
    ///
    /// # Errors
    ///
    /// In this order; only a database error comes after a write has started:
    /// [`EventBrokerError::InvalidEventField`] for an event the producer does
    /// not take ([`EventV1::check_producer_input`]: a broker-stamped member,
    /// `meta` or `source` set); [`EventBrokerError::InvalidTextField`] or
    /// [`EventBrokerError::InvalidEventField`] for a decoded event whose
    /// `subject` or `trace_parent` breaks a builder rule;
    /// [`EventBrokerError::Internal`] when `data` does not serialize;
    /// [`EventBrokerError::EventTypeNotDeclared`] when the type matches no
    /// declared pattern; [`EventBrokerError::SchemaNotPrepared`] as above;
    /// [`EventBrokerError::EventDataInvalid`] when `data` breaks the type's
    /// schema; [`EventBrokerError::TopicNotFound`] when the type's topic is
    /// not prepared; the error of resolving the partition key;
    /// [`EventBrokerError::Internal`] with the outbox's size text for an
    /// envelope over its payload limit (64 KiB, `toolkit_db::outbox`
    /// `MAX_PAYLOAD_SIZE`);
    /// [`EventBrokerError::OutboxQueueMismatch`] when the bound outbox has no
    /// queue of this name with the event's partition; otherwise
    /// [`EventBrokerError::Internal`] with the outbox's or the database's
    /// error text. An envelope without a source, or one that does not
    /// serialize, is also [`EventBrokerError::Internal`]; only a broken
    /// internal invariant reaches either: the source is stamped just before,
    /// and every envelope member serializes.
    pub async fn enqueue<P: EventPayload>(
        &self,
        tx: &(impl DBRunner + Sync + ?Sized),
        event: EventV1<P>,
    ) -> Result<Wake, EventBrokerError> {
        Self::check_input(&event)?;
        let (partition, payload) = self.outbox_payload(&event).await?;
        let record = Record::to(&self.queue, partition)
            .payload(payload, PRODUCER_OUTBOX_PAYLOAD_TYPE)
            .build()?;
        Ok(self.outbox.enqueue(tx, record).await?)
    }

    /// Writes `events` to the outbox queue within `tx` as one batch, each
    /// stamped as [`enqueue`](Self::enqueue) stamps one, with the same
    /// guarantees: no broker call, and nothing remote inside `tx`. Every event
    /// is checked before any is converted, and all are converted before
    /// anything is written. All or nothing: one failing event writes none.
    /// Fire the returned [`Wake`] once `tx` has committed; discard it on
    /// rollback.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidEventField`] for an empty `events`;
    /// otherwise the errors of [`enqueue`](Self::enqueue) in the same order,
    /// stage by stage across the batch: every event's input checks, then
    /// every event's conversion, then every envelope's payload limit, then
    /// the write. Within a stage, the first event that fails decides.
    pub async fn enqueue_batch<P: EventPayload>(
        &self,
        tx: &(impl DBRunner + Sync + ?Sized),
        events: Vec<EventV1<P>>,
    ) -> Result<Wake, EventBrokerError> {
        if events.is_empty() {
            return Err(EventBrokerError::InvalidEventField {
                field: "events",
                detail: "empty batch".to_owned(),
            });
        }
        for event in &events {
            Self::check_input(event)?;
        }
        // Every event shares the queue and the payload type. No trace is
        // attached: an event is identified by its own id and `trace_parent`.
        let mut batch = Records::to(&self.queue).payload_type(PRODUCER_OUTBOX_PAYLOAD_TYPE);
        for event in &events {
            let (partition, payload) = self.outbox_payload(event).await?;
            batch = batch.push(partition, payload);
        }
        Ok(self.outbox.enqueue_batch(tx, batch.build()?).await?)
    }

    /// Prepares the event type `type_id` and the declared topics, as
    /// [`OutboxSetup::prepare_event_type`] does, in the cache this producer
    /// shares with its setup. Makes broker calls: call it outside any
    /// transaction.
    ///
    /// # Errors
    ///
    /// As [`OutboxSetup::prepare_event_type`].
    pub async fn prepare_event_type(&self, type_id: &GtsTypeId) -> Result<(), EventBrokerError> {
        self.producer.prepare_event_type(type_id).await
    }

    /// Prepares every event type the declared patterns match, and the
    /// declared topics. Makes broker calls: call it outside any transaction.
    ///
    /// # Errors
    ///
    /// As [`OutboxSetup::prepare_all_event_types`].
    pub async fn prepare_all_event_types(&self) -> Result<(), EventBrokerError> {
        self.producer.prepare_all().await
    }

    /// Resets, over `scope`, the broker chain of every partition id stored
    /// under the managed registration's key, each keeping its id.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] on a stateless registration; whatever
    /// the registration store or the broker's reset fail with.
    pub async fn reset_chain(&self, scope: ResetScope<'_>) -> Result<(), EventBrokerError> {
        self.producer.reset_chain(scope).await
    }

    /// Replaces every partition id stored under the managed registration's
    /// key with a newly registered one, under the next generation. Messages
    /// drained after it returns publish under the new ids.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] on a stateless registration, or when a
    /// replace task panics; whatever the broker or the registration store
    /// fail with.
    pub async fn rotate_registration(&self) -> Result<(), EventBrokerError> {
        self.producer.rotate_registration().await
    }

    /// Refuses an event the producer does not take, or whose envelope breaks
    /// a builder rule: a decoded event never passed through the builder.
    /// `source` in `check_envelope` cannot fire here: a set source was just
    /// refused.
    fn check_input<P: EventPayload>(event: &EventV1<P>) -> Result<(), EventBrokerError> {
        event.check_producer_input()?;
        event.check_envelope()
    }

    /// The outbox partition and the serialized envelope of `event`.
    async fn outbox_payload<P: EventPayload>(
        &self,
        event: &EventV1<P>,
    ) -> Result<(u32, Vec<u8>), EventBrokerError> {
        let (partition, envelope) = self
            .producer
            .build_outbox_envelope(event, self.partitions)
            .await?;
        let payload = serde_json::to_vec(&envelope).map_err(|err| {
            EventBrokerError::Internal(format!(
                "serialize producer outbox envelope ({})",
                json_error_category(&err)
            ))
        })?;
        Ok((partition, payload))
    }
}

#[cfg(all(test, feature = "outbox"))]
#[path = "outbox_setup_tests.rs"]
mod tests;
