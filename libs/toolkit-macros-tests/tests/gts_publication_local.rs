//! `gts(...)` end to end through `HostRuntime`, with a co-located registry:
//! generated descriptor, registry wiring, publication admitted before the
//! publisher's `init`, which reads its admitted manifest, then start.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    reason = "test gears: the `entities` signature is fixed by `gts(...)`"
)]

#[path = "common/fake_publisher.rs"]
mod fake_publisher;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fake_publisher::{Config, LocalRegistry, until};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::gts_publication::{
    GtsPublicationCapability, GtsPublicationInputs, linked_descriptors,
};
use toolkit::runtime::HostRuntime;
use toolkit::{ClientHub, DbOptions, DependencyChecker, Gear, GearCtx, GearRegistry};

/// What `toolkit_gts::declare_gts_inventory!()` provides in a declaring crate.
#[must_use]
pub fn gts_declarations() -> Vec<(String, String)> {
    vec![(
        "gts.cf.test.widget.v1~".to_owned(),
        "{\"type\":\"object\"}".to_owned(),
    )]
}

/// Provides the registry in this process: infrastructure, initialized before
/// the application group's publication.
#[toolkit::gear(name = "fake-registry", stage = infrastructure)]
#[derive(Default)]
pub struct FakeRegistryGear;

#[async_trait::async_trait]
impl Gear for FakeRegistryGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let local = ctx.client_hub().get::<LocalRegistry>()?;
        ctx.client_hub().register::<fake_publisher::Registry>(local);
        Ok(())
    }
}

#[toolkit::gear(
    name = "widget-publisher",
    gts(
        crates = [crate],
        publisher = fake_publisher,
        entities = Self::configured,
    ),
)]
#[derive(Default)]
pub struct WidgetPublisher {
    bound: Arc<AtomicBool>,
}

impl WidgetPublisher {
    fn configured(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        #[derive(serde::Deserialize, Default)]
        struct Cfg {
            vendor: String,
        }
        let cfg: Cfg = inputs.config_or_default()?;
        Ok(vec![(
            format!("gts.cf.test.widget.v1~cf.test.{}.v1", cfg.vendor),
            json!({ "vendor": cfg.vendor }),
        )])
    }
}

#[async_trait::async_trait]
impl Gear for WidgetPublisher {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Admitted before `init`: the registry holds the manifest, and the
        // gear reads it from its context.
        let manifest = ctx.gts_publication_manifest()?;
        assert_eq!(manifest.documents().len(), 2);
        let registry = ctx.client_hub().get::<LocalRegistry>()?;
        assert_eq!(
            registry.admitted.lock().len(),
            1,
            "admitted before the publisher's init"
        );
        self.bound.store(true, Ordering::SeqCst);
        ctx.client_hub()
            .register::<AtomicBool>(Arc::new(AtomicBool::new(true)));
        ctx.client_hub().register::<dyn Echo>(Arc::new(LocalEcho));
        Ok(())
    }
}

/// The client contract `widget-publisher` provides in process.
#[async_trait::async_trait]
pub trait Echo: Send + Sync {
    async fn echo(&self, text: &str) -> Result<String, String>;
}

struct LocalEcho;

#[async_trait::async_trait]
impl Echo for LocalEcho {
    async fn echo(&self, text: &str) -> Result<String, String> {
        Ok(text.to_owned())
    }
}

/// The `Echo` client `echo-consumer` cached in its `init`, and what calling it
/// there returned.
type Cached = (Arc<dyn Echo>, Result<String, String>);

static CACHED: std::sync::OnceLock<Cached> = std::sync::OnceLock::new();

/// Caches the `Echo` client in `init` and calls it at once.
#[toolkit::gear(name = "echo-consumer", deps = [widget_publisher])]
#[derive(Default)]
pub struct EchoConsumer;

#[async_trait::async_trait]
impl Gear for EchoConsumer {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let echo = ctx.client_hub().get::<dyn Echo>()?;
        let early = echo.echo("early").await;
        CACHED
            .set((echo, early))
            .map_err(|_| anyhow::anyhow!("echo-consumer initialized twice"))?;
        Ok(())
    }
}

#[test]
fn the_generated_descriptor_is_linked_with_the_crate_version() {
    let descriptors = linked_descriptors();
    assert_eq!(descriptors.len(), 1);
    let descriptor = descriptors[0];
    assert_eq!(descriptor.gear, "widget-publisher");
    assert_eq!(descriptor.publisher_version, env!("CARGO_PKG_VERSION"));
    assert!(std::ptr::eq(
        WidgetPublisher::default().descriptor(),
        descriptor
    ));

    let config = Config(json!({ "widget-publisher": { "config": { "vendor": "acme" } } }));
    let manifest = descriptor.build_gts_manifest(&config).unwrap();
    let ids: Vec<_> = manifest
        .documents()
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "gts.cf.test.widget.v1~",
            "gts.cf.test.widget.v1~cf.test.acme.v1"
        ]
    );
}

#[tokio::test]
async fn the_generated_backend_forwards_the_callers_absolute_deadline() {
    let hub = ClientHub::new();
    let registry = Arc::new(LocalRegistry::default());
    hub.register::<fake_publisher::Registry>(registry.clone());
    let config = Config(json!({ "widget-publisher": { "config": { "vendor": "acme" } } }));
    let descriptor = WidgetPublisher::default().descriptor();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let publication =
        toolkit::gts_publication::PreparedGtsPublication::prepare(descriptor, &config, &hub)
            .unwrap()
            .with_deadline(deadline);

    publication
        .publish(&CancellationToken::new(), Arc::new(|_| {}))
        .await
        .unwrap();

    assert_eq!(*registry.deadlines.lock(), [Some(deadline)]);
    assert_eq!(registry.admitted.lock().len(), 1);
}

#[tokio::test]
async fn the_runtime_publishes_through_the_local_registry_before_init() {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(LocalRegistry::default());
    hub.register::<LocalRegistry>(Arc::clone(&registry));
    let cancel = CancellationToken::new();
    let runtime = HostRuntime::new(
        GearRegistry::discover_and_build().unwrap(),
        Arc::new(Config(
            json!({ "widget-publisher": { "config": { "vendor": "acme" } } }),
        )),
        DbOptions::None,
        Arc::clone(&hub),
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    );
    let checker = hub.get::<DependencyChecker>().unwrap();
    let running = tokio::spawn(runtime.run_gear_phases());

    until("the publisher's init", || hub.get::<AtomicBool>().is_ok()).await;
    until("readiness", || checker.unresolved_deps().is_empty()).await;

    // A consumer's `init` runs after the publisher's: the client it caches
    // serves at once, through the same reference later.
    let (cached, early) = CACHED.get().expect("the consumer ran");
    assert_eq!(early, &Ok("early".to_owned()));
    assert_eq!(cached.echo("late").await, Ok("late".to_owned()));
    let admitted = registry.admitted.lock().clone();
    assert_eq!(admitted.len(), 1);
    let ((name, version), documents) = &admitted[0];
    assert_eq!(
        (name.as_str(), version.as_str()),
        ("widget-publisher", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(documents.len(), 2);

    cancel.cancel();
    running.await.unwrap().unwrap();
}
