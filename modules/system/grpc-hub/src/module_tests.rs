use super::*;
use async_trait::async_trait;
use http::{Request, Response};
use modkit::contracts::Module;
use modkit::lifecycle::ReadySignal;
use modkit::runtime::{GrpcInstallerData, GrpcInstallerStore, ModuleInstallers};
use modkit::{client_hub::ClientHub, config::ConfigProvider, context::ModuleCtx};
use std::{
    convert::Infallible,
    future,
    sync::Arc,
    task::{Context as TaskContext, Poll},
};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;
use tonic::{body::Body, server::NamedService};
use tower::Service;
use uuid::Uuid;

const SERVICE_A: &str = "grpc_hub.test.ServiceA";
const SERVICE_B: &str = "grpc_hub.test.ServiceB";

#[derive(Clone)]
struct ServiceAImpl;

#[derive(Clone)]
struct ServiceBImpl;

impl NamedService for ServiceAImpl {
    const NAME: &'static str = SERVICE_A;
}

impl NamedService for ServiceBImpl {
    const NAME: &'static str = SERVICE_B;
}

impl Service<Request<Body>> for ServiceAImpl {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Request<Body>) -> Self::Future {
        future::ready(Ok(Response::new(Body::empty())))
    }
}

impl Service<Request<Body>> for ServiceBImpl {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Request<Body>) -> Self::Future {
        future::ready(Ok(Response::new(Body::empty())))
    }
}

fn installer_a() -> modkit::contracts::RegisterGrpcServiceFn {
    modkit::contracts::RegisterGrpcServiceFn {
        service_name: SERVICE_A,
        register: Box::new(|routes| {
            routes.add_service(ServiceAImpl);
        }),
    }
}

fn installer_b() -> modkit::contracts::RegisterGrpcServiceFn {
    modkit::contracts::RegisterGrpcServiceFn {
        service_name: SERVICE_B,
        register: Box::new(|routes| {
            routes.add_service(ServiceBImpl);
        }),
    }
}

#[tokio::test]
async fn test_run_with_installers_rejects_duplicates() {
    let hub = GrpcHub::default();
    hub.set_listen_addr_tcp("127.0.0.1:0".parse().unwrap());
    let data = GrpcInstallerData {
        modules: vec![ModuleInstallers {
            module_name: "test".to_owned(),
            installers: vec![installer_a(), installer_a()],
        }],
    };
    let cancel = CancellationToken::new();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let ready = ReadySignal::from_sender(tx);

    let result = hub.run_with_installers(data, cancel, ready).await;

    assert!(result.is_err(), "duplicate services should error");
}

