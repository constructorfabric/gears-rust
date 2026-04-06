use super::*;
use std::thread::sleep;
use std::time::Duration;

#[test]
fn test_register_and_retrieve_instances() {
    let dir = ModuleManager::new();
    let instance_id = Uuid::new_v4();
    let instance = Arc::new(
        ModuleInstance::new("test_module", instance_id)
            .with_control(Endpoint::http("localhost", 8080))
            .with_version("1.0.0"),
    );

    dir.register_instance(instance);

    let instances = dir.instances_of("test_module");
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_id, instance_id);
    assert_eq!(instances[0].module, "test_module");
    assert_eq!(instances[0].version, Some("1.0.0".to_owned()));
}

#[test]
fn test_register_multiple_instances() {
    let dir = ModuleManager::new();

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let instance1 = Arc::new(ModuleInstance::new("test_module", id1));
    let instance2 = Arc::new(ModuleInstance::new("test_module", id2));

    dir.register_instance(instance1);
    dir.register_instance(instance2);

    let registered = dir.instances_of("test_module");
    assert_eq!(registered.len(), 2);

    let ids: Vec<_> = registered.iter().map(|i| i.instance_id).collect();
    assert!(ids.contains(&id1));
    assert!(ids.contains(&id2));
}

#[test]
fn test_update_existing_instance() {
    let dir = ModuleManager::new();
    let instance_id = Uuid::new_v4();

    let initial_instance =
        Arc::new(ModuleInstance::new("test_module", instance_id).with_version("1.0.0"));
    dir.register_instance(initial_instance);

    let updated_instance =
        Arc::new(ModuleInstance::new("test_module", instance_id).with_version("2.0.0"));
    dir.register_instance(updated_instance);

    let registered = dir.instances_of("test_module");
    assert_eq!(registered.len(), 1, "Should not duplicate instance");
    assert_eq!(registered[0].version, Some("2.0.0".to_owned()));
}

#[test]
fn test_mark_ready() {
    let dir = ModuleManager::new();
    let instance_id = Uuid::new_v4();
    let instance = Arc::new(ModuleInstance::new("test_module", instance_id));

    dir.register_instance(instance);

    dir.mark_ready("test_module", instance_id);

    let instances = dir.instances_of("test_module");
    assert_eq!(instances.len(), 1);
    assert!(matches!(instances[0].state(), InstanceState::Ready));
}

#[test]
fn test_update_heartbeat() {
    let dir = ModuleManager::new();
    let instance_id = Uuid::new_v4();
    let instance = Arc::new(ModuleInstance::new("test_module", instance_id));
    let initial_heartbeat = instance.last_heartbeat();

    dir.register_instance(instance);

    // Sleep to ensure time difference
    sleep(Duration::from_millis(10));

    let new_heartbeat = Instant::now();
    dir.update_heartbeat("test_module", instance_id, new_heartbeat);

    let instances = dir.instances_of("test_module");
    assert!(instances[0].last_heartbeat() > initial_heartbeat);
    assert!(matches!(instances[0].state(), InstanceState::Healthy));
}

#[test]
fn test_all_instances() {
    let dir = ModuleManager::new();

    let instance1 = Arc::new(ModuleInstance::new("module_a", Uuid::new_v4()));
    let instance2 = Arc::new(ModuleInstance::new("module_b", Uuid::new_v4()));
    let instance3 = Arc::new(ModuleInstance::new("module_a", Uuid::new_v4()));

    dir.register_instance(instance1);
    dir.register_instance(instance2);
    dir.register_instance(instance3);

    let all = dir.all_instances();
    assert_eq!(all.len(), 3);

    let modules: Vec<_> = all.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(modules.iter().filter(|&m| *m == "module_a").count(), 2);
    assert_eq!(modules.iter().filter(|&m| *m == "module_b").count(), 1);
}

#[test]
fn test_pick_instance_round_robin() {
    let dir = ModuleManager::new();

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let instance1 = Arc::new(ModuleInstance::new("test_module", id1));
    let instance2 = Arc::new(ModuleInstance::new("test_module", id2));

    dir.register_instance(instance1);
    dir.register_instance(instance2);

    // Pick three times to verify round-robin behavior
    let picked1 = dir.pick_instance_round_robin("test_module").unwrap();
    let picked2 = dir.pick_instance_round_robin("test_module").unwrap();
    let picked3 = dir.pick_instance_round_robin("test_module").unwrap();

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
    let dir = ModuleManager::new();
    let picked = dir.pick_instance_round_robin("nonexistent_module");
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
fn test_module_instance_builder() {
    let instance_id = Uuid::new_v4();
    let instance = ModuleInstance::new("test_module", instance_id)
        .with_control(Endpoint::http("localhost", 8080))
        .with_version("1.2.3")
        .with_grpc_service("service1", Endpoint::http("localhost", 8082))
        .with_grpc_service("service2", Endpoint::http("localhost", 8083));

    assert_eq!(instance.module, "test_module");
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
    let dir = ModuleManager::new().with_heartbeat_policy(ttl, grace);

    let now = Instant::now();
    let instance = ModuleInstance::new("test_module", Uuid::new_v4());
    // Set the last heartbeat to be stale
    instance.inner.write().last_heartbeat = now
        .checked_sub(ttl)
        .and_then(|t| t.checked_sub(Duration::from_millis(10)))
        .expect("test duration subtraction should not underflow");

    dir.register_instance(Arc::new(instance));

    dir.evict_stale(now);
    let instances = dir.instances_of("test_module");
    assert_eq!(instances.len(), 1);
    assert!(matches!(instances[0].state(), InstanceState::Quarantined));

    let later = now + grace + Duration::from_millis(10);
    dir.evict_stale(later);

    let instances_after = dir.instances_of("test_module");
    assert!(instances_after.is_empty());
}

#[test]
fn test_instances_of_empty() {
    let dir = ModuleManager::new();
    let instances = dir.instances_of("nonexistent");
    assert!(instances.is_empty());
}

#[test]
fn test_rr_prefers_healthy() {
    let dir = ModuleManager::new();

    // Create two instances: one healthy, one quarantined
    let healthy_id = Uuid::new_v4();
    let healthy = Arc::new(ModuleInstance::new("test_module", healthy_id));
    dir.register_instance(healthy);
    dir.update_heartbeat("test_module", healthy_id, Instant::now());

    let quarantined_id = Uuid::new_v4();
    let quarantined = Arc::new(ModuleInstance::new("test_module", quarantined_id));
    dir.register_instance(quarantined);
    dir.mark_quarantined("test_module", quarantined_id);

    // RR should only pick the healthy instance
    for _ in 0..5 {
        let picked = dir.pick_instance_round_robin("test_module").unwrap();
        assert_eq!(picked.instance_id, healthy_id);
    }
}

#[test]
fn test_pick_service_round_robin() {
    let dir = ModuleManager::new();

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    // Register two instances providing the same service
    let inst1 = Arc::new(
        ModuleInstance::new("test_module", id1)
            .with_grpc_service("test.Service", Endpoint::http("127.0.0.1", 8001)),
    );
    let inst2 = Arc::new(
        ModuleInstance::new("test_module", id2)
            .with_grpc_service("test.Service", Endpoint::http("127.0.0.1", 8002)),
    );

    dir.register_instance(inst1);
    dir.register_instance(inst2);

    // Mark both as healthy
    dir.update_heartbeat("test_module", id1, Instant::now());
    dir.update_heartbeat("test_module", id2, Instant::now());

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
