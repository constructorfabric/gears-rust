//! Validated credential-store configuration.
//!
//! Controls backend plugin selection, hierarchy-cache lifetime, the
//! collection-read caps and the secret-write intent lease. ADR-0006 withdraws the `reaper` block (`tick_secs`,
//! `provisioning_timeout_secs`, `deprovisioning_timeout_secs`) and there is
//! no `gc` block either: the gear has no resident loop and no maintenance
//! job. `deny_unknown_fields` makes an old `reaper:` or `gc:` key a hard
//! config-validation failure rather than a silently ignored no-op.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CredStoreConfig {
    pub vendor: String,
    pub hierarchy: HierarchyCfg,
    pub list: ListCfg,
    pub write: WriteCfg,
}

impl Default for CredStoreConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            hierarchy: HierarchyCfg::default(),
            list: ListCfg::default(),
            write: WriteCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HierarchyCfg {
    pub ancestor_cache_ttl_secs: u64,
}

impl Default for HierarchyCfg {
    fn default() -> Self {
        Self {
            ancestor_cache_ttl_secs: 300,
        }
    }
}

/// Settings for the collection read (`GET /credstore/v1/credentials`,
/// ADR-0005/ADR-0004): the metadata-mode page-size cap and the secret-mode
/// (`$select` containing `secret`) match-set cap. Both keys, `list.max_limit`
/// and `list.secret_mode_cap`, are documented in DESIGN §4.3.2.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListCfg {
    /// Maximum `limit`/`$top` for a metadata-mode page; a caller-supplied
    /// value above this is rejected (400 `INVALID_LIMIT`) rather than
    /// silently clamped.
    pub max_limit: u64,
    /// Cap on how many references a secret-mode (`$select=…,secret`) request
    /// may match. Enforced by fetching `cap + 1` candidate references and
    /// failing closed with `400 TOO_MANY_MATCHES` if the `(cap + 1)`th
    /// appears — never by a `COUNT` query.
    pub secret_mode_cap: u64,
}

impl Default for ListCfg {
    fn default() -> Self {
        Self {
            max_limit: 200,
            secret_mode_cap: 25,
        }
    }
}

/// Smallest accepted `write.intent_lease_secs`. The lease must comfortably
/// exceed a plugin `put`: the writer refuses to start a put after half the
/// lease, and the gear cannot see the plugin's own per-call timeout.
pub const MIN_INTENT_LEASE_SECS: u64 = 60;

/// Settings for the secret-write protocol's write intents (ADR-0006): a
/// write announces itself in `credstore_write_intents` before `plugin.put`,
/// and an intent no writer retired within its lease is reclaimed.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WriteCfg {
    /// How long, on the database clock, an intent is protected from
    /// reclaim. A writer that finds more than half of it spent before its
    /// `put` abandons the write (`503`), which bounds how long a stalled
    /// writer can still land a version after its intent was reclaimed.
    /// Seconds, `>= MIN_INTENT_LEASE_SECS` (60).
    pub intent_lease_secs: u64,
    /// Maximum expired intents one reclaim pass handles (startup passes
    /// repeat until a pass returns fewer; every secret write runs one pass
    /// after it finishes). `> 0`.
    pub reclaim_batch: u64,
}

