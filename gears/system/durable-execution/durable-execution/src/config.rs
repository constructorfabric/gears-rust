use serde::Deserialize;
use std::time::Duration;

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn valid_queue_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub execute_activities: bool,
    /// Enable durable submissions/delivery on API nodes without executing Activities.
    pub delivery_enabled: bool,
    pub queue_database_url_env: String,
    /// Default queue used when no versioned definition route matches.
    pub default_queue: String,
    /// Named worker pools and their concurrency limits.
    pub queues: std::collections::BTreeMap<String, usize>,
    /// Exact versioned definition names mapped to named queues.
    pub definition_queues: std::collections::BTreeMap<String, String>,
    pub service_client_id: String,
    /// Environment variable holding the client secret; never the secret itself.
    pub service_client_secret_env: String,
    pub service_scopes: Vec<String>,
    pub lease_secs: u64,
    pub heartbeat_secs: u64,
    pub dispatch_interval_secs: u64,
    /// Consecutive database failures before the control plane stops (1..=128).
    pub control_plane_failure_limit: u32,
    pub shutdown_timeout_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            execute_activities: false,
            delivery_enabled: false,
            queue_database_url_env: "DATABASE_URL".into(),
            default_queue: "durable-v1".into(),
            queues: [("durable-v1".into(), 2)].into(),
            definition_queues: std::collections::BTreeMap::default(),
            service_client_id: String::new(),
            service_client_secret_env: "DURABLE_SERVICE_CLIENT_SECRET".into(),
            service_scopes: Vec::new(),
            lease_secs: 120,
            heartbeat_secs: 20,
            dispatch_interval_secs: 5,
            control_plane_failure_limit: 5,
            shutdown_timeout_secs: 30,
        }
    }
}
impl Config {
    /// # Errors
    /// Returns a validation message for invalid queues, credentials or timing.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_env_name(&self.queue_database_url_env) {
            return Err("invalid queue database environment variable name");
        }
        if !self.queues.contains_key(&self.default_queue) {
            return Err("default queue must be declared");
        }
        if self
            .queues
            .iter()
            .any(|(name, concurrency)| !valid_queue_name(name) || !(1..=128).contains(concurrency))
        {
            return Err("invalid queue name or concurrency (1..=128)");
        }
        if self.definition_queues.iter().any(|(name, queue)| {
            durable_execution_sdk::contracts::ExecutionContract::validate_name(name).is_err()
                || !self.queues.contains_key(queue)
        }) {
            return Err("definition route must refer to a declared queue");
        }
        if !valid_env_name(&self.service_client_secret_env)
            || self.service_client_id.len() > 255
            || self.service_scopes.len() > 128
            || self
                .service_scopes
                .iter()
                .any(|s| s.is_empty() || s.len() > 255)
            || ((self.execute_activities || self.delivery_enabled)
                && self.service_client_id.is_empty())
        {
            return Err("invalid service credentials: client/scopes <=255 bytes, env <=128 bytes");
        }
        if self.heartbeat_secs == 0 || self.heartbeat_secs.saturating_mul(3) >= self.lease_secs {
            return Err("heartbeat must be less than one third of lease");
        }
        if !(1..=86_400).contains(&self.dispatch_interval_secs)
            || !(1..=86_400).contains(&self.shutdown_timeout_secs)
        {
            return Err("dispatch and shutdown durations must be within one day");
        }
        if !(1..=128).contains(&self.control_plane_failure_limit) {
            return Err("control plane failure limit must be within 1..=128");
        }
        if self.lease_secs > 86_400 {
            return Err("lease must not exceed one day");
        }
        Ok(())
    }
    #[must_use]
    pub fn queue_for(&self, definition: &str) -> &str {
        self.definition_queues
            .get(definition)
            .map_or(&self.default_queue, String::as_str)
    }
    #[must_use]
    pub fn lease(&self) -> Duration {
        Duration::from_secs(self.lease_secs)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/config_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/config_property_tests.rs"]
mod property_tests;
