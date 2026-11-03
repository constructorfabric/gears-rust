//! `gts_publication(...)` with an empty manifest and no registry in the process: the
//! application group starts without any request, and nothing waits for the
//! registry: a publication with nothing to publish consumes nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    reason = "test gears: the `entities` signature is fixed by `gts_publication(...)`"
)]

#[path = "common/fake_publisher.rs"]
mod fake_publisher;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use fake_publisher::{Config, until};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::runtime::HostRuntime;
use toolkit::{ClientHub, DbOptions, DependencyChecker, Gear, GearCtx, GearRegistry};

#[toolkit::gear(
    name = "optional-publisher",
    gts_publication(publisher = fake_publisher, entities = Self::nothing),
)]
#[derive(Default)]
pub struct OptionalPublisher;

impl OptionalPublisher {
    fn nothing(
        _inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl Gear for OptionalPublisher {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        assert!(ctx.gts_publication_manifest()?.is_empty());
        ctx.client_hub()
            .register::<AtomicBool>(Arc::new(AtomicBool::new(true)));
        Ok(())
    }
}

#[tokio::test]
async fn an_empty_manifest_starts_and_is_ready_without_a_registry() {
    let hub = Arc::new(ClientHub::new());
    let cancel = CancellationToken::new();
    let runtime = HostRuntime::new(
        GearRegistry::discover_and_build().unwrap(),
        Arc::new(Config(json!({}))),
        DbOptions::None,
        Arc::clone(&hub),
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    );
    let checker = hub.get::<DependencyChecker>().unwrap();
    let running = tokio::spawn(runtime.run_gear_phases());

    until("init", || hub.get::<AtomicBool>().is_ok()).await;
    until("readiness", || checker.unresolved_deps().is_empty()).await;

    cancel.cancel();
    running.await.unwrap().unwrap();
}
