//! The backend conformance suite against a database file (`docs/TESTING.md`
//! layer 2).

#![allow(
    clippy::expect_used,
    reason = "a failed fixture setup is the test failure"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use event_broker_backend_conformance::{Harness, Unreachable, run_all};
use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use sqlite_event_broker_plugin::SqliteBackendProvider;
use uuid::Uuid;

/// Every `build` opens one file, as instances of one process share it.
struct SqliteHarness {
    files: common::Files,
}

#[async_trait]
impl Harness for SqliteHarness {
    async fn build(&self) -> Arc<dyn Backend> {
        common::built(&self.files.path("shared.db")).await
    }

    async fn build_isolated(&self) -> Arc<dyn Backend> {
        common::built(&self.files.path(&format!("{}.db", Uuid::new_v4().simple()))).await
    }

    /// A path under a regular file: no directory to create the database in,
    /// whatever the user's privileges.
    async fn build_unreachable(&self) -> Unreachable {
        let blocker = self.files.path("not-a-directory");
        std::fs::write(&blocker, b"").expect("a regular file");
        let serde_json::Value::Object(settings) =
            serde_json::json!({ "path": blocker.join("events.db").to_string_lossy() })
        else {
            unreachable!("an object literal");
        };
        match SqliteBackendProvider::default()
            .build(&BuildContext::new(&settings, Role::Standalone))
            .await
        {
            Ok(backend) => Unreachable::Backend(backend),
            Err(err) => panic!("an unwritable path must not fail the build: {err}"),
        }
    }

    fn retention_deadline(&self) -> Duration {
        Duration::from_secs(2)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance() {
    let harness = SqliteHarness {
        files: common::Files::new(),
    };
    run_all(&harness).await;
}

/// The same contract when commits are synced at checkpoints only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance_with_synchronous_normal() {
    struct Normal(SqliteHarness);
    #[async_trait]
    impl Harness for Normal {
        async fn build(&self) -> Arc<dyn Backend> {
            common::built_with(
                &self.0.files.path("shared.db"),
                Role::Standalone,
                serde_json::json!({ "synchronous": "normal" }),
            )
            .await
        }
        async fn build_isolated(&self) -> Arc<dyn Backend> {
            common::built_with(
                &self
                    .0
                    .files
                    .path(&format!("{}.db", Uuid::new_v4().simple())),
                Role::Standalone,
                serde_json::json!({ "synchronous": "normal" }),
            )
            .await
        }
        async fn build_unreachable(&self) -> Unreachable {
            self.0.build_unreachable().await
        }
        fn retention_deadline(&self) -> Duration {
            self.0.retention_deadline()
        }
    }
    run_all(&Normal(SqliteHarness {
        files: common::Files::new(),
    }))
    .await;
}
