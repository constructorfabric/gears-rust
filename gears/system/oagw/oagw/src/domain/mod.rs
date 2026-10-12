pub(crate) mod cors;
/// Domain error types returned by services and mapped to API errors.
pub(crate) mod error;
pub(crate) mod gts_helpers;
/// Domain model: upstreams, routes, endpoints and request/query types.
pub(crate) mod model;
/// Auth, guard and transform plugin traits and their shared types.
pub(crate) mod plugin;
pub(crate) mod ports;
/// Rate limiting (token-bucket) logic and bucket-key construction.
pub(crate) mod rate_limit;
/// Repository traits for upstream and route persistence.
pub(crate) mod repo;
/// Control-plane and data-plane service traits and their implementations.
pub(crate) mod services;
/// SSRF guard that filters outbound IP addresses.
pub(crate) mod ssrf;
pub(crate) mod type_catalog;
pub(crate) mod type_provisioning;

#[cfg(any(test, feature = "test-utils"))]
pub(crate) mod test_support;
