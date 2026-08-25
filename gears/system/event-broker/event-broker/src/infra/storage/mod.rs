//! Storage backend plugin resolution
//! (`DESIGN.md` §"Storage Backend Plugin System").

pub mod builtin;
pub mod registry;

pub use registry::StorageBackendRegistry;
