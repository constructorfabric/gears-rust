// Created: 2026-10-03 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Infrastructure adapters: the `reqwest`-backed HTTP transport that
//! implements the domain's [`crate::domain::transport::VaultTransport`] port,
//! and the token source it authenticates with.

pub mod http;
pub mod token;
