//! Domain layer of the llm-gateway gear.

pub mod authz;
pub mod capability_checker;
pub mod credential_broker;
pub mod local_client;
pub mod media;
pub mod output_validator;
pub mod ports;
pub mod pricing;
pub mod provider_rules;
pub mod quota_guard;
pub mod service;
pub mod shutdown;
pub mod stream_supervisor;
#[cfg(test)]
pub mod test_support;
pub mod tool_resolver;

pub use credential_broker::KeylessCredentialBroker;
pub use local_client::LlmGatewayLocalClient;
pub use service::{CreateEmbeddingFailure, CreateResponseFailure, Service};
