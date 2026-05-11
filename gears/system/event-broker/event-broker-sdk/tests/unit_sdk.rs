//! Unit tests for SDK-level defaults.

use event_broker_sdk::EventBrokerSdk;

#[test]
fn default_client_agent_is_shared_sdk_identifier() {
    assert_eq!(
        EventBrokerSdk::default_client_agent(),
        EventBrokerSdk::DEFAULT_CLIENT_AGENT
    );
    assert_eq!(
        EventBrokerSdk::default_client_agent(),
        concat!("event-broker-sdk/", env!("CARGO_PKG_VERSION"))
    );
}
