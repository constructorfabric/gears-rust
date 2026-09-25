//! Configuration for the Types Registry gear.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer, de};

use crate::domain::policy::{PolicyConfigError, RegistrationPolicy};
use crate::infra::cache::{CacheConfig, DEFAULT_CACHE_CAPACITY, DEFAULT_CACHE_TTL};
pub use crate::policy_config::PolicyEntry;

/// Lease time reserved after admission.
pub const LEASE_HEADROOM: Duration = Duration::from_secs(2);

/// A discovery page carries the same documents a batch read does, so it shares
/// that ceiling (C10).
pub const PAGE_SIZE_CEILING: u32 = 100;

/// Largest attempt budget that fits the outbox's signed counter.
const MAX_DELIVERY_ATTEMPTS: u32 = i16::MAX as u32 - 1;

/// Configuration for the Types Registry gear.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TypesRegistryConfig {
    /// Fields to check for GTS entity ID (in order of priority).
    /// Default: `["$id", "gtsId", "id"]`
    pub entity_id_fields: Vec<String>,

    /// Fields to check for schema ID reference (in order of priority).
    /// Default: `["$schema", "gtsTid", "type"]`
    pub schema_id_fields: Vec<String>,

    /// Raw GTS entity JSON values to register at startup.
    ///
    /// Each entry must be a valid GTS entity with at least an `$id` (or
    /// `gtsId`/`id`) field. Entities are registered in order.
    #[serde(default)]
    pub entities: Vec<serde_json::Value>,

    /// In-process client tuning.
    #[serde(default)]
    pub local_client: LocalClientSettings,

    /// Allow ADR-0004 `force` for cross-minor checks; disabled by default.
    /// Acceptance and each worker pass check this setting. Intra-entity checks
    /// remain unwaivable.
    #[serde(default)]
    pub allow_compatibility_force: bool,

    /// Bounds on one request's work and on one document's size (SPEC §10.3).
    #[serde(default)]
    pub limits: Limits,

    /// New-entity allowlist by GTS region; `cf` is always allowed.
    #[serde(default)]
    pub registration_policy: BTreeMap<String, PolicyEntry>,

    /// Admission-worker tuning (SPEC §10.3).
    #[serde(default)]
    pub worker: WorkerSettings,

    /// Metrics naming configuration.
    #[serde(default)]
    pub metrics: MetricsConfig,
}

/// Metrics configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    /// Metric name prefix.
    #[serde(default)]
    pub prefix: String,
}

impl MetricsConfig {
    /// Resolve the effective prefix: explicit config value, or `snake_case(gear_name)`.
    #[must_use]
    pub fn effective_prefix(&self, gear_name: &str) -> String {
        let trimmed = self.prefix.trim();
        if trimmed.is_empty() {
            heck::ToSnakeCase::to_snake_case(gear_name)
        } else {
            trimmed.to_owned()
        }
    }
}

/// Request-work and document-size refusal thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Maximum canonical size of an authored document.
    pub authored_document: ByteSize,
    /// Largest resolved document the registry will materialize (§3.2).
    /// Enforced on the canonical bytes of each effective artifact at admission and refresh.
    pub resolved_document: ByteSize,
    /// Largest reference-resolution closure one candidate may need.
    /// Enforced before resolution, per candidate or refreshed schema, including its own document.
    /// Distinct documents count once; unrelated documents in a shared store do not count.
    pub resolution_closure: usize,
    /// Maximum batch size.
    pub batch_candidates: usize,
    /// Maximum dependents reached by one revision; also caps CTE depth (SPEC §4).
    pub activation_write_set: usize,
    /// Default `GET /entities` page size.
    pub page_size_default: u32,
    /// Maximum `GET /entities` page size, at most [`PAGE_SIZE_CEILING`].
    pub page_size_max: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            authored_document: ByteSize::from_bytes(256 * 1024),
            resolved_document: ByteSize::from_bytes(1024 * 1024),
            resolution_closure: 64,
            batch_candidates: 100,
            activation_write_set: 512,
            page_size_default: 50,
            page_size_max: 100,
        }
    }
}

