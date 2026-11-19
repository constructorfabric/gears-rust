// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Direct construction of the plugin client, without the gear or the `ClientHub`.
//!
//! The gear's `init` and [`client_from_config`] go through the same
//! [`build_service`], so a client built by hand behaves exactly like the one
//! the gear registers: the same validation, the same HTTP transport, the same
//! retry policy. Neither talks to Vault while building.

use std::sync::Arc;

use credstore_sdk::CredStorePluginClientV2;

use crate::config::VaultCredStorePluginConfig;
use crate::domain::Service;
use crate::infra::http::ReqwestTransport;

/// Validates `cfg` and builds the service over the `reqwest` transport.
///
/// Returns the service and the name of the configuration key the token comes
/// from (`token`, `token_file` or `token_env`), for the gear's startup log.
///
/// # Errors
/// The configuration is invalid (see
/// [`VaultCredStorePluginConfig::validate`]), an inline or environment token
/// is missing or malformed, or the HTTP client cannot be built.
pub fn build_service(
    cfg: &VaultCredStorePluginConfig,
) -> anyhow::Result<(Arc<Service>, &'static str)> {
    cfg.validate()?;
    let transport = ReqwestTransport::from_config(cfg)?;
    let token_source = transport.token_source();
    Ok((
        Arc::new(Service::new(Arc::new(transport), cfg)),
        token_source,
    ))
}

/// Builds the Vault / `OpenBao` plugin client directly from its configuration,
/// for a binary that runs outside the gear (for example a value-migration tool
/// that writes into this plugin) and so has no `ClientHub` to resolve it from.
///
/// The configuration is checked exactly as the gear checks it at startup
/// ([`VaultCredStorePluginConfig::validate`]), and the client is the very one
/// the gear registers. Nothing else happens: no request is sent to Vault, no
/// instance is published to the types-registry, no gear is started. Use the
/// same `address`, `mount`, `path_prefix`, `namespace` and token as the running
/// gear, or the versions this client writes are not the ones the gear reads.
///
/// The configuration is used as given: `${VAR}` placeholders are **not**
/// expanded here (the gear expands them while it loads its `config` block), so
/// expand them first or build the struct in code.
///
/// # Errors
/// An error naming the offending key when the configuration is invalid, or
/// when an inline or `token_env` token is missing or malformed. It never
/// contains the token. A `token_file` is read on the first request, not here.
pub fn client_from_config(
    cfg: &VaultCredStorePluginConfig,
) -> anyhow::Result<Arc<dyn CredStorePluginClientV2>> {
    let (service, _token_source) = build_service(cfg)?;
    Ok(service)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "factory_tests.rs"]
mod factory_tests;
