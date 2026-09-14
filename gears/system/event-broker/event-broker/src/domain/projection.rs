//! Projections from a resolved GTS type schema onto the broker's API shapes.
//!
//! A topic is an instance of the topic base type, so its values are properties of
//! the instance document. An event type is a derived type schema, so its
//! configuration lives in `x-gts-traits` and its payload contract is the schema's
//! narrowing of the base event's `data` member. Neither shape is what a
//! REST client should have to parse, so the API reports the DTOs the SDK owns
//! ([`event_broker_sdk::models`]) instead - computed here, once, from the
//! registered schema. The DTOs are the wire contract and so belong to the SDK;
//! deriving them from a schema is broker-side work and belongs here, where the
//! registry's resolved-schema model is already a dependency.
//!
//! Every failure here is [`DomainError::Internal`]. A resolved schema reaching
//! the broker without a trait its base declares `required` is a registration
//! `types-registry` should not have admitted; it is not something a caller
//! supplied or can correct.

use event_broker_sdk::gts::data_contract;
use event_broker_sdk::models::{EventType, Topic};
use types_registry_sdk::{GtsInstance, GtsTypeSchema};

use toolkit_utils::iso8601_duration::{Iso8601Duration, Iso8601DurationError};

use crate::domain::error::DomainError;
use crate::domain::event_type::{
    partition_key as event_type_partition_key, topic as event_type_topic,
};

/// Projects a registered topic instance onto the API's topic shape.
///
/// The base type requires `description` and admits an absent `retention`, so a
/// document missing the former is a registration `types-registry` should not have
/// admitted.
///
/// # Errors
/// Returns [`DomainError::Internal`] when the document carries no string
/// `description`, or a `retention` that is not an ISO 8601 duration.
pub fn topic(instance: &GtsInstance) -> Result<Topic, DomainError> {
    let broken = |detail: &str| DomainError::Internal(format!("{} {detail}", instance.id.as_ref()));

    let description = instance
        .object
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| broken("carries no string `description`"))?;

    let retention = instance
        .object
        .get("retention")
        .filter(|value| !value.is_null())
        .and_then(serde_json::Value::as_str)
        .map(str::parse::<Iso8601Duration>)
        .transpose()
        .map_err(|err: Iso8601DurationError| {
            broken(&format!(
                "has a `retention` that is not an ISO 8601 duration: {err}"
            ))
        })?;

    Ok(Topic {
        id: instance.id.clone(),
        description,
        retention,
    })
}

/// Projects a resolved event type schema onto the API's event-type shape.
///
/// # Errors
/// Returns [`DomainError::Internal`] when the schema resolves to no `topic`
/// trait, to one that is not a well-formed GTS instance identifier, or to no
/// `partition_key` trait - the base declares a default for the last, so an
/// absent value means the chain never carried the base's trait schema.
pub fn event_type(schema: &GtsTypeSchema) -> Result<EventType, DomainError> {
    let topic = event_type_topic(schema)?;

    let allowed_subject_types = schema
        .effective_traits()
        .get("allowed_subject_types")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let partition_key = event_type_partition_key(schema)?;

    Ok(EventType {
        id: schema.type_id.clone(),
        topic,
        description: schema.description.clone(),
        allowed_subject_types,
        partition_key,
        data_schema: data_contract(&schema.effective_schema()),
    })
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod projection_tests;
