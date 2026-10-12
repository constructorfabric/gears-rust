//! Admission-control configuration, read from `gears.admission-control.config`.
//!
//! Every struct rejects unknown keys and every absent key takes its default.
//! The numeric keys are checked at init ([`AdmissionControlConfig::validate`]);
//! any error fails startup.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::domain::service::ServiceSettings;

/// Configuration of the admission-control gear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdmissionControlConfig {
    /// Engine selection. Default: none; the gate refuses every request that
    /// passes its own checks.
    pub engine: Option<EngineConfig>,
    /// Engine call bound in milliseconds. Default `100`.
    pub engine_timeout_ms: u64,
    /// Largest number of properties per request. Default `256`.
    pub max_properties: usize,
    /// Largest serialized size of a request's properties, in bytes. Default `65_536`.
    pub max_context_bytes: usize,
    /// Capacity of the refusal-event queue. Default `1_024`.
    pub event_queue_capacity: usize,
}

impl Default for AdmissionControlConfig {
    fn default() -> Self {
        Self {
            engine: None,
            engine_timeout_ms: 100,
            max_properties: 256,
            max_context_bytes: 65_536,
            event_queue_capacity: 1_024,
        }
    }
}

/// Which admission engine plugin to resolve at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    /// Vendor of the engine plugin.
    pub vendor: String,
    /// GTS instance id pinning one engine plugin instance. Default: none; the
    /// instance is chosen by vendor and priority.
    #[serde(default)]
    pub instance_id: Option<String>,
}

impl AdmissionControlConfig {
    /// Checks the numeric keys. Zero is rejected for every one: a zero
    /// timeout or bound would refuse every request while startup succeeded.
    ///
    /// # Errors
    ///
    /// The first key out of range, naming it.
    pub fn validate(&self) -> anyhow::Result<()> {
        let keys = [
            ("engine_timeout_ms", self.engine_timeout_ms == 0),
            ("max_properties", self.max_properties == 0),
            ("max_context_bytes", self.max_context_bytes == 0),
            ("event_queue_capacity", self.event_queue_capacity == 0),
        ];
        for (key, zero) in keys {
            anyhow::ensure!(!zero, "`{key}` must be greater than zero");
        }
        Ok(())
    }

    /// Service settings from the numeric keys.
    #[must_use]
    pub fn service_settings(&self) -> ServiceSettings {
        ServiceSettings {
            engine_timeout: Duration::from_millis(self.engine_timeout_ms),
            max_properties: self.max_properties,
            max_context_bytes: self.max_context_bytes,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
