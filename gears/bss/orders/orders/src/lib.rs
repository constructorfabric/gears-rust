//! The Orders gear: the transition engine, draft authoring and draft reads (Stage 2).
//!
//! Eight catalog operations are delivered over REST and the local SDK (`OrdersV1`): the five
//! draft authoring writes (`create`, `patch_order`, `add_line`, `patch_line`, `remove_line`) and
//! the three draft reads (`get`, `list`, `list_lines`); see [`api::rest::DELIVERED_OPERATIONS`].
//! Every other catalog operation stays registered but undelivered: REST does not mount it and the
//! local SDK answers unavailable. The Orders-owned maintenance workers run when configured.
#![forbid(unsafe_code)]
// D-184: `AccessScope::allow_all` is a disallowed method (crate-local clippy.toml); the only
// permitted use is the read-only maintenance `DiscoveryScope` constructor.
#![deny(clippy::disallowed_methods)]
pub mod api;
/// The PEP: PDP requests, decision checks and the authorization types the storage adapters
/// accept. Hidden from docs and not a consumer contract: public only so the crate's integration
/// tests can check the operation census and action mapping, as `domain` below is public for the
/// boundary fixture.
#[doc(hidden)]
pub mod authz;
pub mod config;
/// The domain registries and rules. Hidden from docs and not a consumer contract: public only so
/// the crate's integration tests can execute the frozen S1-02 boundary fixture against the real
/// state table, guard registry and field classification (the BSS Products and Pricing gears
/// expose their modules the same way).
#[doc(hidden)]
pub mod domain;
pub mod gear;
pub mod gts;
/// Infrastructure adapters: engine, events, storage, capture and read services, workers.
mod infra;
pub use bss_orders_sdk as sdk;
pub use gear::BssOrdersGear;
