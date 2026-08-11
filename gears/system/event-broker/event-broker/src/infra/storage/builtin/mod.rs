//! Built-in storage backends bundled with the event broker
//! (`DESIGN.md:602-603`: in-memory, for dev/test).

pub mod memory;

pub use memory::InMemoryStorageBackend;
