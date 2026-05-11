use std::sync::Arc;

use toolkit_security::SecurityContext;
use types_registry_sdk::TypesRegistryClient;

use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::internal::schema_cache::SchemaCache;

#[cfg(feature = "outbox")]
use super::AsyncProducer;
use super::SyncProducer;
use super::backend::ProducerBackend;

/// Producer dedup mode. Declared at registration and inferred per-event from the
/// `meta` chain fields. Default: `Chained`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProducerMode {
    /// No `producer_id` - no broker-side dedup.
    Stateless,
    /// `producer_id + sequence` - monotonic dedup.
    Monotonic,
    /// `producer_id + previous + sequence` - full chain dedup.
    #[default]
    Chained,
}

/// Controls when schemas are fetched from types-registry. Validation itself is always ON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ValidationTiming {
    #[default]
    Eager,
    Lazy,
}

/// Builder for [`SyncProducer`] and [`AsyncProducer`].
/// Obtained from [`EventBroker::producer_builder`](crate::broker::EventBroker::producer_builder).
pub struct ProducerBuilder {
    pub(crate) topics: Vec<String>,
    pub(crate) event_type_patterns: Vec<String>,
    pub(crate) source: Option<String>,
    pub(crate) client_agent: Option<String>,
    pub chain_mode: ProducerMode,
    pub validation_timing: ValidationTiming,
    pub(crate) reuse: Option<ProducerId>,
    /// Transport for broker calls. Wired by the impl crate's EventBroker implementation.
    pub(crate) backend: Option<Arc<dyn ProducerBackend>>,
    /// Schema cache backed by types-registry. Wired by the impl crate.
    pub(crate) schema_cache: Option<Arc<SchemaCache>>,
}

impl ProducerBuilder {
    /// Create a new builder. Called internally by `EventBroker::producer_builder()`.
    pub fn new(
        backend: Arc<dyn ProducerBackend>,
        types_registry: Arc<dyn TypesRegistryClient>,
    ) -> Self {
        Self {
            topics: Vec::new(),
            event_type_patterns: Vec::new(),
            source: None,
            client_agent: None,
            chain_mode: ProducerMode::default(),
            validation_timing: ValidationTiming::default(),
            reuse: None,
            backend: Some(backend),
            schema_cache: Some(Arc::new(SchemaCache::new(types_registry))),
        }
    }

    /// Create a builder with no backend (for testing without a live broker).
    pub fn new_unbound() -> Self {
        Self {
            topics: Vec::new(),
            event_type_patterns: Vec::new(),
            source: None,
            client_agent: None,
            chain_mode: ProducerMode::default(),
            validation_timing: ValidationTiming::default(),
            reuse: None,
            backend: None,
            schema_cache: None,
        }
    }

    // ---- Fluent configuration ----

    pub fn topic(mut self, topic: impl Into<String>) -> Self {
        self.topics.push(topic.into());
        self
    }

    pub fn topics<I, S>(mut self, topics: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.topics.extend(topics.into_iter().map(Into::into));
        self
    }

    pub fn event_type_pattern(mut self, pat: impl Into<String>) -> Self {
        self.event_type_patterns.push(pat.into());
        self
    }

    pub fn event_type_patterns<I, S>(mut self, pats: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.event_type_patterns
            .extend(pats.into_iter().map(Into::into));
        self
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn client_agent(mut self, ua: impl Into<String>) -> Self {
        self.client_agent = Some(ua.into());
        self
    }

    pub fn reuse(mut self, producer_id: ProducerId) -> Self {
        self.reuse = Some(producer_id);
        self
    }

    pub fn chain_mode(mut self, mode: ProducerMode) -> Self {
        self.chain_mode = mode;
        self
    }

    pub fn lazy_validation(mut self) -> Self {
        self.validation_timing = ValidationTiming::Lazy;
        self
    }

    // ---- Build-time validation ----

    fn validate(&self) -> Result<(), EventBrokerError> {
        if self.topics.is_empty() {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: "at least one topic must be declared".into(),
                instance: String::new(),
            });
        }
        if self.event_type_patterns.is_empty() {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: "at least one event_type_pattern must be declared".into(),
                instance: String::new(),
            });
        }
        let source = self.source.as_deref().unwrap_or("");
        if source.is_empty() {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: "source is required".into(),
                instance: String::new(),
            });
        }
        if self.chain_mode == ProducerMode::Stateless && self.reuse.is_some() {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: "reuse(producer_id) is meaningless for Stateless mode".into(),
                instance: String::new(),
            });
        }
        Ok(())
    }

    /// Public alias for integration tests.
    #[doc(hidden)]
    pub fn validate_pub(&self) -> Result<(), EventBrokerError> {
        self.validate()
    }

    // ---- Terminal build methods ----

    /// Build a [`SyncProducer`]. Registers the producer (chained/monotonic) and
    /// pre-fetches schemas (eager) before returning.
    pub async fn build_sync(self, ctx: &SecurityContext) -> Result<SyncProducer, EventBrokerError> {
        self.validate()?;
        SyncProducer::build(ctx, self).await
    }

    /// Build an [`AsyncProducer`] backed by a modkit-db transactional outbox.
    #[cfg(feature = "outbox")]
    #[allow(clippy::missing_panics_doc)]
    pub async fn build_async(
        self,
        ctx: &SecurityContext,
        db: toolkit_db::Db,
        queue_name: impl Into<String>,
    ) -> Result<AsyncProducer, EventBrokerError> {
        self.validate()?;
        AsyncProducer::build(ctx, self, db, queue_name.into()).await
    }
}
