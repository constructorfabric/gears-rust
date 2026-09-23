//! Configuration for the Vault / `OpenBao` credential backend.
use std::fmt;

use serde::Deserialize;
use toolkit::var_expand::{ExpandVars as ExpandVarsTrait, ExpandVarsError};

/// Wrapper around the Vault token so it never leaks through `Debug`,
/// `Display`, logging, or panic-formatter dumps, while still supporting
/// `${VAR}` env-var substitution via the `#[expand_vars]` derive (which only
/// substitutes into plain `String` fields — see `toolkit::var_expand`).
#[derive(Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct VaultToken(String);

impl VaultToken {
    /// Read the resolved token. Use only at the request boundary (setting
    /// the `X-Vault-Token` header); never log the returned value.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<&str> for VaultToken {
    /// Wraps a literal token. Mainly useful for tests that need a
    /// `VaultCredStorePluginConfig` without going through YAML/`Deserialize`.
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Debug for VaultToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl ExpandVarsTrait for VaultToken {
    fn expand_vars(&mut self) -> Result<(), ExpandVarsError> {
        self.0.expand_vars()
    }
}

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize, toolkit_macros::ExpandVars)]
#[serde(default, deny_unknown_fields)]
pub struct VaultCredStorePluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,

    /// Base URL of the Vault / `OpenBao` server, e.g. `http://127.0.0.1:8200`.
    pub address: String,

    /// Vault token sent as `X-Vault-Token`. Supports `${VAR}` expansion from
    /// the process environment; never logged.
    #[expand_vars]
    pub token: VaultToken,

    /// KV v2 secrets-engine mount point.
    pub mount: String,

    /// Path segment under the mount that all credstore values are written
    /// under, so the plugin never collides with unrelated secrets sharing
    /// the same mount.
    pub path_prefix: String,

    /// Optional Vault Enterprise / `OpenBao` namespace, sent as
    /// `X-Vault-Namespace` when set.
    pub namespace: Option<String>,

    /// Per-request HTTP timeout, in seconds.
    pub timeout_secs: u64,
}

impl Default for VaultCredStorePluginConfig {
    fn default() -> Self {
        Self {
            vendor: "openbao".to_owned(),
            priority: 100,
            address: "http://127.0.0.1:8200".to_owned(),
            token: VaultToken::default(),
            mount: "secret".to_owned(),
            path_prefix: "credstore".to_owned(),
            namespace: None,
            timeout_secs: 5,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
