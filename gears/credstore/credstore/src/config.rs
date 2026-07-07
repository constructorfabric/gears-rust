use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CredStoreConfig {
    pub vendor: String,
    pub hierarchy: HierarchyCfg,
    pub reaper: ReaperCfg,
}

impl Default for CredStoreConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            hierarchy: HierarchyCfg::default(),
            reaper: ReaperCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HierarchyCfg {
    pub ancestor_cache_ttl_secs: u64,
    /// Whether the shared tenant-closure table is co-located in this module's
    /// database. When true, advertise the tenant-hierarchy PDP capability so the
    /// PDP emits a structured subtree predicate resolved by a closure subquery.
    /// When false, the PDP pre-expands the subtree to a flat membership list,
    /// enforced without any local closure access. Defaults to false — opt in
    /// explicitly only where the closure is known to be co-located.
    pub tenant_closure_colocated: bool,
}

impl Default for HierarchyCfg {
    fn default() -> Self {
        Self {
            ancestor_cache_ttl_secs: 300,
            tenant_closure_colocated: false, // conservative: degraded unless opted in
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "serialized config keys; renaming would break existing configs"
)]
pub struct ReaperCfg {
    pub tick_secs: u64,
    pub provisioning_timeout_secs: u64,
    /// Age after which a stuck `deprovisioning` row is completed by the
    /// reaper (backend value deleted, row removed).
    pub deprovisioning_timeout_secs: u64,
}

impl Default for ReaperCfg {
    fn default() -> Self {
        Self {
            tick_secs: 60,
            provisioning_timeout_secs: 300,
            deprovisioning_timeout_secs: 300,
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
        if self.reaper.tick_secs == 0 {
            return Err("reaper.tick_secs must be > 0".to_owned());
        }
        if self.reaper.provisioning_timeout_secs == 0 {
            return Err("reaper.provisioning_timeout_secs must be > 0".to_owned());
        }
        if self.reaper.deprovisioning_timeout_secs == 0 {
            return Err("reaper.deprovisioning_timeout_secs must be > 0".to_owned());
        }
        if self.hierarchy.ancestor_cache_ttl_secs == 0 {
            return Err("hierarchy.ancestor_cache_ttl_secs must be > 0".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::CredStoreConfig;

    #[test]
    fn default_config_is_valid() {
        let cfg = CredStoreConfig::default();
        // Must match the backend plugin's default vendor (static-credstore-plugin
        // defaults to "constructorfabric"); otherwise a default-config deployment
        // resolves no backend plugin and 503s on every secret op.
        assert_eq!(cfg.vendor, "constructorfabric");
        assert_eq!(cfg.hierarchy.ancestor_cache_ttl_secs, 300);
        // Co-location defaults off: degraded (flat-In) mode unless opted in.
        assert!(!cfg.hierarchy.tenant_closure_colocated);
        assert_eq!(cfg.reaper.tick_secs, 60);
        assert_eq!(cfg.reaper.provisioning_timeout_secs, 300);
        assert_eq!(cfg.reaper.deprovisioning_timeout_secs, 300);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn deserializes_partial_config_with_defaults() {
        let cfg: CredStoreConfig =
            serde_json::from_str(r#"{"vendor":"acme","reaper":{"tick_secs":5}}"#)
                .expect("deserialize");
        assert_eq!(cfg.vendor, "acme");
        assert_eq!(cfg.reaper.tick_secs, 5);
        // Unspecified fields fall back to defaults.
        assert_eq!(cfg.reaper.provisioning_timeout_secs, 300);
        assert_eq!(cfg.reaper.deprovisioning_timeout_secs, 300);
        assert_eq!(cfg.hierarchy.ancestor_cache_ttl_secs, 300);
        assert!(!cfg.hierarchy.tenant_closure_colocated);
    }

    #[test]
    fn deserializes_explicit_tenant_closure_colocated_true() {
        let cfg: CredStoreConfig =
            serde_json::from_str(r#"{"hierarchy":{"tenant_closure_colocated":true}}"#)
                .expect("deserialize");
        // Explicit opt-in overrides the conservative default.
        assert!(cfg.hierarchy.tenant_closure_colocated);
        // Co-location is independent of validation (a bool is always valid).
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_each_invalid_field() {
        use super::{HierarchyCfg, ReaperCfg};

        let empty_vendor = CredStoreConfig {
            vendor: String::new(),
            ..Default::default()
        };
        assert!(empty_vendor.validate().is_err());

        let zero_tick = CredStoreConfig {
            reaper: ReaperCfg {
                tick_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(zero_tick.validate().is_err());

        let zero_timeout = CredStoreConfig {
            reaper: ReaperCfg {
                provisioning_timeout_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(zero_timeout.validate().is_err());

        let zero_deprov_timeout = CredStoreConfig {
            reaper: ReaperCfg {
                deprovisioning_timeout_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(zero_deprov_timeout.validate().is_err());

        let zero_ttl = CredStoreConfig {
            hierarchy: HierarchyCfg {
                ancestor_cache_ttl_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(zero_ttl.validate().is_err());
    }
}
