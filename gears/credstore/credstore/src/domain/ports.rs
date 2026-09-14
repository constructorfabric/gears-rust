//! Outbound ports used by the credential-store domain.
//!
//! Infrastructure adapters implement backend plugin selection and metrics
//! recording without coupling domain services to concrete providers.

pub mod metrics;
pub mod plugin;
