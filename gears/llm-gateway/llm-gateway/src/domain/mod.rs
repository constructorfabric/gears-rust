//! Domain layer of the llm-gateway gear.

pub mod authz;
pub mod credential_broker;
pub mod local_client;
pub mod ports;
pub mod service;
#[cfg(test)]
pub mod test_support;

pub use credential_broker::KeylessCredentialBroker;
pub use local_client::LlmGatewayLocalClient;
pub use service::{CreateResponseFailure, Service};
