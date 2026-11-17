//! Gear configuration (DESIGN §4 Configuration).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use toolkit_db::odata::sea_orm_filter::LimitCfg;

/// Largest accepted `chain_cache_ttl_seconds`.
pub const MAX_CHAIN_CACHE_TTL_SECONDS: u64 = 300;

/// Configuration keys of the `config` section; absent keys take the defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRegistryConfig {
    /// Chain cache TTL; 0 disables the cache; at most 300.
    #[serde(default = "default_chain_cache_ttl_seconds")]
    pub chain_cache_ttl_seconds: u64,
    /// Chain cache capacity (entries).
    #[serde(default = "default_chain_cache_max_entries")]
    pub chain_cache_max_entries: usize,
    /// Page size when the request has no limit.
    #[serde(default = "default_page_size")]
    pub default_page_size: u32,
    /// Largest accepted page size.
    #[serde(default = "default_max_page_size")]
    pub max_page_size: u32,
    /// Size limit for each JSON document (`provider_settings`, `metadata`,
    /// `additional_info`, `default_parameters`).
    #[serde(default = "default_max_json_document_bytes")]
    pub max_json_document_bytes: usize,
    /// Deadline for one PDP decision.
    #[serde(default = "default_pdp_deadline_ms")]
    pub pdp_deadline_ms: u64,
    /// Deadline for one `get_ancestors` call.
    #[serde(default = "default_tenant_resolver_deadline_ms")]
    pub tenant_resolver_deadline_ms: u64,
    /// Deadline for one Types Registry schema read.
    #[serde(default = "default_types_registry_deadline_ms")]
    pub types_registry_deadline_ms: u64,
    /// Deadline for one `CredStore` record read in the `secret_ref` ownership
    /// check.
    #[serde(default = "default_credstore_deadline_ms")]
    pub credstore_deadline_ms: u64,
    /// Largest value list in one PDP row-constraint filter.
    #[serde(default = "default_max_pdp_constraint_values")]
    pub max_pdp_constraint_values: usize,
}

impl Default for ModelRegistryConfig {
    fn default() -> Self {
        Self {
            chain_cache_ttl_seconds: default_chain_cache_ttl_seconds(),
            chain_cache_max_entries: default_chain_cache_max_entries(),
            default_page_size: default_page_size(),
            max_page_size: default_max_page_size(),
            max_json_document_bytes: default_max_json_document_bytes(),
            pdp_deadline_ms: default_pdp_deadline_ms(),
            tenant_resolver_deadline_ms: default_tenant_resolver_deadline_ms(),
            types_registry_deadline_ms: default_types_registry_deadline_ms(),
            credstore_deadline_ms: default_credstore_deadline_ms(),
            max_pdp_constraint_values: default_max_pdp_constraint_values(),
        }
    }
}

const fn default_chain_cache_ttl_seconds() -> u64 {
    30
}
const fn default_chain_cache_max_entries() -> usize {
    100_000
}
const fn default_page_size() -> u32 {
    20
}
const fn default_max_page_size() -> u32 {
    100
}
const fn default_max_json_document_bytes() -> usize {
    65_536
}
const fn default_pdp_deadline_ms() -> u64 {
    1000
}
const fn default_tenant_resolver_deadline_ms() -> u64 {
    1000
}
const fn default_types_registry_deadline_ms() -> u64 {
    3000
}
const fn default_credstore_deadline_ms() -> u64 {
    1000
}
const fn default_max_pdp_constraint_values() -> usize {
    1000
}

/// A configuration value that fails the start checks.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid model-registry config: {0}")]
pub struct ConfigError(String);

