#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! SDK-only `#[toolkit::consumes]` host (SPEC §8.4, T35), with no local registry.
//! Proxy wiring installs both resolving clients. Calls return `ServiceUnavailable`
//! until directory registration, then reach the same TCP registry using each
//! plane's credential.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use secrecy::SecretString;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use toolkit::directory::{
    DirectoryClient, DirectoryNotFound, RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo,
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::GearCtx;
use toolkit::client_hub::ClientHub;
use toolkit::config::ConfigProvider;
use toolkit::contracts::Gear;
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::{DbOptions, HostRuntime};
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::{CredentialState, InternalTokenProvider};
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::{
    BatchGetEntitiesRequest, EntityKey, ListEntitiesRequest, PlatformTypesRegistryApi, Projection,
    TypesRegistryApi,
};

/// The kebab case of [`ConsumesFixture`], the name `#[toolkit::consumes]` derives.
const CONSUMER: &str = "consumes-fixture";
/// The one entity the stub registry knows: proof an answer came from it.
const STUB_TYPE: &str = "gts.cf.core.example.stub_only.v1~";
const BEARER: &str = "consumes-remote-bearer";
/// The process's internal token, which the runtime hands every resolving client.
const INTERNAL_TOKEN: &str = "consumes-remote-internal-token";

/// A directory that names the registry only once the test says so.
#[derive(Default)]
struct StubDirectory(Mutex<Option<String>>);

#[async_trait]
impl DirectoryClient for StubDirectory {
    async fn resolve_grpc_service(&self, name: &str) -> anyhow::Result<ServiceEndpoint> {
        Err(DirectoryNotFound::new(format!("service {name}")).into())
    }

    async fn resolve_rest_service(&self, gear: &str) -> anyhow::Result<ServiceEndpoint> {
        match (gear, self.0.lock().clone()) {
            ("types-registry", Some(uri)) => Ok(ServiceEndpoint::new(uri)),
            _ => Err(DirectoryNotFound::new(format!("gear {gear}")).into()),
        }
    }

    async fn get_openapi_spec(&self, gear: &str) -> anyhow::Result<String> {
        Err(DirectoryNotFound::new(format!("spec {gear}")).into())
    }

    async fn list_instances(&self, _gear: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(Vec::new())
    }

    async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(Vec::new())
    }

    async fn register_instance(&self, _info: RegisterInstanceInfo) -> anyhow::Result<()> {
        Ok(())
    }

    async fn deregister_instance(&self, _gear: &str, _instance_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn send_heartbeat(&self, _gear: &str, _instance_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// One request the stub registry received: its target and credential headers.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Received {
    target: String,
    authorization: Option<String>,
    internal_token: Option<String>,
}

/// A registry stand-in on TCP: every `GET` answers a discovery page holding
/// [`STUB_TYPE`]; everything it receives is recorded.
async fn stub_registry() -> (String, Arc<Mutex<Vec<Received>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&received);
    let page = json!({
        "items": [{
            "gts_id": STUB_TYPE,
            "gts_uuid": gts::GtsId::try_new(STUB_TYPE).unwrap().to_uuid(),
            "kind": "type_schema",
            "lifecycle_status": "active",
        }],
        "page_info": {},
    })
    .to_string();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut byte = [0_u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap_or(0) == 1 {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let header = |name: &str| {
                head.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.trim()
                        .eq_ignore_ascii_case(name)
                        .then(|| value.trim().to_owned())
                })
            };
            log.lock().push(Received {
                target: head.lines().next().unwrap_or_default().to_owned(),
                authorization: header("authorization"),
                internal_token: header("x-toolkit-internal-token"),
            });
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{page}",
                page.len()
            );
            drop(stream.write_all(response.as_bytes()).await);
        }
    });
    (format!("http://{addr}"), received)
}

#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]
#[toolkit::consumes(contract = types_registry_sdk::TypesRegistryApi, from = "types-registry")]
struct ConsumesFixture;

#[async_trait]
impl Gear for ConsumesFixture {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Config(HashMap<String, Value>);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.0.get(gear_name)
    }
}

fn assert_unavailable<T: std::fmt::Debug>(result: Result<T, CanonicalError>) {
    let error = result.expect_err("no registry to resolve");
    assert!(
        matches!(error, CanonicalError::ServiceUnavailable { .. }),
        "{error:?}"
    );
}

