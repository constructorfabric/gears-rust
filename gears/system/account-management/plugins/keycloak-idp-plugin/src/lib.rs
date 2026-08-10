//! VHP `IdP` Plugin (Keycloak) — implements `account_management_sdk::IdpPluginClient`.
//!
//! Spec: [`docs/DESIGN.md`](../docs/DESIGN.md) / [`docs/PRD.md`](../docs/PRD.md)
//! adjacent to this crate.

// Ported verbatim from vhp-core (`crates/gears/plugins/vp-idp-plugin`), which
// builds under a laxer clippy profile. These style lints are allowed crate-wide
// to keep the port diffable against the vhp-core original; the same pattern is
// used by other ported crates (see `chat-engine`, `mini-chat`).
#![allow(
    clippy::non_ascii_literal,
    clippy::str_to_string,
    clippy::redundant_pub_crate,
    clippy::cognitive_complexity,
    clippy::let_underscore_must_use
)]

// `config` stays `pub` so the upstream AM-SDK conformance harness (once
// it lands — see `tests/contract_am_sdk.rs`) can build a test plugin via
// the typed config. Everything else is implementation detail reached
// through the two re-exports below and `Arc<dyn IdpPluginClient>` in
// ClientHub.
pub mod config;
pub(crate) mod domain;
pub(crate) mod idp_impl;
pub(crate) mod infra;
pub(crate) mod module;
pub(crate) mod sp_impl;

pub use idp_impl::KeycloakIdpPlugin;
pub use module::VpIdpPluginGear;
