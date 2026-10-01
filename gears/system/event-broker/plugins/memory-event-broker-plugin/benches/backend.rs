//! The backend bench against the memory backend (`make bench-eb-memory`).

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use event_broker_backend_bench::{Harness, Settings, Sizing, run_bench};
use event_broker_sdk::backend::{Backend, Provider};
use memory_event_broker_plugin::MemoryBackendProvider;
use tokio_util::sync::CancellationToken;

/// A provider per build: a cell's events are dropped with its instance rather
/// than held for the rest of the run.
struct MemoryHarness;

#[async_trait]
impl Harness for MemoryHarness {
    async fn build(&self, _sizing: Sizing, stop: CancellationToken) -> Arc<dyn Backend> {
        match MemoryBackendProvider::default()
            .build(&serde_json::Map::new(), stop)
            .await
        {
            Ok(backend) => backend,
            Err(err) => panic!("the memory backend builds from empty settings: {err}"),
        }
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let settings = Settings::parse_from(std::env::args());
    run_bench(&MemoryHarness, &settings).await
}
