//! A memory registry entry's settings.
//!
//! Parsed by hand rather than through serde, so an error names the setting
//! and the rule it breaks and never quotes a value (feature 0007 §2.7).

use std::time::Duration;

use event_broker_sdk::backend::BackendError;

const STORE: &str = "store";
const RETENTION_INTERVAL_MS: &str = "retention_interval_ms";
const MAX_STORE_LEN: usize = 255;

/// One registry entry's settings, defaults applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySettings {
    /// Instances naming one store share it. Default `"default"`.
    pub store: String,
    /// How often the retention worker runs. Default 1 s.
    pub retention_interval: Duration,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            store: "default".to_owned(),
            retention_interval: Duration::from_secs(1),
        }
    }
}

impl MemorySettings {
    /// Reads an entry's settings; an unknown setting or a broken rule is
    /// `InvalidConfig`.
    pub fn parse(
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, BackendError> {
        let mut parsed = Self::default();
        for (name, value) in settings {
            match name.as_str() {
                STORE => {
                    value
                        .as_str()
                        .filter(|store| !store.trim().is_empty() && store.len() <= MAX_STORE_LEN)
                        .ok_or_else(|| {
                            BackendError::invalid_config(format!(
                                "`{STORE}` must be a non-empty string of at most {MAX_STORE_LEN} bytes"
                            ))
                        })?
                        .clone_into(&mut parsed.store);
                }
                RETENTION_INTERVAL_MS => {
                    let millis = value.as_u64().filter(|millis| *millis > 0).ok_or_else(|| {
                        BackendError::invalid_config(format!(
                            "`{RETENTION_INTERVAL_MS}` must be a positive integer"
                        ))
                    })?;
                    parsed.retention_interval = Duration::from_millis(millis);
                }
                other => {
                    return Err(BackendError::invalid_config(format!(
                        "`{other}` is not a memory backend setting"
                    )));
                }
            }
        }
        Ok(parsed)
    }
}