impl ModelRegistryConfig {
    /// The start checks of DESIGN §4 Configuration.
    ///
    /// # Errors
    /// The first failing check.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.chain_cache_ttl_seconds > MAX_CHAIN_CACHE_TTL_SECONDS {
            return Err(ConfigError(format!(
                "chain_cache_ttl_seconds must be at most {MAX_CHAIN_CACHE_TTL_SECONDS}"
            )));
        }
        let positive: [(&str, u64); 8] = [
            (
                "chain_cache_max_entries",
                self.chain_cache_max_entries as u64,
            ),
            ("default_page_size", u64::from(self.default_page_size)),
            ("max_page_size", u64::from(self.max_page_size)),
            (
                "max_json_document_bytes",
                self.max_json_document_bytes as u64,
            ),
            ("pdp_deadline_ms", self.pdp_deadline_ms),
            (
                "tenant_resolver_deadline_ms",
                self.tenant_resolver_deadline_ms,
            ),
            (
                "types_registry_deadline_ms",
                self.types_registry_deadline_ms,
            ),
            ("credstore_deadline_ms", self.credstore_deadline_ms),
        ];
        for (key, value) in positive {
            if value == 0 {
                return Err(ConfigError(format!("{key} must be greater than 0")));
            }
        }
        if self.max_pdp_constraint_values == 0 {
            return Err(ConfigError(
                "max_pdp_constraint_values must be greater than 0".to_owned(),
            ));
        }
        if self.default_page_size > self.max_page_size {
            return Err(ConfigError(
                "default_page_size must be at most max_page_size".to_owned(),
            ));
        }
        Ok(())
    }

    /// Pagination bounds handed to every list query. Only meaningful after
    /// [`Self::validate`].
    #[must_use]
    pub fn page_limits(&self) -> PageLimits {
        let max = u64::from(self.max_page_size).max(1);
        PageLimits {
            default: u64::from(self.default_page_size).clamp(1, max),
            max,
        }
    }

    #[must_use]
    pub fn chain_cache_ttl(&self) -> Duration {
        Duration::from_secs(self.chain_cache_ttl_seconds)
    }

    #[must_use]
    pub fn pdp_deadline(&self) -> Duration {
        Duration::from_millis(self.pdp_deadline_ms)
    }

    #[must_use]
    pub fn tenant_resolver_deadline(&self) -> Duration {
        Duration::from_millis(self.tenant_resolver_deadline_ms)
    }

    #[must_use]
    pub fn types_registry_deadline(&self) -> Duration {
        Duration::from_millis(self.types_registry_deadline_ms)
    }

    #[must_use]
    pub fn credstore_deadline(&self) -> Duration {
        Duration::from_millis(self.credstore_deadline_ms)
    }
}

/// Pagination bounds; `default <= max` and both at least 1.
#[derive(Debug, Clone, Copy)]
pub struct PageLimits {
    default: u64,
    max: u64,
}

impl PageLimits {
    /// The bounds in the shape the `toolkit-db` pagination helpers take.
    #[must_use]
    pub fn limit_cfg(self) -> LimitCfg {
        LimitCfg {
            default: self.default,
            max: self.max,
        }
    }