#[tokio::test]
async fn test_run_with_installers_starts_server() {
    let hub = Arc::new(GrpcHub::default());
    hub.set_listen_addr_tcp("127.0.0.1:0".parse().unwrap());
    let data = GrpcInstallerData {
        modules: vec![ModuleInstallers {
            module_name: "test".to_owned(),
            installers: vec![installer_a(), installer_b()],
        }],
    };
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let ready = ReadySignal::from_sender(tx);

    let hub_task = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.run_with_installers(data, cancel, ready).await })
    };

    tokio::spawn(async move {
        sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    tokio::time::timeout(Duration::from_secs(1), rx)
        .await
        .expect("ready signal should fire")
        .expect("ready channel should complete");

    hub_task
        .await
        .expect("task should join successfully")
        .expect("server should exit cleanly");
}

#[tokio::test]
async fn test_serve_with_system_context() {
    let hub = Arc::new(GrpcHub::default());
    hub.set_listen_addr_tcp("127.0.0.1:0".parse().unwrap());

    // Wire system context with installers
    let installer_store = Arc::new(GrpcInstallerStore::new());
    installer_store
        .set(GrpcInstallerData {
            modules: vec![ModuleInstallers {
                module_name: "test".to_owned(),
                installers: vec![installer_a()],
            }],
        })
        .expect("store should accept installers");

    let module_manager = Arc::new(modkit::runtime::ModuleManager::new());
    let sys_ctx = modkit::runtime::SystemContext::new(
        Uuid::new_v4(),
        module_manager,
        Arc::clone(&installer_store),
    );

    hub.pre_init(&sys_ctx)
        .expect("pre_init should set installer_store and instance_id");

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let ready = ReadySignal::from_sender(tx);

    let serve_task = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.serve(cancel, ready).await })
    };

    tokio::spawn(async move {
        sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    tokio::time::timeout(Duration::from_secs(1), rx)
        .await
        .expect("ready signal should fire")
        .expect("ready signal should complete");

    serve_task
        .await
        .expect("task should join")
        .expect("serve should complete without error");

    // After serve completes, installer_store should be empty (consumed)
    assert!(
        installer_store.is_empty(),
        "installers should be consumed after serve completes"
    );
}

#[tokio::test]
async fn test_init_parses_listen_addr() {
    #[derive(Default)]
    struct ConfigProviderWithAddr;
    impl ConfigProvider for ConfigProviderWithAddr {
        fn get_module_config(&self, module_name: &str) -> Option<&serde_json::Value> {
            if module_name == "grpc-hub" {
                use std::sync::OnceLock;
                static CONFIG: OnceLock<serde_json::Value> = OnceLock::new();
                Some(CONFIG.get_or_init(|| {
                    serde_json::json!({
                        "config": {
                            "listen_addr": "127.0.0.1:10"
                        }
                    })
                }))
            } else {
                None
            }
        }
    }

    let hub = GrpcHub::default();
    let cancel = CancellationToken::new();

    let ctx = ModuleCtx::new(
        "grpc-hub",
        Uuid::new_v4(),
        Arc::new(ConfigProviderWithAddr),
        Arc::new(ClientHub::default()),
        cancel,
        None,
    );

    hub.init(&ctx).await.expect("init should succeed");

    assert_eq!(
        hub.listen_addr_tcp().expect("should be TCP"),
        "127.0.0.1:10".parse().unwrap()
    );
}

#[tokio::test]
#[cfg(unix)]
async fn test_init_parses_uds_addr() {
    #[derive(Default)]
    struct ConfigProviderWithUds;
    impl ConfigProvider for ConfigProviderWithUds {
        fn get_module_config(&self, module_name: &str) -> Option<&serde_json::Value> {
            if module_name == "grpc-hub" {
                use std::sync::OnceLock;
                static CONFIG: OnceLock<serde_json::Value> = OnceLock::new();
                Some(CONFIG.get_or_init(|| {
                    serde_json::json!({
                        "config": {
                            "listen_addr": "uds:///tmp/test_grpc.sock"
                        }
                    })
                }))
            } else {
                None
            }
        }
    }

    let hub = GrpcHub::default();
    let cancel = CancellationToken::new();

    let ctx = ModuleCtx::new(
        "grpc-hub",
        Uuid::new_v4(),
        Arc::new(ConfigProviderWithUds),
        Arc::new(ClientHub::default()),
        cancel,
        None,
    );

    hub.init(&ctx).await.expect("init should succeed");

    // Verify that listen_addr_tcp returns None for UDS config
    assert!(
        hub.listen_addr_tcp().is_none(),
        "Expected UDS config, not TCP"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn test_init_parses_uds_listen_addr_and_serves() {
    use tempfile::TempDir;

    // Custom ConfigProvider returning uds:// path
    struct ConfigProviderWithUds {
        config_value: serde_json::Value,
    }
    impl ConfigProvider for ConfigProviderWithUds {
        fn get_module_config(&self, module_name: &str) -> Option<&serde_json::Value> {
            if module_name == "grpc-hub" {
                Some(&self.config_value)
            } else {
                None
            }
        }
    }

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let socket_path = temp_dir.path().join("test_grpc_hub.sock");
    let socket_path_str = format!("uds://{}", socket_path.display());

    let hub = Arc::new(GrpcHub::default());
    let cancel = CancellationToken::new();

    let config_provider = ConfigProviderWithUds {
        config_value: serde_json::json!({
            "config": {
                "listen_addr": socket_path_str
            }
        }),
    };

    let ctx = ModuleCtx::new(
        "grpc-hub",
        Uuid::new_v4(),
        Arc::new(config_provider),
        Arc::new(ClientHub::default()),
        cancel.clone(),
        None,
    );

    hub.init(&ctx).await.expect("init should succeed");

    let installers = vec![installer_a()];
    let data = GrpcInstallerData {
        modules: vec![ModuleInstallers {
            module_name: "test".to_owned(),
            installers,
        }],
    };
    let cancel_clone = cancel.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let ready = ReadySignal::from_sender(tx);

    let hub_task = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.run_with_installers(data, cancel, ready).await })
    };

    tokio::spawn(async move {
        sleep(Duration::from_millis(100)).await;
        cancel_clone.cancel();
    });

    tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("ready signal should fire")
        .expect("ready channel should complete");

    // Verify socket file was created
    assert!(socket_path.exists(), "Unix socket file should be created");

    hub_task
        .await
        .expect("task should join successfully")
        .expect("server should exit cleanly");
}

