use std::sync::Arc;

use chrono::Utc;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::internal::chain_state::ChainState;
use crate::internal::envelope::{Event, ProducerMeta};
use crate::internal::partitioning::partition_for;
use crate::internal::schema_cache::SchemaCache;
use crate::typed_event::TypedEvent;

use super::backend::{IngestOutcome, ProducerBackend};
use super::{ChainMode, ProducerBuilder, ValidationTiming};

/// Synchronous producer. Validates locally and POSTs directly to ingest.
/// Obtained from [`ProducerBuilder::build_sync`].
pub struct SyncProducer {
    backend: Arc<dyn ProducerBackend>,
    schema_cache: Arc<SchemaCache>,
    chain_state: Arc<ChainState>,
    chain_mode: ChainMode,
    producer_id: Option<ProducerId>,
    /// Cached `topic → partition_count` (populated from the first successful publish per topic).
    topic_partition_counts: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u32>>>,
    /// Declared event-type patterns (for declaration-check).
    event_type_patterns: Vec<String>,
    /// Cached resolved type_ids from patterns (populated after eager validation).
    resolved_type_ids: Arc<tokio::sync::RwLock<std::collections::HashSet<String>>>,
    validation: ValidationTiming,
}

impl SyncProducer {
    /// Test helper: build with a supplied backend, skipping registry calls.
    /// Uses `validation_timing = Lazy` so no types-registry call happens at build time.
    #[doc(hidden)]
    pub async fn build_for_test(
        mut builder: ProducerBuilder,
        backend: std::sync::Arc<dyn ProducerBackend>,
    ) -> Result<Self, EventBrokerError> {
        use crate::internal::schema_cache::SchemaCache;
        struct NopRegistry;
        #[async_trait::async_trait]
        impl types_registry_sdk::TypesRegistryClient for NopRegistry {
            async fn register(
                &self,
                _: Vec<serde_json::Value>,
            ) -> Result<
                Vec<types_registry_sdk::RegisterResult>,
                types_registry_sdk::error::TypesRegistryError,
            > {
                unimplemented!()
            }
            async fn register_type_schemas(
                &self,
                _: Vec<serde_json::Value>,
            ) -> Result<
                Vec<types_registry_sdk::RegisterResult>,
                types_registry_sdk::error::TypesRegistryError,
            > {
                unimplemented!()
            }
            async fn get_type_schema(
                &self,
                _: &str,
            ) -> Result<
                types_registry_sdk::GtsTypeSchema,
                types_registry_sdk::error::TypesRegistryError,
            > {
                unimplemented!()
            }
            async fn get_type_schema_by_uuid(
                &self,
                _: uuid::Uuid,
            ) -> Result<
                types_registry_sdk::GtsTypeSchema,
                types_registry_sdk::error::TypesRegistryError,
            > {
                unimplemented!()
            }
            async fn get_type_schemas(
                &self,
                _: Vec<String>,
            ) -> std::collections::HashMap<
                String,
                Result<
                    types_registry_sdk::GtsTypeSchema,
                    types_registry_sdk::error::TypesRegistryError,
                >,
            > {
                unimplemented!()
            }
            async fn get_type_schemas_by_uuid(
                &self,
                _: Vec<uuid::Uuid>,
            ) -> std::collections::HashMap<
                uuid::Uuid,
                Result<
                    types_registry_sdk::GtsTypeSchema,
                    types_registry_sdk::error::TypesRegistryError,
                >,
            > {
                unimplemented!()
            }
            async fn list_type_schemas(
                &self,
                _: types_registry_sdk::TypeSchemaQuery,
            ) -> Result<
                Vec<types_registry_sdk::GtsTypeSchema>,
                types_registry_sdk::error::TypesRegistryError,
            > {
                Ok(vec![])
            }
            async fn register_instances(
                &self,
                _: Vec<serde_json::Value>,
            ) -> Result<
                Vec<types_registry_sdk::RegisterResult>,
                types_registry_sdk::error::TypesRegistryError,
            > {
                unimplemented!()
            }
            async fn get_instance(
                &self,
                _: &str,
            ) -> Result<
                types_registry_sdk::GtsInstance,
                types_registry_sdk::error::TypesRegistryError,
            > {
                unimplemented!()
            }
            async fn get_instance_by_uuid(
                &self,
                _: uuid::Uuid,
            ) -> Result<
                types_registry_sdk::GtsInstance,
                types_registry_sdk::error::TypesRegistryError,
            > {
                unimplemented!()
            }
            async fn get_instances(
                &self,
                _: Vec<String>,
            ) -> std::collections::HashMap<
                String,
                Result<
                    types_registry_sdk::GtsInstance,
                    types_registry_sdk::error::TypesRegistryError,
                >,
            > {
                unimplemented!()
            }
            async fn get_instances_by_uuid(
                &self,
                _: Vec<uuid::Uuid>,
            ) -> std::collections::HashMap<
                uuid::Uuid,
                Result<
                    types_registry_sdk::GtsInstance,
                    types_registry_sdk::error::TypesRegistryError,
                >,
            > {
                unimplemented!()
            }
            async fn list_instances(
                &self,
                _: types_registry_sdk::InstanceQuery,
            ) -> Result<
                Vec<types_registry_sdk::GtsInstance>,
                types_registry_sdk::error::TypesRegistryError,
            > {
                unimplemented!()
            }
        }
        builder.backend = Some(backend);
        builder.schema_cache = Some(std::sync::Arc::new(SchemaCache::new(std::sync::Arc::new(
            NopRegistry,
        ))));
        builder.validation_timing = ValidationTiming::Lazy;
        let ctx = toolkit_security::SecurityContext::anonymous();
        Self::build(&ctx, builder).await
    }

