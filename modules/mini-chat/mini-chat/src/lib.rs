#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// === MODULE DEFINITION ===
pub mod module;
pub use module::MiniChatModule;

// === PLUGIN MODULES ===
pub use infra::plugins::StaticMiniChatAuditPlugin;
pub use infra::plugins::StaticMiniChatModelPolicyPlugin;

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
