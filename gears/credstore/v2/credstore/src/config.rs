// Updated: 2026-10-06 by Constructor Tech
//! Validated credential-store configuration.
//!
//! Controls backend plugin selection (vendor), the collection-read caps and
//! the secret-write intent lease. Unknown keys are rejected
//! (`deny_unknown_fields`).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CredStoreConfig {
    pub vendor: String,
    pub list: ListCfg,
    pub write: WriteCfg,
}

impl Default for CredStoreConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            list: ListCfg::default(),
            write: WriteCfg::default(),
        }
    }
}

/// Settings for the collection read (`GET /credstore/v1/credentials`,
/// ADR-0005/ADR-0004): the page-size cap, `list.max_limit`, documented in
/// DESIGN §4.3.1.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListCfg {
    /// Maximum `limit`/`$top` for a page; a caller-supplied
    /// value above this is rejected (400 `INVALID_LIMIT`) rather than
    /// silently clamped.
    pub max_limit: u64,
}

impl Default for ListCfg {
    fn default() -> Self {
        Self { max_limit: 200 }
    }
}

/// Smallest accepted `write.intent_lease_secs`. The lease must be well
/// above the longest time the value store may still apply a request; the
/// gear cannot see the plugin's own per-call timeout, so that is a
/// deployment requirement, not checked in process.
pub const MIN_INTENT_LEASE_SECS: u64 = 60;

/// Settings for the secret-write protocol's write intents (ADR-0006): a
/// write announces itself in `credstore_write_intents` before `plugin.put`,
/// and an intent no writer retired within its lease is healed by a later
/// request that touches its record or reference.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WriteCfg {
    /// The time, on the database clock, after which an expired intent of a
    /// crashed writer may be healed by a later request. It must be well
    /// above the longest time the value store may still apply a request
    /// (a deployment requirement, not checked in process).
    /// Seconds, `>= MIN_INTENT_LEASE_SECS` (60).
    pub intent_lease_secs: u64,
}

impl Default for WriteCfg {
    fn default() -> Self {
        Self {
            intent_lease_secs: 300,
        }
    }
}

impl CredStoreConfig {
    /// # Errors
    /// Returns `Err` with a description if any field is invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.vendor.trim().is_empty() {
            return Err("vendor must be non-empty".to_owned());
        }
        if self.list.max_limit == 0 {
            return Err("list.max_limit must be > 0".to_owned());
        }
        if self.write.intent_lease_secs < MIN_INTENT_LEASE_SECS {
            return Err(format!(
                "write.intent_lease_secs must be >= {MIN_INTENT_LEASE_SECS}: the lease is the time \
                 after which the intent of a crashed writer may be healed, and it must be well \
                 above the longest time the value store may still apply a request"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CredStoreConfig, MIN_INTENT_LEASE_SECS, WriteCfg};

    #[test]
    fn default_config_is_valid() {
        let cfg = CredStoreConfig::default();
        // Must match the backend plugin's default vendor (static-credstore-plugin
        // defaults to "constructorfabric"); otherwise a default-config deployment
        // resolves no backend plugin and 503s on every secret op.
        assert_eq!(cfg.vendor, "constructorfabric");
        assert_eq!(cfg.list.max_limit, 200);
        assert_eq!(cfg.write.intent_lease_secs, 300);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn deserializes_partial_config_with_defaults() {
        let cfg: CredStoreConfig =
            serde_json::from_str(r#"{"vendor":"acme","list":{"max_limit":5}}"#)
                .expect("deserialize");
        assert_eq!(cfg.vendor, "acme");
        assert_eq!(cfg.list.max_limit, 5);
    }

    #[test]
    fn deserializes_partial_write_config_with_defaults() {
        let cfg: CredStoreConfig =
            serde_json::from_str(r#"{"write":{"intent_lease_secs":60}}"#).expect("deserialize");
        assert_eq!(cfg.write.intent_lease_secs, 60);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_enforces_the_intent_lease_floor() {
        let with_lease = |intent_lease_secs: u64| CredStoreConfig {
            write: WriteCfg { intent_lease_secs },
            ..Default::default()
        };

        assert_eq!(MIN_INTENT_LEASE_SECS, 60);
        let err = with_lease(MIN_INTENT_LEASE_SECS - 1)
            .validate()
            .expect_err("a lease below the floor must be rejected");
        assert!(
            err.contains("write.intent_lease_secs must be >= 60"),
            "{err}"
        );
        assert!(with_lease(MIN_INTENT_LEASE_SECS).validate().is_ok());
        assert!(CredStoreConfig::default().validate().is_ok());
    }

    #[test]
    fn rejects_the_removed_secret_mode_cap_key() {
        let err = serde_json::from_str::<CredStoreConfig>(r#"{"list":{"secret_mode_cap":25}}"#)
            .expect_err("a removed list key must be rejected");
        assert!(err.to_string().contains("secret_mode_cap"));
    }

    #[test]
    fn rejects_an_unknown_write_key() {
        let err = serde_json::from_str::<CredStoreConfig>(r#"{"write":{"lease":60}}"#)
            .expect_err("unknown write key must be rejected");
        assert!(err.to_string().contains("lease"));
    }

    #[test]
    fn validate_rejects_each_invalid_field() {
        use super::ListCfg;

        let empty_vendor = CredStoreConfig {
            vendor: String::new(),
            ..Default::default()
        };
        assert!(empty_vendor.validate().is_err());

        let zero_max_limit = CredStoreConfig {
            list: ListCfg { max_limit: 0 },
            ..Default::default()
        };
        assert!(zero_max_limit.validate().is_err());

        let zero_intent_lease = CredStoreConfig {
            write: WriteCfg {
                intent_lease_secs: 0,
            },
            ..Default::default()
        };
        assert!(zero_intent_lease.validate().is_err());
    }
}
