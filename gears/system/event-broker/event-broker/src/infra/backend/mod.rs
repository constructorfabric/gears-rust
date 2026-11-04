//! The backend registry: every `backends` entry built once per process, and
//! the per-topic resolution over it (feature 0005 §2.1).

mod registry;

pub use registry::{BuildError, Registry};

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
