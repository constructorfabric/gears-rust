use chrono::Utc;
use uuid::Uuid;

use crate::error::EventBrokerError;
use crate::models::{Event, ProducerMeta};
use crate::typed_event::TypedEvent;

use super::partitioning::{broker_partition, broker_partition_input};
use super::schema_cache::ProducerSchemaCache;
use super::types::ProducerIdentity;

pub(crate) struct PreparedEvent {
    pub(crate) event: Event,
    pub(crate) broker_partition: u32,
}

pub(crate) async fn prepare_event<E: TypedEvent>(
    cache: &ProducerSchemaCache,
    identity: &ProducerIdentity,
    ctx: &toolkit_security::SecurityContext,
    event: E,
    meta_for_partition: impl FnOnce(&str, u32) -> Option<ProducerMeta>,
) -> Result<PreparedEvent, EventBrokerError> {
    let type_id = E::TYPE_ID;
    let topic = E::TOPIC;
    let subject = event.subject();
    let partition_key = event.partition_key();
    let tenant_id = event.tenant_id().unwrap_or_else(|| ctx.subject_tenant_id());
    let data = serde_json::to_value(&event)
        .map_err(|err| EventBrokerError::Internal(format!("serialize event data: {err}")))?;

    cache.validate_prepared(type_id, topic, &data).await?;

    let partition_input = broker_partition_input(partition_key.as_deref(), tenant_id);
    let partition_count = cache.partition_count(topic).await?;
    let partition = broker_partition(&partition_input, partition_count);
    let meta = meta_for_partition(topic, partition);

    Ok(PreparedEvent {
        broker_partition: partition,
        event: Event {
            id: Uuid::now_v7(),
            type_id: type_id.to_owned(),
            topic: topic.to_owned(),
            tenant_id,
            source: identity.source_ref().to_owned(),
            subject: subject.into_owned(),
            subject_type: E::SUBJECT_TYPE.to_owned(),
            partition_key: partition_key.map(|value| value.into_owned()),
            occurred_at: Utc::now(),
            trace_parent: event.trace_parent().map(|value| value.into_owned()),
            data: Some(data),
            partition: None,
            sequence: None,
            sequence_time: None,
            offset: None,
            offset_time: None,
            meta,
        },
    })
}
