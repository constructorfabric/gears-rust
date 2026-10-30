//! A `PostgreSQL` server for layers 2 and 3 (`docs/TESTING.md`): a container on
//! `libs/test-containers`' pinned tag, or the server `EB_TEST_POSTGRES_URL`
//! names.

#![cfg(feature = "integration")]
#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test harness: a fixture setup failure is the test failure, and not every helper is used by every test binary"
)]

use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::{
    Backend, BackendError, BuildContext, Partition, Provider, Role, TopicPartition,
};
use event_broker_sdk::{GtsInstanceId, Position};
use postgres_event_broker_plugin::PostgresBackendProvider;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

pub mod invariants;

/// The server, kept alive for as long as this value lives.
pub struct Server {
    pub url: String,
    _container: Option<ContainerAsync<Postgres>>,
}

/// `purpose` names the container `eb-pg-test-<purpose>-<6 hex>`, so `docker ps`
/// shows which test started it.
pub async fn server(purpose: &str) -> Server {
    if let Ok(url) = std::env::var("EB_TEST_POSTGRES_URL") {
        return Server {
            url,
            _container: None,
        };
    }
    // Every scenario of the suite builds instances with a pool and a LISTEN
    // connection each, all at once: more than the default 100 connections.
    let container = test_containers::postgres()
        .with_cmd(["postgres", "-c", "max_connections=600"])
        .with_container_name(container_name(purpose))
        .start()
        .await
        .expect("postgres container starts");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("mapped port");
    Server {
        url: format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
        _container: Some(container),
    }
}

/// A readable, unique container name: parallel tests may share a purpose.
fn container_name(purpose: &str) -> String {
    format!(
        "eb-pg-test-{purpose}-{}",
        &Uuid::new_v4().simple().to_string()[..6]
    )
}

/// A schema no other test uses.
pub fn fresh_schema() -> String {
    format!("evbk_t{}", &Uuid::new_v4().simple().to_string()[..12])
}

/// A backend over `url` in `schema`, built and not started, in a process
/// that both ingests and delivers.
pub async fn built(url: &str, schema: &str, extra: serde_json::Value) -> Arc<dyn Backend> {
    built_as(url, schema, extra, Role::Standalone).await
}

/// A backend over `url` in `schema`, built and not started, in a process
/// that is `role`.
pub async fn built_as(
    url: &str,
    schema: &str,
    extra: serde_json::Value,
    role: Role,
) -> Arc<dyn Backend> {
    let mut settings = serde_json::json!({
        "url": url,
        "schema": schema,
        "pools": { "main": 4 },
    });
    if let (serde_json::Value::Object(base), serde_json::Value::Object(extra)) =
        (&mut settings, extra)
    {
        base.extend(extra);
    }
    let serde_json::Value::Object(settings) = settings else {
        unreachable!("an object literal");
    };
    PostgresBackendProvider::default()
        .build(&BuildContext::new(&settings, role))
        .await
        .expect("build")
}

/// A backend over `url` in `schema`, started and with its startup completed.
pub async fn backend(url: &str, schema: &str, extra: serde_json::Value) -> Arc<dyn Backend> {
    backend_as(url, schema, extra, Role::Standalone).await
}

/// Like [`backend`], in a process that is `role`.
pub async fn backend_as(
    url: &str,
    schema: &str,
    extra: serde_json::Value,
    role: Role,
) -> Arc<dyn Backend> {
    let backend = built_as(url, schema, extra, role).await;
    backend.start().await.expect("start");
    started(backend.as_ref()).await;
    backend
}

/// Waits until `backend` answers something other than `Unavailable`.
pub async fn started(backend: &dyn Backend) {
    let probe = TopicPartition::new(
        GtsInstanceId::try_new("gts.cf.core.events.topic.v1~cf.conformance.x.probe.v1").unwrap(),
        Partition::new(0),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    loop {
        match backend.resolve(&probe, Position::Latest).await {
            Err(BackendError::Unavailable { .. }) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the backend did not start within a minute"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            _ => return,
        }
    }
}
