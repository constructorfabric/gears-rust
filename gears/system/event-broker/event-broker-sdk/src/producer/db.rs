use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, PoisonError};

use gts::GtsTypeId;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use toolkit_security::SecurityContext;
use tracing::Instrument;

use crate::api::{EventBrokerApi, ProducerMode};
use crate::error::EventBrokerError;
use crate::gts::{EventPayload, EventV1};
use crate::ids::ProducerId;
use crate::models::ResetScope;
use crate::sdk::EventBrokerSdk;

use super::direct::{Has, Missing};
use super::event_factory::PreparedEvent;
use super::outbox::ProducerOutboxEnvelope;
use super::registration::{
    NewRegistration, ProducerRegistration, ProducerRegistrationStore, RegistrationSlot, Replaced,
};
use super::schema_cache::ProducerSchemaCache;
use super::types::{OutboxRegistration, ValidationTiming};

/// One slot's registration: set once per process, by whichever drain first
/// resolves the slot, and swapped whole when the slot's id is replaced.
type RegistrationCell = Arc<OnceCell<ProducerRegistration>>;

/// One slot's write lock: held while the slot's id is first stored or
/// replaced, so this process changes a slot one writer at a time.
type SlotLock = Arc<AsyncMutex<()>>;

type DbProducerBuilderState<Broker, Db, Ctx, Source, Registration, Topics, Patterns> =
    PhantomData<(Broker, Db, Ctx, Source, Registration, Topics, Patterns)>;

/// Builds the [`DbProducer`] an [`OutboxSetup`](super::OutboxSetup) runs on.
/// Checks none of its values: `OutboxSetupBuilder::build` checks each one
/// before it builds this.
pub(crate) struct DbProducerBuilder<
    Broker = Missing,
    Db = Missing,
    Ctx = Missing,
    Source = Missing,
    Registration = Missing,
    Topics = Missing,
    Patterns = Missing,
> {
    broker: Option<Arc<dyn EventBrokerApi>>,
    db: Option<toolkit_db::Db>,
    ctx: Option<SecurityContext>,
    source: Option<String>,
    client_agent: String,
    registration: Option<OutboxRegistration>,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    validation_timing: ValidationTiming,
    _state: DbProducerBuilderState<Broker, Db, Ctx, Source, Registration, Topics, Patterns>,
}

impl DbProducerBuilder {
    /// Creates a builder with the SDK's default client agent and every
    /// required part unset.
    fn new() -> Self {
        Self {
            broker: None,
            db: None,
            ctx: None,
            source: None,
            client_agent: EventBrokerSdk::default_client_agent().to_owned(),
            registration: None,
            topics: Vec::new(),
            event_type_patterns: Vec::new(),
            validation_timing: ValidationTiming::Eager,
            _state: PhantomData,
        }
    }
}

impl<B, Db, C, S, R, T, P> DbProducerBuilder<B, Db, C, S, R, T, P> {
    pub(crate) fn broker(
        mut self,
        broker: Arc<dyn EventBrokerApi>,
    ) -> DbProducerBuilder<Has, Db, C, S, R, T, P> {
        self.broker = Some(broker);
        self.into_state()
    }

    pub(crate) fn db(mut self, db: toolkit_db::Db) -> DbProducerBuilder<B, Has, C, S, R, T, P> {
        self.db = Some(db);
        self.into_state()
    }

    pub(crate) fn security_context(
        mut self,
        ctx: SecurityContext,
    ) -> DbProducerBuilder<B, Db, Has, S, R, T, P> {
        self.ctx = Some(ctx);
        self.into_state()
    }

    /// The `source` stamped on every event the producer enqueues.
    pub(crate) fn source(
        mut self,
        source: impl Into<String>,
    ) -> DbProducerBuilder<B, Db, C, Has, R, T, P> {
        self.source = Some(source.into());
        self.into_state()
    }

    /// The client agent partition ids are registered with; the SDK's default
    /// unless set.
    pub(crate) fn client_agent(mut self, client_agent: impl Into<String>) -> Self {
        self.client_agent = client_agent.into();
        self
    }

    pub(crate) fn registration(
        mut self,
        registration: OutboxRegistration,
    ) -> DbProducerBuilder<B, Db, C, S, Has, T, P> {
        self.registration = Some(registration);
        self.into_state()
    }

    pub(crate) fn topics<I, It>(mut self, topics: It) -> DbProducerBuilder<B, Db, C, S, R, Has, P>
    where
        I: Into<String>,
        It: IntoIterator<Item = I>,
    {
        self.topics = topics.into_iter().map(Into::into).collect();
        self.into_state()
    }

