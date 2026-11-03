//! The backend bench against `PostgreSQL` (`make bench-eb-pg`): the server
//! `EB_BENCH_POSTGRES_URL` names, or a container on `libs/test-containers`'
//! pinned tag.

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use event_broker_backend_bench::{Harness, Settings, Sizing, run_bench};
use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use postgres_event_broker_plugin::PostgresBackendProvider;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

/// Every build reaches one schema of one server with the plugin's default
/// settings: the bench measures the plugin as configured out of the box.
struct PostgresHarness {
    url: String,
    schema: String,
    _container: Option<ContainerAsync<Postgres>>,
}

impl PostgresHarness {
    async fn start() -> Result<Self, String> {
        let schema = format!("evbk_bench{}", &Uuid::new_v4().simple().to_string()[..8]);
        if let Ok(url) = std::env::var("EB_BENCH_POSTGRES_URL") {
            return Ok(Self {
                url,
                schema,
                _container: None,
            });
        }
        let container = test_containers::postgres()
            .with_container_name(format!(
                "eb-pg-bench-{}",
                &Uuid::new_v4().simple().to_string()[..6]
            ))
            .start()
            .await
            .map_err(|err| format!("postgres container did not start: {err}"))?;
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .map_err(|err| format!("postgres container port: {err}"))?;
        Ok(Self {
            url: format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
            schema,
            _container: Some(container),
        })
    }
}

#[async_trait]
impl Harness for PostgresHarness {
    async fn build(&self, sizing: Sizing) -> Arc<dyn Backend> {
        let serde_json::Value::Object(settings) = serde_json::json!({
            "url": self.url,
            "schema": self.schema,
            "pools": { "main": sizing.connections },
        }) else {
            unreachable!("an object literal");
        };
        match PostgresBackendProvider::default()
            .build(&BuildContext::new(&settings, Role::Standalone))
            .await
        {
            Ok(backend) => backend,
            Err(err) => panic!("the postgres backend builds from url and schema: {err}"),
        }
    }

    fn name(&self) -> &'static str {
        "postgres"
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let settings = Settings::parse_from(std::env::args());
    match PostgresHarness::start().await {
        Ok(harness) => run_bench(&harness, &settings).await,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
