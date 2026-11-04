//! The backend conformance suite against the memory backend.

#![allow(
    clippy::expect_used,
    reason = "test harness: a backend that fails to build is the test failure"
)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use event_broker_backend_conformance::{Harness, Unreachable, run_all};
use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use memory_event_broker_plugin::MemoryBackendProvider;
use uuid::Uuid;

/// Every `build` names one store of one provider, as registry entries naming
/// the same store do in a process: a rebuilt instance sees what an earlier one
/// stored.
struct MemoryHarness {
    provider: MemoryBackendProvider,
    store: String,
}

impl MemoryHarness {
    fn new() -> Self {
        Self {
            provider: MemoryBackendProvider::default(),
            store: format!("conformance-{}", Uuid::new_v4().simple()),
        }
    }

    async fn build_store(&self, store: &str) -> Arc<dyn Backend> {
        let settings = serde_json::json!({ "store": store, "retention_interval_ms": 50 });
        let serde_json::Value::Object(settings) = settings else {
            unreachable!("a JSON object literal");
        };
        self.provider
            .build(&BuildContext::new(&settings, Role::Standalone))
            .await
            .expect("build")
    }
}

#[async_trait]
impl Harness for MemoryHarness {
    async fn build(&self) -> Arc<dyn Backend> {
        self.build_store(&self.store).await
    }

    async fn build_isolated(&self) -> Arc<dyn Backend> {
        self.build_store(&format!("isolated-{}", Uuid::new_v4().simple()))
            .await
    }

    async fn build_unreachable(&self) -> Unreachable {
        Unreachable::NotApplicable {
            reason: "the memory backend has no medium that can be down",
        }
    }

    fn retention_deadline(&self) -> Duration {
        Duration::from_secs(2)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance() {
    run_all(&MemoryHarness::new()).await;
}
