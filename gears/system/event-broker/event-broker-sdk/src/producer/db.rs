use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use tokio::sync::Mutex;
use toolkit_security::SecurityContext;

use crate::api::EventBrokerApi;
use crate::error::EventBrokerError;
#[cfg(feature = "outbox")]
use crate::ids::ProducerId;
use crate::models::ResetScope;
use crate::sdk::EventBrokerSdk;
use crate::typed_event::TypedEvent;

use super::direct::{Has, Missing};
#[cfg(feature = "outbox")]
use super::event_factory::prepare_event;
#[cfg(feature = "outbox")]
use super::outbox::{ProducerOutboxEnvelope, ProducerOutboxQueue};
#[cfg(feature = "outbox")]
use super::registration::NewRegistration;
use super::registration::{ProducerRegistration, ProducerRegistrationStore, RegistrationSlot};
use super::schema_cache::ProducerSchemaCache;
use super::types::{DbDeduplication, ProducerIdentity, ValidationTiming};

type DbProducerBuilderState<Broker, Db, Ctx, Identity, Dedup, Topics, Patterns> =
    PhantomData<(Broker, Db, Ctx, Identity, Dedup, Topics, Patterns)>;

pub struct DbProducerBuilder<
    Broker = Missing,
    Db = Missing,
    Ctx = Missing,
    Identity = Missing,
    Dedup = Missing,
    Topics = Missing,
    Patterns = Missing,
> {
    broker: Option<Arc<dyn EventBrokerApi>>,
    db: Option<toolkit_db::Db>,
    ctx: Option<SecurityContext>,
    identity: Option<ProducerIdentity>,
    deduplication: Option<DbDeduplication>,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    validation_timing: ValidationTiming,
    _state: DbProducerBuilderState<Broker, Db, Ctx, Identity, Dedup, Topics, Patterns>,
}

