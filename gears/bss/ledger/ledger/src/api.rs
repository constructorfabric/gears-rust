//! Ledger integration boundaries.
//!
//! [`local_client`] implements the `bss-ledger-sdk` contracts registered in
//! `ClientHub`, while [`rest`] exposes the authenticated HTTP API and canonical
//! error responses.

pub mod local_client;
pub mod rest;

pub use local_client::LedgerLocalClient;