    pub(crate) fn event_type_patterns<I, It>(
        mut self,
        patterns: It,
    ) -> DbProducerBuilder<B, Db, C, S, R, T, Has>
    where
        I: Into<String>,
        It: IntoIterator<Item = I>,
    {
        self.event_type_patterns = patterns.into_iter().map(Into::into).collect();
        self.into_state()
    }

    /// Skips preparing the event types at build; an enqueue of an unprepared
    /// type then fails with [`EventBrokerError::SchemaNotPrepared`].
    pub(crate) fn lazy_validation(mut self) -> Self {
        self.validation_timing = ValidationTiming::Lazy;
        self
    }

    fn into_state<B2, Db2, C2, S2, R2, T2, P2>(
        self,
    ) -> DbProducerBuilder<B2, Db2, C2, S2, R2, T2, P2> {
        DbProducerBuilder {
            broker: self.broker,
            db: self.db,
            ctx: self.ctx,
            source: self.source,
            client_agent: self.client_agent,
            registration: self.registration,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            _state: PhantomData,
        }
    }
}

impl DbProducerBuilder<Has, Has, Has, Has, Has, Has, Has> {
    /// Prepares every declared topic and event type unless
    /// [`lazy_validation`](Self::lazy_validation) was set, then, for a
    /// managed registration, reads the key's stored registrations, which must
    /// match its mode and client agent.
    ///
    /// # Errors
    ///
    /// Whatever preparing the topics and event types, or reading the stored
    /// registrations, fails with; [`EventBrokerError::InvalidProducerOptions`]
    /// for a stored registration of the key whose mode or client agent
    /// differs.
    pub(crate) async fn build(self) -> Result<DbProducer, EventBrokerError> {
        let broker = self.broker.expect("typestate requires broker");
        let db = self.db.expect("typestate requires db");
        let ctx = self.ctx.expect("typestate requires security context");
        let source = self.source.expect("typestate requires source");
        let registration = self.registration.expect("typestate requires registration");

        let cache = Arc::new(ProducerSchemaCache::new());
        if self.validation_timing == ValidationTiming::Eager {
            cache
                .prepare_all(&broker, &ctx, &self.topics, &self.event_type_patterns)
                .await?;
        }

        let registration_store = ProducerRegistrationStore::new(db);
        // Partition ids are registered as their partitions first drain; what
        // is already stored has to agree with this configuration, since a
        // producer id's mode never changes (ADR-0004).
        if let OutboxRegistration::Managed(managed) = &registration {
            for stored in registration_store.load_all(managed.key()).await? {
                stored.validate_matches(managed, &self.client_agent)?;
            }
        }

        Ok(DbProducer {
            broker,
            ctx,
            source,
            registration,
            client_agent: self.client_agent,
            registrations: Arc::new(Mutex::new(HashMap::new())),
            slot_locks: Arc::new(Mutex::new(HashMap::new())),
            registration_store,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            cache,
            #[cfg(test)]
            unlock_pause: Arc::default(),
        })
    }
}

/// The core an outbox setup builds on: the schema cache, the partition
/// registrations and the drain's broker calls.
#[derive(Clone)]
pub(crate) struct DbProducer {
    /// Broker the producer publishes to.
    pub(crate) broker: Arc<dyn EventBrokerApi>,
    /// Security context publishes run under.
    pub(crate) ctx: SecurityContext,
    /// The `source` stamped on every enqueued event.
    source: String,
    /// How the producer is known to the broker.
    pub(crate) registration: OutboxRegistration,
    /// Sent when a partition's id is registered on its first drain, and
    /// stamped on every envelope as its diagnostic client agent.
    client_agent: String,
    /// The producer id of each outbox partition drained or replaced so far,
    /// by slot. The lock guards only the map: a slot registers inside its own
    /// cell, so one slot's registration never waits on another's, and a slot
    /// registers at most once in this process unless its row vanishes.
    registrations: Arc<Mutex<HashMap<RegistrationSlot, RegistrationCell>>>,
    /// The write lock of each slot stored or replaced so far. Like
    /// `registrations`, the outer lock guards only the map.
    slot_locks: Arc<Mutex<HashMap<RegistrationSlot, SlotLock>>>,
    registration_store: ProducerRegistrationStore,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    /// A lazy producer refuses to enqueue an event type it has not prepared.
    validation_timing: ValidationTiming,
    cache: Arc<ProducerSchemaCache>,
    /// Holds a first registration between releasing its slot's write lock
    /// and caching what it read, when a test arms it.
    #[cfg(test)]
    unlock_pause: Arc<tests::UnlockPause>,
}

