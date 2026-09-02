use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use toolkit_security::SecurityContext;

use crate::api::EventBrokerApi;
use crate::error::EventBrokerError;
use crate::models::EventType;

#[derive(Clone)]
pub(crate) struct PreparedEventType {
    pub(crate) type_id: String,
    pub(crate) topic: String,
    /// JSON Pointer the type declares as its partition key.
    pub(crate) partition_key: String,
    validator: Arc<jsonschema::Validator>,
}

impl PreparedEventType {
    fn new(event_type: &EventType) -> Result<Self, EventBrokerError> {
        let type_id = event_type.id.as_ref().to_owned();
        let topic = event_type.topic.as_ref().to_owned();
        let partition_key = event_type.partition_key.clone();
        // `data_schema` is the payload contract the broker composed out of the
        // type's `data` narrowings - not the whole event schema, which marks the
        // server-stamped members required and so would reject every publish.
        let validator = jsonschema::validator_for(&event_type.data_schema).map_err(|err| {
            EventBrokerError::Internal(format!("compile schema for {type_id}: {err}"))
        })?;
        Ok(Self {
            type_id,
            topic,
            partition_key,
            validator: Arc::new(validator),
        })
    }

    pub(crate) fn validate(&self, data: &serde_json::Value) -> Result<(), EventBrokerError> {
        let errors = self
            .validator
            .iter_errors(data)
            .map(|err| err.to_string())
            .collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(EventBrokerError::EventDataInvalid {
                type_id: self.type_id.clone(),
                errors,
                detail: "event payload failed schema validation".to_owned(),
                instance: String::new(),
            })
        }
    }
}

/// Partition count a producer assumes for every topic it publishes to, unless it
/// says otherwise. It matches the broker's own default; a producer talking to a
/// broker configured differently must declare that count, since the topic does
/// not report it.
pub(crate) const DEFAULT_BROKER_PARTITIONS: u32 = 8;

pub(crate) struct ProducerSchemaCache {
    /// Topics confirmed to exist at the broker. A topic carries no partition
    /// count, so the count below applies to all of them.
    topics: tokio::sync::RwLock<HashSet<String>>,
    broker_partitions: u32,
    event_types: tokio::sync::RwLock<HashMap<String, PreparedEventType>>,
    resolved_type_ids: tokio::sync::RwLock<HashSet<String>>,
}

impl Default for ProducerSchemaCache {
    fn default() -> Self {
        Self::new(DEFAULT_BROKER_PARTITIONS)
    }
}

impl ProducerSchemaCache {
    pub(crate) fn new(broker_partitions: u32) -> Self {
        Self {
            topics: tokio::sync::RwLock::new(HashSet::new()),
            broker_partitions: broker_partitions.max(1),
            event_types: tokio::sync::RwLock::new(HashMap::new()),
            resolved_type_ids: tokio::sync::RwLock::new(HashSet::new()),
        }
    }

    pub(crate) async fn prepare_all(
        &self,
        broker: &Arc<dyn EventBrokerApi>,
        ctx: &SecurityContext,
        topics: &[String],
        patterns: &[String],
    ) -> Result<(), EventBrokerError> {
        self.prepare_topics(broker, ctx, topics).await?;

        let event_types = broker.list_event_types(ctx).await?;
        let selected = event_types
            .into_iter()
            .filter(|event_type| {
                patterns
                    .iter()
                    .any(|pattern| gts_pattern_matches(pattern, event_type.id.as_ref()))
            })
            .collect::<Vec<_>>();

        if selected.is_empty() {
            return Err(EventBrokerError::EventTypeUnknown {
                type_id: patterns.join(","),
                detail: "declared producer event type patterns matched zero event types".to_owned(),
                instance: String::new(),
            });
        }

        let declared_topics = topics.iter().cloned().collect::<HashSet<_>>();
        let mut cached = self.event_types.write().await;
        let mut resolved = self.resolved_type_ids.write().await;
        for event_type in &selected {
            let prepared = PreparedEventType::new(event_type)?;
            if !declared_topics.contains(&prepared.topic) {
                return Err(EventBrokerError::TypeNotInDeclaredTopic {
                    type_id: prepared.type_id,
                    expected_topic: topics.join(","),
                    detail: "resolved event type belongs to a topic not declared on producer"
                        .to_owned(),
                    instance: String::new(),
                });
            }
            resolved.insert(prepared.type_id.clone());
            cached.insert(prepared.type_id.clone(), prepared);
        }
        Ok(())
    }

