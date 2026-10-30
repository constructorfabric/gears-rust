//! A `MySQL` or `MariaDB` server for the integration tests
//! (`docs/TESTING.md`): a container on `libs/test-containers`' pinned tag
//! (`GEARS_TEST_MYSQL_TAG` / `GEARS_TEST_MARIADB_TAG` override it), or the
//! server `EB_TEST_MYSQL_URL` / `EB_TEST_MARIADB_URL` names.

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
use mysql_event_broker_plugin::MysqlBackendProvider;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::{AssertSqlSafe, MySqlPool};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use testcontainers_modules::mysql::Mysql;
use uuid::Uuid;

/// The engine a test runs against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Mysql,
    Mariadb,
}

enum Container {
    Mysql(ContainerAsync<Mysql>),
    Mariadb(ContainerAsync<GenericImage>),
}

/// The server, kept alive for as long as this value lives.
pub struct Server {
    pub engine: Engine,
    /// `mysql://root@127.0.0.1:{port}/{database}`, the url's own database
    /// being one that always exists.
    pub url: String,
    container: Option<Container>,
}

/// Room for every instance the conformance suite builds at once.
const MAX_CONNECTIONS: &str = "--max-connections=2000";

/// `purpose` names the container `eb-my-test-<purpose>-<6 hex>`, so `docker
/// ps` shows which test started it.
pub async fn server(engine: Engine, purpose: &str) -> Server {
    let name = format!(
        "eb-{}-test-{purpose}-{}",
        match engine {
            Engine::Mysql => "my",
            Engine::Mariadb => "maria",
        },
        &Uuid::new_v4().simple().to_string()[..6]
    );
    let external = match engine {
        Engine::Mysql => std::env::var("EB_TEST_MYSQL_URL"),
        Engine::Mariadb => std::env::var("EB_TEST_MARIADB_URL"),
    };
    if let Ok(url) = external {
        return Server {
            engine,
            url,
            container: None,
        };
    }
    let container = match engine {
        Engine::Mysql => Container::Mysql(
            test_containers::mysql()
                .with_cmd([MAX_CONNECTIONS])
                .with_container_name(name)
                .start()
                .await
                .expect("mysql container starts"),
        ),
        Engine::Mariadb => Container::Mariadb(
            test_containers::mariadb()
                .with_exposed_port(3306.tcp())
                // The init server listens on port 0; the real one on 3306.
                .with_wait_for(WaitFor::message_on_stderr("port: 3306"))
                .with_env_var("MARIADB_ALLOW_EMPTY_ROOT_PASSWORD", "1")
                .with_env_var("MARIADB_DATABASE", "test")
                .with_cmd([MAX_CONNECTIONS])
                .with_container_name(name)
                .start()
                .await
                .expect("mariadb container starts"),
        ),
    };
    let mut server = Server {
        engine,
        url: String::new(),
        container: Some(container),
    };
    server.url = server.mapped_url().await;
    server
}

impl Server {
    async fn mapped_url(&self) -> String {
        let port = match &self.container {
            Some(Container::Mysql(c)) => c.get_host_port_ipv4(3306).await,
            Some(Container::Mariadb(c)) => c.get_host_port_ipv4(3306).await,
            None => return self.url.clone(),
        }
        .expect("mapped port");
        format!("mysql://root@127.0.0.1:{port}/test")
    }

    /// Stops and starts the server's container, as a crash and a restart of
    /// the server would; the url follows the port it is mapped to now.
    pub async fn restart(&mut self) {
        match &self.container {
            Some(Container::Mysql(c)) => {
                c.stop().await.expect("stop");
                c.start().await.expect("start");
            }
            Some(Container::Mariadb(c)) => {
                c.stop().await.expect("stop");
                c.start().await.expect("start");
            }
            None => panic!("an external server is not restarted by a test"),
        }
        self.url = self.mapped_url().await;
    }

    /// The url of `database` on this server.
    pub fn url_of(&self, database: &str) -> String {
        let base = self
            .url
            .rsplit_once('/')
            .map_or(self.url.as_str(), |(base, _)| base);
        format!("{base}/{database}")
    }

    /// A pool over `database`, created when missing, retried until the
    /// server answers: a test's own view of what the plugin left.
    pub async fn control(&self, database: &str) -> MySqlPool {
        let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
        loop {
            match MySqlPoolOptions::new()
                .max_connections(4)
                .connect(&self.url)
                .await
            {
                Ok(pool) => {
                    sqlx::query(AssertSqlSafe(format!(
                        "CREATE DATABASE IF NOT EXISTS {database}"
                    )))
                    .execute(&pool)
                    .await
                    .unwrap();
                    pool.close().await;
                    return MySqlPoolOptions::new()
                        .max_connections(4)
                        .after_connect(|conn, _| {
                            Box::pin(async move {
                                sqlx::query(
                                    "SET SESSION TRANSACTION ISOLATION LEVEL READ COMMITTED",
                                )
                                .execute(conn)
                                .await?;
                                Ok(())
                            })
                        })
                        .connect(&self.url_of(database))
                        .await
                        .unwrap();
                }
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(err) => panic!("the server did not answer within a minute: {err}"),
            }
        }
    }
}

/// A database no other test uses.
pub fn fresh_database() -> String {
    format!("evbk_t{}", &Uuid::new_v4().simple().to_string()[..12])
}

/// Settings the tests run with: small pools, and a maintenance and head poll
/// fast enough to watch.
pub fn quick() -> serde_json::Value {
    serde_json::json!({
        "pools": { "main": 4, "background": 2 },
        "poll_interval_ms": 100,
        "maintenance": { "rescan_interval_ms": 200 },
    })
}

/// A backend over `url` in `database`, built and not started, in a process
/// that is `role`; `extra` settings over [`quick`].
pub async fn built_as(
    url: &str,
    database: &str,
    extra: serde_json::Value,
    role: Role,
) -> Arc<dyn Backend> {
    let mut settings = quick();
    if let (serde_json::Value::Object(base), serde_json::Value::Object(extra)) =
        (&mut settings, extra)
    {
        base.insert("url".to_owned(), url.into());
        base.insert("database".to_owned(), database.into());
        base.extend(extra);
    }
    let serde_json::Value::Object(settings) = settings else {
        unreachable!("an object literal");
    };
    MysqlBackendProvider::default()
        .build(&BuildContext::new(&settings, role))
        .await
        .expect("build")
}

/// A standalone backend, built and not started.
pub async fn built(url: &str, database: &str, extra: serde_json::Value) -> Arc<dyn Backend> {
    built_as(url, database, extra, Role::Standalone).await
}

/// A standalone backend, started and with its startup completed.
pub async fn backend(url: &str, database: &str, extra: serde_json::Value) -> Arc<dyn Backend> {
    let backend = built(url, database, extra).await;
    backend.start().await.expect("start");
    started(backend.as_ref()).await;
    backend
}

/// Waits until `backend` answers something other than `Unavailable`.
pub async fn started(backend: &dyn Backend) {
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    while !answers(backend).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the backend did not start within a minute"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether `backend` answers something other than `Unavailable` now.
pub async fn answers(backend: &dyn Backend) -> bool {
    let probe = TopicPartition::new(
        GtsInstanceId::try_new("gts.cf.core.events.topic.v1~cf.conformance.x.probe.v1").unwrap(),
        Partition::new(0),
    );
    !matches!(
        backend.resolve(&probe, Position::Latest).await,
        Err(BackendError::Unavailable { .. })
    )
}