impl Default for WriteCfg {
    fn default() -> Self {
        Self {
            intent_lease_secs: 300,
            reclaim_batch: 16,
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
        if self.hierarchy.ancestor_cache_ttl_secs == 0 {
            return Err("hierarchy.ancestor_cache_ttl_secs must be > 0".to_owned());
        }
        if self.list.max_limit == 0 {
            return Err("list.max_limit must be > 0".to_owned());
        }
        if self.list.secret_mode_cap == 0 {
            return Err("list.secret_mode_cap must be > 0".to_owned());
        }
        if self.write.intent_lease_secs < MIN_INTENT_LEASE_SECS {
            return Err(format!(
                "write.intent_lease_secs must be >= {MIN_INTENT_LEASE_SECS}: the lease must \
                 comfortably exceed a plugin put (the writer refuses to start a put after \
                 half the lease, and the gear cannot see the plugin's own per-call timeout)"
            ));
        }
        if self.write.reclaim_batch == 0 {
            return Err("write.reclaim_batch must be > 0".to_owned());
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
        assert_eq!(cfg.hierarchy.ancestor_cache_ttl_secs, 300);
        assert_eq!(cfg.list.max_limit, 200);
        assert_eq!(cfg.list.secret_mode_cap, 25);
        assert_eq!(cfg.write.intent_lease_secs, 300);
        assert_eq!(cfg.write.reclaim_batch, 16);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn deserializes_partial_config_with_defaults() {
        let cfg: CredStoreConfig =
            serde_json::from_str(r#"{"vendor":"acme","list":{"max_limit":5}}"#)
                .expect("deserialize");
        assert_eq!(cfg.vendor, "acme");
        assert_eq!(cfg.list.max_limit, 5);
        // Unspecified fields fall back to defaults.
        assert_eq!(cfg.list.secret_mode_cap, 25);
        assert_eq!(cfg.hierarchy.ancestor_cache_ttl_secs, 300);
    }

    #[test]
    fn deserializes_partial_list_config_with_defaults() {
        let cfg: CredStoreConfig =
            serde_json::from_str(r#"{"list":{"max_limit":50}}"#).expect("deserialize");
        assert_eq!(cfg.list.max_limit, 50);
        // Unspecified fields fall back to defaults.
        assert_eq!(cfg.list.secret_mode_cap, 25);
    }

    #[test]
    fn deserializes_partial_write_config_with_defaults() {
        let cfg: CredStoreConfig =
            serde_json::from_str(r#"{"write":{"intent_lease_secs":60}}"#).expect("deserialize");
        assert_eq!(cfg.write.intent_lease_secs, 60);
        // Unspecified fields fall back to defaults.
        assert_eq!(cfg.write.reclaim_batch, 16);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_enforces_the_intent_lease_floor() {
        let with_lease = |intent_lease_secs: u64| CredStoreConfig {
            write: WriteCfg {
                intent_lease_secs,
                ..Default::default()
            },
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
    fn rejects_an_unknown_write_key() {
        let err = serde_json::from_str::<CredStoreConfig>(r#"{"write":{"lease":60}}"#)
            .expect_err("unknown write key must be rejected");
        assert!(err.to_string().contains("lease"));
    }

    #[test]
    fn validate_rejects_each_invalid_field() {
        use super::{HierarchyCfg, ListCfg};

        let empty_vendor = CredStoreConfig {
            vendor: String::new(),
            ..Default::default()
        };
        assert!(empty_vendor.validate().is_err());

        let zero_ttl = CredStoreConfig {
            hierarchy: HierarchyCfg {
                ancestor_cache_ttl_secs: 0,
            },
            ..Default::default()
        };
        assert!(zero_ttl.validate().is_err());

        let zero_max_limit = CredStoreConfig {
            list: ListCfg {
                max_limit: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(zero_max_limit.validate().is_err());

        let zero_secret_mode_cap = CredStoreConfig {
            list: ListCfg {
                secret_mode_cap: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(zero_secret_mode_cap.validate().is_err());

        let zero_intent_lease = CredStoreConfig {
            write: WriteCfg {
                intent_lease_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(zero_intent_lease.validate().is_err());

        let zero_reclaim_batch = CredStoreConfig {
            write: WriteCfg {
                reclaim_batch: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(zero_reclaim_batch.validate().is_err());
    }

    #[test]
    fn rejects_the_withdrawn_reaper_config_block() {
        // ADR-0006 withdraws the reaper outright, not a rename: an old
        // `reaper:` key must fail config validation rather than being
        // silently ignored (`deny_unknown_fields`).
        let err = serde_json::from_str::<CredStoreConfig>(
            r#"{"reaper":{"tick_secs":60,"provisioning_timeout_secs":300,"deprovisioning_timeout_secs":300}}"#,
        )
        .expect_err("reaper key must be rejected");
        assert!(err.to_string().contains("reaper"));
    }

    #[test]
    fn rejects_the_withdrawn_gc_config_block() {
        // ADR-0006 withdraws the maintenance job and its `gc` block.
        let err = serde_json::from_str::<CredStoreConfig>(
            r#"{"gc":{"pending_max_age_secs":3600,"batch_size":256}}"#,
        )
        .expect_err("gc key must be rejected");
        assert!(err.to_string().contains("gc"));
    }
}
