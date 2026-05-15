//! Gateway domain: [`Service`], [`UsageCollectorLocalClient`] and authorization utilities.

mod circuit_breaker;
mod error;
mod local_client;
mod service;

pub use error::DomainError;
pub use local_client::UsageCollectorLocalClient;
pub use service::Service;
