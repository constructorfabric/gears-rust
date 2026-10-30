//! The backend conformance suite against a real `MySQL` and a real `MariaDB`
//! server (`docs/TESTING.md` layer 2).

#![cfg(feature = "integration")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::Engine;
use event_broker_backend_conformance::{Harness, Unreachable, run_all};
use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use mysql_event_broker_plugin::MysqlBackendProvider;

/// Every `build` reaches one database of one server, as instances in
/// different processes reach one database.
struct MysqlHarness {
    server: common::Server,
    database: String,
}

#[async_trait]
impl Harness for MysqlHarness {
    async fn build(&self) -> Arc<dyn Backend> {
        common::built(&self.server.url, &self.database, serde_json::json!({})).await
    }

    async fn build_isolated(&self) -> Arc<dyn Backend> {
        common::built(
            &self.server.url,
            &common::fresh_database(),
            serde_json::json!({}),
        )
        .await
    }

    async fn build_unreachable(&self) -> Unreachable {
        let serde_json::Value::Object(settings) =
            serde_json::json!({ "url": "mysql://root@127.0.0.1:1/test" })
        else {
            unreachable!("an object literal");
        };
        match MysqlBackendProvider::default()
            .build(&BuildContext::new(&settings, Role::Standalone))
            .await
        {
            Ok(backend) => Unreachable::Backend(backend),
            Err(err) => panic!("an unreachable server must not fail the build: {err}"),
        }
    }

    fn retention_deadline(&self) -> Duration {
        Duration::from_secs(5)
    }
}

async fn conformance(engine: Engine) {
    let harness = MysqlHarness {
        server: common::server(engine, "conformance").await,
        database: common::fresh_database(),
    };
    run_all(&harness).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance_mysql() {
    conformance(Engine::Mysql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance_mariadb() {
    conformance(Engine::Mariadb).await;
}
