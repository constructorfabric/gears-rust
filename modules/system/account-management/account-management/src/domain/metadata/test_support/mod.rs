//! In-crate test support for the metadata domain layer.
//!
//! Mirrors the layout used by [`crate::domain::conversion::test_support`]:
//! the fake repo lives in its own submodule, the parent re-exports the
//! public surface, and the tests of the fake itself live under
//! [`repo_tests`] (compiled only under `#[cfg(test)]`).
//!
//! Gated on `#[cfg(test)]` at the parent (`domain::metadata::mod`)
//! site, matching `domain::conversion::test_support` — production
//! binaries do not ship these types.

pub mod repo;

#[allow(
    unused_imports,
    reason = "FakeMetadataRepo re-export anchors the public surface for later-phase service-level tests; phase 1 only uses the in-module test paths"
)]
pub use repo::FakeMetadataRepo;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "repo_tests.rs"]
mod repo_tests;
