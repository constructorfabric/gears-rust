//! Domain layer for the usage-collector module.

pub mod authz;
pub mod error;
pub mod local_client;
pub mod query;
pub mod service;
#[cfg(test)]
pub mod test_support;
pub mod validation;

pub use error::DomainError;
pub use local_client::UsageCollectorLocalClient;
pub use service::Service;
