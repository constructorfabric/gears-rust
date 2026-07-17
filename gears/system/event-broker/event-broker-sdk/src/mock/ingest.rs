use chrono::Utc;

use crate::api::{IngestOutcome, ProducerMode};
use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::models::Event;

use super::core::Core;
use super::partitioning::partition_for;

/// Infer the producer mode from the `meta` chain fields on the event.
fn detect_mode(event: &Event) -> ProducerMode {
    match &event.meta {
        Some(m) if m.producer_id.is_some() && m.previous.is_some() && m.sequence.is_some() => {
            ProducerMode::Chained
        }
        Some(m) if m.producer_id.is_some() && m.sequence.is_some() => ProducerMode::Monotonic,
        _ => ProducerMode::Stateless,
    }
}

/// Core ingest pipeline. Called under the `Core` mutex (held by caller).
///
/// Steps: topic lookup → event-type lookup → schema validation → partition
/// derivation (ADR-0002: `partition_key` else `tenant_id`) → mode detection →
/// chain/monotonic dedup → offset assignment → append to log.
///
/// Returns `(IngestOutcome, stamped_event)` on success where the stamped event
/// has `partition`/`sequence`/`offset` populated.
pub(super) fn ingest_one(
    core: &mut Core,
    event: &Event,
) -> Result<(IngestOutcome, Event), EventBrokerError> {
    // -- 0. GTS format validation ----------------------------------------------
    if let Err(e) = gts_id::GtsId::try_new(&event.topic) {
        return Err(EventBrokerError::InvalidEventField {
            field: "topic",
            detail: format!("topic must be a GTS identifier: {e}"),
            instance: String::new(),
        });
    }
    if let Err(e) = gts_id::GtsId::try_new(&event.type_id) {
        return Err(EventBrokerError::InvalidEventField {
            field: "type",
            detail: format!("event type must be a GTS identifier: {e}"),
            instance: String::new(),
        });
    }
    if event.partition.is_some() {
        return Err(EventBrokerError::InvalidEventField {
            field: "partition",
            detail: "partition is broker-stamped and read-only on publish".to_owned(),
            instance: "/v1/events".to_owned(),
        });
    }

    // -- 1. Topic lookup -------------------------------------------------------
    if !core.topics.contains_key(&event.topic) {
        return Err(EventBrokerError::TopicNotFound {
            topic: event.topic.clone(),
            detail: format!("topic '{}' not registered in mock", event.topic),
            instance: String::new(),
        });
    }

    // -- 2. Event-type lookup --------------------------------------------------
    let has_event_type = core
        .topics
        .get(&event.topic)
        .map(|t| t.event_types.contains_key(&event.type_id))
        .unwrap_or(false);
    if !has_event_type {
        // No event type registered - if the mock is in permissive mode
        // (no event types registered at all), skip validation. Otherwise error.
        let any_types = core
            .topics
            .get(&event.topic)
            .map(|t| !t.event_types.is_empty())
            .unwrap_or(false);
        if any_types {
            return Err(EventBrokerError::EventTypeUnknown {
                type_id: event.type_id.clone(),
                detail: format!("event type '{}' not registered in mock", event.type_id),
                instance: String::new(),
            });
        }
        // Permissive: no event types registered → skip schema validation.
    } else {
        let reg = core
            .topics
            .get(&event.topic)
            .and_then(|t| t.event_types.get(&event.type_id))
            .expect("event type presence checked above");
        if !reg.allowed_subject_types.is_empty()
            && !reg
                .allowed_subject_types
                .iter()
                .any(|allowed| allowed == &event.subject_type)
        {
            return Err(EventBrokerError::InvalidEventField {
                field: "subject_type",
                detail: format!(
                    "subject_type '{}' is not allowed for event type '{}'",
                    event.subject_type, event.type_id
                ),
                instance: "/v1/events".to_owned(),
            });
        }

        // -- 3. Payload schema validation (M4) ---------------------------------
        let schema_val = Some(reg.data_schema.clone());

        if let (Some(data), Some(schema_val)) = (&event.data, schema_val) {
            match jsonschema::validator_for(&schema_val) {
                Ok(validator) => {
                    let errs: Vec<String> =
                        validator.iter_errors(data).map(|e| e.to_string()).collect();
                    if !errs.is_empty() {
                        let detail = errs.join("; ");
                        return Err(EventBrokerError::EventDataInvalid {
                            type_id: event.type_id.clone(),
                            errors: vec![detail.clone()],
                            detail,
                            instance: String::new(),
                        });
                    }
                }
                Err(e) => {
                    return Err(EventBrokerError::EventDataInvalid {
                        type_id: event.type_id.clone(),
                        errors: vec![e.to_string()],
                        detail: format!("schema compile error: {e}"),
                        instance: String::new(),
                    });
                }
            }
        }
    }

    let partitions = core.topics[&event.topic].partitions;

    // -- 4. Partition derivation (ADR-0002: partition_key else tenant_id) -------
    let partition_key_owned;
    let partition_input: &str = if let Some(pk) = event.partition_key.as_deref() {
        pk
    } else {
        partition_key_owned = event.tenant_id.to_string();
        &partition_key_owned
    };
    let partition = partition_for(partition_input, partitions);

    // -- 5. Producer mode detection --------------------------------------------
    let mode = detect_mode(event);

    // -- 6. Chain / monotonic dedup --------------------------------------------
    if mode != ProducerMode::Stateless {
        let meta = event.meta.as_ref().expect("meta present for non-stateless");
        let producer_id = ProducerId(meta.producer_id.expect("producer_id present"));
        // B1: a chained/monotonic publish must carry a registered Producer-Id
        // (issued by POST /v1/producers). An unknown/expired id is rejected.
        let producer_reg = core.producers.get(&producer_id).ok_or_else(|| {
            EventBrokerError::UnknownProducer {
                producer_id,
                detail: format!(
                    "unknown producer_id {producer_id:?}; register via POST /v1/producers before publishing"
                ),
                instance: "/v1/events".to_owned(),
            }
        })?;
        if producer_reg.mode != mode {
            return Err(EventBrokerError::InvalidEventField {
                field: "meta",
                detail: format!(
                    "producer_id {producer_id:?} is registered as {:?}, but event metadata is {:?}",
                    producer_reg.mode, mode
                ),
                instance: "/v1/events".to_owned(),
            });
        }
        let seq = meta.sequence.expect("sequence present");
        let key = (producer_id, event.topic.clone(), partition);
        let last = core.producer_state.get(&key).copied().unwrap_or(-1);

        match mode {
            ProducerMode::Chained => {
                let prev = meta.previous.expect("previous present for chained");
                if seq <= last {
                    // Duplicate - do NOT advance state (M2).
                    return Ok((IngestOutcome::Duplicate, event.clone()));
                }
                if prev != last {
                    return Err(EventBrokerError::SequenceViolation {
                        expected_previous: last,
                        detail: format!(
                            "expected previous={last}, got previous={prev} for ({producer_id:?}, {}, {partition})",
                            event.topic
                        ),
                        instance: String::new(),
                    });
                }
                core.producer_state.insert(key, seq);
            }
            ProducerMode::Monotonic => {
                if seq <= last {
                    return Ok((IngestOutcome::Duplicate, event.clone()));
                }
                core.producer_state.insert(key, seq);
            }
            ProducerMode::Stateless => unreachable!(),
        }
    }

    // -- 7. Offset assignment + append (serialised under Mutex → prevents M1) --
    let now = Utc::now();
    let topic_state = core.topics.get_mut(&event.topic).expect("checked above");
    let offset = topic_state.next_offset_for(partition);
    let mut stamped = event.clone();
    stamped.partition = Some(partition);
    stamped.sequence = Some(offset);
    stamped.sequence_time = Some(now);
    stamped.offset = Some(offset);
    stamped.offset_time = Some(now);
    // Strip writeOnly publish-input fields from the stored read-projection.
    stamped.meta = None;
    topic_state.append(partition, stamped.clone());

    Ok((IngestOutcome::Accepted, stamped))
}

