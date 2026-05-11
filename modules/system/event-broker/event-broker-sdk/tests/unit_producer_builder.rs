//! Unit tests for ProducerBuilder validation.
#![allow(unknown_lints, de0901_gts_string_pattern)]

use event_broker_sdk::{ChainMode, EventBrokerError, ProducerBuilder, ValidationTiming};
use uuid::Uuid;

fn base() -> ProducerBuilder {
    ProducerBuilder::new_unbound()
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .event_type_patterns(["gts.cf.core.events.event.v1~orders.*"])
        .source("test-service")
}

#[test]
fn missing_topics_fails() {
    let b = ProducerBuilder::new_unbound()
        .event_type_patterns(["p"])
        .source("svc");
    let err = b.validate_pub().unwrap_err();
    assert!(matches!(
        err,
        EventBrokerError::InvalidProducerOptions { .. }
    ));
}

#[test]
fn missing_patterns_fails() {
    let b = ProducerBuilder::new_unbound().topics(["t"]).source("svc");
    let err = b.validate_pub().unwrap_err();
    assert!(matches!(
        err,
        EventBrokerError::InvalidProducerOptions { .. }
    ));
}

#[test]
fn missing_source_fails() {
    let b = ProducerBuilder::new_unbound()
        .topics(["t"])
        .event_type_patterns(["p"]);
    let err = b.validate_pub().unwrap_err();
    assert!(matches!(
        err,
        EventBrokerError::InvalidProducerOptions { .. }
    ));
}

#[test]
fn stateless_plus_reuse_fails() {
    let pid = event_broker_sdk::ProducerId(Uuid::new_v4());
    let b = base().chain_mode(ChainMode::Stateless).reuse(pid);
    let err = b.validate_pub().unwrap_err();
    assert!(matches!(
        err,
        EventBrokerError::InvalidProducerOptions { .. }
    ));
}

#[test]
fn default_chain_mode_is_chained() {
    let b = ProducerBuilder::new_unbound();
    assert_eq!(b.chain_mode, ChainMode::Chained);
}

#[test]
fn default_validation_is_eager() {
    let b = ProducerBuilder::new_unbound();
    assert_eq!(b.validation_timing, ValidationTiming::Eager);
}

#[test]
fn valid_builder_passes() {
    assert!(base().validate_pub().is_ok());
}
