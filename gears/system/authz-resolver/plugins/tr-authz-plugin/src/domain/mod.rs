// Created: 2026-02-24 by Constructor Tech
// Updated: 2026-04-29 by Constructor Tech

pub mod client;
pub mod service;

#[cfg(test)]
pub(crate) mod test_support;

pub use service::Service;
