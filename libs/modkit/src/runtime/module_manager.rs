//! Module Manager - tracks and manages all live module instances in the runtime

use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Represents an endpoint where a module instance can be reached
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub uri: String,
}

/// Typed view of an endpoint for parsing and matching
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointKind {
    /// TCP endpoint with resolved socket address
    Tcp(std::net::SocketAddr),
    /// Unix domain socket with file path
    Uds(std::path::PathBuf),
    /// Other/unparsed endpoint URI
    Other(String),
}

impl Endpoint {
    pub fn from_uri<S: Into<String>>(s: S) -> Self {
        Self { uri: s.into() }
    }

    pub fn uds(path: impl AsRef<std::path::Path>) -> Self {
        Self {
            uri: format!("unix://{}", path.as_ref().display()),
        }
    }

    #[must_use]
    pub fn http(host: &str, port: u16) -> Self {
        Self {
            uri: format!("http://{host}:{port}"),
        }
    }

    #[must_use]
    pub fn https(host: &str, port: u16) -> Self {
        Self {
            uri: format!("https://{host}:{port}"),
        }
    }

    /// Parse the endpoint URI into a typed view
    #[must_use]
    pub fn kind(&self) -> EndpointKind {
        if let Some(rest) = self.uri.strip_prefix("unix://") {
            return EndpointKind::Uds(std::path::PathBuf::from(rest));
        }
        if let Some(rest) = self.uri.strip_prefix("http://")
            && let Ok(addr) = rest.parse::<std::net::SocketAddr>()
        {
            return EndpointKind::Tcp(addr);
        }
        if let Some(rest) = self.uri.strip_prefix("https://")
            && let Ok(addr) = rest.parse::<std::net::SocketAddr>()
        {
            return EndpointKind::Tcp(addr);
        }
        EndpointKind::Other(self.uri.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceState {
    Registered,
    Ready,
    Healthy,
    Quarantined,
    Draining,
}

/// Runtime state of an instance (guarded by `RwLock` for safe mutation)
#[derive(Clone, Debug)]
pub struct InstanceRuntimeState {
    pub last_heartbeat: Instant,
    pub state: InstanceState,
}

/// Represents a single instance of a module
#[derive(Debug)]
#[must_use]
pub struct ModuleInstance {
    pub module: String,
    pub instance_id: Uuid,
    pub control: Option<Endpoint>,
    pub grpc_services: HashMap<String, Endpoint>,
    pub version: Option<String>,
    inner: Arc<parking_lot::RwLock<InstanceRuntimeState>>,
}

impl Clone for ModuleInstance {
    fn clone(&self) -> Self {
        Self {
            module: self.module.clone(),
            instance_id: self.instance_id,
            control: self.control.clone(),
            grpc_services: self.grpc_services.clone(),
            version: self.version.clone(),
            inner: Arc::clone(&self.inner),
        }
    }
}

impl ModuleInstance {
    pub fn new(module: impl Into<String>, instance_id: Uuid) -> Self {
        Self {
            module: module.into(),
            instance_id,
            control: None,
            grpc_services: HashMap::new(),
            version: None,
            inner: Arc::new(parking_lot::RwLock::new(InstanceRuntimeState {
                last_heartbeat: Instant::now(),
                state: InstanceState::Registered,
            })),
        }
    }

    pub fn with_control(mut self, ep: Endpoint) -> Self {
        self.control = Some(ep);
        self
    }

    pub fn with_version(mut self, v: impl Into<String>) -> Self {
        self.version = Some(v.into());
        self
    }

    pub fn with_grpc_service(mut self, name: impl Into<String>, ep: Endpoint) -> Self {
        self.grpc_services.insert(name.into(), ep);
        self
    }

    /// Get the current state of this instance
    #[must_use]
    pub fn state(&self) -> InstanceState {
        self.inner.read().state
    }

    /// Get the last heartbeat timestamp
    #[must_use]
    pub fn last_heartbeat(&self) -> Instant {
        self.inner.read().last_heartbeat
    }
}

/// Central registry that tracks all running module instances in the system.
/// Provides discovery, health tracking, and round-robin load balancing.
#[derive(Clone)]
#[must_use]
pub struct ModuleManager {
    inner: DashMap<String, Vec<Arc<ModuleInstance>>>,
    rr_counters: DashMap<String, usize>,
    hb_ttl: Duration,
    hb_grace: Duration,
}

impl std::fmt::Debug for ModuleManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let modules: Vec<String> = self.inner.iter().map(|e| e.key().clone()).collect();
        f.debug_struct("ModuleManager")
            .field("instances_count", &self.inner.len())
            .field("modules", &modules)
            .field("heartbeat_ttl", &self.hb_ttl)
            .field("heartbeat_grace", &self.hb_grace)
            .finish_non_exhaustive()
    }
}

impl ModuleManager {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
            rr_counters: DashMap::new(),
            hb_ttl: Duration::from_secs(15),
            hb_grace: Duration::from_secs(30),
        }
    }

    pub fn with_heartbeat_policy(mut self, ttl: Duration, grace: Duration) -> Self {
        self.hb_ttl = ttl;
        self.hb_grace = grace;
        self
    }

    /// Register or update a module instance
    pub fn register_instance(&self, instance: Arc<ModuleInstance>) {
        let module = instance.module.clone();
        let mut vec = self.inner.entry(module).or_default();
        // replace by instance_id if it already exists
        if let Some(pos) = vec
            .iter()
            .position(|i| i.instance_id == instance.instance_id)
        {
            vec[pos] = instance;
        } else {
            vec.push(instance);
        }
    }

    /// Mark an instance as ready
    pub fn mark_ready(&self, module: &str, instance_id: Uuid) {
        if let Some(mut vec) = self.inner.get_mut(module)
            && let Some(inst) = vec.iter_mut().find(|i| i.instance_id == instance_id)
        {
            let mut state = inst.inner.write();
            state.state = InstanceState::Ready;
        }
    }

    /// Update the heartbeat timestamp for an instance
    pub fn update_heartbeat(&self, module: &str, instance_id: Uuid, at: Instant) {
        if let Some(mut vec) = self.inner.get_mut(module)
            && let Some(inst) = vec.iter_mut().find(|i| i.instance_id == instance_id)
        {
            let mut state = inst.inner.write();
            state.last_heartbeat = at;
            // Transition Registered -> Healthy on first heartbeat
            if state.state == InstanceState::Registered {
                state.state = InstanceState::Healthy;
            }
        }
    }

    /// Mark an instance as quarantined
    pub fn mark_quarantined(&self, module: &str, instance_id: Uuid) {
        if let Some(mut vec) = self.inner.get_mut(module)
            && let Some(inst) = vec.iter_mut().find(|i| i.instance_id == instance_id)
        {
            inst.inner.write().state = InstanceState::Quarantined;
        }
    }

    /// Mark an instance as draining (graceful shutdown in progress)
    pub fn mark_draining(&self, module: &str, instance_id: Uuid) {
        if let Some(mut vec) = self.inner.get_mut(module)
            && let Some(inst) = vec.iter_mut().find(|i| i.instance_id == instance_id)
        {
            inst.inner.write().state = InstanceState::Draining;
        }
    }

    /// Remove an instance from the directory
    pub fn deregister(&self, module: &str, instance_id: Uuid) {
        let mut remove_module = false;
        {
            if let Some(mut vec) = self.inner.get_mut(module) {
                let list = vec.value_mut();
                list.retain(|inst| inst.instance_id != instance_id);
                if list.is_empty() {
                    remove_module = true;
                }
            }
        }

        if remove_module {
            self.inner.remove(module);
            self.rr_counters.remove(module);
        }
    }

    /// Get all instances of a specific module
    #[must_use]
    pub fn instances_of(&self, module: &str) -> Vec<Arc<ModuleInstance>> {
        self.inner
            .get(module)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Get all instances across all modules
    #[must_use]
    pub fn all_instances(&self) -> Vec<Arc<ModuleInstance>> {
        self.inner
            .iter()
            .flat_map(|entry| entry.value().clone())
            .collect()
    }

    /// Quarantine or evict stale instances based on heartbeat policy
    pub fn evict_stale(&self, now: Instant) {
        use InstanceState::{Draining, Quarantined};
        let mut empty_modules = Vec::new();

        for mut entry in self.inner.iter_mut() {
            let module = entry.key().clone();
            let vec = entry.value_mut();
            vec.retain(|inst| {
                let state = inst.inner.read();
                let age = now.saturating_duration_since(state.last_heartbeat);

                // Quarantine instances that have exceeded TTL
                if age >= self.hb_ttl && !matches!(state.state, Quarantined | Draining) {
                    drop(state); // Release read lock before write
                    inst.inner.write().state = Quarantined;
                    return true; // Keep quarantined instances for now
                }

                // Evict quarantined instances that exceed grace period
                if state.state == Quarantined && age >= self.hb_ttl + self.hb_grace {
                    return false; // Remove from directory
                }

                true
            });

            if vec.is_empty() {
                empty_modules.push(module);
            }
        }

        for module in empty_modules {
            self.inner.remove(&module);
            self.rr_counters.remove(&module);
        }
    }

    /// Pick an instance using round-robin selection, preferring healthy instances
    #[must_use]
    pub fn pick_instance_round_robin(&self, module: &str) -> Option<Arc<ModuleInstance>> {
        let instances_entry = self.inner.get(module)?;
        let instances = instances_entry.value();

        if instances.is_empty() {
            return None;
        }

        // Prefer healthy or ready instances
        let healthy: Vec<_> = instances
            .iter()
            .filter(|inst| matches!(inst.state(), InstanceState::Healthy | InstanceState::Ready))
            .cloned()
            .collect();

        let candidates: Vec<_> = if healthy.is_empty() {
            instances.clone()
        } else {
            healthy
        };

        if candidates.is_empty() {
            return None;
        }

        let len = candidates.len();
        let mut counter = self.rr_counters.entry(module.to_owned()).or_insert(0);
        let idx = *counter % len;
        *counter = (*counter + 1) % len;

        candidates.get(idx).cloned()
    }

    /// Pick a service endpoint using round-robin, returning (module, instance, endpoint).
    /// Prefers healthy/ready instances and automatically rotates among them.
    #[must_use]
    pub fn pick_service_round_robin(
        &self,
        service_name: &str,
    ) -> Option<(String, Arc<ModuleInstance>, Endpoint)> {
        // Collect all instances that provide this service
        let mut candidates = Vec::new();
        for entry in &self.inner {
            let module = entry.key().clone();
            for inst in entry.value() {
                if let Some(ep) = inst.grpc_services.get(service_name) {
                    let state = inst.state();
                    if matches!(state, InstanceState::Healthy | InstanceState::Ready) {
                        candidates.push((module.clone(), inst.clone(), ep.clone()));
                    }
                }
            }
        }

        if candidates.is_empty() {
            return None;
        }

        // Use a counter keyed by service name for round-robin
        let len = candidates.len();
        let service_key = service_name.to_owned();
        let mut counter = self.rr_counters.entry(service_key).or_insert(0);
        let idx = *counter % len;
        *counter = (*counter + 1) % len;

        candidates.get(idx).cloned()
    }
}

impl Default for ModuleManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "module_manager_tests.rs"]
mod tests;