/// Admission-worker tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorkerSettings {
    /// Positive wall-clock admission budget. Expiry causes redelivery.
    /// This does not guarantee spawned work finishes before gear shutdown.
    #[serde(with = "toolkit_utils::humantime_serde")]
    pub operation_timeout: Duration,
    /// Revalidation attempts before failure; `1` allows no retry.
    pub max_revalidation_attempts: u32,
    /// Admission attempts before remaining items are terminalized as `system_failure`
    /// and the operation completes; `1` disables retries.
    pub max_delivery_attempts: u32,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            operation_timeout: Duration::from_mins(5),
            max_revalidation_attempts: 8,
            max_delivery_attempts: 8,
        }
    }
}

/// Binary byte count such as `256KB`, `1MB` or `1MiB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(usize);

impl ByteSize {
    #[must_use]
    pub const fn from_bytes(bytes: usize) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn bytes(self) -> usize {
        self.0
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bytes", self.0)
    }
}

impl ByteSize {
    /// Parse the `256KB` form. Returns the reason on failure so the caller can
    /// name the offending key.
    fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        let split = trimmed
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(trimmed.len());
        let (digits, suffix) = trimmed.split_at(split);
        if digits.is_empty() {
            return Err(format!("'{trimmed}' does not start with a number"));
        }
        // Preserve numeric overflow as the parse error.
        let value: usize = digits
            .parse()
            .map_err(|e| format!("'{digits}' is not a byte count: {e}"))?;
        let multiplier: usize = match suffix.trim().to_ascii_uppercase().as_str() {
            "" | "B" => 1,
            "KB" | "KIB" => 1024,
            "MB" | "MIB" => 1024 * 1024,
            "GB" | "GIB" => 1024 * 1024 * 1024,
            other => return Err(format!("'{other}' is not a known unit")),
        };
        value
            .checked_mul(multiplier)
            .map(Self)
            .ok_or_else(|| format!("'{trimmed}' overflows a byte count"))
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl de::Visitor<'_> for Visitor {
            type Value = ByteSize;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a byte count, as an integer or a string like \"256KB\"")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<ByteSize, E> {
                usize::try_from(v)
                    .map(ByteSize)
                    .map_err(|_| E::custom(format!("{v} does not fit a byte count")))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<ByteSize, E> {
                usize::try_from(v)
                    .map(ByteSize)
                    .map_err(|_| E::custom(format!("{v} is not a byte count")))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<ByteSize, E> {
                ByteSize::parse(v).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// Settings for the in-process local client adapter.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct LocalClientSettings {
    /// Per-kind cache tuning. Defaults match
    /// [`DEFAULT_CACHE_CAPACITY`] / [`DEFAULT_CACHE_TTL`].
    pub cache: CacheSettings,
}

/// Per-kind cache settings.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct CacheSettings {
    /// Cache settings for the type-schema cache.
    pub type_schemas: SingleCacheSettings,
    /// Cache settings for the instance cache.
    pub instances: SingleCacheSettings,
}

/// Settings for a single LRU cache.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SingleCacheSettings {
    /// Maximum number of entries before LRU eviction. Clamped to `1` if `0`.
    pub capacity: usize,
    /// Maximum age of an entry before it's treated as a miss. Accepts a
    /// human-readable duration string (e.g. `"60s"`, `"2m"`); explicit
    /// `null` disables TTL entirely. Omitting the field falls back to
    /// [`DEFAULT_CACHE_TTL`].
    #[serde(with = "toolkit_utils::humantime_serde::option")]
    pub ttl: Option<Duration>,
}

impl Default for SingleCacheSettings {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_CACHE_CAPACITY,
            ttl: Some(DEFAULT_CACHE_TTL),
        }
    }
}

impl SingleCacheSettings {
    /// Converts to the infra-layer [`CacheConfig`].
    #[must_use]
    pub const fn to_cache_config(&self) -> CacheConfig {
        CacheConfig {
            capacity: self.capacity,
            ttl: self.ttl,
        }
    }
}

