//! Validated engine settings. Built by [`EngineBuilder`](crate::EngineBuilder); the
//! runtime components read them directly.
use std::collections::BTreeMap;
use std::time::Duration;

fn valid_queue_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

/// Engine settings shared by the executor, the dispatcher and the workers.
#[derive(Clone, Debug)]
pub struct Config {
    /// Run activity workers on the queues; otherwise only dispatch.
    pub execute_activities: bool,
    /// Default queue used when no versioned definition route matches.
    pub default_queue: String,
    /// Named worker pools and their concurrency limits.
    pub queues: BTreeMap<String, usize>,
    /// Exact versioned definition names mapped to named queues.
    pub definition_queues: BTreeMap<String, String>,
    /// How long a claimed activity attempt stays leased without a heartbeat.
    pub lease_secs: u64,
    /// Interval between lease heartbeats of a running attempt.
    pub heartbeat_secs: u64,
    /// Interval between dispatch and recovery passes.
    pub dispatch_interval_secs: u64,
    /// Consecutive database failures before the control plane stops (1..=128).
    pub control_plane_failure_limit: u32,
    /// How long a stopping engine waits for in-flight work to drain.
    pub shutdown_timeout_secs: u64,
}

/// Test fixtures only: the engine builds `Config` field by field from its builder,
/// so a new field cannot be defaulted silently.
#[cfg(test)]
impl Default for Config {
    fn default() -> Self {
        Self {
            execute_activities: false,
            default_queue: "durable-v1".into(),
            queues: [("durable-v1".into(), 2)].into(),
            definition_queues: BTreeMap::default(),
            lease_secs: 120,
            heartbeat_secs: 20,
            dispatch_interval_secs: 5,
            control_plane_failure_limit: 5,
            shutdown_timeout_secs: 30,
        }
    }
}

impl Config {
    /// Returns the name of the invalid setting and the violated rule.
    pub(crate) fn validate(&self) -> Result<(), (&'static str, &'static str)> {
        if !self.queues.contains_key(&self.default_queue) {
            return Err(("default_queue", "default queue must be declared"));
        }
        if self
            .queues
            .iter()
            .any(|(name, concurrency)| !valid_queue_name(name) || !(1..=128).contains(concurrency))
        {
            return Err((
                "queues",
                "queue names must be 1..=100 bytes of [A-Za-z0-9_-] with concurrency 1..=128",
            ));
        }
        if self.definition_queues.iter().any(|(name, queue)| {
            crate::contracts::ExecutionContract::validate_name(name).is_err()
                || !self.queues.contains_key(queue)
        }) {
            return Err((
                "routes",
                "a route needs a versioned definition name and a declared queue",
            ));
        }
        if self.heartbeat_secs == 0 || self.heartbeat_secs.saturating_mul(3) >= self.lease_secs {
            return Err((
                "heartbeat",
                "heartbeat must be less than one third of lease",
            ));
        }
        if self.lease_secs > 86_400 {
            return Err(("lease", "lease must not exceed one day"));
        }
        if !(1..=86_400).contains(&self.dispatch_interval_secs) {
            return Err(("dispatch_interval", "must be within 1 second and one day"));
        }
        if !(1..=86_400).contains(&self.shutdown_timeout_secs) {
            return Err(("shutdown_timeout", "must be within 1 second and one day"));
        }
        if !(1..=128).contains(&self.control_plane_failure_limit) {
            return Err(("control_plane_failure_limit", "must be within 1..=128"));
        }
        Ok(())
    }
    /// Queue for `definition`: its route if one exists, otherwise the default queue.
    #[must_use]
    pub fn queue_for(&self, definition: &str) -> &str {
        self.definition_queues
            .get(definition)
            .map_or(&self.default_queue, String::as_str)
    }
    /// Lease duration of a claimed activity attempt.
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
