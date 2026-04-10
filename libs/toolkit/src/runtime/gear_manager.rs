//! Gear Manager - tracks and manages all live gear instances in the runtime

use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Represents an endpoint where a gear instance can be reached
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

/// Represents a single instance of a gear
#[derive(Debug)]
#[must_use]
pub struct GearInstance {
    /// Gear name this instance belongs to
    pub gear: String,
    pub instance_id: Uuid,
    /// Optional control endpoint for lifecycle management
    pub control: Option<Endpoint>,
    /// Map of gRPC service name to endpoint
    pub grpc_services: HashMap<String, Endpoint>,
    /// Optional version string
    pub version: Option<String>,
    /// Optional REST endpoint (not all modules expose REST)
    pub rest_endpoint: Option<Endpoint>,
    inner: Arc<parking_lot::RwLock<InstanceRuntimeState>>,
}

impl Clone for GearInstance {
    fn clone(&self) -> Self {
        Self {
            gear: self.gear.clone(),
            instance_id: self.instance_id,
            control: self.control.clone(),
            grpc_services: self.grpc_services.clone(),
            version: self.version.clone(),
            rest_endpoint: self.rest_endpoint.clone(),
            inner: Arc::clone(&self.inner),
        }
    }
}

impl GearInstance {
    pub fn new(gear: impl Into<String>, instance_id: Uuid) -> Self {
        Self {
            gear: gear.into(),
            instance_id,
            control: None,
            grpc_services: HashMap::new(),
            version: None,
            rest_endpoint: None,
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

    /// Set the REST endpoint for this instance
    pub fn with_rest_endpoint(mut self, ep: Endpoint) -> Self {
        self.rest_endpoint = Some(ep);
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

/// Round-robin counter key. Typed instead of stringly-typed to keep the
/// gRPC / REST / service buckets disjoint (a gear named `"rest:foo"`
/// can't collide with the REST counter for gear `"foo"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum RrKey {
    /// Counter for `pick_instance_round_robin(gear)`.
    Gear(String),
    /// Counter for `pick_rest_gear(gear)`.
    Rest(String),
    /// Counter for `pick_service_round_robin(service_name)`.
    Service(String),
}

/// Central registry that tracks all running gear instances in the system.
/// Provides discovery, health tracking, and round-robin load balancing.
#[derive(Clone)]
#[must_use]
pub struct GearManager {
    inner: DashMap<String, Vec<Arc<GearInstance>>>,
    rr_counters: DashMap<RrKey, usize>,
    hb_ttl: Duration,
    hb_grace: Duration,
}

impl std::fmt::Debug for GearManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let gears: Vec<String> = self.inner.iter().map(|e| e.key().clone()).collect();
        f.debug_struct("GearManager")
            .field("instances_count", &self.inner.len())
            .field("gears", &gears)
            .field("heartbeat_ttl", &self.hb_ttl)
            .field("heartbeat_grace", &self.hb_grace)
            .finish_non_exhaustive()
    }
}

impl GearManager {
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