    /// The effective limit for a requested page size, mirroring the clamp the
    /// pagination helpers apply. Used when a page is synthesized without a
    /// query.
    #[must_use]
    pub fn clamp(self, requested: Option<u64>) -> u64 {
        requested.unwrap_or(self.default).clamp(1, self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_are_rejected_and_absent_keys_default() {
        let cfg: ModelRegistryConfig =
            serde_json::from_value(serde_json::json!({ "chain_cache_ttl_seconds": 5 })).unwrap();
        assert_eq!(cfg.chain_cache_ttl_seconds, 5);
        assert_eq!(cfg.max_page_size, 100);
        let err = serde_json::from_value::<ModelRegistryConfig>(
            serde_json::json!({ "chain_cache_ttl_second": 5 }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("chain_cache_ttl_second"), "{err}");
    }

    #[test]
    fn defaults_match_design() {
        let cfg = ModelRegistryConfig::default();
        assert_eq!(cfg.chain_cache_ttl_seconds, 30);
        assert_eq!(cfg.chain_cache_max_entries, 100_000);
        assert_eq!(cfg.default_page_size, 20);
        assert_eq!(cfg.max_page_size, 100);
        assert_eq!(cfg.max_json_document_bytes, 65_536);
        assert_eq!(cfg.pdp_deadline_ms, 1000);
        assert_eq!(cfg.tenant_resolver_deadline_ms, 1000);
        assert_eq!(cfg.types_registry_deadline_ms, 3000);
        assert_eq!(cfg.credstore_deadline_ms, 1000);
        assert_eq!(cfg.max_pdp_constraint_values, 1000);
        cfg.validate().unwrap();
    }

    #[test]
    fn empty_and_partial_json_take_defaults() {
        let cfg: ModelRegistryConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.chain_cache_ttl_seconds, 30);
        let cfg: ModelRegistryConfig =
            serde_json::from_str(r#"{"chain_cache_ttl_seconds": 0, "pdp_deadline_ms": 5}"#)
                .unwrap();
        assert_eq!(cfg.chain_cache_ttl_seconds, 0);
        assert_eq!(cfg.pdp_deadline_ms, 5);
        assert_eq!(cfg.max_page_size, 100);
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_out_of_range_values() {
        let cases: Vec<(ModelRegistryConfig, &str)> = vec![
            (
                ModelRegistryConfig {
                    chain_cache_ttl_seconds: 301,
                    ..Default::default()
                },
                "chain_cache_ttl_seconds",
            ),
            (
                ModelRegistryConfig {
                    chain_cache_max_entries: 0,
                    ..Default::default()
                },
                "chain_cache_max_entries",
            ),
            (
                ModelRegistryConfig {
                    default_page_size: 0,
                    ..Default::default()
                },
                "default_page_size",
            ),
            (
                ModelRegistryConfig {
                    max_page_size: 0,
                    ..Default::default()
                },
                "max_page_size",
            ),
            (
                ModelRegistryConfig {
                    max_json_document_bytes: 0,
                    ..Default::default()
                },
                "max_json_document_bytes",
            ),
            (
                ModelRegistryConfig {
                    pdp_deadline_ms: 0,
                    ..Default::default()
                },
                "pdp_deadline_ms",
            ),
            (
                ModelRegistryConfig {
                    tenant_resolver_deadline_ms: 0,
                    ..Default::default()
                },
                "tenant_resolver_deadline_ms",
            ),
            (
                ModelRegistryConfig {
                    types_registry_deadline_ms: 0,
                    ..Default::default()
                },
                "types_registry_deadline_ms",
            ),
            (
                ModelRegistryConfig {
                    credstore_deadline_ms: 0,
                    ..Default::default()
                },
                "credstore_deadline_ms",
            ),
            (
                ModelRegistryConfig {
                    max_pdp_constraint_values: 0,
                    ..Default::default()
                },
                "max_pdp_constraint_values",
            ),
            (
                ModelRegistryConfig {
                    default_page_size: 50,
                    max_page_size: 10,
                    ..Default::default()
                },
                "default_page_size",
            ),
        ];
        for (cfg, key) in cases {
            let err = cfg.validate().expect_err(key);
            assert!(err.to_string().contains(key), "{err}");
        }
    }

    #[test]
    fn page_limits_clamp() {
        let cfg = ModelRegistryConfig {
            default_page_size: 3,
            max_page_size: 7,
            ..Default::default()
        };
        let limits = cfg.page_limits();
        assert_eq!(limits.limit_cfg().default, 3);
        assert_eq!(limits.limit_cfg().max, 7);
        assert_eq!(limits.clamp(None), 3);
        assert_eq!(limits.clamp(Some(5)), 5);
        assert_eq!(limits.clamp(Some(99)), 7);
        assert_eq!(limits.clamp(Some(0)), 1);
    }
}