impl DbProducer {
    pub(crate) fn builder() -> DbProducerBuilder {
        DbProducerBuilder::new()
    }

    /// Prepares every event type the declared patterns match, and the
    /// declared topics, as
    /// [`OutboxSetup::prepare_all_event_types`](super::OutboxSetup::prepare_all_event_types)
    /// does, with its errors.
    pub(crate) async fn prepare_all(&self) -> Result<(), EventBrokerError> {
        self.cache
            .prepare_all(
                &self.broker,
                &self.ctx,
                &self.topics,
                &self.event_type_patterns,
            )
            .await
    }

    /// Resets the broker chain of every stored partition id, each keeping its
    /// id.
    pub(crate) async fn reset_chain(&self, scope: ResetScope<'_>) -> Result<(), EventBrokerError> {
        for registration in self.stored_registrations("reset_chain").await? {
            self.broker
                .reset_producer_chain(&self.ctx, registration.producer_id, scope)
                .await?;
        }
        Ok(())
    }

    /// Replaces every stored partition id with a newly registered one, under
    /// the next generation, slot by slot in queue and partition order.
    /// Messages drained after it returns, including ones enqueued before,
    /// publish under the new ids. A drain already holding a retired id may
    /// publish under it once more, which the broker still accepts. A slot
    /// another writer replaced since it was read keeps that writer's id.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] on a stateless producer, or when a
    /// replace task panics; whatever the broker or the registration store
    /// fail with.
    pub(crate) async fn rotate_registration(&self) -> Result<(), EventBrokerError> {
        let mut registrations = self.stored_registrations("rotate_registration").await?;
        registrations.sort_by(|a, b| {
            (&a.slot.queue, a.slot.partition).cmp(&(&b.slot.queue, b.slot.partition))
        });
        for registration in registrations {
            self.replace_rejected(registration.slot, registration.producer_id)
                .await?;
        }
        Ok(())
    }

