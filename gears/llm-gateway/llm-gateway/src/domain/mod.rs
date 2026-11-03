//! Domain layer of the llm-gateway gear.

pub mod authz;
pub mod capability_checker;
pub mod credential_broker;
pub mod local_client;
pub mod output_validator;
pub mod ports;
pub mod provider_rules;
pub mod service;
#[cfg(test)]
pub mod test_support;
pub mod tool_resolver;

pub use credential_broker::KeylessCredentialBroker;
pub use local_client::LlmGatewayLocalClient;
pub use service::{CreateResponseFailure, Service};