impl Default for TypesRegistryConfig {
    fn default() -> Self {
        Self {
            entity_id_fields: vec!["$id".to_owned(), "gtsId".to_owned(), "id".to_owned()],
            schema_id_fields: vec!["$schema".to_owned(), "gtsTid".to_owned(), "type".to_owned()],
            entities: Vec::new(),
            local_client: LocalClientSettings::default(),
            allow_compatibility_force: false,
            limits: Limits::default(),
            registration_policy: BTreeMap::new(),
            worker: WorkerSettings::default(),
            metrics: MetricsConfig::default(),
        }
    }
}

impl TypesRegistryConfig {
    /// Validate limits and compile the registration policy once.
    ///
    /// # Errors
    /// [`ConfigError::Policy`] for an unparsable region or vendor list, and
    /// [`ConfigError::Limits`] for an invalid limit, or [`ConfigError::Worker`]
    /// for an invalid worker setting.
    pub fn validate(&self) -> Result<RegistrationPolicy, ConfigError> {
        if self.limits.page_size_max > PAGE_SIZE_CEILING {
            return Err(ConfigError::Limits(format!(
                "limits.page_size_max ({}) exceeds {PAGE_SIZE_CEILING}",
                self.limits.page_size_max
            )));
        }
        if self.limits.page_size_default > self.limits.page_size_max {
            return Err(ConfigError::Limits(format!(
                "limits.page_size_default ({}) exceeds limits.page_size_max ({})",
                self.limits.page_size_default, self.limits.page_size_max
            )));
        }
        if self.limits.page_size_default == 0 || self.limits.page_size_max == 0 {
            return Err(ConfigError::Limits(
                "limits.page_size_default and limits.page_size_max must be positive".to_owned(),
            ));
        }
        // Zero would reject every batch or document.
        if self.limits.batch_candidates == 0 {
            return Err(ConfigError::Limits(
                "limits.batch_candidates must be positive: 0 refuses every request".to_owned(),
            ));
        }
        if self.limits.authored_document.bytes() == 0 {
            return Err(ConfigError::Limits(
                "limits.authored_document must be positive: 0 refuses every candidate".to_owned(),
            ));
        }
        if self.limits.resolved_document.bytes() == 0 {
            return Err(ConfigError::Limits(
                "limits.resolved_document must be positive".to_owned(),
            ));
        }
        if self.limits.resolution_closure == 0 {
            return Err(ConfigError::Limits(
                "limits.resolution_closure must be positive".to_owned(),
            ));
        }
        // Zero would reject every revision with dependents.
        if self.limits.activation_write_set == 0 {
            return Err(ConfigError::Limits(
                "limits.activation_write_set must be positive: 0 refuses every revision of a \
                 type anything depends on"
                    .to_owned(),
            ));
        }
        // At least one evaluation attempt is required.
        if self.worker.max_revalidation_attempts == 0 {
            return Err(ConfigError::Worker(
                "worker.max_revalidation_attempts must be positive: 0 refuses every candidate \
                 without evaluating it"
                    .to_owned(),
            ));
        }
        if self.worker.operation_timeout.is_zero() {
            return Err(ConfigError::Worker(
                "worker.operation_timeout must be positive: 0 cannot provide a leased worker budget"
                    .to_owned(),
            ));
        }
        if self.worker.max_delivery_attempts == 0 {
            return Err(ConfigError::Worker(
                "worker.max_delivery_attempts must be positive: 0 terminalizes every \
                 operation's items as system_failure without attempting them"
                    .to_owned(),
            ));
        }
        if self.worker.max_delivery_attempts > MAX_DELIVERY_ATTEMPTS {
            return Err(ConfigError::Worker(format!(
                "worker.max_delivery_attempts ({}) must not exceed {MAX_DELIVERY_ATTEMPTS}: the \
                 outbox stores the attempt count in an i16 and the handler sees the value from \
                 before its own delivery's increment, so a larger budget is one no delivery can \
                 reach",
                self.worker.max_delivery_attempts,
            )));
        }
        Ok(RegistrationPolicy::compile(&self.registration_policy)?)
    }

