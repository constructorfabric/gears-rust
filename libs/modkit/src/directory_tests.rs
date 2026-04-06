use super::*;

#[tokio::test]
async fn test_resolve_grpc_service_not_found() {
    let dir = Arc::new(ModuleManager::new());
    let api = LocalDirectoryClient::new(dir);

    let result = api.resolve_grpc_service("nonexistent.Service").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_register_instance_via_api() {
    let dir = Arc::new(ModuleManager::new());
    let api = LocalDirectoryClient::new(dir.clone());

    let instance_id = Uuid::new_v4();
    // Register an instance through the API
    let register_info = RegisterInstanceInfo {
        module: "test_module".to_owned(),
        instance_id: instance_id.to_string(),
        grpc_services: vec![(
            "test.Service".to_owned(),
            ServiceEndpoint::http("127.0.0.1", 8001),
        )],
        version: Some("1.0.0".to_owned()),
    };

    api.register_instance(register_info).await.unwrap();

    // Verify the instance was registered
    let instances = dir.instances_of("test_module");
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_id, instance_id);
    assert_eq!(instances[0].version, Some("1.0.0".to_owned()));
    assert!(instances[0].grpc_services.contains_key("test.Service"));
}

#[tokio::test]
async fn test_deregister_instance_via_api() {
    let dir = Arc::new(ModuleManager::new());
    let api = LocalDirectoryClient::new(dir.clone());

    let instance_id = Uuid::new_v4();
    // Register an instance first
    let inst = Arc::new(ModuleInstance::new("test_module", instance_id));
    dir.register_instance(inst);

    // Verify it exists
    assert_eq!(dir.instances_of("test_module").len(), 1);

    // Deregister via API
    api.deregister_instance("test_module", &instance_id.to_string())
        .await
        .unwrap();

    // Verify it's gone
    assert_eq!(dir.instances_of("test_module").len(), 0);
}

#[tokio::test]
async fn test_send_heartbeat_via_api() {
    use crate::runtime::InstanceState;

    let dir = Arc::new(ModuleManager::new());
    let api = LocalDirectoryClient::new(dir.clone());

    let instance_id = Uuid::new_v4();
    // Register an instance first
    let inst = Arc::new(ModuleInstance::new("test_module", instance_id));
    dir.register_instance(inst);

    // Verify initial state is Registered
    let instances = dir.instances_of("test_module");
    assert_eq!(instances[0].state(), InstanceState::Registered);

    // Send heartbeat via API
    api.send_heartbeat("test_module", &instance_id.to_string())
        .await
        .unwrap();

    // Verify state transitioned to Healthy
    let instances = dir.instances_of("test_module");
    assert_eq!(instances[0].state(), InstanceState::Healthy);
}
