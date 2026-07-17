use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use toolkit_security::SecurityContext;

use crate::api::EventBroker;
use crate::error::EventBrokerError;
use crate::models::EventType;

#[derive(Clone)]
pub(crate) struct PreparedEventType {
    pub(crate) type_id: String,
    pub(crate) topic: String,
    validator: Arc<jsonschema::Validator>,
}

impl PreparedEventType {
    fn new(event_type: EventType) -> Result<Self, EventBrokerError> {
        let validator = jsonschema::validator_for(&event_type.data_schema).map_err(|err| {
            EventBrokerError::Internal(format!("compile schema for {}: {err}", event_type.id))
        })?;
        Ok(Self {
            type_id: event_type.id,
            topic: event_type.topic,
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

#[derive(Default)]
pub(crate) struct ProducerSchemaCache {
    topics: tokio::sync::RwLock<HashMap<String, u32>>,
    event_types: tokio::sync::RwLock<HashMap<String, PreparedEventType>>,
    resolved_type_ids: tokio::sync::RwLock<HashSet<String>>,
}

impl ProducerSchemaCache {
    pub(crate) async fn prepare_all(
        &self,
        broker: &Arc<dyn EventBroker>,
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
                    .any(|pattern| gts_pattern_matches(pattern, &event_type.id))
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
        for event_type in selected {
            if !declared_topics.contains(&event_type.topic) {
                return Err(EventBrokerError::TypeNotInDeclaredTopic {
                    type_id: event_type.id,
                    expected_topic: topics.join(","),
                    detail: "resolved event type belongs to a topic not declared on producer"
                        .to_owned(),
                    instance: String::new(),
                });
            }
            let prepared = PreparedEventType::new(event_type)?;
            resolved.insert(prepared.type_id.clone());
            cached.insert(prepared.type_id.clone(), prepared);
        }
        Ok(())
    }

    pub(crate) async fn prepare_one(
        &self,
        broker: &Arc<dyn EventBroker>,
        ctx: &SecurityContext,
        topics: &[String],
        patterns: &[String],
        type_id: &str,
    ) -> Result<(), EventBrokerError> {
        self.ensure_declared(patterns, type_id).await?;
        self.prepare_topics(broker, ctx, topics).await?;
        let event_type = broker.get_event_type(ctx, type_id).await?;
        if !topics.iter().any(|topic| topic == &event_type.topic) {
            return Err(EventBrokerError::TypeNotInDeclaredTopic {
                type_id: type_id.to_owned(),
                expected_topic: topics.join(","),
                detail: "event type belongs to a topic not declared on producer".to_owned(),
                instance: String::new(),
            });
        }
        let prepared = PreparedEventType::new(event_type)?;
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
        topic: &str,
        data: &serde_json::Value,
    ) -> Result<(), EventBrokerError> {
        let prepared = self
            .event_types
            .read()
            .await
            .get(type_id)
            .cloned()
            .ok_or_else(|| EventBrokerError::SchemaNotPrepared {
                type_id: type_id.to_owned(),
                detail: "schema must be prepared before validating this event".to_owned(),
                instance: String::new(),
            })?;
        if prepared.topic != topic {
            return Err(EventBrokerError::TypeNotInDeclaredTopic {
                type_id: type_id.to_owned(),
                expected_topic: topic.to_owned(),
                detail: format!("event type belongs to topic {}", prepared.topic),
                instance: String::new(),
            });
        }
        prepared.validate(data)
    }

    pub(crate) async fn partition_count(&self, topic: &str) -> Result<u32, EventBrokerError> {
        self.topics.read().await.get(topic).copied().ok_or_else(|| {
            EventBrokerError::TopicNotFound {
                topic: topic.to_owned(),
                detail: "topic was not prepared for this producer".to_owned(),
                instance: String::new(),
            }
        })
    }

    async fn prepare_topics(
        &self,
        broker: &Arc<dyn EventBroker>,
        ctx: &SecurityContext,
        topics: &[String],
    ) -> Result<(), EventBrokerError> {
        let cached_topics = self.topics.read().await;
        let missing = topics
            .iter()
            .filter(|topic| !cached_topics.contains_key(*topic))
            .count();
        drop(cached_topics);
        if missing == 0 {
            return Ok(());
        }

        let declared = topics.iter().cloned().collect::<HashSet<_>>();
        let remote = broker.list_topics(ctx).await?;
        let mut cached = self.topics.write().await;
        for topic in remote {
            if declared.contains(&topic.id) {
                cached.insert(topic.id, topic.partitions.max(1));
            }
        }
        for topic in topics {
            if !cached.contains_key(topic) {
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
