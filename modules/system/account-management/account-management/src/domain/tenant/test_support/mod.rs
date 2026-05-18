//! In-crate test support surface — split across submodules so each
//! fake lives in its own file:
//!
//! * [`repo`] — `FakeTenantRepo`, `RepoState`, claim-invariant tests.
//! * [`idp`] — `FakeIdpProvisioner` plus the four-outcome stubs that
//!   drive its provision / deprovision branches.
//! * [`auth`] — `MockAuthZResolver` and the `mock_enforcer` factory.
//!
//! Everything is gated on `cfg(test)` (the parent `mod test_support;`
//! declaration in `domain::tenant::mod` carries the gate); production
//! binaries do not ship these types.

pub mod auth;
pub mod idp;
pub mod repo;

pub use auth::{
    constraint_bearing_enforcer, deny_all_enforcer, mock_enforcer, schema_selective_enforcer,
    schema_unavailable_enforcer,
};
pub use idp::{FakeDeprovisionOutcome, FakeIdpProvisioner, FakeOutcome};
pub use repo::FakeTenantRepo;
