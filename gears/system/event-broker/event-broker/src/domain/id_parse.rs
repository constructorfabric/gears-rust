//! Shared parsing of the identifier kinds the broker's API carries in paths,
//! queries and bodies - a topic id, an event-type id, a subscription id and a
//! consumer-group id - into their typed form.
//!
//! One helper per id kind, so every producer of a rejection - the REST
//! handlers and the in-process [`crate::domain::local_broker::LocalBroker`]
//! facade alike - rejects a malformed id with the *same* [`DomainError`],
//! rather than each call site inventing its own code, resource and message.
//! Before this, the facade and the REST layer diverged on identical input (a
//! bad interest topic was a typed error on one side and an untyped `Other` on
//! the other; a bad consumer-group id was a `500 Internal` on the facade but a
//! `400` over REST), and several sites echoed the rejected value back.
//!
//! The rejection is a field violation, not free-text `detail`, so it survives
//! the wire as a typed [`event_broker_sdk::EventBrokerError::InvalidEventField`]
//! rather than degrading to `Other`. It names the field and states what the
//! rule requires, and carries **no part of the submitted value** - a malformed
//! id has not passed validation, so echoing it would turn a rejection into a
//! channel for request content (the no-user-input-in-error-bodies rule).
//!
//! This lives in `domain/` rather than beside `api/rest/handlers/action_suffix`
//! (the other shared path-parse helper) because the facade is a `domain/` type
//! and `domain/` must not depend on `api/`; the REST handlers reach down into
//! `domain/` freely.

use gts::GtsTypeId;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use event_broker_sdk::error::reasons;

use crate::domain::error::DomainError;

/// The common shape: a field violation naming the field and the rule, with no
/// part of the submitted value. `reason` is [`reasons::INVALID_EVENT_FIELD`] so
/// the SDK recovers the typed `InvalidEventField` off the wire.
fn invalid_id(field: &'static str, detail: &'static str) -> DomainError {
    DomainError::TextField {
        field,
        detail: detail.to_owned(),
        reason: reasons::INVALID_EVENT_FIELD,
    }
}

/// Parses a topic identifier (a GTS instance id).
///
/// # Errors
/// [`DomainError::TextField`] on `topic` when `raw` is not a GTS instance id.
pub fn parse_topic_id(raw: &str) -> Result<GtsInstanceId, DomainError> {
    GtsInstanceId::try_new(raw)
        .map_err(|_| invalid_id("topic", "must be a valid GTS instance identifier"))
}

/// Parses an event-type identifier (a GTS type id).
///
/// # Errors
/// [`DomainError::TextField`] on `type` when `raw` is not a GTS type id.
pub fn parse_type_id(raw: &str) -> Result<GtsTypeId, DomainError> {
    GtsTypeId::try_new(raw).map_err(|_| invalid_id("type", "must be a valid GTS type identifier"))
}

/// Parses a subscription identifier (a UUID).
///
/// # Errors
/// [`DomainError::TextField`] on `subscription_id` when `raw` is not a UUID.
pub fn parse_subscription_id(raw: &str) -> Result<Uuid, DomainError> {
    Uuid::parse_str(raw).map_err(|_| invalid_id("subscription_id", "must be a valid UUID"))
}

/// Parses a consumer-group identifier (a GTS instance id).
///
/// # Errors
/// [`DomainError::TextField`] on `consumer_group` when `raw` is not a GTS
/// instance id.
pub fn parse_consumer_group_id(raw: &str) -> Result<GtsInstanceId, DomainError> {
    GtsInstanceId::try_new(raw)
        .map_err(|_| invalid_id("consumer_group", "must be a valid GTS instance identifier"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_topic_is_a_field_violation_naming_the_field() {
        let err = parse_topic_id("not a gts id ~~").unwrap_err();
        assert!(matches!(
            err,
            DomainError::TextField {
                field: "topic",
                reason,
                ..
            } if reason == reasons::INVALID_EVENT_FIELD
        ));
    }

    #[test]
    fn a_well_formed_topic_parses() {
        let raw = "gts.cf.core.events.topic.v1~example.eb.orders.acme.v1";
        assert_eq!(parse_topic_id(raw).unwrap().as_ref(), raw);
    }

    #[test]
    fn a_malformed_subscription_is_a_field_violation() {
        let err = parse_subscription_id("not-a-uuid").unwrap_err();
        assert!(matches!(
            err,
            DomainError::TextField {
                field: "subscription_id",
                ..
            }
        ));
    }
}
