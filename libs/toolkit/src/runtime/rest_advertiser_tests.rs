//! REST registration retries must run while application admission is pending.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Notify;

use super::RestAdvertiser;
use crate::{DirectoryClient, RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo};

#[derive(Default)]
struct Directory {
    attempts: AtomicUsize,
    accepted: Notify,
    entered_retry: Notify,
    release_retry: Notify,
    block_retry: bool,
    hang_reads: bool,
    events: Mutex<Vec<&'static str>>,
    sent: Mutex<Vec<RegisterInstanceInfo>>,
    instance_id: String,
}

#[async_trait]
impl DirectoryClient for Directory {
    async fn register_instance(&self, info: RegisterInstanceInfo) -> anyhow::Result<()> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            anyhow::bail!("directory temporarily unavailable");
        }
        self.entered_retry.notify_one();
        if self.block_retry {
            self.release_retry.notified().await;
        }
        self.sent.lock().push(info);
        self.events.lock().push("registered");
        self.accepted.notify_one();
        Ok(())
    }

    async fn deregister_instance(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        self.events.lock().push("deregistered");
        Ok(())
    }

    async fn list_instances(&self, gear: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        if self.hang_reads {
            std::future::pending::<()>().await;
        }
        Ok(vec![
            ServiceInstanceInfo::new(gear, &self.instance_id)
                .with_version(Some("1.2.3".to_owned()))
                .with_grpc_services(vec![(
                    "test.Registry".to_owned(),
                    ServiceEndpoint::new("http://grpc:9000"),
                )]),
        ])
    }

    async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(Vec::new())
    }
    async fn resolve_grpc_service(&self, _service: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("unused")
    }
    async fn resolve_rest_service(&self, _gear: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("unused")
    }
    async fn get_openapi_spec(&self, _gear: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }
    async fn send_heartbeat(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        panic!("registration retries must not introduce a heartbeat owner")
    }
}

fn setup(mut directory: Directory) -> (Arc<RestAdvertiser>, Arc<Directory>) {
    let instance = uuid::Uuid::new_v4();
    directory.instance_id = instance.to_string();
    let directory = Arc::new(directory);
    let advertiser = Arc::new(RestAdvertiser::new(
        instance,
        &tokio_util::sync::CancellationToken::new(),
    ));
    advertiser.enable(directory.clone(), "http://registry:8080".to_owned());
    (advertiser, directory)
}

#[tokio::test(start_paused = true)]
async fn transient_failure_retries_before_a_later_startup_phase() {
    let (advertiser, directory) = setup(Directory::default());
    advertiser.advertise("registry").await;
    // Repeated phases must reuse the retry loop.
    advertiser.advertise("registry").await;
    tokio::time::timeout(Duration::from_secs(2), directory.accepted.notified())
        .await
        .expect("the registry must become discoverable while admission still waits");
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(directory.attempts.load(Ordering::SeqCst), 2);
    let sent = directory.sent.lock().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].version.as_deref(), Some("1.2.3"));
    assert_eq!(sent[0].grpc_services[0].0, "test.Registry");
    assert_eq!(
        sent[0].rest_endpoint.as_ref().unwrap().uri,
        "http://registry:8080"
    );
    assert!(
        sent[0].labels.is_empty(),
        "labels must not be read-modify-written"
    );
    advertiser.deregister_all().await;
}

#[tokio::test(start_paused = true)]
async fn deregistration_waits_for_an_inflight_retry_then_prevents_more_writes() {
    let (advertiser, directory) = setup(Directory {
        block_retry: true,
        ..Directory::default()
    });
    advertiser.advertise("registry").await;
    tokio::time::timeout(Duration::from_secs(2), directory.entered_retry.notified())
        .await
        .expect("retry entered its directory write");
    let stopping = {
        let advertiser = Arc::clone(&advertiser);
        tokio::spawn(async move { advertiser.deregister_all().await })
    };
    tokio::task::yield_now().await;
    assert!(
        directory.events.lock().is_empty(),
        "join registration before deregistering"
    );
    directory.release_retry.notify_one();
    stopping.await.unwrap();
    advertiser.advertise("registry").await;
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(*directory.events.lock(), ["registered", "deregistered"]);
}

#[tokio::test(start_paused = true)]
async fn a_hung_directory_read_does_not_hold_startup_or_shutdown() {
    let (advertiser, _) = setup(Directory {
        hang_reads: true,
        ..Directory::default()
    });
    tokio::time::timeout(Duration::from_secs(10), advertiser.advertise("registry"))
        .await
        .expect("the initial registration attempt must be bounded");
    tokio::time::timeout(Duration::from_secs(10), advertiser.deregister_all())
        .await
        .expect("shutdown must cancel retrying advertisement");
}

#[derive(Default)]
struct RestGear(crate::api::OpenApiRegistryImpl);

#[async_trait]
impl crate::Gear for RestGear {
    async fn init(&self, _ctx: &crate::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl crate::contracts::RestApiCapability for RestGear {
    fn register_rest(
        &self,
        _ctx: &crate::GearCtx,
        router: axum::Router,
        _registry: &dyn crate::contracts::OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }
}

impl crate::contracts::ApiGatewayCapability for RestGear {
    fn bound_endpoint(&self) -> Option<String> {
        Some("http://registry:8080".to_owned())
    }
    fn rest_prepare(
        &self,
        _ctx: &crate::GearCtx,
        router: axum::Router,
        _health: Arc<crate::RestHealthcheckRegistry>,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }
    fn rest_finalize(
        &self,
        _ctx: &crate::GearCtx,
        router: axum::Router,
        _health: Arc<crate::RestHealthcheckRegistry>,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }
    fn as_registry(&self) -> &dyn crate::contracts::OpenApiRegistry {
        &self.0
    }
}

struct NoConfig;
impl crate::config::ConfigProvider for NoConfig {
    fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
        None
    }
}

#[tokio::test(start_paused = true)]
async fn a_directory_added_after_early_startup_can_register_the_providers() {
    let mut builder = crate::registry::RegistryBuilder::default();
    let gateway = Arc::new(RestGear::default());
    let provider = Arc::new(RestGear::default());
    builder.register_core_with_meta("gateway", &[], gateway.clone());
    builder.register_rest_host_with_meta("gateway", gateway);
    builder.register_core_with_meta("registry", &[], provider.clone());
    builder.register_rest_with_meta("registry", provider);
    let hub = Arc::new(crate::ClientHub::new());
    let instance = uuid::Uuid::new_v4();
    let runtime = super::HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(NoConfig),
        super::DbOptions::None,
        Arc::clone(&hub),
        tokio_util::sync::CancellationToken::new(),
        instance,
        None,
    );
    let entries: Vec<_> = runtime.registry.gears().iter().collect();
    runtime
        .run_directory_register_phase(&entries)
        .await
        .unwrap();
    let directory = Arc::new(Directory {
        instance_id: instance.to_string(),
        ..Directory::default()
    });
    hub.register::<dyn DirectoryClient>(directory.clone());
    runtime
        .run_directory_register_phase(&entries)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), directory.accepted.notified())
        .await
        .expect("the final phase must retry directory lookup after an early miss");
    runtime.advertiser.deregister_all().await;
}
