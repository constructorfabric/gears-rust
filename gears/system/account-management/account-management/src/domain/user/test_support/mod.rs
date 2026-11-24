//! In-crate test support for the user-operations domain layer.
//!
//! Mirrors the layout used by [`crate::domain::tenant::test_support`]
//! and [`crate::domain::conversion::test_support`]: the fake plugin
//! lives in its own subgear, the parent re-exports the public
//! surface, and tests of the fake itself live alongside the user
//! service tests.
//!
//! Gated on `#[cfg(test)]` at the parent (`domain::user::mod`) site;
//! production binaries do not ship these types.

pub mod idp;
pub mod pending;

#[allow(
    unused_imports,
    reason = "Fake re-export anchors the public surface for service-level tests"
)]
pub use idp::{FakeIdpUserProvisioner, FakeUserOutcome};
pub use pending::FakePendingDeprovisions;
