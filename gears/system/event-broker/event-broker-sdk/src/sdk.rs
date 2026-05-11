pub struct EventBrokerSdk;

impl EventBrokerSdk {
    pub const DEFAULT_CLIENT_AGENT: &'static str =
        concat!("event-broker-sdk/", env!("CARGO_PKG_VERSION"));

    pub fn default_client_agent() -> &'static str {
        Self::DEFAULT_CLIENT_AGENT
    }
}
