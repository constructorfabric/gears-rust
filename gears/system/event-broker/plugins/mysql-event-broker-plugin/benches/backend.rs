//! The backend bench against `MySQL` or `MariaDB`: the server
//! `EB_BENCH_MYSQL_URL` names, or a container on `libs/test-containers`'
//! pinned tag - `MySQL` unless `EB_BENCH_MYSQL_ENGINE=mariadb`.

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use event_broker_backend_bench::{Harness, Settings, Sizing, run_bench};
use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use mysql_event_broker_plugin::MysqlBackendProvider;
use testcontainers::ImageExt;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use uuid::Uuid;

/// A container, kept alive for as long as the harness lives.
type Container = Box<dyn std::any::Any + Send + Sync>;

/// Every build reaches one database of one server with the plugin's default
/// settings: the bench measures the plugin as configured out of the box.
struct MysqlHarness {
    url: String,
    database: String,
    name: &'static str,
    _container: Option<Container>,
}

impl MysqlHarness {
    async fn start() -> Result<Self, String> {
        let database = format!("evbk_bench{}", &Uuid::new_v4().simple().to_string()[..8]);
        let mariadb = std::env::var("EB_BENCH_MYSQL_ENGINE").is_ok_and(|e| e == "mariadb");
        let name = if mariadb { "mariadb" } else { "mysql" };
        if let Ok(url) = std::env::var("EB_BENCH_MYSQL_URL") {
            return Ok(Self {
                url,
                database,
                name,
                _container: None,
            });
        }
        let container_name = format!(
            "eb-{name}-bench-{}",
            &Uuid::new_v4().simple().to_string()[..6]
        );
        let (container, port) = if mariadb {
            let container = test_containers::mariadb()
                .with_exposed_port(3306.tcp())
                .with_wait_for(WaitFor::message_on_stderr("port: 3306"))
                .with_env_var("MARIADB_ALLOW_EMPTY_ROOT_PASSWORD", "1")
                .with_env_var("MARIADB_DATABASE", "test")
                .with_container_name(container_name)
                .start()
                .await
                .map_err(|err| format!("mariadb container did not start: {err}"))?;
            let port = container
                .get_host_port_ipv4(3306)
                .await
                .map_err(|err| format!("mariadb container port: {err}"))?;
            (Box::new(container) as Container, port)
        } else {
            let container = test_containers::mysql()
                .with_container_name(container_name)
                .start()
                .await
                .map_err(|err| format!("mysql container did not start: {err}"))?;
            let port = container
                .get_host_port_ipv4(3306)
                .await
                .map_err(|err| format!("mysql container port: {err}"))?;
            (Box::new(container) as Container, port)
        };
        Ok(Self {
            url: format!("mysql://root@127.0.0.1:{port}/test"),
            database,
            name,
            _container: Some(container),
        })
    }
}

#[async_trait]
impl Harness for MysqlHarness {
    async fn build(&self, sizing: Sizing) -> Arc<dyn Backend> {
        let serde_json::Value::Object(settings) = serde_json::json!({
            "url": self.url,
            "database": self.database,
            "pools": { "main": sizing.connections },
        }) else {
            unreachable!("an object literal");
        };
        match MysqlBackendProvider::default()
            .build(&BuildContext::new(&settings, Role::Standalone))
            .await
        {
            Ok(backend) => backend,
            Err(err) => panic!("the mysql backend builds from url and database: {err}"),
        }
    }

    fn name(&self) -> &str {
        self.name
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let settings = Settings::parse_from(std::env::args());
    match MysqlHarness::start().await {
        Ok(harness) => run_bench(&harness, &settings).await,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
