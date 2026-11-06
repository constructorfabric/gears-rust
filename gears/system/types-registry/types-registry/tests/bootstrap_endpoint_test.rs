#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! The registry is infrastructure: on one listener it serves platform-plane
//! publications while its own process's application group still waits for
//! admission. A co-hosted publisher waits for a base type another process
//! publishes over that endpoint; the registry admits it, the group initializes
//! and its tenant plane goes live on the same URL — authenticated, never
//! served before.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::runtime::{DbOptions, HostRuntime, OopServeOptions};
use toolkit::{
    ClientHub, DirectoryClient, Gear, GearCtx, GearRegistry, RegisterInstanceInfo, ServiceEndpoint,
    ServiceInstanceInfo,
};
use toolkit_gts::gts_id;
use toolkit_http::HttpClientBuilder;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_security::{
    AuthNError, BearerAuthenticator, DynBearerAuthenticator, DynInternalAuthenticator,
    SecurityContext, SharedSecretInternalAuthenticator,
};

mod common;

const BASE: &str = gts_id!("cf.core.bootstrap_endpoint.base.v1~");
const DERIVED: &str =
    gts_id!("cf.core.bootstrap_endpoint.base.v1~cf.core.bootstrap_endpoint.derived.v1~");
const INTERNAL_TOKEN: &str = "bootstrap-endpoint-internal-token";
const TENANT_BEARER: &str = "bootstrap-endpoint-tenant-bearer";
const PLATFORM: &str = "/types-registry/v2/platform";
const TENANT: &str = "/types-registry/v2";

fn schema(id: &str) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    })
}

/// What `toolkit_gts::declare_gts_inventory!()` provides in a declaring crate:
/// a derived type whose base another process publishes.
#[must_use]
pub fn gts_declarations() -> Vec<(String, String)> {
    vec![(DERIVED.to_owned(), schema(DERIVED).to_string())]
}

/// A co-hosted application gear publishing the derived type.
#[toolkit::gear(name = "waiting-publisher", gts_publication(crates = [crate]))]
#[derive(Default)]
pub struct WaitingPublisher;

#[async_trait]
impl Gear for WaitingPublisher {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        assert_eq!(ctx.gts_publication_manifest()?.documents().len(), 1);
        ctx.client_hub()
            .register::<AtomicBool>(Arc::new(AtomicBool::new(true)));
        Ok(())
    }
}

/// Accepts [`TENANT_BEARER`] only.
struct StubBearer;

impl BearerAuthenticator for StubBearer {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        if token == TENANT_BEARER {
            Ok(SecurityContext::builder()
                .subject_id(uuid::Uuid::from_u128(1))
                .subject_tenant_id(uuid::Uuid::from_u128(2))
                .build()
                .expect("a tenant context"))
        } else {
            Err(AuthNError::InvalidToken)
        }
    }
}

/// Records the gears registered.
#[derive(Default)]
struct RecordingDirectory {
    registered: Mutex<Vec<String>>,
}

