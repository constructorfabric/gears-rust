//! Storage implementations for the Types Registry gear.

mod debug_diagnostics;
pub mod entity;
mod in_memory_repo;
pub mod migrations;
pub mod repo;
// The adapter behind the domain's persistence ports.
pub mod store;

pub use in_memory_repo::InMemoryGtsRepository;
pub use migrations::Migrator;
pub use store::Repos;
