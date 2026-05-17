//! Gateway domain: [`Service`], [`UsageCollectorLocalClient`], and supporting
//! types. The PDP `authorize_and_compile_scope` helper is an internal detail of
//! the `authz` submodule, reached only via [`Service::query_aggregated`] /
//! [`Service::query_raw`] so the gateway has no public no-authz code path.

mod authz;
mod circuit_breaker;
mod error;
mod local_client;
mod query;
mod service;

pub use error::DomainError;
pub use local_client::UsageCollectorLocalClient;
pub use query::{AggregationQueryRequest, RawQueryRequest};
pub use service::Service;