impl DbProducerBuilder {
    /// Creates an empty builder; every required part starts unset.
    pub(crate) fn new() -> Self {
        Self {
            broker: None,
            db: None,
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

impl<B, Db, C, I, D, T, P> DbProducerBuilder<B, Db, C, I, D, T, P> {
    #[must_use]
    pub fn broker(
        self,
        broker: Arc<dyn EventBrokerApi>,
    ) -> DbProducerBuilder<Has, Db, C, I, D, T, P> {
        DbProducerBuilder {
            broker: Some(broker),
            db: self.db,
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
    pub fn db(self, db: toolkit_db::Db) -> DbProducerBuilder<B, Has, C, I, D, T, P> {
        DbProducerBuilder {
            broker: self.broker,
            db: Some(db),
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
    pub fn security_context(
        self,
        ctx: SecurityContext,
    ) -> DbProducerBuilder<B, Db, Has, I, D, T, P> {
        DbProducerBuilder {
            broker: self.broker,
            db: self.db,
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
    pub fn identity(self, identity: ProducerIdentity) -> DbProducerBuilder<B, Db, C, Has, D, T, P> {
        DbProducerBuilder {
            broker: self.broker,
            db: self.db,
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
        deduplication: impl Into<DbDeduplication>,
    ) -> DbProducerBuilder<B, Db, C, I, Has, T, P> {
        DbProducerBuilder {
            broker: self.broker,
            db: self.db,
            ctx: self.ctx,
            identity: self.identity,
            deduplication: Some(deduplication.into()),
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            _state: PhantomData,
        }
    }

    #[must_use]
    pub fn topics<S, It>(self, topics: It) -> DbProducerBuilder<B, Db, C, I, D, Has, P>
    where
        S: Into<String>,
        It: IntoIterator<Item = S>,
    {
        DbProducerBuilder {
            broker: self.broker,
            db: self.db,
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
    pub fn event_type_patterns<S, It>(
        self,
        patterns: It,
    ) -> DbProducerBuilder<B, Db, C, I, D, T, Has>
    where
        S: Into<String>,
        It: IntoIterator<Item = S>,
    {
        DbProducerBuilder {
            broker: self.broker,
            db: self.db,
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

impl DbProducerBuilder<Has, Has, Has, Has, Has, Has, Has> {
    pub async fn prepare_all(self) -> Result<DbProducer, EventBrokerError> {
        self.build_inner(true).await
    }

    pub async fn build(self) -> Result<DbProducer, EventBrokerError> {
        let prepare_all = matches!(self.validation_timing, ValidationTiming::Eager);
        self.build_inner(prepare_all).await
    }

    async fn build_inner(self, prepare_all: bool) -> Result<DbProducer, EventBrokerError> {
        let broker = self.broker.expect("typestate requires broker");
        let db = self.db.expect("typestate requires db");
        let ctx = self.ctx.expect("typestate requires security context");
        let identity = self.identity.expect("typestate requires identity");
        let deduplication = self
            .deduplication
            .expect("typestate requires deduplication");
        identity.validate()?;
        if self.topics.is_empty() || self.event_type_patterns.is_empty() {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: "topics and event_type_patterns are required".to_owned(),
            });
        }

        let cache = Arc::new(ProducerSchemaCache::new());
        if prepare_all {
            cache
                .prepare_all(&broker, &ctx, &self.topics, &self.event_type_patterns)
                .await?;
        }

        let client_agent = identity.client_agent_ref().map_or_else(
            || EventBrokerSdk::default_client_agent().to_owned(),
            str::to_owned,
        );
        let registration_store = ProducerRegistrationStore::new(db.clone());
        // Partition ids are registered as their partitions first drain; what
        // is already stored has to agree with this configuration, since a
        // producer id's mode never changes (ADR-0004).
        if let DbDeduplication::Managed(managed) = &deduplication {
            for registration in registration_store.load_all(&managed.key).await? {
                registration.validate_matches(managed, &client_agent)?;
            }
        }

        Ok(DbProducer {
            broker,
            ctx,
            identity,
            deduplication,
            client_agent,
            registrations: Arc::new(Mutex::new(HashMap::new())),
            registration_store,
            topics: self.topics,
            event_type_patterns: self.event_type_patterns,
            validation_timing: self.validation_timing,
            cache,
        })
    }
}

#[derive(Clone)]
pub struct DbProducer {
    /// Broker the producer publishes to.
    pub(crate) broker: Arc<dyn EventBrokerApi>,
    /// Security context publishes run under.
    pub(crate) ctx: SecurityContext,
    /// Identity stamped on every published event.
    // Read only by `outbox_envelope`; kept on the producer so the outbox path can
    // stamp the client agent without re-deriving it from the builder.
    #[cfg_attr(not(feature = "outbox"), allow(dead_code))]
    pub(crate) identity: ProducerIdentity,
    /// Deduplication setting the producer registers with.
    pub(crate) deduplication: DbDeduplication,
    // Sent when a partition's id is registered on its first drain, which only
    // the outbox does.
    #[cfg_attr(not(feature = "outbox"), allow(dead_code))]
    client_agent: String,
    /// The producer id of each outbox partition drained so far, by slot.
    #[cfg_attr(not(feature = "outbox"), allow(dead_code))]
    registrations: Arc<Mutex<HashMap<RegistrationSlot, ProducerRegistration>>>,
    registration_store: ProducerRegistrationStore,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
    // Only the outbox path re-checks the timing at publish time; the direct path
    // resolves it once in `build_inner`.
    #[cfg_attr(not(feature = "outbox"), allow(dead_code))]
    validation_timing: ValidationTiming,
    cache: Arc<ProducerSchemaCache>,
}

impl DbProducer {
    #[must_use]
    pub fn builder() -> DbProducerBuilder {
        DbProducerBuilder::new()
    }

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

    #[cfg(feature = "outbox")]
    pub fn outbox_queue(
        &self,
        queue: impl Into<String>,
        partitions: toolkit_db::outbox::Partitions,
    ) -> Result<ProducerOutboxQueue, EventBrokerError> {
        ProducerOutboxQueue::new(self.clone(), queue.into(), partitions)
    }

    /// Resets the broker chain of every stored partition id, each keeping its
    /// id.
    pub async fn reset_chain(&self, scope: ResetScope<'_>) -> Result<(), EventBrokerError> {
        for registration in self.stored_registrations("reset_chain").await? {
            self.broker
                .reset_producer_chain(&self.ctx, registration.producer_id, scope)
                .await?;
        }
        Ok(())
    }

    /// Replaces every stored partition id with a newly registered one, under
    /// the next generation. Messages drained afterwards, including ones
    /// enqueued before, publish under the new ids.
    pub async fn rotate_registration(&self) -> Result<(), EventBrokerError> {
        for registration in self.stored_registrations("rotate_registration").await? {
            self.replace(&registration).await?;
        }
        Ok(())
    }

    /// Every stored partition registration of this producer's key, as
    /// `(queue, partition, producer id, generation)`, ordered by queue and
    /// partition.
    #[cfg(feature = "outbox")]
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
        let DbDeduplication::Managed(managed) = &self.deduplication else {
            return Err(EventBrokerError::Internal(format!(
                "{operation} called on a stateless producer"
            )));
        };
        self.registration_store.load_all(&managed.key).await
    }

    /// The producer id messages of `partition` of `queue` publish under:
    /// cached, else stored, else registered with the broker and stored.
    ///
    /// # Errors
    /// [`EventBrokerError::Internal`] on a stateless producer; whatever the
    /// broker or the registration store fail with.
    #[cfg(feature = "outbox")]
    pub(crate) async fn partition_producer(
        &self,
        queue: &str,
        partition: i64,
    ) -> Result<ProducerId, EventBrokerError> {
        let DbDeduplication::Managed(managed) = &self.deduplication else {
            return Err(EventBrokerError::Internal(
                "a stateless producer has no producer id".to_owned(),
            ));
        };
        let slot = RegistrationSlot {
            key: managed.key.clone(),
            queue: queue.to_owned(),
            partition,
        };
        let mut registrations = self.registrations.lock().await;
        if let Some(registration) = registrations.get(&slot) {
            return Ok(registration.producer_id);
        }
        let registration = match self.registration_store.load(&slot).await? {
            Some(registration) => registration,
            None => {
                let producer_id = self
                    .broker
                    .register_producer(&self.ctx, managed.mode, &self.client_agent)
                    .await?;
                self.registration_store
                    .insert_new(NewRegistration {
                        slot: &slot,
                        producer_id,
                        mode: managed.mode,
                        client_agent: &self.client_agent,
                    })
                    .await?
            }
        };
        let producer_id = registration.producer_id;
        registrations.insert(slot, registration);
        Ok(producer_id)
    }

    /// Replaces `rejected`, the id of `partition` of `queue` the broker
    /// answered as unknown, with a newly registered one. A no-op when the
    /// partition already moved on to another id.
    ///
    /// # Errors
    /// Whatever the broker or the registration store fail with.
    #[cfg(feature = "outbox")]
    pub(crate) async fn replace_unknown_producer(
        &self,
        queue: &str,
        partition: i64,
        rejected: ProducerId,
    ) -> Result<(), EventBrokerError> {
        let DbDeduplication::Managed(managed) = &self.deduplication else {
            return Ok(());
        };
        let slot = RegistrationSlot {
            key: managed.key.clone(),
            queue: queue.to_owned(),
            partition,
        };
        let Some(current) = self.registration_store.load(&slot).await? else {
            return Ok(());
        };
        if current.producer_id != rejected {
            self.registrations.lock().await.insert(slot, current);
            return Ok(());
        }
        self.replace(&current).await
    }

    async fn replace(&self, current: &ProducerRegistration) -> Result<(), EventBrokerError> {
        let producer_id = self
            .broker
            .register_producer(&self.ctx, current.mode, &current.client_agent)
            .await?;
        let next = self
            .registration_store
            .replace_with_new_generation(current, producer_id)
            .await?;
        self.registrations
            .lock()
            .await
            .insert(next.slot.clone(), next);
        Ok(())
    }

    #[doc(hidden)]
    #[cfg(feature = "outbox")]
    pub async fn outbox_envelope<E: TypedEvent>(
        &self,
        event: E,
        outbox_partitions: u32,
    ) -> Result<(u32, ProducerOutboxEnvelope), EventBrokerError> {
        self.cache
            .ensure_declared(&self.event_type_patterns, E::TYPE_ID)
            .await?;
        if matches!(self.validation_timing, ValidationTiming::Lazy)
            && !self.cache.is_prepared(E::TYPE_ID).await
        {
            return Err(EventBrokerError::SchemaNotPrepared {
                type_id: E::TYPE_ID.to_owned(),
                detail: "call producer.prepare::<E>() outside the business transaction first"
                    .to_owned(),
            });
        }
        // Producer metadata is stamped when the message drains, under the id of
        // the partition it drains from; nothing of it is known here.
        let prepared =
            prepare_event(&self.cache, &self.identity, &self.ctx, event, |_| None).await?;
        let outbox_partition = super::partitioning::producer_outbox_partition(
            &prepared.topic,
            &prepared.key,
            outbox_partitions,
        );
        let envelope = ProducerOutboxEnvelope::from_event(
            prepared.event,
            prepared.topic,
            self.deduplication.mode(),
            self.identity.client_agent_ref().map_or_else(
                || EventBrokerSdk::default_client_agent().to_owned(),
                str::to_owned,
            ),
        );
        Ok((outbox_partition, envelope))
    }
}
