//! In-process data-access API layer: the gear's local implementation of the
//! `bss-ledger-sdk` `ClientHub` contracts.

pub mod local_client;
pub mod rest;

pub use local_client::LedgerLocalClient;