fn read() -> BatchGetEntitiesRequest {
    BatchGetEntitiesRequest {
        items: vec![EntityKey::GtsId(gts::GtsId::try_new(STUB_TYPE).unwrap()).into()],
        projection: Projection::Default,
        fresh: false,
    }
}

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid::Uuid::from_u128(1))
        .subject_tenant_id(uuid::Uuid::from_u128(2))
        .bearer_token(BEARER.to_owned())
        .build()
        .expect("a tenant context")
}

#[tokio::test]
async fn a_host_without_the_registry_reaches_it_only_through_the_directory() {
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta(CONSUMER, &[], Arc::new(ConsumesFixture) as Arc<dyn Gear>);
    let registry = builder.build_topo_sorted().expect("registry");
    assert!(
        registry
            .gears()
            .iter()
            .all(|gear| gear.name() != "types-registry"),
        "this host has no registry gear"
    );

    let hub = Arc::new(ClientHub::default());
    let directory = Arc::new(StubDirectory::default());
    hub.register::<dyn DirectoryClient>(Arc::clone(&directory) as Arc<dyn DirectoryClient>);
    let cancel = CancellationToken::new();
    let runtime = HostRuntime::new(
        registry,
        Arc::new(Config(HashMap::from([(
            CONSUMER.to_owned(),
            json!({ "config": {} }),
        )]))),
        DbOptions::None,
        Arc::clone(&hub),
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    )
    .with_internal_token_provider(Some(InternalTokenProvider::new(|| {
        CredentialState::Available(SecretString::from(INTERNAL_TOKEN.to_owned()))
    })));
    let host = tokio::spawn(runtime.run_gear_phases());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !(hub.has_remote_proxy::<dyn PlatformTypesRegistryApi>()
        && hub.has_remote_proxy::<dyn TypesRegistryApi>())
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "proxy wiring registers both resolving clients"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        hub.try_get_local::<dyn PlatformTypesRegistryApi>()
            .is_none()
    );
    assert!(hub.try_get_local::<dyn TypesRegistryApi>().is_none());
    let platform = hub
        .get::<dyn PlatformTypesRegistryApi>()
        .expect("the platform resolving client");
    let tenant = hub
        .get::<dyn TypesRegistryApi>()
        .expect("the tenant resolving client");
    let marker = PlatformSecurityContext::outbound_marker();

    // Nothing to resolve: every method of both is unavailable.
    assert_unavailable(
        platform
            .list_entities(&marker, ListEntitiesRequest::default())
            .await,
    );
    assert_unavailable(platform.batch_get_entities(&marker, read()).await);
    assert_unavailable(
        tenant
            .list_entities(&caller(), ListEntitiesRequest::default())
            .await,
    );
    assert_unavailable(tenant.batch_get_entities(&caller(), read()).await);

    // The directory names the registry: the same clients now reach it.
    let (uri, received) = stub_registry().await;
    *directory.0.lock() = Some(uri);
    let platform_page = platform
        .list_entities(&marker, ListEntitiesRequest::default())
        .await
        .expect("the resolved registry answers");
    let tenant_page = tenant
        .list_entities(&caller(), ListEntitiesRequest::default())
        .await
        .expect("the resolved registry answers");
    for page in [&platform_page, &tenant_page] {
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            page.items[0].gts_id.as_ref(),
            STUB_TYPE,
            "answered by the stub"
        );
    }
    let received = received.lock().clone();
    assert_eq!(received.len(), 2, "{received:?}");
    assert!(
        received[0]
            .target
            .starts_with("GET /types-registry/v2/platform/entities")
            && received[0].internal_token.as_deref() == Some(INTERNAL_TOKEN)
            && received[0].authorization.is_none(),
        "the platform read sends the process's token and no bearer: {received:?}"
    );
    assert!(
        received[1]
            .target
            .starts_with("GET /types-registry/v2/entities")
            && received[1].authorization.as_deref() == Some(&format!("Bearer {BEARER}"))
            && received[1].internal_token.is_none(),
        "the tenant read sends its caller's bearer and no token: {received:?}"
    );

    cancel.cancel();
    host.await.expect("host task").expect("a clean shutdown");
}
