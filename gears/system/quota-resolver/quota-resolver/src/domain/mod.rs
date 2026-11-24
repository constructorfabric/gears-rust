//! Domain layer of the quota resolver.

pub mod local_client;
pub mod mapper;
pub mod ports;
pub mod service;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod test_support;

pub use local_client::QuotaResolverLocalClient;
pub use mapper::PrincipalMapper;
pub use ports::{NoopMetrics, Operation, Outcome, Plane, QuotaMetrics};
pub use service::Service;
