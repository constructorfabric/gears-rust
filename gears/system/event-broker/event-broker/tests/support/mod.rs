//! Test-binary-local plumbing for the engine-matrix tests
//! (`tests/*_backends_test.rs`): the database servers and the per-engine
//! wiring of a harness. Everything that is harness API lives in the library's
//! `test_support` instead.

// Each `_backends_test` binary includes this module and uses a subset of it.
#![allow(dead_code, reason = "each test binary uses a subset of the plumbing")]

pub mod containers;
pub mod matrix;
pub mod stored;