    pub(crate) async fn prepare_one(
        &self,
        broker: &Arc<dyn EventBrokerApi>,
        ctx: &SecurityContext,
        topics: &[String],
        patterns: &[String],
        type_id: &str,
    ) -> Result<(), EventBrokerError> {
        self.ensure_declared(patterns, type_id).await?;
        self.prepare_topics(broker, ctx, topics).await?;
        let event_type = broker.get_event_type(ctx, type_id).await?;
        let prepared = PreparedEventType::new(&event_type)?;
        if !topics.iter().any(|topic| topic == &prepared.topic) {
            return Err(EventBrokerError::TypeNotInDeclaredTopic {
                type_id: type_id.to_owned(),
                expected_topic: topics.join(","),
                detail: "event type belongs to a topic not declared on producer".to_owned(),
                instance: String::new(),
            });
        }
        self.resolved_type_ids
            .write()
            .await
            .insert(type_id.to_owned());
        self.event_types
            .write()
            .await
            .insert(type_id.to_owned(), prepared);
        Ok(())
    }

    pub(crate) async fn ensure_declared(
        &self,
        patterns: &[String],
        type_id: &str,
    ) -> Result<(), EventBrokerError> {
        if self.resolved_type_ids.read().await.contains(type_id) {
            return Ok(());
        }
        if patterns
            .iter()
            .any(|pattern| gts_pattern_matches(pattern, type_id))
        {
            Ok(())
        } else {
            Err(EventBrokerError::EventTypeNotDeclared {
                type_id: type_id.to_owned(),
                detail: "this event type does not match any declared event_type_patterns"
                    .to_owned(),
                instance: String::new(),
            })
        }
    }

    pub(crate) async fn is_prepared(&self, type_id: &str) -> bool {
        self.event_types.read().await.contains_key(type_id)
    }

    pub(crate) async fn validate_prepared(
        &self,
        type_id: &str,
        data: &serde_json::Value,
    ) -> Result<(), EventBrokerError> {
        self.prepared(type_id).await?.validate(data)
    }

    /// The topic the prepared event type publishes to, taken from its `topic`
    /// trait. A typed event declares no topic - this is the only source.
    pub(crate) async fn prepared_topic(&self, type_id: &str) -> Result<String, EventBrokerError> {
        Ok(self.prepared(type_id).await?.topic)
    }

    pub(crate) async fn prepared_partition_key(
        &self,
        type_id: &str,
    ) -> Result<String, EventBrokerError> {
        Ok(self.prepared(type_id).await?.partition_key)
    }

    async fn prepared(&self, type_id: &str) -> Result<PreparedEventType, EventBrokerError> {
        self.event_types
            .read()
            .await
            .get(type_id)
            .cloned()
            .ok_or_else(|| EventBrokerError::SchemaNotPrepared {
                type_id: type_id.to_owned(),
                detail: "schema must be prepared before validating this event".to_owned(),
                instance: String::new(),
            })
    }

    pub(crate) async fn partition_count(&self, topic: &str) -> Result<u32, EventBrokerError> {
        self.topics
            .read()
            .await
            .contains(topic)
            .then_some(self.broker_partitions)
            .ok_or_else(|| EventBrokerError::TopicNotFound {
                topic: topic.to_owned(),
                detail: "topic was not prepared for this producer".to_owned(),
                instance: String::new(),
            })
    }

    async fn prepare_topics(
        &self,
        broker: &Arc<dyn EventBrokerApi>,
        ctx: &SecurityContext,
        topics: &[String],
    ) -> Result<(), EventBrokerError> {
        let cached_topics = self.topics.read().await;
        let missing = topics
            .iter()
            .filter(|topic| !cached_topics.contains(*topic))
            .count();
        drop(cached_topics);
        if missing == 0 {
            return Ok(());
        }

        let declared = topics.iter().cloned().collect::<HashSet<_>>();
        let remote = broker.list_topics(ctx).await?;
        let mut cached = self.topics.write().await;
        for topic in remote {
            let id = topic.id.into_string();
            if declared.contains(&id) {
                cached.insert(id);
            }
        }
        for topic in topics {
            if !cached.contains(topic) {
                return Err(EventBrokerError::TopicNotFound {
                    topic: topic.clone(),
                    detail: "declared producer topic was not returned by Event Broker".to_owned(),
                    instance: String::new(),
                });
            }
        }
        Ok(())
    }
}

pub(crate) fn gts_pattern_matches(pattern: &str, type_id: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(".*") {
        type_id.starts_with(prefix)
    } else {
        pattern == type_id
    }
}
