use crate::api::ProducerMode;
use crate::error::EventBrokerError;
use crate::gts::EventV1;

use super::schema_cache::ProducerSchemaCache;

pub struct PreparedEvent {
    pub(crate) event: EventV1,
    /// Resolved from the event type, which owns the topic binding.
    pub(crate) topic: String,
    /// The value at the event type's partition-key pointer. Places the event in
    /// a producer outbox partition; the broker's backend assigns the topic
    /// partition from the same value, and the producer never computes one.
    #[cfg_attr(not(feature = "outbox"), allow(dead_code))]
    pub(crate) key: String,
}

impl PreparedEvent {
    /// Checks a built event's size as a producer of `mode` sends it, and its `data`
    /// against its prepared type's schema, and resolves its topic and partition key from
    /// that type, refusing a key over its cap. The envelope is checked before this, by
    /// the builder or by the caller.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::EventTooLarge`] when the event, stamped as `mode` stamps it,
    /// encodes to more than [`EventV1::MAX_ENCODED_BYTES`];
    /// [`EventBrokerError::SchemaNotPrepared`] when the event's type is not prepared,
    /// the schema check's error, [`EventBrokerError::TopicNotFound`] when the type's
    /// topic is not prepared, the error of resolving the partition key, or
    /// [`EventBrokerError::PartitionKeyTooLong`] when the key takes more than
    /// [`EventV1::MAX_PARTITION_KEY_BYTES`].
    pub(crate) async fn new(
        cache: &ProducerSchemaCache,
        event: EventV1,
        mode: ProducerMode,
    ) -> Result<Self, EventBrokerError> {
        event.check_size_before_stamp(mode)?;
        let type_id = event.event_type().as_ref();
        let data = event.data().unwrap_or(&serde_json::Value::Null);
        cache.validate_prepared(type_id, data).await?;
        // The event type owns the topic binding and the partition key, so both come
        // from the prepared type.
        let topic = cache.prepared_topic(type_id).await?;
        let pointer = cache.prepared_partition_key(type_id).await?;

        cache.ensure_topic(&topic).await?;
        let key = event.partition_input(&pointer)?;
        EventV1::check_partition_key(&key)?;

        Ok(Self { event, topic, key })
    }
}
