use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SettingsConfig {
    #[serde(default = "default_max_field_length")]
    pub max_field_length: usize,

    /// How long one call to the deployment's `SettingsOwnerResolver` may take
    /// before the request fails with `ServiceUnavailable`. Unused when no
    /// resolver is registered.
    #[serde(default = "default_owner_resolver_timeout_ms")]
    pub owner_resolver_timeout_ms: u64,
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            max_field_length: default_max_field_length(),
            owner_resolver_timeout_ms: default_owner_resolver_timeout_ms(),
        }
    }
}

fn default_max_field_length() -> usize {
    100
}

fn default_owner_resolver_timeout_ms() -> u64 {
    2000
}

/// Upper bound on `owner_resolver_timeout_ms`. Longer than this, the timeout
/// no longer protects the request path it exists to protect.
pub const MAX_OWNER_RESOLVER_TIMEOUT_MS: u64 = 30_000;

impl SettingsConfig {
    /// Reject values that would break the gear at request time rather than
    /// at startup, where a typo is cheap to notice.
    ///
    /// # Errors
    ///
    /// When `owner_resolver_timeout_ms` is 0 (every resolver call would time
    /// out at once) or above [`MAX_OWNER_RESOLVER_TIMEOUT_MS`].
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.owner_resolver_timeout_ms == 0
            || self.owner_resolver_timeout_ms > MAX_OWNER_RESOLVER_TIMEOUT_MS
        {
            anyhow::bail!(
                "simple-user-settings: owner_resolver_timeout_ms must be between 1 and \
                 {MAX_OWNER_RESOLVER_TIMEOUT_MS}, got {}",
                self.owner_resolver_timeout_ms
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_valid() {
        SettingsConfig::default().validate().expect("defaults");
    }

    #[test]
    fn a_zero_or_oversized_resolver_timeout_is_refused() {
        for bad in [0, MAX_OWNER_RESOLVER_TIMEOUT_MS + 1] {
            let cfg = SettingsConfig {
                owner_resolver_timeout_ms: bad,
                ..SettingsConfig::default()
            };
            assert!(cfg.validate().is_err(), "{bad} must be refused");
        }
        for good in [1, MAX_OWNER_RESOLVER_TIMEOUT_MS] {
            let cfg = SettingsConfig {
                owner_resolver_timeout_ms: good,
                ..SettingsConfig::default()
            };
            cfg.validate().expect("within bounds");
        }
    }
}
