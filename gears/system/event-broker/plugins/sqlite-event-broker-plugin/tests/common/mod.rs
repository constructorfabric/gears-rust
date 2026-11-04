//! A database file for the integration tests (`docs/TESTING.md`): a
//! temporary directory each, removed when the fixture drops.

#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test harness: a fixture setup failure is the test failure, and not every helper is used by every test binary"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::{
    Backend, BackendError, BuildContext, LogClaim, LogId, Partition, Provider, Replacing,
    Retention, Role, TopicPartition, TopicSpec,
};
use event_broker_sdk::{GtsInstanceId, Position};
use sqlite_event_broker_plugin::SqliteBackendProvider;
use uuid::Uuid;

/// A temporary directory holding the database files of one test.
pub struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temporary directory"),
        }
    }

    /// The file `name` in the directory; it need not exist yet.
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// A backend over `path` with a fast sweep, and `extra` settings over it;
/// built, not started.
pub async fn built_with(path: &Path, role: Role, extra: serde_json::Value) -> Arc<dyn Backend> {
    let mut settings = serde_json::json!({
        "path": path.to_string_lossy(),
        "sweep": { "interval_ms": 100 },
    });
    if let (Some(settings), serde_json::Value::Object(extra)) = (settings.as_object_mut(), extra) {
        settings.extend(extra);
    }
    let serde_json::Value::Object(settings) = settings else {
        unreachable!("an object literal");
    };
    SqliteBackendProvider::default()
        .build(&BuildContext::new(&settings, role))
        .await
        .expect("the sqlite backend builds from a path")
}

pub async fn built(path: &Path) -> Arc<dyn Backend> {
    built_with(path, Role::Standalone, serde_json::json!({})).await
}

/// `backend` started, once its file is open.
pub async fn started(backend: Arc<dyn Backend>) -> Arc<dyn Backend> {
    backend.start().await.expect("start");
    let probe = TopicPartition::new(topic("probe"), Partition::new(0));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Err(BackendError::Unavailable { .. }) =
        backend.resolve(&probe, Position::Latest).await
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "still Unavailable 30s after start"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    backend
}

/// A topic no other test uses.
pub fn topic(name: &str) -> GtsInstanceId {
    let unique = Uuid::new_v4().simple().to_string();
    GtsInstanceId::try_new(&format!(
        "{}cf.sqlite_test.{name}.t{}.v1",
        toolkit_gts::gts_id!("cf.core.events.topic.v1~"),
        &unique[..12]
    ))
    .expect("a valid GTS instance id")
}

/// `spec` provisioned under a fresh log id.
pub async fn provisioned(
    backend: &dyn Backend,
    name: &str,
    partitions: u32,
    retention: Retention,
) -> TopicSpec {
    let spec = TopicSpec {
        topic: topic(name),
        partitions,
        retention,
    };
    backend
        .provision(
            &spec,
            &LogClaim {
                replacing: Replacing::Nothing,
                new: LogId::new_v4(),
            },
        )
        .await
        .expect("provision");
    spec
}