/// Batch ingest: all events must resolve to the same `(topic, partition)`.
/// Called under the `Core` mutex.
pub(super) fn ingest_batch(
    core: &mut Core,
    events: &[Event],
) -> Result<Vec<(IngestOutcome, Event)>, EventBrokerError> {
    if events.is_empty() {
        return Ok(vec![]);
    }

    // Validate batch homogeneity (same topic + same partition key → same partition).
    let first = &events[0];
    let partitions = core
        .topics
        .get(&first.topic)
        .map(|t| t.partitions)
        .unwrap_or(1);
    let first_partition_key_owned;
    let first_partition_input: &str = if let Some(pk) = first.partition_key.as_deref() {
        pk
    } else {
        first_partition_key_owned = first.tenant_id.to_string();
        &first_partition_key_owned
    };
    let expected_partition = partition_for(first_partition_input, partitions);

    for event in events.iter().skip(1) {
        if event.topic != first.topic {
            return Err(EventBrokerError::InvalidEventField {
                field: "topic",
                detail: format!(
                    "batch.mixed_partition: all events must share the same topic; got '{}' and '{}'",
                    first.topic, event.topic
                ),
                instance: String::new(),
            });
        }
        let partition_key_owned;
        let partition_input: &str = if let Some(pk) = event.partition_key.as_deref() {
            pk
        } else {
            partition_key_owned = event.tenant_id.to_string();
            &partition_key_owned
        };
        let this_partition = partition_for(partition_input, partitions);
        if this_partition != expected_partition {
            return Err(EventBrokerError::InvalidEventField {
                field: "partition_key",
                detail: format!(
                    "batch.mixed_partition: events resolve to different partitions ({expected_partition} vs {this_partition})"
                ),
                instance: String::new(),
            });
        }
    }

    let mut staged = Core {
        topics: core.topics.clone(),
        producers: core.producers.clone(),
        producer_state: core.producer_state.clone(),
        ..Core::default()
    };
    let results = events
        .iter()
        .map(|event| ingest_one(&mut staged, event))
        .collect::<Result<Vec<_>, _>>()?;
    core.topics = staged.topics;
    core.producer_state = staged.producer_state;
    Ok(results)
}
