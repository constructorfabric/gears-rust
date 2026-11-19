// Created: 2026-09-23 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Configuration for the Vault / `OpenBao` credential backend.
//!
//! The plugin authenticates with a Vault token only. The token comes from
//! exactly one of three sources (`token`, `token_file`, `token_env`); see
//! [`VaultCredStorePluginConfig::validate`] for the rules enforced before the
//! plugin starts.
use std::fmt;

use anyhow::{bail, ensure};
use serde::Deserialize;
use toolkit::var_expand::{ExpandVars as ExpandVarsTrait, ExpandVarsError};

/// Default for [`RetryConfig::max_attempts`].
pub const DEFAULT_RETRY_MAX_ATTEMPTS: u32 = 3;
/// Upper bound accepted for [`RetryConfig::max_attempts`].
pub const MAX_RETRY_MAX_ATTEMPTS: u32 = 10;
/// Default for [`RetryConfig::base_delay_ms`].
pub const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 100;
/// Longest pause between two attempts, in milliseconds. It caps the
/// exponential growth and is the upper bound accepted for
/// [`RetryConfig::base_delay_ms`].
pub const MAX_RETRY_DELAY_MS: u64 = 2_000;

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

/// Retry policy for idempotent operations (`get`, `delete_key`, `destroy`).
/// `put` is never retried, whatever these values are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetryConfig {
    /// Total number of attempts per operation, the first one included
    /// (`1` disables retries). Allowed range: `1..=10`.
    pub max_attempts: u32,

    /// Pause before the second attempt, in milliseconds; it doubles for every
    /// further attempt and never exceeds two seconds. Allowed range:
    /// `1..=2000`.
    pub base_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_RETRY_MAX_ATTEMPTS,
            base_delay_ms: DEFAULT_RETRY_BASE_DELAY_MS,
        }
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

    /// Base URL of the Vault / `OpenBao` server, e.g. `http://127.0.0.1:8200`
    /// or `https://vault.internal:8200`. TLS is whatever `reqwest` does by
    /// default; the plugin has no TLS settings.
    pub address: String,

    /// Vault token, inline. Supports `${VAR}` expansion from the process
    /// environment; never logged. One of `token`, `token_file`, `token_env`.
    #[expand_vars]
    pub token: Option<VaultToken>,

    /// Path of a file holding the Vault token (for example the sink of a
    /// Vault Agent sidecar). Read on first use and again when Vault answers
    /// `403`, so a rotated token is picked up without a restart. Supports
    /// `${VAR}` expansion. One of `token`, `token_file`, `token_env`.
    #[expand_vars]
    pub token_file: Option<String>,

    /// Name of an environment variable holding the Vault token, read when the
    /// plugin starts. One of `token`, `token_file`, `token_env`.
    pub token_env: Option<String>,

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

    /// Retry policy for idempotent operations.
    pub retry: RetryConfig,
}

impl Default for VaultCredStorePluginConfig {
    fn default() -> Self {
        Self {
            vendor: "openbao".to_owned(),
            priority: 100,
            address: "http://127.0.0.1:8200".to_owned(),
            token: None,
            token_file: None,
            token_env: None,
            mount: "secret".to_owned(),
            path_prefix: "credstore".to_owned(),
            namespace: None,
            timeout_secs: 5,
            retry: RetryConfig::default(),
        }
    }
}

impl VaultCredStorePluginConfig {
    /// Checks the configuration without touching the network or the token
    /// source.
    ///
    /// * exactly one of `token`, `token_file`, `token_env` is set, and it is
    ///   not empty;
    /// * `address` is a non-empty `http://` or `https://` URL;
    /// * `mount` and `path_prefix` are non-empty `/`-separated paths with no
    ///   leading or trailing slash, no empty segment and no whitespace, `?`
    ///   or `#`;
    /// * `namespace`, when set, is not blank;
    /// * `timeout_secs` is positive;
    /// * `retry.max_attempts` is in `1..=10` and `retry.base_delay_ms` in
    ///   `1..=2000`.
    ///
    /// # Errors
    /// An error naming the offending key. It never contains the token.
    pub fn validate(&self) -> anyhow::Result<()> {
        self.validate_token_source()?;
        self.validate_addressing()?;
        ensure!(
            self.timeout_secs > 0,
            "`timeout_secs` must be greater than 0"
        );
        ensure!(
            (1..=MAX_RETRY_MAX_ATTEMPTS).contains(&self.retry.max_attempts),
            "`retry.max_attempts` must be in 1..={MAX_RETRY_MAX_ATTEMPTS} (got {})",
            self.retry.max_attempts
        );
        ensure!(
            (1..=MAX_RETRY_DELAY_MS).contains(&self.retry.base_delay_ms),
            "`retry.base_delay_ms` must be in 1..={MAX_RETRY_DELAY_MS} (got {})",
            self.retry.base_delay_ms
        );
        Ok(())
    }

    fn validate_token_source(&self) -> anyhow::Result<()> {
        let set: Vec<&str> = [
            ("token", self.token.is_some()),
            ("token_file", self.token_file.is_some()),
            ("token_env", self.token_env.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, is_set)| is_set.then_some(name))
        .collect();
        match set.as_slice() {
            [] => bail!("exactly one of `token`, `token_file`, `token_env` must be set (none is)"),
            [_] => {}
            many => bail!(
                "exactly one of `token`, `token_file`, `token_env` must be set (found {})",
                many.join(", ")
            ),
        }
        if let Some(token) = &self.token {
            ensure!(
                !token.expose().trim().is_empty(),
                "`token` is empty (is the variable it expands from empty?)"
            );
        }
        if let Some(path) = &self.token_file {
            ensure!(!path.trim().is_empty(), "`token_file` is empty");
        }
        if let Some(name) = &self.token_env {
            ensure!(!name.trim().is_empty(), "`token_env` is empty");
        }
        Ok(())
    }

    fn validate_addressing(&self) -> anyhow::Result<()> {
        let address = self.address.trim();
        ensure!(!address.is_empty(), "`address` must not be empty");
        ensure!(
            address.starts_with("http://") || address.starts_with("https://"),
            "`address` must start with http:// or https://"
        );
        validate_path("mount", &self.mount)?;
        validate_path("path_prefix", &self.path_prefix)?;
        if let Some(ns) = &self.namespace {
            ensure!(
                !ns.trim().is_empty(),
                "`namespace` must not be blank when set"
            );
        }
        Ok(())
    }
}

/// A KV v2 mount or prefix: non-empty segments joined by single slashes.
fn validate_path(key: &str, value: &str) -> anyhow::Result<()> {
    ensure!(!value.is_empty(), "`{key}` must not be empty");
    ensure!(
        !value.starts_with('/') && !value.ends_with('/'),
        "`{key}` must not start or end with a slash"
    );
    ensure!(
        value.split('/').all(|segment| !segment.is_empty()),
        "`{key}` must not contain an empty path segment"
    );
    ensure!(
        !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '?' || c == '#'),
        "`{key}` must not contain whitespace, `?` or `#`"
    );
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
