//! Configuration for the api-contracts gear.
//!
//! Transport wiring is no longer carried here — it lives under
//! `gears.api-contracts.config.client_wiring.payment_api` and is parsed
//! by `#[toolkit::provides]` into a typed
//! [`ClientWiring`](toolkit_contract::wiring::ClientWiring).

use serde::Deserialize;

/// Gear configuration. Empty for now — kept for parity with the rest of
/// the example and for future gear-level (non-wiring) knobs.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiContractsConfig {}
