//! Infrastructure layer — source-plugin discovery.
//!
//! [`discovery::DiscoveringRateProvider`] is the adapter the gear registers: it
//! queries the types-registry over the wire, resolves scoped `ClientHub` clients,
//! and hands the resulting ordered ports to the domain layer's
//! [`crate::domain::composite::CompositeRateProvider`] to apply the fallback policy.

pub mod discovery;
