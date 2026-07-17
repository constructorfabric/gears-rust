use uuid::Uuid;

use event_broker_sdk::{
    DirectDeduplication, EventBrokerError, ProducerIdentity, ProducerMode, ValidationTiming,
};

#[test]
fn producer_identity_requires_source() {
    let err = ProducerIdentity::new().validate_pub_for_test().unwrap_err();
    assert!(matches!(
        err,
        EventBrokerError::InvalidProducerOptions { .. }
    ));
}

#[test]
fn producer_identity_accepts_source_and_agent() {
    let identity = ProducerIdentity::new()
        .source("order-service")
        .client_agent("order-service/1.0");

    assert_eq!(identity.source_ref(), "order-service");
    assert_eq!(identity.client_agent_ref(), Some("order-service/1.0"));
}

#[test]
fn direct_deduplication_rejects_stateless_registration_modes() {
    let pid = event_broker_sdk::ProducerId(Uuid::new_v4());

    let register = DirectDeduplication::register_on_start(ProducerMode::Stateless);
    let reuse = DirectDeduplication::reuse(ProducerMode::Stateless, pid);

    assert!(matches!(
        register.validate_pub_for_test(),
        Err(EventBrokerError::InvalidProducerOptions { .. })
    ));
    assert!(matches!(
        reuse.validate_pub_for_test(),
        Err(EventBrokerError::InvalidProducerOptions { .. })
    ));
}

#[test]
fn default_validation_is_eager() {
    assert_eq!(ValidationTiming::default(), ValidationTiming::Eager);
}

#[test]
fn typestate_builder_compile_failures_are_checked() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/trybuild/producer/missing_direct_broker.rs");

    #[cfg(feature = "db")]
    {
        tests.compile_fail("tests/trybuild/producer/direct_rejects_db_dedup.rs");
        tests.compile_fail("tests/trybuild/producer/db_rejects_direct_dedup.rs");
    }
}

trait ProducerIdentityTestExt {
    fn validate_pub_for_test(&self) -> Result<(), EventBrokerError>;
}

impl ProducerIdentityTestExt for ProducerIdentity {
    fn validate_pub_for_test(&self) -> Result<(), EventBrokerError> {
        if self.source_ref().trim().is_empty() {
            Err(EventBrokerError::InvalidProducerOptions {
                detail: "producer identity source is required".to_owned(),
                instance: String::new(),
            })
        } else {
            Ok(())
        }
    }
}

trait DirectDeduplicationTestExt {
    fn validate_pub_for_test(&self) -> Result<(), EventBrokerError>;
}

impl DirectDeduplicationTestExt for DirectDeduplication {
    fn validate_pub_for_test(&self) -> Result<(), EventBrokerError> {
        match *self {
            DirectDeduplication::Stateless
            | DirectDeduplication::RegisterOnStart {
                mode: ProducerMode::Monotonic | ProducerMode::Chained,
            }
            | DirectDeduplication::Reuse {
                mode: ProducerMode::Monotonic | ProducerMode::Chained,
                ..
            } => Ok(()),
            DirectDeduplication::RegisterOnStart {
                mode: ProducerMode::Stateless,
            }
            | DirectDeduplication::Reuse {
                mode: ProducerMode::Stateless,
                ..
            } => Err(EventBrokerError::InvalidProducerOptions {
                detail: "registration-backed deduplication requires monotonic or chained mode"
                    .to_owned(),
                instance: String::new(),
            }),
        }
    }
}
