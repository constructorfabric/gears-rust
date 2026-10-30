//! The backend conformance suite against a real `PostgreSQL` server
//! (`docs/TESTING.md` layer 2).

#![cfg(feature = "integration")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use event_broker_backend_conformance::{Harness, Unreachable, run_all};
use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use postgres_event_broker_plugin::PostgresBackendProvider;

/// Every `build` reaches one schema of one server, as instances in different
/// processes reach one database.
struct PostgresHarness {
    server: common::Server,
    schema: String,
    /// Settings every instance gets besides the url and schema.
    extra: serde_json::Value,
}

#[async_trait]
impl Harness for PostgresHarness {
    async fn build(&self) -> Arc<dyn Backend> {
        common::built(&self.server.url, &self.schema, self.extra.clone()).await
    }

    async fn build_isolated(&self) -> Arc<dyn Backend> {
        common::built(
            &self.server.url,
            &common::fresh_schema(),
            self.extra.clone(),
        )
        .await
    }

    async fn build_unreachable(&self) -> Unreachable {
        let serde_json::Value::Object(settings) =
            serde_json::json!({ "url": "postgres://postgres:postgres@127.0.0.1:1/postgres" })
        else {
            unreachable!("an object literal");
        };
        match PostgresBackendProvider::default()
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance() {
    let harness = PostgresHarness {
        server: common::server("conformance").await,
        schema: common::fresh_schema(),
        extra: serde_json::json!({}),
    };
    run_all(&harness).await;
}

/// The same contract when heads are notified after the commit by a worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance_with_heads_notified_after_commit() {
    let harness = PostgresHarness {
        server: common::server("conformance-after-commit").await,
        schema: common::fresh_schema(),
        extra: serde_json::json!({ "head_notify": "after_commit" }),
    };
    run_all(&harness).await;
}