#[async_trait]
impl DirectoryClient for RecordingDirectory {
    async fn resolve_grpc_service(&self, service: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("no provider of {service}")
    }
    async fn resolve_rest_service(&self, gear: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("no provider of {gear}")
    }
    async fn get_openapi_spec(&self, gear: &str) -> anyhow::Result<String> {
        anyhow::bail!("no provider of {gear}")
    }
    async fn list_instances(&self, _gear: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn register_instance(&self, info: RegisterInstanceInfo) -> anyhow::Result<()> {
        self.registered.lock().push(info.gear);
        Ok(())
    }
    async fn deregister_instance(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn send_heartbeat(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Config(HashMap<String, Value>);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.0.get(gear_name)
    }
}

fn free_addr() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

/// `GET path` with the given headers; the status.
async fn status(base: &str, path: &str, headers: &[(&str, &str)]) -> u16 {
    let http = HttpClientBuilder::new().retry(None).build().unwrap();
    let mut request = http.get(&format!("{base}{path}"));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    request.send().await.expect("a response").status().as_u16()
}

async fn eventually(what: &str, mut done: impl AsyncFnMut() -> bool) {
    for _ in 0..600 {
        if done().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn the_registry_serves_platform_publications_while_its_application_group_waits() {
    let dir = common::TestDir::new("tr-bootstrap-endpoint");
    let dsn = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("registry.db").display()
    );
    let sections = HashMap::from([(
        "types-registry".to_owned(),
        json!({ "database": { "dsn": dsn, "pool": { "max_conns": 4 } }, "config": {} }),
    )]);
    let figment = figment::Figment::new().merge(figment::providers::Serialized::defaults(
        json!({ "gears": sections }),
    ));
    let db_manager = Arc::new(
        toolkit_db::DbManager::from_figment(figment, dir.path().to_path_buf())
            .expect("database manager"),
    );
    let hub = Arc::new(ClientHub::new());
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        GearRegistry::discover_and_build().expect("registry"),
        Arc::new(Config(sections)),
        DbOptions::Manager(db_manager),
        Arc::clone(&hub),
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    );
    let addr = free_addr();
    let base = format!("http://{addr}");
    let directory = Arc::new(RecordingDirectory::default());
    let options = OopServeOptions::new(
        "types-registry".to_owned(),
        "i-1".to_owned(),
        base.clone(),
        addr,
        Arc::clone(&directory) as Arc<dyn DirectoryClient>,
    )
    .with_bearer_authenticator(Some(DynBearerAuthenticator::new(StubBearer)))
    .with_internal_authenticator(Some(DynInternalAuthenticator::new(
        SharedSecretInternalAuthenticator::try_new(
            SecretString::from(INTERNAL_TOKEN.to_owned()),
            "bootstrap-endpoint".to_owned(),
        )
        .expect("a usable shared secret"),
    )));
    let server = tokio::spawn(host.run_oop_serving(options));
    let token = [(INTERNAL_TOKEN_HEADER, INTERNAL_TOKEN)];
    let bearer = format!("Bearer {TENANT_BEARER}");
    let tenant = [("authorization", bearer.as_str())];

    // The application group waits for the base type: the registry already
    // serves the platform plane, refuses a caller without a credential, and
    // serves no tenant call, not even an authenticated one.
    eventually("the platform plane", async || {
        status(&base, &format!("{PLATFORM}/entities"), &token).await == 200
    })
    .await;
    assert_eq!(
        status(&base, &format!("{PLATFORM}/entities"), &[]).await,
        401
    );
    assert_eq!(
        status(&base, &format!("{TENANT}/entities"), &tenant).await,
        503
    );
    assert!(hub.get::<AtomicBool>().is_err(), "no application init yet");
    assert!(
        directory
            .registered
            .lock()
            .contains(&"types-registry".to_owned()),
        "the registry is discoverable while the group waits"
    );

    // Another process publishes the base type over the same endpoint.
    let http = HttpClientBuilder::new().retry(None).build().unwrap();
    let submitted = http
        .post(&format!("{base}{PLATFORM}/entities"))
        .header(INTERNAL_TOKEN_HEADER, INTERNAL_TOKEN)
        .header("idempotency-key", "bootstrap-endpoint-base")
        .json(&json!({ "items": [{ "gts_id": BASE, "content": schema(BASE) }] }))
        .unwrap()
        .send()
        .await
        .expect("a response");
    assert!(submitted.status().is_success(), "{}", submitted.status());

    // The co-hosted publication is admitted, the group initializes, and the
    // tenant plane is live on the same URL, authenticated.
    eventually("the application group", async || {
        hub.get::<AtomicBool>().is_ok()
    })
    .await;
    eventually("the tenant plane", async || {
        status(&base, &format!("{TENANT}/entities"), &tenant).await == 200
    })
    .await;
    assert_eq!(
        status(
            &base,
            &format!("{TENANT}/entities"),
            &[("authorization", "Bearer nope")]
        )
        .await,
        401
    );
    assert_eq!(
        status(&base, &format!("{PLATFORM}/entities"), &token).await,
        200
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("stops")
        .expect("no panic")
        .expect("clean shutdown");
}