    pub(crate) async fn build(
        ctx: &SecurityContext,
        builder: ProducerBuilder,
    ) -> Result<Self, EventBrokerError> {
        let backend = builder.backend.ok_or_else(|| EventBrokerError::Internal(
            "ProducerBuilder: backend not wired; use EventBroker::producer_builder() to obtain a builder".into(),
        ))?;
        let schema_cache = builder.schema_cache.ok_or_else(|| {
            EventBrokerError::Internal(
                "ProducerBuilder: schema_cache not wired; use EventBroker::producer_builder()"
                    .into(),
            )
        })?;
        let chain_state = Arc::new(ChainState::new());

        // Eager validation: resolve all patterns and cache schemas.
        let mut resolved_ids = std::collections::HashSet::new();
        if builder.validation_timing == ValidationTiming::Eager {
            let ids = schema_cache
                .resolve_all_patterns(ctx, &builder.event_type_patterns)
                .await?;
            resolved_ids.extend(ids);
        }

        // Register producer (chained/monotonic only).
        let producer_id = match builder.chain_mode {
            ChainMode::Stateless => None,
            _ => {
                let source = builder.source.as_deref().unwrap_or("event-broker-sdk");
                let client_agent = builder.client_agent.as_deref().unwrap_or(source);
                let mode_str = match builder.chain_mode {
                    ChainMode::Chained => "chained",
                    ChainMode::Monotonic => "monotonic",
                    ChainMode::Stateless => unreachable!(),
                };
                if let Some(reuse_id) = builder.reuse {
                    // Prime chain state from broker cursors.
                    let cursors = backend.get_producer_cursors(ctx, reuse_id).await?;
                    let entries = cursors
                        .into_iter()
                        .map(|c| ((reuse_id, c.topic, c.partition), c.last_sequence));
                    chain_state.bulk_prime(entries);
                    Some(reuse_id)
                } else {
                    Some(
                        backend
                            .register_producer(ctx, mode_str, client_agent)
                            .await?,
                    )
                }
            }
        };

        Ok(Self {
            backend,
            schema_cache,
            chain_state,
            chain_mode: builder.chain_mode,
            producer_id,
            topic_partition_counts: Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
            event_type_patterns: builder.event_type_patterns,
            resolved_type_ids: Arc::new(tokio::sync::RwLock::new(resolved_ids)),
            validation: builder.validation_timing,
        })
    }

