//! The backend conformance suite against a real Kafka broker
//! (`docs/TESTING.md` layer 2).

#![cfg(feature = "integration")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use event_broker_backend_conformance::{Harness, Unreachable, run_all};
use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use kafka_event_broker_plugin::KafkaBackendProvider;

/// Every `build` reaches one prefix of one cluster, as instances in
/// different processes do.
struct KafkaHarness {
    cluster: common::Cluster,
    prefix: String,
}

#[async_trait]
impl Harness for KafkaHarness {
    async fn build(&self) -> Arc<dyn Backend> {
        common::built(&self.cluster.bootstrap, &self.prefix, serde_json::json!({})).await
    }

    /// A prefix of its own: topics, fences and logs no other instance sees.
    async fn build_isolated(&self) -> Arc<dyn Backend> {
        common::built(
            &self.cluster.bootstrap,
            &common::fresh_prefix(),
            serde_json::json!({}),
        )
        .await
    }

    /// A closed port: nothing listens on port 1.
    async fn build_unreachable(&self) -> Unreachable {
        let settings = common::settings(
            "127.0.0.1:1",
            &common::fresh_prefix(),
            serde_json::json!({ "request_timeout_ms": 5000 }),
        );
        match KafkaBackendProvider::default()
            .build(&BuildContext::new(&settings, Role::Standalone))
            .await
        {
            Ok(backend) => Unreachable::Backend(backend),
            Err(err) => panic!("an unreachable cluster must not fail the build: {err}"),
        }
    }

    /// The broker checks retention every second (`test_containers::kafka`).
    fn retention_deadline(&self) -> Duration {
        Duration::from_secs(8)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance() {
    let harness = KafkaHarness {
        cluster: common::cluster("conformance").await,
        prefix: common::fresh_prefix(),
    };
    run_all(&harness).await;
}
