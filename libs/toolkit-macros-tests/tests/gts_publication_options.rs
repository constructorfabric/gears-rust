//! `gears.<gear>.config.gts_publication` through the generated publisher adapter:
//! two publishers with strict gear configs share one local registry, each with its
//! own policy, and an invalid policy fails the startup before any registry write.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unnecessary_wraps,
    reason = "test gears: the `entities` signature is fixed by `gts_publication(...)`"
)]

#[path = "common/fake_publisher.rs"]
mod fake_publisher;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use fake_publisher::{Config, LocalRegistry, Policy};
use parking_lot::Mutex;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::gts_publication::{
    GtsPublicationCapability, GtsPublicationInputs, PreparedGtsPublication, publish_all,
};
use toolkit::runtime::HostRuntime;
use toolkit::{ClientHub, DbOptions, Gear, GearCtx, GearRegistry};

/// What `toolkit_gts::declare_gts_inventory!()` provides in a declaring crate.
#[must_use]
pub fn gts_declarations() -> Vec<(String, String)> {
    vec![(
        "gts.cf.test.publication.widget.v1~".to_owned(),
        "{\"type\":\"object\"}".to_owned(),
    )]
}

/// As strict as the real resolver, plugin and AM configs.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictCfg {
    vendor: String,
}

/// What each publisher's `init` read through `GearCtx`, by gear.
#[derive(Default)]
struct Inits(Mutex<Vec<(&'static str, String)>>);

#[toolkit::gear(name = "fake-registry", capabilities = [system])]
#[derive(Default)]
pub struct FakeRegistryGear;

impl toolkit::contracts::SystemCapability for FakeRegistryGear {}

#[async_trait::async_trait]
impl Gear for FakeRegistryGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let local = ctx.client_hub().get::<LocalRegistry>()?;
        ctx.client_hub().register::<fake_publisher::Registry>(local);
        Ok(())
    }
}

/// Reads its strict config in `entities`.
#[toolkit::gear(
    name = "fast-publisher",
    gts_publication(crates = [crate], publisher = fake_publisher, entities = Self::configured),
)]
#[derive(Default)]
pub struct FastPublisher;

impl FastPublisher {
    fn configured(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(gts::GtsId, serde_json::Value)>> {
        let cfg: StrictCfg = inputs.config()?;
        Ok(vec![(
            gts::GtsId::try_new(&format!(
                "gts.cf.test.publication.widget.v1~cf.test.publication.{}.v1",
                cfg.vendor
            ))?,
            json!({ "vendor": cfg.vendor }),
        )])
    }
}

#[async_trait::async_trait]
impl Gear for FastPublisher {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: StrictCfg = ctx.config()?;
        ctx.client_hub()
            .get::<Inits>()?
            .0
            .lock()
            .push(("fast-publisher", cfg.vendor));
        Ok(())
    }
}

/// Publishes a static entity and reads its strict config only through `GearCtx`.
#[toolkit::gear(
    name = "slow-publisher",
    gts_publication(publisher = fake_publisher, entities = Self::fixed),
)]
#[derive(Default)]
pub struct SlowPublisher;

impl SlowPublisher {
    fn fixed(
        _inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(gts::GtsId, serde_json::Value)>> {
        Ok(vec![(
            gts::GtsId::try_new("gts.cf.test.publication.slow.v1~")?,
            json!({ "type": "object" }),
        )])
    }
}

#[async_trait::async_trait]
impl Gear for SlowPublisher {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let required: StrictCfg = ctx.config()?;
        let lenient: StrictCfg = ctx.config_expanded()?;
        assert_eq!(required.vendor, lenient.vendor);
        ctx.client_hub()
            .get::<Inits>()?
            .0
            .lock()
            .push(("slow-publisher", required.vendor));
        Ok(())
    }
}

impl toolkit::var_expand::ExpandVars for StrictCfg {
    fn expand_vars(&mut self) -> Result<(), toolkit::var_expand::ExpandVarsError> {
        Ok(())
    }
}

const FAST: &str = "fast-publisher";
const SLOW: &str = "slow-publisher";

fn fast_policy() -> serde_json::Value {
    json!({ "timeout": "7s", "retry_backoff": "10ms", "retry_backoff_max": "20ms", "batch_size": 1 })
}

fn slow_policy() -> serde_json::Value {
    json!({ "timeout": "10m", "retry_backoff": "2s", "retry_backoff_max": "1m", "batch_size": 500 })
}

fn config(fast: Option<serde_json::Value>, slow: Option<serde_json::Value>) -> Config {
    let gear = |vendor: &str, policy: Option<serde_json::Value>| {
        let mut config = json!({ "vendor": vendor });
        if let Some(policy) = policy {
            config["gts_publication"] = policy;
        }
        json!({ "config": config })
    };
    Config(json!({ FAST: gear("acme", fast), SLOW: gear("zeta", slow) }))
}

fn policy(
    timeout: Option<u64>,
    backoff_ms: (u64, u64),
    batch_size: usize,
    deadline: Option<tokio::time::Instant>,
) -> Policy {
    Policy {
        deadline,
        timeout: timeout.map(Duration::from_secs),
        retry_backoff: (
            Duration::from_millis(backoff_ms.0),
            Duration::from_millis(backoff_ms.1),
        ),
        batch_size: NonZeroUsize::new(batch_size).unwrap(),
    }
}

fn sorted(policies: &Mutex<Vec<(String, Policy)>>) -> Vec<(String, Policy)> {
    let mut policies = policies.lock().clone();
    policies.sort_by(|left, right| left.0.cmp(&right.0));
    policies
}

