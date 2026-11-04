//! Gear configuration.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::model::{ContentLimits, Document, DocumentId};

/// Configuration of the `policy-engine` gear (besides its database section).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyEngineConfig {
    /// How the engine registers as an admission engine plugin.
    pub engine_plugin: EnginePluginConfig,
    /// Budget for evaluating all documents of one request, built-in
    /// policies included; only evaluation time is charged. A `humantime`
    /// duration such as `5ms`.
    #[serde(with = "toolkit_utils::humantime_serde")]
    pub evaluation_timeout: Duration,
    /// Bound on each tenant-hierarchy call of the decision path
    /// (`humantime`).
    #[serde(with = "toolkit_utils::humantime_serde")]
    pub hierarchy_timeout: Duration,
    /// Bound on each tenant-hierarchy call of the management surface, which
    /// reads the caller's whole subtree to list assignments (`humantime`).
    /// Kept apart from `hierarchy_timeout` so a slow listing never loosens
    /// the decision path's bound.
    #[serde(with = "toolkit_utils::humantime_serde")]
    pub management_hierarchy_timeout: Duration,
    /// Bound on the types-registry lookup at validation time (`humantime`).
    #[serde(with = "toolkit_utils::humantime_serde")]
    pub registry_timeout: Duration,
    /// Bound on the decision path's read of active policy content
    /// (`humantime`).
    #[serde(with = "toolkit_utils::humantime_serde")]
    pub store_timeout: Duration,
    /// Compiled versions kept in memory.
    pub compile_cache_capacity: usize,
    /// Most documents in one version.
    pub max_documents_per_version: usize,
    /// Most content bytes in one document.
    pub max_document_bytes: usize,
    /// Platform rules evaluated before tenant policy on every request,
    /// whatever the tenant, in this order. Validated like a bundle version's
    /// documents at startup. Default: none.
    pub builtin_policies: Vec<BuiltinPolicyConfig>,
}

/// One built-in policy as configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinPolicyConfig {
    /// Stable identity, unique among built-ins; names the policy in logs.
    pub id: String,
    /// Operator-facing description.
    #[serde(default)]
    pub description: Option<String>,
    /// GTS type patterns the policy applies to (a concrete id is a pattern
    /// without a wildcard).
    pub resource_types: Vec<String>,
    /// Actions the policy applies to. Default: empty, every action.
    #[serde(default)]
    pub actions: Vec<String>,
    /// Rego content defining the boolean rule `deny`.
    pub content: String,
}

/// Admission engine plugin registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnginePluginConfig {
    /// Vendor the plugin registers under.
    pub vendor: String,
    /// Plugin priority; the lowest value wins among engines.
    pub priority: i16,
}

impl Default for EnginePluginConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 100,
        }
    }
}

impl Default for PolicyEngineConfig {
    fn default() -> Self {
        Self {
            engine_plugin: EnginePluginConfig::default(),
            evaluation_timeout: Duration::from_millis(5),
            hierarchy_timeout: Duration::from_millis(20),
            management_hierarchy_timeout: Duration::from_secs(1),
            registry_timeout: Duration::from_millis(500),
            store_timeout: Duration::from_millis(100),
            compile_cache_capacity: 256,
            max_documents_per_version: 256,
            max_document_bytes: 65_536,
            builtin_policies: Vec::new(),
        }
    }
}

/// A configuration value the gear refuses to start with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A duration or count that must be positive is zero.
    #[error("`{field}` must be greater than zero")]
    NotPositive {
        /// The offending key.
        field: &'static str,
    },
    /// A duration or count exceeds the most the gear accepts (see
    /// [`PolicyEngineConfig::validate`]).
    #[error("`{field}` must be at most {max}")]
    TooLarge {
        /// The offending key.
        field: &'static str,
        /// The largest accepted value, as written in the configuration.
        max: String,
    },
    /// `engine_plugin.vendor` is empty.
    #[error("`engine_plugin.vendor` must not be blank")]
    EnginePluginVendorBlank,
    /// A built-in policy's `id` is empty.
    #[error("`builtin_policies[{index}].id` must not be blank")]
    BuiltinPolicyIdBlank {
        /// Position of the policy in `builtin_policies`.
        index: usize,
    },
}

/// The longest accepted `evaluation_timeout`, `hierarchy_timeout`,
/// `management_hierarchy_timeout`, `registry_timeout` and `store_timeout`,
/// in that order: a decision runs on the admission path, so none of its
/// bounds may grow without limit, and a management call holds a request.
const MAX_DURATIONS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(10),
    Duration::from_secs(60),
    Duration::from_secs(60),
    Duration::from_secs(10),
];

/// The largest accepted `compile_cache_capacity`, `max_documents_per_version`
/// and `max_document_bytes` (1 MiB), in that order.
const MAX_COUNTS: [usize; 3] = [65_536, 4_096, 1_048_576];

impl PolicyEngineConfig {
    /// Checks the configuration.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] for a zero or oversized bound, a blank vendor or a
    /// blank built-in policy id. The built-in policies' content is validated
    /// at startup.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let durations = [
            ("evaluation_timeout", self.evaluation_timeout),
            ("hierarchy_timeout", self.hierarchy_timeout),
            (
                "management_hierarchy_timeout",
                self.management_hierarchy_timeout,
            ),
            ("registry_timeout", self.registry_timeout),
            ("store_timeout", self.store_timeout),
        ];
        let counts = [
            ("compile_cache_capacity", self.compile_cache_capacity),
            ("max_documents_per_version", self.max_documents_per_version),
            ("max_document_bytes", self.max_document_bytes),
        ];
        let zero = durations
            .iter()
            .find(|(_, d)| d.is_zero())
            .map(|(field, _)| *field)
            .or_else(|| {
                counts
                    .iter()
                    .find(|(_, n)| *n == 0)
                    .map(|(field, _)| *field)
            });
        if let Some(field) = zero {
            return Err(ConfigError::NotPositive { field });
        }
        let too_long = durations
            .iter()
            .zip(MAX_DURATIONS)
            .find(|((_, value), max)| value > max)
            .map(|((field, _), max)| (*field, format!("{max:?}")));
        let too_many = || {
            counts
                .iter()
                .zip(MAX_COUNTS)
                .find(|((_, value), max)| value > max)
                .map(|((field, _), max)| (*field, max.to_string()))
        };
        if let Some((field, max)) = too_long.or_else(too_many) {
            return Err(ConfigError::TooLarge { field, max });
        }
        if self.engine_plugin.vendor.trim().is_empty() {
            return Err(ConfigError::EnginePluginVendorBlank);
        }
        if let Some(index) = self
            .builtin_policies
            .iter()
            .position(|p| p.id.trim().is_empty())
        {
            return Err(ConfigError::BuiltinPolicyIdBlank { index });
        }
        Ok(())
    }

    /// The built-in policies as documents named by their ids. Built-ins are
    /// never stored, so every document has the nil id.
    #[must_use]
    pub fn builtin_documents(&self) -> Vec<Document> {
        self.builtin_policies
            .iter()
            .map(|p| Document {
                id: DocumentId(Uuid::nil()),
                name: p.id.clone(),
                content: p.content.clone(),
                resource_types: p.resource_types.clone(),
                actions: p.actions.clone(),
            })
            .collect()
    }

    /// The content limits enforced on write and on validation.
    #[must_use]
    pub const fn content_limits(&self) -> ContentLimits {
        ContentLimits {
            max_documents_per_version: self.max_documents_per_version,
            max_document_bytes: self.max_document_bytes,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