    /// Every stored partition registration of this producer's key, as
    /// `(queue, partition, producer id, generation)`, ordered by queue and
    /// partition.
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) async fn stored_partition_registrations(
        &self,
    ) -> Result<Vec<(String, i64, ProducerId, i64)>, EventBrokerError> {
        let mut rows: Vec<_> = self
            .stored_registrations("registrations")
            .await?
            .into_iter()
            .map(|r| (r.slot.queue, r.slot.partition, r.producer_id, r.generation))
            .collect();
        rows.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        Ok(rows)
    }

    async fn stored_registrations(
        &self,
        operation: &str,
    ) -> Result<Vec<ProducerRegistration>, EventBrokerError> {
        let OutboxRegistration::Managed(managed) = &self.registration else {
            return Err(EventBrokerError::Internal(format!(
                "{operation} called on a stateless producer"
            )));
        };
        self.registration_store.load_all(managed.key()).await
    }

    /// The producer id messages of `partition` of `queue` publish under:
    /// cached, else stored, else registered with the broker and stored.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] on a stateless producer, or when the
    /// registration task panics; whatever the broker or the registration
    /// store fail with.
    pub(crate) async fn partition_producer(
        &self,
        queue: &str,
        partition: i64,
    ) -> Result<ProducerId, EventBrokerError> {
        let OutboxRegistration::Managed(managed) = &self.registration else {
            return Err(EventBrokerError::Internal(
                "a stateless producer has no producer id".to_owned(),
            ));
        };
        let slot = RegistrationSlot {
            key: managed.key().to_owned(),
            queue: queue.to_owned(),
            partition,
        };
        let cell = self.registration_cell(&slot);
        if let Some(registration) = cell.get() {
            return Ok(registration.producer_id);
        }
        // The registration runs in its own task, so a caller dropped
        // mid-registration (the outbox cancels a handler at its lease
        // deadline) leaves it running through the store insert; the next
        // caller joins the same cell instead of registering a second id.
        let producer = self.clone();
        let mode = managed.mode();
        tokio::spawn(
            async move {
                let mut cell = cell;
                loop {
                    let producer_id = cell
                        .get_or_try_init(|| producer.load_or_register(&slot, mode))
                        .await?
                        .producer_id;
                    // A replace that swapped the slot's cell while this one
                    // filled retired what it read; the current cell holds
                    // the slot's id.
                    let current = producer.registration_cell(&slot);
                    if Arc::ptr_eq(&current, &cell) {
                        return Ok(producer_id);
                    }
                    cell = current;
                }
            }
            .in_current_span(),
        )
        .await
        .map_err(|err| EventBrokerError::Internal(format!("producer registration task: {err}")))?
    }

    /// The stored registration of `slot`, else a new one registered with the
    /// broker and stored. Runs under the slot's write lock, so it reads what
    /// a replace in flight stores rather than the id that replace retires.
    async fn load_or_register(
        &self,
        slot: &RegistrationSlot,
        mode: ProducerMode,
    ) -> Result<ProducerRegistration, EventBrokerError> {
        let lock = self.slot_lock(slot);
        let registration = {
            let _writing = lock.lock().await;
            if let Some(stored) = self.registration_store.load(slot).await? {
                stored
            } else {
                let producer_id = self
                    .broker
                    .register_producer(&self.ctx, mode, &self.client_agent)
                    .await?;
                self.registration_store
                    .insert_new(NewRegistration {
                        slot,
                        producer_id,
                        mode,
                        client_agent: &self.client_agent,
                    })
                    .await?
            }
        };
        #[cfg(test)]
        self.unlock_pause.pause().await;
        Ok(registration)
    }

    /// The cell of `slot`, created empty on first sight.
    fn registration_cell(&self, slot: &RegistrationSlot) -> RegistrationCell {
        self.registrations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(slot.clone())
            .or_default()
            .clone()
    }

    /// The write lock of `slot`, created on first sight.
    fn slot_lock(&self, slot: &RegistrationSlot) -> SlotLock {
        self.slot_locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(slot.clone())
            .or_default()
            .clone()
    }

    /// Caches `registration` as its slot's id, replacing whatever the slot
    /// held. A cell that already holds the same id stays, so a first drain
    /// waiting on it does not retry.
    fn cache_registration(&self, registration: ProducerRegistration) {
        let mut registrations = self
            .registrations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let unchanged = registrations
            .get(&registration.slot)
            .and_then(|cell| cell.get())
            .is_some_and(|cached| cached.producer_id == registration.producer_id);
        if !unchanged {
            let slot = registration.slot.clone();
            registrations.insert(slot, Arc::new(OnceCell::from(registration)));
        }
    }

    /// Drops the cached id of `slot`, so its next drain loads or registers
    /// afresh.
    fn forget_registration(&self, slot: &RegistrationSlot) {
        self.registrations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(slot);
    }

    /// Replaces `rejected`, the id of `partition` of `queue` the broker
    /// answered as unknown, with a newly registered one. A no-op when the
    /// partition already moved on to another id.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] when the replace task panics; whatever
    /// the broker or the registration store fail with.
    pub(crate) async fn replace_unknown_producer(
        &self,
        queue: &str,
        partition: i64,
        rejected: ProducerId,
    ) -> Result<(), EventBrokerError> {
        let OutboxRegistration::Managed(managed) = &self.registration else {
            return Ok(());
        };
        let slot = RegistrationSlot {
            key: managed.key().to_owned(),
            queue: queue.to_owned(),
            partition,
        };
        self.replace_rejected(slot, rejected).await
    }

    /// Moves `slot` off `rejected` to a newly registered id under the next
    /// generation, and caches what the slot then stores. A no-op, apart from
    /// caching the stored id, when the slot holds another id. A slot whose row
    /// is gone, before or during the replace, drops its cached id, so its next
    /// drain loads or registers afresh.
    ///
    /// The replace runs in its own task under the slot's write lock: a caller
    /// dropped mid-replace leaves it running through the store update, and a
    /// second caller in this process waits, then finds the new id and
    /// registers nothing. Across processes the update applies only to the
    /// generation it read; a writer that loses keeps the stored id, and the
    /// id it registered stays unused, since the broker has no unregister.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] when the replace task panics; whatever
    /// the broker or the registration store fail with.
    async fn replace_rejected(
        &self,
        slot: RegistrationSlot,
        rejected: ProducerId,
    ) -> Result<(), EventBrokerError> {
        let producer = self.clone();
        tokio::spawn(
            async move {
                let lock = producer.slot_lock(&slot);
                let _writing = lock.lock().await;
                let Some(current) = producer.registration_store.load(&slot).await? else {
                    tracing::warn!(
                        key = %slot.key,
                        queue = %slot.queue,
                        partition = slot.partition,
                        "the producer registration vanished before its replace; \
                         the slot's next drain loads or registers afresh"
                    );
                    producer.forget_registration(&slot);
                    return Ok(());
                };
                if current.producer_id != rejected {
                    producer.cache_registration(current);
                    return Ok(());
                }
                let producer_id = producer
                    .broker
                    .register_producer(&producer.ctx, current.mode, &current.client_agent)
                    .await?;
                match producer
                    .registration_store
                    .replace_with_new_generation(&current, producer_id)
                    .await?
                {
                    Replaced::Done(next) => producer.cache_registration(next),
                    Replaced::Lost => {
                        tracing::debug!(
                            key = %slot.key,
                            queue = %slot.queue,
                            partition = slot.partition,
                            unused_producer_id = %producer_id.0,
                            "another process replaced the producer registration first; \
                             the newly registered id stays unused"
                        );
                        if let Some(stored) = producer.registration_store.load(&slot).await? {
                            producer.cache_registration(stored);
                        } else {
                            tracing::warn!(
                                key = %slot.key,
                                queue = %slot.queue,
                                partition = slot.partition,
                                "the producer registration vanished during its replace; \
                                 the slot's next drain loads or registers afresh"
                            );
                            producer.forget_registration(&slot);
                        }
                    }
                }
                Ok(())
            }
            .in_current_span(),
        )
        .await
        .map_err(|err| EventBrokerError::Internal(format!("producer replace task: {err}")))?
    }

    /// Prepares the event type `type_id` and the declared topics, as
    /// [`DirectProducer::prepare_event_type`](super::direct::DirectProducer::prepare_event_type)
    /// does, with its errors.
    pub(crate) async fn prepare_event_type(
        &self,
        type_id: &GtsTypeId,
    ) -> Result<(), EventBrokerError> {
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

    /// The outbox partition, of `outbox_partitions`, and the envelope of the
    /// typed `event`, an event the caller has checked as producer input; see
    /// [`Self::outbox_envelope_of`].
    ///
    /// # Errors
    ///
    /// The errors of converting `event` to a raw [`EventV1`] and of
    /// [`Self::outbox_envelope_of`].
    pub(crate) async fn build_outbox_envelope<P: EventPayload>(
        &self,
        event: &EventV1<P>,
        outbox_partitions: u32,
    ) -> Result<(u32, ProducerOutboxEnvelope), EventBrokerError> {
        let raw = EventV1::try_from(event)?;
        self.outbox_envelope_of(raw, outbox_partitions).await
    }

    /// The outbox partition, of `outbox_partitions`, and the envelope of
    /// `event`, a raw event the caller has checked as producer input, stamped
    /// with this producer's source. Reads only the schema cache: no broker
    /// call, so it may run inside the caller's transaction.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::EventTypeNotDeclared`] when the event's type matches
    /// no declared pattern; [`EventBrokerError::SchemaNotPrepared`] when a lazy
    /// producer has not prepared it; otherwise the errors of
    /// [`PreparedEvent::new`] and of [`ProducerOutboxEnvelope::from_event`].
    pub(crate) async fn outbox_envelope_of(
        &self,
        mut event: EventV1,
        outbox_partitions: u32,
    ) -> Result<(u32, ProducerOutboxEnvelope), EventBrokerError> {
        let type_id = event.event_type().to_string();
        self.cache
            .ensure_declared(&self.event_type_patterns, &type_id)
            .await?;
        if matches!(self.validation_timing, ValidationTiming::Lazy)
            && !self.cache.is_prepared(&type_id).await
        {
            return Err(EventBrokerError::SchemaNotPrepared {
                type_id,
                detail: "call prepare_event_type outside the business transaction first".to_owned(),
            });
        }
        event.stamp_source(self.source.clone());
        // Producer metadata is stamped when the message drains, under the id of
        // the partition it drains from; nothing of it is known here.
        let prepared = PreparedEvent::new(&self.cache, event).await?;
        let outbox_partition = super::partitioning::producer_outbox_partition(
            &prepared.topic,
            &prepared.key,
            outbox_partitions,
        );
        let envelope = ProducerOutboxEnvelope::from_event(
            prepared.event,
            prepared.topic,
            self.registration.mode(),
            self.client_agent.clone(),
        )?;
        Ok((outbox_partition, envelope))
    }
}

#[cfg(test)]
#[path = "db_tests.rs"]
mod tests;
