//! Infrastructure adapters (HTTP, external clients).
//!
//! `infra` is a private (`mod infra;` in `lib.rs`) implementation detail of the
//! crate. The `pub` on the submodules and re-exports below is therefore bounded
//! by `infra`'s own visibility — these types are reachable from siblings
//! (`module.rs`) and from within `infra`, but not from outside the crate.
//! Clippy's `redundant_pub_crate` lint enforces plain `pub` rather than
//! `pub(crate)` in this position.

pub use bearer_token_auth_layer::BearerTokenAuthLayer;
pub use rest_client::UsageCollectorRestClient;

mod bearer_token_auth_layer;
mod rest_client;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod test_support;
