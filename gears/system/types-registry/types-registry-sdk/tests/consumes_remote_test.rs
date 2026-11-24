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
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use toolkit_contract::runtime::resolving::{EndpointResolver, ResolveError};
use toolkit_contract::wiring::ClientTuning;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::{
    BatchGetEntitiesRequest, CacheSettings, EntityField, EntityKey, EntityLookup, FieldSelection,
    GtsTypeId, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
    PlatformTypesRegistryApiRestResolvingClient, Projection, ReadOptions, TypesRegistryApi,
    TypesRegistryApiRestResolvingClient,
};

/// The kebab case of [`ConsumesFixture`], the name `#[toolkit::consumes]` derives.
const CONSUMER: &str = "consumes-fixture";
/// The one entity the stub registry knows: proof an answer came from it.
const STUB_TYPE: &str = "gts.cf.core.example.stub_only.v1~";
const STUB_UUID: uuid::Uuid = uuid::Uuid::from_u128(137);
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
        match (gear, self.0.lock().unwrap().clone()) {
            ("types-registry", Some(uri)) => Ok(ServiceEndpoint::new(uri)),
            _ => Err(DirectoryNotFound::new(format!("gear {gear}")).into()),
        }
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

/// Request target, credentials and batch item validator observed by the stub.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Received {
    target: String,
    authorization: Option<String>,
    internal_token: Option<String>,
    if_none_match: Option<String>,
}

/// A registry stand-in serving one entity over discovery and batch reads.
async fn stub_registry() -> (String, Arc<Mutex<Vec<Received>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&received);
    let mut entity = json!({
        "gts_id": STUB_TYPE,
        "gts_uuid": STUB_UUID,
        "kind": "type_schema",
        "lifecycle_status": "active",
        "ownership": "global",
    });
    let page = json!({"items": [entity], "page_info": {}}).to_string();
    entity["content"] = json!({"title": "from batchGet"});
    let batch = json!({"items": [{
        "entity_key": STUB_TYPE, "status": "found", "etag": "\"t1\"", "entity": entity,
    }]})
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
            let mut if_none_match = None;
            let body = if head.starts_with("POST ") {
                let len: usize = header("content-length").unwrap().parse().unwrap();
                let mut bytes = vec![0; len];
                stream.read_exact(&mut bytes).await.unwrap();
                let mut request: Value = serde_json::from_slice(&bytes).unwrap();
                if_none_match = request["items"][0]
                    .as_object_mut()
                    .and_then(|item| item.remove("if_none_match"))
                    .map(|etag| etag.as_str().unwrap().to_owned());
                assert_eq!(request["items"], json!([{"entity_key": STUB_TYPE}]));
                &batch
            } else {
                &page
            };
            log.lock().unwrap().push(Received {
                target: head.lines().next().unwrap_or_default().to_owned(),
                authorization: header("authorization"),
                internal_token: header("x-toolkit-internal-token"),
                if_none_match,
            });
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
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

    // The proxies belong to the consuming gear, so read them through its view.
    let hub = hub.for_gear(CONSUMER);
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
    *directory.0.lock().unwrap() = Some(uri);
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
    // The platform page supplies both keys, even though no exact read has run.
    let id = GtsTypeId::try_new(STUB_TYPE).unwrap();
    assert_eq!(
        platform.resolve_type_uuid(&marker, &id).await.unwrap(),
        STUB_UUID
    );
    assert_eq!(
        platform.resolve_type_id(&marker, STUB_UUID).await.unwrap(),
        id
    );
    assert_eq!(
        received.lock().unwrap().len(),
        2,
        "both resolves used the page identity"
    );
    let options = ReadOptions::new().with_projection(FieldSelection::with(&[EntityField::Content]));
    let schema = platform
        .get_type_schema(&marker, &id, options)
        .await
        .expect("exact read");
    assert_eq!(schema.content, Some(json!({"title": "from batchGet"})));
    let received = received.lock().unwrap().clone();
    assert_eq!(received.len(), 3, "{received:?}");
    assert!(
        received[2]
            .target
            .starts_with("POST /types-registry/v2/platform/entities:batchGet ")
            && received[2].if_none_match.is_none(),
        "a first exact read is unconditional: {received:?}"
    );
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

struct Fixed(String);

#[async_trait]
impl EndpointResolver for Fixed {
    async fn resolve_endpoint(&self, _gear: &str) -> Result<Option<String>, ResolveError> {
        Ok(Some(self.0.clone()))
    }
}

fn revalidating() -> CacheSettings {
    CacheSettings {
        freshness_window: Duration::ZERO,
        ..CacheSettings::default()
    }
}

fn disabled() -> CacheSettings {
    CacheSettings {
        max_entries: 0,
        max_identity_entries: 0,
        ..CacheSettings::default()
    }
}

/// Validators sent on two reads per plane. `None` selects the default constructor.
async fn validators_sent(settings: Option<CacheSettings>) -> [Vec<Option<String>>; 2] {
    let (uri, received) = stub_registry().await;
    let resolver: Arc<dyn EndpointResolver> = Arc::new(Fixed(uri));
    let tuning =
        ClientTuning::default().with_internal_token_provider(Some(InternalTokenProvider::new(
            || CredentialState::Available(SecretString::from(INTERNAL_TOKEN.to_owned())),
        )));
    let (platform, tenant) = match settings {
        None => (
            PlatformTypesRegistryApiRestResolvingClient::new(
                Arc::clone(&resolver),
                "types-registry",
                tuning.clone(),
            ),
            TypesRegistryApiRestResolvingClient::new(resolver, "types-registry", tuning),
        ),
        Some(settings) => (
            PlatformTypesRegistryApiRestResolvingClient::with_cache_settings(
                Arc::clone(&resolver),
                "types-registry",
                tuning.clone(),
                settings,
            ),
            TypesRegistryApiRestResolvingClient::with_cache_settings(
                resolver,
                "types-registry",
                tuning,
                settings,
            ),
        ),
    };
    let request = || read().with_projection(FieldSelection::with(&[EntityField::Content]));
    let marker = PlatformSecurityContext::outbound_marker();
    for _ in 0..2 {
        let response = platform
            .batch_get_entities(&marker, request())
            .await
            .unwrap();
        assert!(matches!(
            response.0.values().next(),
            Some(EntityLookup::Found { .. })
        ));
    }
    for _ in 0..2 {
        let response = tenant
            .batch_get_entities(&caller(), request())
            .await
            .unwrap();
        assert!(matches!(
            response.0.values().next(),
            Some(EntityLookup::Found { .. })
        ));
    }
    let received = received.lock().unwrap().clone();
    let plane = |prefix: &str| {
        received
            .iter()
            .filter(|r| r.target.starts_with(prefix))
            .map(|r| r.if_none_match.clone())
            .collect()
    };
    [
        plane("POST /types-registry/v2/platform/entities:batchGet "),
        plane("POST /types-registry/v2/entities:batchGet "),
    ]
}

#[tokio::test]
async fn both_resolving_clients_cache_with_defaults_or_the_settings_they_are_built_with() {
    let etag = Some("\"t1\"".to_owned());
    assert_eq!(
        validators_sent(None).await,
        [vec![None], vec![None]],
        "the default window serves each second read"
    );
    assert_eq!(
        validators_sent(Some(revalidating())).await,
        [vec![None, etag.clone()], vec![None, etag]],
        "a zero window revalidates the one cache's snapshot"
    );
    assert_eq!(
        validators_sent(Some(disabled())).await,
        [vec![None, None], vec![None, None]],
        "nothing retained, so nothing to revalidate"
    );
}