    /// Register or update a gear instance
    pub fn register_instance(&self, instance: Arc<GearInstance>) {
        let gear = instance.gear.clone();
        let mut vec = self.inner.entry(gear).or_default();
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
    pub fn mark_ready(&self, gear: &str, instance_id: Uuid) {
        if let Some(mut vec) = self.inner.get_mut(gear)
            && let Some(inst) = vec.iter_mut().find(|i| i.instance_id == instance_id)
        {
            let mut state = inst.inner.write();
            state.state = InstanceState::Ready;
        }
    }

    /// Update the heartbeat timestamp for an instance
    pub fn update_heartbeat(&self, gear: &str, instance_id: Uuid, at: Instant) {
        if let Some(mut vec) = self.inner.get_mut(gear)
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
    pub fn mark_quarantined(&self, gear: &str, instance_id: Uuid) {
        if let Some(mut vec) = self.inner.get_mut(gear)
            && let Some(inst) = vec.iter_mut().find(|i| i.instance_id == instance_id)
        {
            inst.inner.write().state = InstanceState::Quarantined;
        }
    }

    /// Mark an instance as draining (graceful shutdown in progress)
    pub fn mark_draining(&self, gear: &str, instance_id: Uuid) {
        if let Some(mut vec) = self.inner.get_mut(gear)
            && let Some(inst) = vec.iter_mut().find(|i| i.instance_id == instance_id)
        {
            inst.inner.write().state = InstanceState::Draining;
        }
    }

    /// Remove an instance from the directory
    pub fn deregister(&self, gear: &str, instance_id: Uuid) {
        let mut remove_gear = false;
        {
            if let Some(mut vec) = self.inner.get_mut(gear) {
                let list = vec.value_mut();
                list.retain(|inst| inst.instance_id != instance_id);
                if list.is_empty() {
                    remove_gear = true;
                }
            }
        }

        if remove_gear {
            self.inner.remove(gear);
            self.rr_counters.remove(&RrKey::Gear(gear.to_owned()));
            self.rr_counters.remove(&RrKey::Rest(gear.to_owned()));
        }
    }

    /// Get all instances of a specific gear
    #[must_use]
    pub fn instances_of(&self, gear: &str) -> Vec<Arc<GearInstance>> {
        self.inner.get(gear).map(|v| v.clone()).unwrap_or_default()
    }

    /// Get all instances across all gears
    #[must_use]
    pub fn all_instances(&self) -> Vec<Arc<GearInstance>> {
        self.inner
            .iter()
            .flat_map(|entry| entry.value().clone())
            .collect()
    }

    /// Quarantine or evict stale instances based on heartbeat policy
    pub fn evict_stale(&self, now: Instant) {
        use InstanceState::{Draining, Quarantined};
        let mut empty_gears = Vec::new();

        for mut entry in self.inner.iter_mut() {
            let gear = entry.key().clone();
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
                empty_gears.push(gear);
            }
        }

        for gear in empty_gears {
            self.inner.remove(&gear);
            self.rr_counters.remove(&RrKey::Gear(gear.clone()));
            self.rr_counters.remove(&RrKey::Rest(gear));
        }
    }

    /// Pick an instance using round-robin selection, preferring healthy instances
    #[must_use]
    pub fn pick_instance_round_robin(&self, gear: &str) -> Option<Arc<GearInstance>> {
        // Holding the `self.inner` shard guard across a `self.rr_counters`
        // access is a deadlock hazard; collect candidates first, then release.
        let candidates: Vec<Arc<GearInstance>> = {
            let instances_entry = self.inner.get(gear)?;
            let instances = instances_entry.value();

            if instances.is_empty() {
                return None;
            }

            let healthy: Vec<Arc<GearInstance>> = instances
                .iter()
                .filter(|inst| {
                    matches!(inst.state(), InstanceState::Healthy | InstanceState::Ready)
                })
                .cloned()
                .collect();

            if healthy.is_empty() {
                instances.clone()
            } else {
                healthy
            }
        };

        if candidates.is_empty() {
            return None;
        }

        let len = candidates.len();
        let idx = {
            let mut counter = self
                .rr_counters
                .entry(RrKey::Gear(gear.to_owned()))
                .or_insert(0);
            let idx = *counter % len;
            *counter = (*counter + 1) % len;
            idx
        };

        candidates.get(idx).cloned()
    }

    /// Pick a gear instance that has a REST endpoint, using round-robin
    /// selection. Returns the instance and a clone of its REST endpoint,
    /// preferring healthy/ready instances.
    #[must_use]
    pub fn pick_rest_gear(&self, gear_name: &str) -> Option<(Arc<GearInstance>, Endpoint)> {
        // Collect candidate `Arc<GearInstance>`s under the `self.inner` shard
        // guard, then drop it before touching `self.rr_counters`. Holding two
        // DashMap guards across an unrelated map access is a deadlock hazard
        // and blocks writes to the `inner` shard for the duration.
        let candidates: Vec<Arc<GearInstance>> = {
            let instances_entry = self.inner.get(gear_name)?;
            let instances = instances_entry.value();

            let healthy: Vec<Arc<GearInstance>> = instances
                .iter()
                .filter(|inst| {
                    inst.rest_endpoint.is_some()
                        && matches!(inst.state(), InstanceState::Healthy | InstanceState::Ready)
                })
                .cloned()
                .collect();

            if healthy.is_empty() {
                instances
                    .iter()
                    .filter(|inst| inst.rest_endpoint.is_some())
                    .cloned()
                    .collect()
            } else {
                healthy
            }
        };

        if candidates.is_empty() {
            return None;
        }

        let len = candidates.len();
        let pick = {
            let mut counter = self
                .rr_counters
                .entry(RrKey::Rest(gear_name.to_owned()))
                .or_insert(0);
            let idx = *counter % len;
            *counter = (*counter + 1) % len;
            idx
        };

        let chosen = &candidates[pick];
        chosen
            .rest_endpoint
            .clone()
            .map(|ep| (Arc::clone(chosen), ep))
    }

    /// Pick a service endpoint using round-robin, returning (gear, instance, endpoint).
    /// Prefers healthy/ready instances and automatically rotates among them.
    #[must_use]
    pub fn pick_service_round_robin(
        &self,
        service_name: &str,
    ) -> Option<(String, Arc<GearInstance>, Endpoint)> {
        // Holding any `self.inner` shard guard across a `self.rr_counters`
        // access is a deadlock hazard; collect candidates first, then release.
        let candidates: Vec<(String, Arc<GearInstance>, Endpoint)> = {
            let mut candidates = Vec::new();
            for entry in &self.inner {
                let gear = entry.key().clone();
                for inst in entry.value() {
                    if let Some(ep) = inst.grpc_services.get(service_name) {
                        let state = inst.state();
                        if matches!(state, InstanceState::Healthy | InstanceState::Ready) {
                            candidates.push((gear.clone(), inst.clone(), ep.clone()));
                        }
                    }
                }
            }
            candidates
        };

        if candidates.is_empty() {
            return None;
        }

        let len = candidates.len();
        let idx = {
            let mut counter = self
                .rr_counters
                .entry(RrKey::Service(service_name.to_owned()))
                .or_insert(0);
            let idx = *counter % len;
            *counter = (*counter + 1) % len;
            idx
        };

        candidates.get(idx).cloned()
    }
}

impl Default for GearManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn test_register_and_retrieve_instances() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();
        let instance = Arc::new(
            GearInstance::new("test_gear", instance_id)
                .with_control(Endpoint::http("localhost", 8080))
                .with_version("1.0.0"),
        );