#[tokio::test]
#[cfg(windows)]
async fn test_named_pipe_listen_and_shutdown() {
    // Custom ConfigProvider returning named pipe address
    struct ConfigProviderWithNamedPipe;
    impl ConfigProvider for ConfigProviderWithNamedPipe {
        fn get_module_config(&self, module_name: &str) -> Option<&serde_json::Value> {
            if module_name == "grpc-hub" {
                use std::sync::OnceLock;
                static CONFIG: OnceLock<serde_json::Value> = OnceLock::new();
                Some(CONFIG.get_or_init(|| {
                    serde_json::json!({
                        "config": {
                            "listen_addr": r"pipe://\\.\pipe\test_grpc_hub"
                        }
                    })
                }))
            } else {
                None
            }
        }
    }

    let hub = Arc::new(GrpcHub::default());
    let cancel = CancellationToken::new();

    let ctx = ModuleCtx::new(
        "grpc-hub",
        Uuid::new_v4(),
        Arc::new(ConfigProviderWithNamedPipe),
        Arc::new(ClientHub::default()),
        cancel.clone(),
        None,
    );

    hub.init(&ctx).await.expect("init should succeed");

    // Verify that listen_addr_tcp returns None for named pipe config
    assert!(
        hub.listen_addr_tcp().is_none(),
        "Expected named pipe config, not TCP"
    );

    let installers = vec![installer_a()];
    let data = GrpcInstallerData {
        modules: vec![ModuleInstallers {
            module_name: "test".to_owned(),
            installers,
        }],
    };
    let cancel_clone = cancel.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let ready = ReadySignal::from_sender(tx);

    let hub_task = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.run_with_installers(data, cancel, ready).await })
    };

    // Give the server a moment to start, then cancel
    tokio::spawn(async move {
        sleep(Duration::from_millis(100)).await;
        cancel_clone.cancel();
    });

    tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("ready signal should fire")
        .expect("ready channel should complete");

    hub_task
        .await
        .expect("task should join successfully")
        .expect("server should exit cleanly");
}

#[tokio::test]
async fn test_run_with_no_installers_exits_gracefully() {
    let hub = GrpcHub::default();
    hub.set_listen_addr_tcp("127.0.0.1:0".parse().unwrap());
    let data = GrpcInstallerData { modules: vec![] };
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let ready = ReadySignal::from_sender(tx);

    let hub_task = tokio::spawn(async move { hub.run_with_installers(data, cancel, ready).await });

    // Schedule cancellation
    tokio::spawn(async move {
        sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    // Should receive ready signal immediately
    tokio::time::timeout(Duration::from_secs(1), rx)
        .await
        .expect("ready signal should fire")
        .expect("ready channel should complete");

    // Task should complete successfully
    hub_task
        .await
        .expect("task should join successfully")
        .expect("should exit cleanly with no services");
}

#[tokio::test]
async fn test_resolve_directory_client_lazy_after_init() {
    use modkit::{
        DirectoryClient as DirectoryClientTrait, RegisterInstanceInfo, ServiceEndpoint,
        ServiceInstanceInfo,
    };

    struct MockDirectoryClient;

    #[async_trait]
    impl DirectoryClientTrait for MockDirectoryClient {
        async fn resolve_grpc_service(
            &self,
            _service_name: &str,
        ) -> anyhow::Result<ServiceEndpoint> {
            Ok(ServiceEndpoint::new("mock://endpoint"))
        }
        async fn list_instances(&self, _module: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
            Ok(vec![])
        }
        async fn register_instance(&self, _info: RegisterInstanceInfo) -> anyhow::Result<()> {
            Ok(())
        }
        async fn deregister_instance(
            &self,
            _module: &str,
            _instance_id: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send_heartbeat(&self, _module: &str, _instance_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct EmptyConfigProvider;
    impl ConfigProvider for EmptyConfigProvider {
        fn get_module_config(&self, _module_name: &str) -> Option<&serde_json::Value> {
            None
        }
    }

    let client_hub = Arc::new(ClientHub::default());
    let hub = GrpcHub::default();
    let cancel = CancellationToken::new();

    // Create context with an empty ClientHub (no DirectoryClient yet)
    let ctx = ModuleCtx::new(
        "grpc-hub",
        Uuid::new_v4(),
        Arc::new(EmptyConfigProvider),
        Arc::clone(&client_hub),
        cancel,
        None,
    );

    hub.init(&ctx).await.expect("init should succeed");

    // DirectoryClient is NOT registered yet — should return None
    assert!(
        hub.resolve_directory_client().is_none(),
        "should be None before DirectoryClient is registered"
    );

    // Simulate module_orchestrator registering DirectoryClient after grpc-hub init
    let mock_dir: Arc<dyn DirectoryClientTrait> = Arc::new(MockDirectoryClient);
    client_hub.register::<dyn DirectoryClientTrait>(mock_dir);

    // Now lazy resolution should find it
    assert!(
        hub.resolve_directory_client().is_some(),
        "should resolve DirectoryClient registered after init()"
    );
}
