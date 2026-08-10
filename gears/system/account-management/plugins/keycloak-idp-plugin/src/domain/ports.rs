//! Domain ports (interfaces) — the abstractions the domain layer depends
//! on. Infra adapters in [`crate::infra`] implement these traits; the
//! domain layer never imports infra directly.

pub mod metrics;
