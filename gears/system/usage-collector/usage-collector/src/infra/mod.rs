//! Infrastructure adapters for the usage-collector module.
//!
//! Houses the host-only REST error-envelope lift. The SDK crate stays
//! `toolkit-canonical-errors`-free; the RFC-9457 `Problem` envelope is
//! produced exclusively in this host crate.

pub mod sdk_error_mapping;
