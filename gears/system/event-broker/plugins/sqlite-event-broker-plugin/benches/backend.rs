//! The backend bench against `SQLite`: one database file in a temporary
//! directory, or the file `EB_BENCH_SQLITE_PATH` names (to put it on the disk
//! being measured).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use event_broker_backend_bench::{Harness, Settings, Sizing, run_bench};
use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use sqlite_event_broker_plugin::SqliteBackendProvider;

/// Every build opens the same file with the plugin's default settings: the
/// bench measures the plugin as configured out of the box.
struct SqliteHarness {
    path: PathBuf,
    _dir: Option<tempfile::TempDir>,
}

impl SqliteHarness {
    fn start() -> Result<Self, String> {
        if let Ok(path) = std::env::var("EB_BENCH_SQLITE_PATH") {
            return Ok(Self {
                path: PathBuf::from(path),
                _dir: None,
            });
        }
        let dir = tempfile::tempdir().map_err(|err| format!("temporary directory: {err}"))?;
        Ok(Self {
            path: dir.path().join("bench.db"),
            _dir: Some(dir),
        })
    }
}

#[async_trait]
impl Harness for SqliteHarness {
    async fn build(&self, sizing: Sizing) -> Arc<dyn Backend> {
        let serde_json::Value::Object(settings) = serde_json::json!({
            "path": self.path.to_string_lossy(),
            "connections": sizing.connections,
        }) else {
            unreachable!("an object literal");
        };
        match SqliteBackendProvider::default()
            .build(&BuildContext::new(&settings, Role::Standalone))
            .await
        {
            Ok(backend) => backend,
            Err(err) => panic!("the sqlite backend builds from a path: {err}"),
        }
    }

    fn name(&self) -> &'static str {
        "sqlite"
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let settings = Settings::parse_from(std::env::args());
    match SqliteHarness::start() {
        Ok(harness) => run_bench(&harness, &settings).await,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
