#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! The legacy ready mode under `HostRuntime`: the registry is a system
//! gear, yet what the application gears register through the
//! legacy client during `init` is validated at once only after every
//! application `init` — a document may depend on one a later `init` registers.
//! A document that never validates fails startup, and what started is stopped.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::GearCtx;
use toolkit::client_hub::ClientHub;
use toolkit::config::ConfigProvider;
use toolkit::contracts::{Gear, RunnableCapability, SystemCapability};
use toolkit::lifecycle::{Status, WithLifecycle};
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::{DbOptions, HostRuntime};
use toolkit_gts::{gts_id, gts_uri};
use types_registry::gear::TypesRegistryGear;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

const REGISTRY: &str = "types-registry";
const PERSON: &str = gts_id!("cf.example.models.late_person.v1~");
const ALICE: &str = gts_id!("cf.example.models.late_person.v1~cf.example.instances.alice.v1");

/// Registers its documents through the legacy client during `init`.
struct Registrant(Vec<Value>);

#[async_trait]
impl Gear for Registrant {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let client = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        RegisterResult::ensure_all_ok(&client.register(self.0.clone()).await?)?;
        Ok(())
    }
}

struct NoConfig;

impl ConfigProvider for NoConfig {
    fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
        None
    }
}

/// The registry without a database, as a system gear, and the
/// `(name, documents)` application registrants, each initialized after the
/// previous one.
fn host(
    registrants: Vec<(&'static str, Vec<Value>)>,
    cancel: &CancellationToken,
) -> (
    HostRuntime,
    Arc<ClientHub>,
    Arc<WithLifecycle<TypesRegistryGear>>,
) {
    let mut builder = RegistryBuilder::default();
    let lifecycle = Arc::new(TypesRegistryGear::default().into_gear());
    let gear = lifecycle.inner_arc();
    builder.register_core_with_meta(REGISTRY, &[], gear.clone() as Arc<dyn Gear>);
    builder.register_system_with_meta(REGISTRY, gear as Arc<dyn SystemCapability>);
    builder.register_stateful_with_meta(
        REGISTRY,
        Arc::clone(&lifecycle) as Arc<dyn RunnableCapability>,
    );
    let mut previous = REGISTRY;
    for (name, documents) in registrants {
        let deps: &'static [&'static str] = Box::leak(Box::new([previous]));
        builder.register_core_with_meta(
            name,
            deps,
            Arc::new(Registrant(documents)) as Arc<dyn Gear>,
        );
        previous = name;
    }
    let hub = Arc::new(ClientHub::default());
    let runtime = HostRuntime::new(
        builder.build_topo_sorted().expect("registry"),
        Arc::new(NoConfig),
        DbOptions::None,
        Arc::clone(&hub),
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    );
    (runtime, hub, lifecycle)
}

fn person_type() -> Value {
    json!({
        "$id": gts_uri!("cf.example.models.late_person.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"],
    })
}

fn alice() -> Value {
    json!({ "id": ALICE, "name": "Alice" })
}

#[tokio::test]
async fn an_application_registration_may_depend_on_a_later_application_init() {
    let cancel = CancellationToken::new();
    let (runtime, hub, _lifecycle) = host(
        vec![("early", vec![alice()]), ("late", vec![person_type()])],
        &cancel,
    );
    let running = tokio::spawn(runtime.run_gear_phases());

    let client = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(client) = hub.get::<dyn TypesRegistryClient>()
                && client.get_instance(ALICE).await.is_ok()
            {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the instance registered before its type is committed with it");
    client.get_type_schema(PERSON).await.expect("the type");

    cancel.cancel();
    running.await.expect("host task").expect("a clean shutdown");
}

#[tokio::test]
async fn a_registration_that_never_validates_fails_startup_and_stops_the_registry() {
    let cancel = CancellationToken::new();
    let (runtime, _hub, lifecycle) = host(vec![("early", vec![alice()])], &cancel);

    let err = tokio::time::timeout(Duration::from_secs(30), runtime.run_gear_phases())
        .await
        .expect("startup ends")
        .expect_err("the instance has no type");

    let message = format!("{err:#}");
    assert!(
        message.contains("legacy finalization") && message.contains("ready mode"),
        "{message}"
    );
    assert_eq!(
        lifecycle.status(),
        Status::Stopped,
        "the started registry is stopped"
    );
    assert!(
        !cancel.is_cancelled(),
        "the caller's token is not cancelled"
    );
}
