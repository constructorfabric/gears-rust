//! The backend conformance suite against a real `PostgreSQL` server
//! (`docs/TESTING.md` layer 2).

#![cfg(feature = "integration")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use event_broker_backend_conformance::{Harness, Unreachable, run_all};
use event_broker_sdk::backend::{Backend, Provider};
use postgres_event_broker_plugin::PostgresBackendProvider;
use tokio_util::sync::CancellationToken;

/// Every `build` reaches one schema of one server, as instances in different
/// processes reach one database.
struct PostgresHarness {
    server: common::Server,
    schema: String,
}

#[async_trait]
impl Harness for PostgresHarness {
    async fn build(&self, stop: CancellationToken) -> Arc<dyn Backend> {
        common::backend(&self.server.url, &self.schema, serde_json::json!({}), stop).await
    }

    async fn build_isolated(&self, stop: CancellationToken) -> Arc<dyn Backend> {
        common::backend(
            &self.server.url,
            &common::fresh_schema(),
            serde_json::json!({}),
            stop,
        )
        .await
    }

    async fn build_unreachable(&self, stop: CancellationToken) -> Unreachable {
        let serde_json::Value::Object(settings) =
            serde_json::json!({ "url": "postgres://postgres:postgres@127.0.0.1:1/postgres" })
        else {
            unreachable!("an object literal");
        };
        match PostgresBackendProvider::default()
            .build(&settings, stop)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance() {
    let harness = PostgresHarness {
        server: common::server("conformance").await,
        schema: common::fresh_schema(),
    };
    run_all(&harness).await;
}