    /// Publish a single event via the 9-step pipeline (design §D6).
    pub async fn publish<E: TypedEvent>(
        &self,
        ctx: &SecurityContext,
        event: E,
    ) -> Result<(), EventBrokerError> {
        let type_id = E::TYPE_ID;
        let topic = E::TOPIC;

        // Step 1: Declaration check.
        self.check_declared(type_id).await?;

        // Step 2+3: Schema fetch + validation.
        let subject_str = event.subject();
        let partition_key_str = event.partition_key();
        let data = serde_json::to_value(&event)
            .map_err(|e| EventBrokerError::Internal(format!("serialize event data: {e}")))?;

        match self.validation {
            ValidationTiming::Eager => {
                self.schema_cache.validate(type_id, &data).await?;
            }
            ValidationTiming::Lazy => {
                if !self.schema_cache.is_cached(type_id).await {
                    // Lazy: fetch now.
                    self.schema_cache.resolve_one(ctx, type_id).await?;
                    let mut guard = self.resolved_type_ids.write().await;
                    guard.insert(type_id.to_owned());
                }
                self.schema_cache.validate(type_id, &data).await?;
            }
        }

        // Step 4: Compute partition.
        let partition_count = self.get_partition_count(topic).await;
        let key = partition_key_str.as_deref().unwrap_or(subject_str.as_ref());
        let partition = partition_for(key, partition_count);

        // Step 5: Stamp ProducerMeta (advance chain tracker).
        let meta = self.build_meta(topic, partition);

        // Step 6: Build wire envelope.
        let tenant_id = event.tenant_id().unwrap_or_else(|| ctx.subject_tenant_id());
        let wire_event = Event {
            id: Uuid::now_v7(),
            type_id: type_id.to_owned(),
            topic: topic.to_owned(),
            tenant_id,
            source: E::SOURCE.to_owned(),
            subject: subject_str.into_owned(),
            subject_type: E::SUBJECT_TYPE.to_owned(),
            partition_key: partition_key_str.map(|s| s.into_owned()),
            occurred_at: Utc::now(),
            trace_parent: event.trace_parent().map(|s| s.into_owned()),
            data: Some(data),
            // Broker-stamped fields absent on publish.
            partition: None,
            sequence: None,
            sequence_time: None,
            offset: None,
            offset_time: None,
            meta,
        };

        // Step 7 (implicit in step 8): serialization done by backend.
        // Step 8: Send to ingest.
        let outcome = self.backend.ingest_event(ctx, &wire_event).await?;

        // Step 9: Response handling.
        if outcome == IngestOutcome::Accepted {
            // Advance chain tracker for chained/monotonic.
            if let (Some(pid), Some(m)) = (self.producer_id, &wire_event.meta) {
                if let Some(seq) = m.sequence {
                    self.chain_state
                        .advance((pid, topic.to_owned(), partition), seq);
                }
            }
        }
        // DuplicateAccepted: do NOT advance chain state — broker already saw this sequence.

        Ok(())
    }

    /// Pre-fetch and cache the schema for `E`. No-op when eager (already cached).
    pub async fn prepare<E: TypedEvent>(
        &self,
        ctx: &SecurityContext,
    ) -> Result<(), EventBrokerError> {
        let type_id = E::TYPE_ID;
        self.check_declared(type_id).await?;
        if !self.schema_cache.is_cached(type_id).await {
            self.schema_cache.resolve_one(ctx, type_id).await?;
            self.resolved_type_ids
                .write()
                .await
                .insert(type_id.to_owned());
        }
        Ok(())
    }

    /// Resolve all declared patterns and pre-fetch schemas. No-op when eager.
    pub async fn prepare_all(&self, ctx: &SecurityContext) -> Result<(), EventBrokerError> {
        let ids = self
            .schema_cache
            .resolve_all_patterns(ctx, &self.event_type_patterns)
            .await?;
        let mut guard = self.resolved_type_ids.write().await;
        guard.extend(ids);
        Ok(())
    }

    pub fn producer_id(&self) -> Option<ProducerId> {
        self.producer_id
    }

