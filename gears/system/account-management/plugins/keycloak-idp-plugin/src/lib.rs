//! Keycloak `IdP` Plugin (Keycloak) — implements `account_management_sdk::IdpPluginClient`.
//!
//! Spec: [`docs/PRD.md`](../docs/PRD.md) and [`docs/DESIGN.md`](../docs/DESIGN.md).

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
pub(crate) mod sa_impl;

pub use idp_impl::KeycloakIdpPlugin;
pub use module::KeycloakIdpPluginGear;
