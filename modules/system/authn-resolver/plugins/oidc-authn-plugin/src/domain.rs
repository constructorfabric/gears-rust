//! Domain logic for the Oidc `AuthN` plugin.
//!
//! This layer owns token validation, claim mapping, and domain-specific errors.

pub mod authenticate;
pub mod claim_mapper;
pub mod error;
pub mod metrics;
pub mod ports;
pub mod token_type;
pub mod validator;
