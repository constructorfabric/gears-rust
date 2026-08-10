//! service-principal SDK — SPI for tenant-scoped machine identities
//! (confidential OAuth `client_credentials` clients).

pub mod api;
pub mod error;
pub mod gts;
pub mod models;

pub use api::ServicePrincipalClientV1;
pub use error::ServicePrincipalFailure;
pub use gts::SERVICE_PRINCIPAL_RESOURCE_TYPE;
pub use models::{
    CreateServicePrincipalRequest, ServicePrincipalCredentials, ServicePrincipalSummary, TenantId,
};