        dir.register_instance(instance);

        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].instance_id, instance_id);
        assert_eq!(instances[0].gear, "test_gear");
        assert_eq!(instances[0].version, Some("1.0.0".to_owned()));
    }

    #[test]
    fn test_register_multiple_instances() {
        let dir = GearManager::new();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let instance1 = Arc::new(GearInstance::new("test_gear", id1));
        let instance2 = Arc::new(GearInstance::new("test_gear", id2));

        dir.register_instance(instance1);
        dir.register_instance(instance2);

        let registered = dir.instances_of("test_gear");
        assert_eq!(registered.len(), 2);

        let ids: Vec<_> = registered.iter().map(|i| i.instance_id).collect();
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    #[test]
    fn test_update_existing_instance() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();

        let initial_instance =
            Arc::new(GearInstance::new("test_gear", instance_id).with_version("1.0.0"));
        dir.register_instance(initial_instance);

        let updated_instance =
            Arc::new(GearInstance::new("test_gear", instance_id).with_version("2.0.0"));
        dir.register_instance(updated_instance);

        let registered = dir.instances_of("test_gear");
        assert_eq!(registered.len(), 1, "Should not duplicate instance");
        assert_eq!(registered[0].version, Some("2.0.0".to_owned()));
    }

    #[test]
    fn test_mark_ready() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();
        let instance = Arc::new(GearInstance::new("test_gear", instance_id));

        dir.register_instance(instance);

        dir.mark_ready("test_gear", instance_id);

        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert!(matches!(instances[0].state(), InstanceState::Ready));
    }

    #[test]
    fn test_update_heartbeat() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();
        let instance = Arc::new(GearInstance::new("test_gear", instance_id));
        let initial_heartbeat = instance.last_heartbeat();

        dir.register_instance(instance);

        // Sleep to ensure time difference
        sleep(Duration::from_millis(10));

        let new_heartbeat = Instant::now();
        dir.update_heartbeat("test_gear", instance_id, new_heartbeat);

        let instances = dir.instances_of("test_gear");
        assert!(instances[0].last_heartbeat() > initial_heartbeat);
        assert!(matches!(instances[0].state(), InstanceState::Healthy));
    }

    #[test]
    fn test_all_instances() {
        let dir = GearManager::new();

        let instance1 = Arc::new(GearInstance::new("gear_a", Uuid::new_v4()));
        let instance2 = Arc::new(GearInstance::new("gear_b", Uuid::new_v4()));
        let instance3 = Arc::new(GearInstance::new("gear_a", Uuid::new_v4()));

        dir.register_instance(instance1);
        dir.register_instance(instance2);
        dir.register_instance(instance3);

        let all = dir.all_instances();
        assert_eq!(all.len(), 3);

        let gears: Vec<_> = all.iter().map(|i| i.gear.as_str()).collect();
        assert_eq!(gears.iter().filter(|&m| *m == "gear_a").count(), 2);
        assert_eq!(gears.iter().filter(|&m| *m == "gear_b").count(), 1);
    }

    #[test]
    fn test_pick_instance_round_robin() {
        let dir = GearManager::new();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let instance1 = Arc::new(GearInstance::new("test_gear", id1));
        let instance2 = Arc::new(GearInstance::new("test_gear", id2));

        dir.register_instance(instance1);
        dir.register_instance(instance2);

        // Pick three times to verify round-robin behavior
        let picked1 = dir.pick_instance_round_robin("test_gear").unwrap();
        let picked2 = dir.pick_instance_round_robin("test_gear").unwrap();
        let picked3 = dir.pick_instance_round_robin("test_gear").unwrap();

        let ids = [
            picked1.instance_id,
            picked2.instance_id,
            picked3.instance_id,
        ];

        // With 2 instances, we expect round-robin pattern like A, B, A
        // Check that both instance IDs appear and that at least one repeats
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
        // First and third pick should be the same (round-robin wraps)
        assert_eq!(picked1.instance_id, picked3.instance_id);
        // Second pick should be different from the first
        assert_ne!(picked1.instance_id, picked2.instance_id);
    }

    #[test]
    fn test_pick_instance_none_available() {
        let dir = GearManager::new();
        let picked = dir.pick_instance_round_robin("nonexistent_gear");
        assert!(picked.is_none());
    }

    #[test]
    fn test_endpoint_creation() {
        let plain_ep = Endpoint::http("localhost", 8080);
        assert_eq!(plain_ep.uri, "http://localhost:8080");

        let secure_ep = Endpoint::https("localhost", 8443);
        assert_eq!(secure_ep.uri, "https://localhost:8443");

        let uds_ep = Endpoint::uds("/tmp/socket.sock");
        assert!(uds_ep.uri.starts_with("unix://"));
        assert!(uds_ep.uri.contains("socket.sock"));

        let custom_ep = Endpoint::from_uri("http://example.com");
        assert_eq!(custom_ep.uri, "http://example.com");
    }

    #[test]
    fn test_endpoint_kind() {
        let plain_ep = Endpoint::http("127.0.0.1", 8080);
        match plain_ep.kind() {
            EndpointKind::Tcp(addr) => {
                assert_eq!(addr.ip().to_string(), "127.0.0.1");
                assert_eq!(addr.port(), 8080);
            }
            _ => panic!("Expected TCP endpoint for http"),
        }

        let secure_ep = Endpoint::https("127.0.0.1", 8443);
        match secure_ep.kind() {
            EndpointKind::Tcp(addr) => {
                assert_eq!(addr.ip().to_string(), "127.0.0.1");
                assert_eq!(addr.port(), 8443);
            }
            _ => panic!("Expected TCP endpoint for https"),
        }

        let uds_ep = Endpoint::uds("/tmp/test.sock");
        match uds_ep.kind() {
            EndpointKind::Uds(path) => {
                assert!(path.to_string_lossy().contains("test.sock"));
            }
            _ => panic!("Expected UDS endpoint"),
        }

        let other_ep = Endpoint::from_uri("grpc://example.com");
        match other_ep.kind() {
            EndpointKind::Other(uri) => {
                assert_eq!(uri, "grpc://example.com");
            }
            _ => panic!("Expected Other endpoint"),
        }
    }

    #[test]
    fn test_gear_instance_builder() {
        let instance_id = Uuid::new_v4();
        let instance = GearInstance::new("test_gear", instance_id)
            .with_control(Endpoint::http("localhost", 8080))
            .with_version("1.2.3")
            .with_grpc_service("service1", Endpoint::http("localhost", 8082))
            .with_grpc_service("service2", Endpoint::http("localhost", 8083));

        assert_eq!(instance.gear, "test_gear");
        assert_eq!(instance.instance_id, instance_id);
        assert!(instance.control.is_some());
        assert_eq!(instance.version, Some("1.2.3".to_owned()));
        assert_eq!(instance.grpc_services.len(), 2);
        assert!(instance.grpc_services.contains_key("service1"));
        assert!(instance.grpc_services.contains_key("service2"));
        assert!(matches!(instance.state(), InstanceState::Registered));
    }

    #[test]
    fn test_quarantine_and_evict() {
        let ttl = Duration::from_millis(50);
        let grace = Duration::from_millis(50);
        let dir = GearManager::new().with_heartbeat_policy(ttl, grace);

        let now = Instant::now();
        let instance = GearInstance::new("test_gear", Uuid::new_v4());
        // Set the last heartbeat to be stale
        instance.inner.write().last_heartbeat = now
            .checked_sub(ttl)
            .and_then(|t| t.checked_sub(Duration::from_millis(10)))
            .expect("test duration subtraction should not underflow");

        dir.register_instance(Arc::new(instance));

        dir.evict_stale(now);
        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert!(matches!(instances[0].state(), InstanceState::Quarantined));

        let later = now + grace + Duration::from_millis(10);
        dir.evict_stale(later);

        let instances_after = dir.instances_of("test_gear");
        assert!(instances_after.is_empty());
    }

    #[test]
    fn test_instances_of_empty() {
        let dir = GearManager::new();
        let instances = dir.instances_of("nonexistent");
        assert!(instances.is_empty());
    }

    #[test]
    fn test_rr_prefers_healthy() {
        let dir = GearManager::new();

        // Create two instances: one healthy, one quarantined
        let healthy_id = Uuid::new_v4();
        let healthy = Arc::new(GearInstance::new("test_gear", healthy_id));
        dir.register_instance(healthy);
        dir.update_heartbeat("test_gear", healthy_id, Instant::now());

        let quarantined_id = Uuid::new_v4();
        let quarantined = Arc::new(GearInstance::new("test_gear", quarantined_id));
        dir.register_instance(quarantined);
        dir.mark_quarantined("test_gear", quarantined_id);

        // RR should only pick the healthy instance
        for _ in 0..5 {
            let picked = dir.pick_instance_round_robin("test_gear").unwrap();
            assert_eq!(picked.instance_id, healthy_id);
        }
    }

    #[test]
    fn test_pick_service_round_robin() {
        let dir = GearManager::new();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        // Register two instances providing the same service
        let inst1 = Arc::new(
            GearInstance::new("test_gear", id1)
                .with_grpc_service("test.Service", Endpoint::http("127.0.0.1", 8001)),
        );
        let inst2 = Arc::new(
            GearInstance::new("test_gear", id2)
                .with_grpc_service("test.Service", Endpoint::http("127.0.0.1", 8002)),
        );

        dir.register_instance(inst1);
        dir.register_instance(inst2);

        // Mark both as healthy
        dir.update_heartbeat("test_gear", id1, Instant::now());
        dir.update_heartbeat("test_gear", id2, Instant::now());

        // Pick should rotate between instances
        let pick1 = dir.pick_service_round_robin("test.Service");
        let pick2 = dir.pick_service_round_robin("test.Service");
        let pick3 = dir.pick_service_round_robin("test.Service");

        assert!(pick1.is_some());
        assert!(pick2.is_some());
        assert!(pick3.is_some());

        let (_, inst1, ep1) = pick1.unwrap();
        let (_, inst2, ep2) = pick2.unwrap();
        let (_, inst3, _) = pick3.unwrap();

        // First and third should be the same (round-robin)
        assert_eq!(inst1.instance_id, inst3.instance_id);
        // First and second should be different
        assert_ne!(inst1.instance_id, inst2.instance_id);
        // Endpoints should differ
        assert_ne!(ep1, ep2);
    }

    // Note: the builder `with_rest_endpoint` is exercised end-to-end by
    // `test_pick_rest_gear_found` and `test_register_instance_with_rest_endpoint`
    // (in `directory.rs`). A standalone constructor-echo test was removed —
    // it would still pass even if `with_rest_endpoint` stored the value
    // under the wrong field, as long as the URL string round-tripped.

    #[test]
    fn test_pick_rest_gear_none_available() {
        let dir = GearManager::new();

        // No instances at all
        let result = dir.pick_rest_gear("nonexistent");
        assert!(result.is_none());

        // Instance exists but has no REST endpoint
        let id = Uuid::new_v4();
        let inst = Arc::new(GearInstance::new("grpc_only", id));
        dir.register_instance(inst);
        dir.update_heartbeat("grpc_only", id, Instant::now());

        let result = dir.pick_rest_gear("grpc_only");
        assert!(result.is_none());
    }

    #[test]
    fn test_pick_rest_gear_found() {
        let dir = GearManager::new();

        let id = Uuid::new_v4();
        let inst = Arc::new(
            GearInstance::new("billing", id)
                .with_rest_endpoint(Endpoint::http("billing-host", 8080)),
        );
        dir.register_instance(inst);
        dir.update_heartbeat("billing", id, Instant::now());

        let result = dir.pick_rest_gear("billing");
        assert!(result.is_some());

        let (picked_inst, ep) = result.unwrap();
        assert_eq!(picked_inst.instance_id, id);
        assert_eq!(ep.uri, "http://billing-host:8080");
    }

    #[test]
    fn test_pick_rest_gear_round_robin() {
        let dir = GearManager::new();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let inst1 = Arc::new(
            GearInstance::new("billing", id1).with_rest_endpoint(Endpoint::http("host1", 8080)),
        );
        let inst2 = Arc::new(
            GearInstance::new("billing", id2).with_rest_endpoint(Endpoint::http("host2", 8080)),
        );
        dir.register_instance(inst1);
        dir.register_instance(inst2);
        dir.update_heartbeat("billing", id1, Instant::now());
        dir.update_heartbeat("billing", id2, Instant::now());

        let pick1 = dir.pick_rest_gear("billing").unwrap();
        let pick2 = dir.pick_rest_gear("billing").unwrap();
        let pick3 = dir.pick_rest_gear("billing").unwrap();

        // Round-robin: first and third should be the same
        assert_eq!(pick1.0.instance_id, pick3.0.instance_id);
        // First and second should differ
        assert_ne!(pick1.0.instance_id, pick2.0.instance_id);
    }
}
