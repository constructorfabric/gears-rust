//! A gear with nothing to publish that consumes the publisher's registry
//! contract for its own business: publishing and consuming are separate.
//! The empty publication needs no registry, while the explicit consumer is
//! wired like any consumed contract and holds readiness until it resolves.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unnecessary_wraps,
    reason = "test gears: the `entities` signature is fixed by `gts_publication(...)`"
)]

#[path = "common/fake_publisher.rs"]
mod fake_publisher;

use std::sync::Arc;

use fake_publisher::{Config, until};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::runtime::HostRuntime;
use toolkit::{ClientHub, DbOptions, DependencyChecker, Gear, GearCtx, GearRegistry};

#[toolkit::consumes(contract = fake_publisher::FakeRegistryApi, from = "fake-registry")]
#[toolkit::gear(
    name = "registry-user",
    gts_publication(publisher = fake_publisher, entities = Self::nothing),
)]
#[derive(Default)]
pub struct RegistryUser;

impl RegistryUser {
    fn nothing(
        _inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(gts::GtsId, serde_json::Value)>> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl Gear for RegistryUser {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn an_explicit_consumer_of_the_registry_still_gates_readiness() {
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

    until("the application group's start", || {
        !checker
            .unresolved_deps()
            .contains(&toolkit::runtime::PREPARATION_GATE.to_owned())
    })
    .await;
    assert_eq!(checker.unresolved_deps(), ["fake-registry"]);
    assert!(
        hub.for_gear("registry-user")
            .get::<fake_publisher::Registry>()
            .is_ok(),
        "the business client is wired"
    );

    cancel.cancel();
    running.await.unwrap().unwrap();
}