    /// Converts this config to a `gts::GtsConfig`.
    #[must_use]
    pub fn to_gts_config(&self) -> gts::GtsConfig {
        gts::GtsConfig {
            entity_id_fields: self.entity_id_fields.clone(),
            type_id_fields: self.schema_id_fields.clone(),
        }
    }
}

/// Startup configuration errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid registration_policy: {0}")]
    Policy(#[from] PolicyConfigError),
    #[error("invalid limits: {0}")]
    Limits(String),
    #[error("invalid worker settings: {0}")]
    Worker(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = TypesRegistryConfig::default();
        assert_eq!(cfg.entity_id_fields, vec!["$id", "gtsId", "id"]);
        assert_eq!(cfg.schema_id_fields, vec!["$schema", "gtsTid", "type"]);
        assert!(cfg.entities.is_empty());
    }

    #[test]
    fn test_to_gts_config() {
        let cfg = TypesRegistryConfig::default();
        let gts_cfg = cfg.to_gts_config();
        assert_eq!(gts_cfg.entity_id_fields, cfg.entity_id_fields);
        assert_eq!(gts_cfg.type_id_fields, cfg.schema_id_fields);
    }

    #[test]
    fn test_default_cache_settings_match_infra_constants() {
        let cfg = TypesRegistryConfig::default();
        assert_eq!(
            cfg.local_client.cache.type_schemas.capacity,
            DEFAULT_CACHE_CAPACITY
        );
        assert_eq!(
            cfg.local_client.cache.type_schemas.ttl,
            Some(DEFAULT_CACHE_TTL)
        );
        assert_eq!(
            cfg.local_client.cache.instances.capacity,
            DEFAULT_CACHE_CAPACITY
        );
        assert_eq!(
            cfg.local_client.cache.instances.ttl,
            Some(DEFAULT_CACHE_TTL)
        );
    }

    #[test]
    fn test_cache_settings_with_explicit_values() {
        // JSON shape matches YAML 1:1 for the fields we care about (humantime
        // accepts duration strings via Visitor::visit_str regardless of the
        // input format).
        let json = serde_json::json!({
            "local_client": {
                "cache": {
                    "type_schemas": { "capacity": 2048, "ttl": "2m" },
                    "instances":    { "capacity": 512,  "ttl": "30s" },
                }
            }
        });
        let cfg: TypesRegistryConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg.local_client.cache.type_schemas.capacity, 2048);
        assert_eq!(
            cfg.local_client.cache.type_schemas.ttl,
            Some(std::time::Duration::from_mins(2))
        );
        assert_eq!(cfg.local_client.cache.instances.capacity, 512);
        assert_eq!(
            cfg.local_client.cache.instances.ttl,
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn test_cache_settings_null_ttl_disables() {
        let json = serde_json::json!({
            "local_client": {
                "cache": {
                    "type_schemas": { "capacity": 100, "ttl": null },
                    "instances":    { "capacity": 100, "ttl": null },
                }
            }
        });
        let cfg: TypesRegistryConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg.local_client.cache.type_schemas.ttl, None);
        assert_eq!(cfg.local_client.cache.instances.ttl, None);
    }

    #[test]
    fn test_cache_settings_omitted_falls_back_to_default() {
        // Whole `cache` block missing — defaults must come from
        // SingleCacheSettings::default(), keeping parity with InMemoryCache's
        // hardcoded defaults.
        let cfg: TypesRegistryConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            cfg.local_client.cache.type_schemas.capacity,
            DEFAULT_CACHE_CAPACITY
        );
        assert_eq!(
            cfg.local_client.cache.type_schemas.ttl,
            Some(DEFAULT_CACHE_TTL)
        );
    }

    #[test]
    fn test_to_cache_config_round_trip() {
        let settings = SingleCacheSettings {
            capacity: 7,
            ttl: Some(std::time::Duration::from_secs(11)),
        };
        let cache_cfg = settings.to_cache_config();
        assert_eq!(cache_cfg.capacity, 7);
        assert_eq!(cache_cfg.ttl, Some(std::time::Duration::from_secs(11)));
    }
}