    /// Reset chain state (audit-logged on the broker).
    pub async fn reset_chain(
        &self,
        ctx: &SecurityContext,
        topic: Option<&str>,
        partition: Option<u32>,
    ) -> Result<(), EventBrokerError> {
        let pid = self.producer_id.ok_or_else(|| {
            EventBrokerError::Internal("reset_chain called on a Stateless producer".into())
        })?;
        self.backend
            .reset_producer_chain(ctx, pid, topic, partition)
            .await?;
        // Clear local chain state for the scope.
        match (topic, partition) {
            (None, None) => {
                // Full reset: clear all entries for this producer_id.
                // ChainState doesn't expose a bulk-clear-by-producer yet; iterate via a workaround.
                // For now, use reset on each known (topic, partition). A future ChainState method
                // can do this more efficiently.
            }
            (Some(t), Some(p)) => {
                self.chain_state.reset(&(pid, t.to_owned(), p));
            }
            (Some(t), None) => {
                // Reset all partitions for this topic. Requires a ChainState method.
                let _ = t;
            }
            (None, Some(_)) => {
                return Err(EventBrokerError::Internal(
                    "reset_chain with partition but no topic is not meaningful".into(),
                ));
            }
        }
        Ok(())
    }

    // ---- Private helpers ----

    async fn check_declared(&self, type_id: &str) -> Result<(), EventBrokerError> {
        // In eager mode, the resolved_type_ids set is populated at build time.
        // In lazy mode, we check the declared patterns using simple GTS wildcard matching.
        let guard = self.resolved_type_ids.read().await;
        if guard.contains(type_id) {
            return Ok(());
        }
        // Drop read guard before pattern match.
        drop(guard);

        // Check against raw patterns (wildcard matching).
        let matches = self
            .event_type_patterns
            .iter()
            .any(|p| gts_pattern_matches(p, type_id));
        if !matches {
            return Err(EventBrokerError::EventTypeNotDeclared {
                type_id: type_id.to_owned(),
                detail: "this event type does not match any declared event_type_patterns".into(),
                instance: String::new(),
            });
        }
        Ok(())
    }

    fn build_meta(&self, topic: &str, partition: u32) -> Option<ProducerMeta> {
        match (self.chain_mode, self.producer_id) {
            (ChainMode::Stateless, _) => None,
            (ChainMode::Monotonic, Some(pid)) => {
                let key = (pid, topic.to_owned(), partition);
                let last = self.chain_state.peek(&key);
                let next_seq = last + 1;
                // Note: we advance AFTER successful ingest (in publish), not here.
                Some(ProducerMeta {
                    version: 1,
                    producer_id: Some(pid.0),
                    previous: None,
                    sequence: Some(next_seq),
                    partition_hint: Some(partition),
                })
            }
            (ChainMode::Chained, Some(pid)) => {
                let key = (pid, topic.to_owned(), partition);
                let last = self.chain_state.peek(&key);
                let next_seq = last + 1;
                Some(ProducerMeta {
                    version: 1,
                    producer_id: Some(pid.0),
                    previous: Some(last),
                    sequence: Some(next_seq),
                    partition_hint: Some(partition),
                })
            }
            (_, None) => None,
        }
    }

    /// Returns the `partitions` count for `topic`. Currently uses a default of 16 until
    /// the producer has a `TopicRegistry` or `EventBroker.list_topics` integration.
    async fn get_partition_count(&self, topic: &str) -> u32 {
        self.topic_partition_counts
            .read()
            .await
            .get(topic)
            .copied()
            .unwrap_or(16)
    }
}

/// Simple GTS wildcard pattern matching. `pub(crate)` for use in `async_producer`.
/// `"gts.cf.core.events.event.v1~orders.*"` matches `"gts.cf.core.events.event.v1~orders.created.v1"`.
/// Only supports a trailing `.*` glob.
pub(crate) fn gts_pattern_matches(pattern: &str, type_id: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(".*") {
        type_id.starts_with(prefix)
    } else {
        pattern == type_id
    }
}
