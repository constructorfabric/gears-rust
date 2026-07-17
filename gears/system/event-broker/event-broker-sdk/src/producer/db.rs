use std::marker::PhantomData;
use std::sync::Arc;

use tokio::sync::Mutex;
use toolkit_security::SecurityContext;

use crate::api::{EventBroker, ProducerMode};
use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::models::{ProducerMeta, ResetScope};
use crate::sdk::EventBrokerSdk;
use crate::typed_event::TypedEvent;

use super::direct::{Has, Missing};
use super::event_factory::prepare_event;
use super::outbox::{ProducerOutboxEnvelope, ProducerOutboxQueue};
use super::registration::{ProducerRegistration, ProducerRegistrationStore};
use super::schema_cache::ProducerSchemaCache;
use super::types::{
    DbDeduplication, MissingProducerRegistration, ProducerIdentity, UnknownProducerRegistration,
    ValidationTiming,
};

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
    broker: Option<Arc<dyn EventBroker>>,
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
    pub fn broker(self, broker: Arc<dyn EventBroker>) -> DbProducerBuilder<Has, Db, C, I, D, T, P> {
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
                instance: String::new(),
            });
        }

        let cache = Arc::new(ProducerSchemaCache::default());
        if prepare_all {
            cache
                .prepare_all(&broker, &ctx, &self.topics, &self.event_type_patterns)
                .await?;
        }

        let registration_store = ProducerRegistrationStore::new(db.clone());
        let registration = match &deduplication {
            DbDeduplication::Stateless => None,
            DbDeduplication::Managed(managed) => {
                let client_agent = identity.client_agent_ref().map_or_else(
                    || EventBrokerSdk::default_client_agent().to_owned(),
                    str::to_owned,
                );
                Some(
                    resolve_managed_registration(
                        &broker,
                        &ctx,
                        &registration_store,
                        managed,
                        &client_agent,
                    )
                    .await?,
                )
            }
        };

        Ok(DbProducer {
            broker,
            ctx,
            identity,
            deduplication,
            registration_state: Arc::new(Mutex::new(RegistrationState::from_registration(
                registration,
            ))),
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
    pub(crate) broker: Arc<dyn EventBroker>,
    pub(crate) ctx: SecurityContext,
    pub(crate) identity: ProducerIdentity,
    pub(crate) deduplication: DbDeduplication,
    pub(crate) registration_state: Arc<Mutex<RegistrationState>>,
    registration_store: ProducerRegistrationStore,
    topics: Vec<String>,
    event_type_patterns: Vec<String>,
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

    pub fn outbox_queue(
        &self,
        queue: impl Into<String>,
        partitions: toolkit_db::outbox::Partitions,
    ) -> Result<ProducerOutboxQueue, EventBrokerError> {
        ProducerOutboxQueue::new(self.clone(), queue.into(), partitions)
    }

    pub async fn reset_chain(&self, scope: ResetScope<'_>) -> Result<(), EventBrokerError> {
        let producer_id = self
            .registration_state
            .lock()
            .await
            .producer_id
            .ok_or_else(|| {
                EventBrokerError::Internal("reset_chain called on a stateless producer".to_owned())
            })?;
        self.broker
            .reset_producer_chain(&self.ctx, producer_id, scope)
            .await
    }

    pub async fn rotate_registration(&mut self) -> Result<ProducerId, EventBrokerError> {
        let current = {
            let state = self.registration_state.lock().await;
            let Some(current) = state.registration.as_ref() else {
                return Err(EventBrokerError::Internal(
                    "rotate_registration called on a stateless producer".to_owned(),
                ));
            };
            current.clone()
        };
        let client_agent = current.client_agent.clone();
        let producer_id = self
            .broker
            .register_producer(&self.ctx, current.mode, &client_agent)
            .await?;
        let next = self
            .registration_store
            .replace_with_new_generation(&current, producer_id)
            .await?;
        self.registration_state.lock().await.set(next);
        Ok(producer_id)
    }

    #[doc(hidden)]
    pub async fn handle_unknown_producer(
        &self,
        rejected_producer_id: ProducerId,
    ) -> Result<UnknownProducerAction, EventBrokerError> {
        let DbDeduplication::Managed(managed) = &self.deduplication else {
            return Ok(UnknownProducerAction::Fail);
        };
        if managed.on_unknown == UnknownProducerRegistration::Fail {
            return Ok(UnknownProducerAction::Fail);
        }
        let current = {
            let state = self.registration_state.lock().await;
            let Some(current) = state.registration.as_ref() else {
                return Ok(UnknownProducerAction::Fail);
            };
            if current.producer_id != rejected_producer_id {
                return Ok(UnknownProducerAction::AlreadyRotated);
            }
            current.clone()
        };
        let producer_id = self
            .broker
            .register_producer(&self.ctx, current.mode, &current.client_agent)
            .await?;
        let next = self
            .registration_store
            .replace_with_new_generation(&current, producer_id)
            .await?;
        self.registration_state.lock().await.set(next);
        Ok(UnknownProducerAction::Rotated)
    }

    #[doc(hidden)]
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
                instance: String::new(),
            });
        }
        let mode = self.deduplication.mode();
        let state = self.registration_state.lock().await.clone();
        let producer_id = state.producer_id;
        let generation = state.generation;
        let prepared = prepare_event(
            &self.cache,
            &self.identity,
            &self.ctx,
            event,
            |_, partition| match (mode, producer_id) {
                (ProducerMode::Stateless, _) => None,
                (ProducerMode::Monotonic, Some(pid)) | (ProducerMode::Chained, Some(pid)) => {
                    Some(ProducerMeta {
                        version: 1,
                        producer_id: Some(pid.0),
                        previous: None,
                        sequence: None,
                        partition_hint: Some(partition),
                    })
                }
                (_, None) => None,
            },
        )
        .await?;
        let outbox_partition = super::partitioning::producer_outbox_partition(
            &prepared.event.topic,
            prepared.broker_partition,
            outbox_partitions,
        );
        let envelope = ProducerOutboxEnvelope::from_event(
            prepared.event,
            prepared.broker_partition,
            mode,
            producer_id,
            generation,
            self.identity.client_agent_ref().map_or_else(
                || EventBrokerSdk::default_client_agent().to_owned(),
                str::to_owned,
            ),
        );
        Ok((outbox_partition, envelope))
    }
}