fn host(
    config: Config,
) -> (
    Arc<ClientHub>,
    Arc<LocalRegistry>,
    CancellationToken,
    HostRuntime,
) {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(LocalRegistry::default());
    hub.register::<LocalRegistry>(Arc::clone(&registry));
    hub.register::<Inits>(Arc::new(Inits::default()));
    let cancel = CancellationToken::new();
    let runtime = HostRuntime::new(
        GearRegistry::discover_and_build().unwrap(),
        Arc::new(config),
        DbOptions::None,
        Arc::clone(&hub),
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    );
    (hub, registry, cancel, runtime)
}

/// Each publisher gets its own policy through the one shared registry client, and
/// both strict gear configs accept the section in `entities` and in `init`.
#[tokio::test]
async fn the_runtime_forwards_each_gears_policy_through_a_shared_registry() {
    let (hub, registry, cancel, runtime) = host(config(Some(fast_policy()), Some(slow_policy())));
    let running = tokio::spawn(runtime.run_gear_phases());

    fake_publisher::until("both inits", || {
        hub.get::<Inits>().unwrap().0.lock().len() == 2
    })
    .await;

    assert_eq!(
        sorted(&registry.policies),
        [
            (FAST.to_owned(), policy(Some(7), (10, 20), 1, None)),
            (
                SLOW.to_owned(),
                policy(Some(600), (2_000, 60_000), 500, None)
            ),
        ]
    );
    let mut inits = hub.get::<Inits>().unwrap().0.lock().clone();
    inits.sort_unstable();
    assert_eq!(
        inits,
        [(FAST, "acme".to_owned()), (SLOW, "zeta".to_owned())]
    );
    cancel.cancel();
    running.await.unwrap().unwrap();
}

/// A gear without the section keeps the defaults, whatever its neighbour sets.
#[tokio::test]
async fn a_gear_without_the_section_keeps_the_defaults() {
    let (hub, registry, cancel, runtime) = host(config(None, Some(slow_policy())));
    let running = tokio::spawn(runtime.run_gear_phases());

    fake_publisher::until("both inits", || {
        hub.get::<Inits>().unwrap().0.lock().len() == 2
    })
    .await;

    assert_eq!(
        sorted(&registry.policies),
        [
            (FAST.to_owned(), policy(None, (1_000, 30_000), 100, None)),
            (
                SLOW.to_owned(),
                policy(Some(600), (2_000, 60_000), 500, None)
            ),
        ]
    );
    cancel.cancel();
    running.await.unwrap().unwrap();
}

/// Neither the order of the publications nor a shared deadline mixes the policies.
#[tokio::test]
async fn the_publication_order_does_not_change_a_gears_policy() {
    let config = config(Some(fast_policy()), Some(slow_policy()));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let expected = [
        (
            FAST.to_owned(),
            policy(Some(7), (10, 20), 1, Some(deadline)),
        ),
        (
            SLOW.to_owned(),
            policy(Some(600), (2_000, 60_000), 500, Some(deadline)),
        ),
    ];
    let fast = FastPublisher.descriptor();
    let slow = SlowPublisher.descriptor();
    for order in [[fast, slow], [slow, fast]] {
        let hub = ClientHub::new();
        let registry = Arc::new(LocalRegistry::default());
        hub.register::<fake_publisher::Registry>(registry.clone());
        let prepared: Vec<_> = order
            .iter()
            .map(|descriptor| {
                PreparedGtsPublication::prepare(descriptor, &config, &hub)
                    .unwrap()
                    .with_deadline(deadline)
            })
            .collect();

        let results = publish_all(
            &prepared,
            &CancellationToken::new(),
            Arc::new(|_, _| {}),
            true,
        )
        .await;

        assert!(
            results.iter().all(|(_, result)| result.is_ok()),
            "{results:?}"
        );
        assert_eq!(sorted(&registry.policies), expected);
    }
}

/// One gear's invalid policy fails the startup before any publication writes, for
/// the valid neighbour too, and the error names the gear and the config path.
#[tokio::test]
async fn an_invalid_policy_prevents_every_publication() {
    let (hub, registry, _cancel, runtime) = host(config(
        Some(fast_policy()),
        Some(json!({ "batch_size": 0 })),
    ));

    let err = runtime.run_gear_phases().await.unwrap_err();

    let message = format!("{err:#}");
    assert!(
        message.contains("slow-publisher")
            && message.contains("gears.slow-publisher.config.gts_publication.batch_size"),
        "{message}"
    );
    assert!(registry.admitted.lock().is_empty());
    assert!(hub.get::<Inits>().unwrap().0.lock().is_empty());
}

/// The section is hidden from the typed config only: a business-field typo still
/// fails, in `entities` before any write, and in `init` through `GearCtx`.
#[tokio::test]
async fn a_business_typo_still_fails_a_strict_config() {
    let mut typo = config(Some(fast_policy()), Some(slow_policy()));
    typo.0[FAST]["config"]["vendro"] = json!("x");
    let (_hub, registry, _cancel, runtime) = host(typo);
    let message = format!("{:#}", runtime.run_gear_phases().await.unwrap_err());
    assert!(message.contains("unknown field `vendro`"), "{message}");
    assert!(registry.admitted.lock().is_empty());

    let mut typo = config(Some(fast_policy()), Some(slow_policy()));
    typo.0[SLOW]["config"]["vendro"] = json!("x");
    let (_hub, _registry, _cancel, runtime) = host(typo);
    let message = format!("{:#}", runtime.run_gear_phases().await.unwrap_err());
    assert!(
        message.contains("slow-publisher") && message.contains("unknown field `vendro`"),
        "{message}"
    );
}
