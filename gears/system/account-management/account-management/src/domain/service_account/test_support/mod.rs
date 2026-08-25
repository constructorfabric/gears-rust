//! In-crate test support for the service-account domain layer.
//!
//! Mirrors [`crate::domain::user::test_support`]: the fake provider
//! lives in its own submodule and the parent re-exports the public
//! surface.
//!
//! Gated on `#[cfg(test)]` at the parent
//! (`domain::service_account::mod`) site; production binaries do not
//! ship these types.

pub mod idp;

#[allow(
    unused_imports,
    reason = "Fake re-export anchors the public surface for service-level tests"
)]
pub use idp::{FakeServiceAccountOutcome, FakeServiceAccountProvider};
