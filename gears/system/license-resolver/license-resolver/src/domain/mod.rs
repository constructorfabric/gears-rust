//! Domain layer for the license resolver.

pub mod error;
pub mod local_client;
pub mod ports;
pub mod principal;
pub mod service;
pub mod validation;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod test_support;

pub use error::DomainError;
pub use local_client::LicenseResolverLocalClient;
pub use ports::{
    CheckOutcome, ContractRegistry, ContractRegistryError, LicenseMetrics, NoopMetrics,
    ViolationKind,
};
pub use principal::PrincipalMapper;
pub use service::Service;
pub use validation::ContractValidator;