#[derive(Clone)]
pub(crate) struct RegistrationState {
    pub(crate) producer_id: Option<ProducerId>,
    pub(crate) generation: Option<i64>,
    pub(crate) registration: Option<ProducerRegistration>,
}

impl RegistrationState {
    fn from_registration(registration: Option<ProducerRegistration>) -> Self {
        let producer_id = registration
            .as_ref()
            .map(|registration| registration.producer_id);
        let generation = registration
            .as_ref()
            .map(|registration| registration.generation);
        Self {
            producer_id,
            generation,
            registration,
        }
    }

    fn set(&mut self, registration: ProducerRegistration) {
        self.producer_id = Some(registration.producer_id);
        self.generation = Some(registration.generation);
        self.registration = Some(registration);
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownProducerAction {
    Fail,
    Rotated,
    AlreadyRotated,
}

async fn resolve_managed_registration(
    broker: &Arc<dyn EventBroker>,
    ctx: &SecurityContext,
    store: &ProducerRegistrationStore,
    managed: &super::types::ManagedDeduplication,
    client_agent: &str,
) -> Result<ProducerRegistration, EventBrokerError> {
    match store.load(&managed.key).await? {
        Some(registration) => {
            registration.validate_matches(managed, client_agent)?;
            Ok(registration)
        }
        None if managed.on_missing == MissingProducerRegistration::Fail => {
            Err(EventBrokerError::InvalidProducerOptions {
                detail: format!("managed producer registration '{}' is missing", managed.key),
                instance: String::new(),
            })
        }
        None => {
            let producer_id = broker
                .register_producer(ctx, managed.mode, client_agent)
                .await?;
            store.insert_new(managed, producer_id, client_agent).await
        }
    }
}
