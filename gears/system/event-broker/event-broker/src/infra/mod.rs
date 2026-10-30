//! Infrastructure layer: persistence, cluster coordination, and GTS
//! provisioning (`DESIGN.md` §1.3 Architecture Layers).

pub mod backend;
pub mod cluster;
pub mod dispatcher;
pub mod health;
pub mod loader;
pub mod metrics;
pub mod partition_cache;
pub mod specification;
pub mod storage;
pub mod wiring;
pub mod workers;

#[cfg(test)]
mod wiring_tests;
