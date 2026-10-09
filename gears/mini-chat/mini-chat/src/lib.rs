#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// === MODULE DEFINITION ===
pub mod gear;
pub use gear::MiniChatGear;

// === PLUGIN MODULES ===
pub use infra::plugins::StaticMiniChatAuditPlugin;
pub use infra::plugins::StaticMiniChatModelPolicyPlugin;

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod api;
/// Background worker configuration and lifecycle handles wired up by the gear.
pub(crate) mod background_workers;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;

/// Link-time GTS content shipped by the mini-chat gear (permission
/// instances today; future content goes here too).
pub(crate) mod gts;
