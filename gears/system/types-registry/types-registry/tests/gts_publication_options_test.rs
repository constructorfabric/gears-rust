#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! `gears.<gear>.config.gts_publication` through the generated adapter into the real
//! `types_registry_sdk::publication`, with a local (in-process) registry client: the
//! batch size, retry cadence and timeout the reconciliation applies are the configured
//! ones, and a caller's deadline keeps its precedence.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit::config::ConfigProvider;
use toolkit::gts_publication::{
    GtsPublicationCapability, GtsPublicationError, PreparedGtsPublication,
};
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::PlatformTypesRegistryApi;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

const A: &str = "gts.cf.test.publication_options.a.v1~";
const B: &str = "gts.cf.test.publication_options.b.v1~";
const C: &str = "gts.cf.test.publication_options.c.v1~";
const ABSENT: &str = "gts.cf.test.publication_options.absent.v1~";

/// What `toolkit_gts::declare_gts_inventory!()` provides in a declaring crate.
#[must_use]
pub fn gts_declarations() -> Vec<(String, String)> {
    [A, B, C]
        .iter()
        .map(|id| {
            (
                (*id).to_owned(),
                json!({ "$id": format!("gts://{id}") }).to_string(),
            )
        })
        .collect()
}

/// The default adapter: `::types_registry_sdk::publication`.
#[toolkit::gear(name = "options-publisher", gts_publication(crates = [crate]))]
#[derive(Default)]
pub struct OptionsPublisher;

#[async_trait::async_trait]
impl toolkit::Gear for OptionsPublisher {
    async fn init(&self, _ctx: &toolkit::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Config(Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        self.0.get(gear)
    }
}

fn config(policy: Option<Value>) -> Config {
    let mut config = json!({});
    if let Some(policy) = policy {
        config["gts_publication"] = policy;
    }
    Config(json!({ "options-publisher": { "config": config } }))
}

/// A publication of the three declared schemas through `registry`, as the runtime binds it.
fn prepared(registry: &Arc<MockTypesRegistry>, config: &Config) -> PreparedGtsPublication {
    let hub = ClientHub::new();
    let local: Arc<dyn PlatformTypesRegistryApi> = registry.clone();
    hub.register::<dyn PlatformTypesRegistryApi>(local);
    PreparedGtsPublication::prepare(OptionsPublisher.descriptor(), config, &hub).unwrap()
}

/// The instants of the pending reports.
type PendingLog = Arc<Mutex<Vec<tokio::time::Instant>>>;
/// The progress callback `PreparedGtsPublication::publish` takes.
type Progress = Arc<dyn Fn(&str) + Send + Sync>;

/// The instant of each pending report.
fn pending_log() -> (PendingLog, Progress) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&log);
    (
        log,
        Arc::new(move |_: &str| record.lock().push(tokio::time::Instant::now())),
    )
}

fn refusal(result: Result<(), GtsPublicationError>) -> String {
    match result {
        Err(error @ GtsPublicationError::Refused { .. }) => format!("{error:#}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// An expired budget, told apart from a refusal across the adapter boundary.
fn expiry(result: Result<(), GtsPublicationError>) -> String {
    match result {
        Err(error @ GtsPublicationError::Expired { .. }) => format!("{error:#}"),
        other => panic!("expected an expired budget, got {other:?}"),
    }
}

/// A registry that takes two candidates per call refuses the default batch of 100;
/// the configured batch size fits it.
#[tokio::test]
async fn the_configured_batch_size_reaches_every_submission() {
    let registry = Arc::new(MockTypesRegistry::new().with_max_batch(2));
    let message = refusal(
        prepared(&registry, &config(None))
            .publish(&CancellationToken::new(), Arc::new(|_| {}))
            .await,
    );
    assert!(message.contains("options-publisher"), "{message}");

    let registry = Arc::new(MockTypesRegistry::new().with_max_batch(2));
    prepared(&registry, &config(Some(json!({ "batch_size": 2 }))))
        .publish(&CancellationToken::new(), Arc::new(|_| {}))
        .await
        .unwrap();

    let mut sizes: Vec<_> = registry
        .submissions()
        .iter()
        .map(|(_, request)| request.items.len())
        .collect();
    sizes.sort_unstable();
    assert_eq!(sizes, [1, 2]);
    for id in [A, B, C] {
        assert!(registry.content(id).is_some(), "{id} admitted");
    }
}

/// The configured backoff paces the passes, and the configured timeout ends them,
/// measured from `publish`.
#[tokio::test(start_paused = true)]
async fn the_configured_backoff_and_timeout_pace_and_end_the_retries() {
    let registry = Arc::new(MockTypesRegistry::new().completing_after(0));
    registry.depends_on(A, ABSENT);
    let publication = prepared(
        &registry,
        &config(Some(json!({
            "timeout": "3s",
            "retry_backoff": "100ms",
            "retry_backoff_max": "300ms",
        }))),
    );
    tokio::time::advance(Duration::from_secs(300)).await;
    let started = tokio::time::Instant::now();
    let (pending, progress) = pending_log();

    let message = expiry(
        publication
            .publish(&CancellationToken::new(), progress)
            .await,
    );

    assert_eq!(started.elapsed(), Duration::from_secs(3));
    assert!(
        message.contains("deadline") && message.contains(A),
        "{message}"
    );
    let pending = pending.lock().clone();
    assert!(pending.len() > 8, "{}", pending.len());
    for (pair, millis) in pending.windows(2).zip([100, 200, 300, 300, 300]) {
        let ceiling = Duration::from_millis(millis);
        let pause = pair[1].duration_since(pair[0]);
        assert!(
            (ceiling / 2..=ceiling).contains(&pause),
            "{pause:?} for {ceiling:?}"
        );
    }
}

/// Without the section a caller's deadline still replaces the 120 s default, even
/// when longer; a configured timeout caps it.
#[tokio::test(start_paused = true)]
async fn a_callers_deadline_replaces_the_default_and_a_timeout_caps_it() {
    let slow_read = || {
        let registry = Arc::new(MockTypesRegistry::new());
        registry.inject(
            Fault::on(Call::BatchGet)
                .first(1)
                .delay(Duration::from_secs(150)),
        );
        registry
    };

    let registry = slow_read();
    let started = tokio::time::Instant::now();
    prepared(&registry, &config(None))
        .with_deadline(started + Duration::from_secs(180))
        .publish(&CancellationToken::new(), Arc::new(|_| {}))
        .await
        .expect("the caller grants more than the default 120 seconds");
    assert!(registry.content(A).is_some());

    let registry = slow_read();
    let started = tokio::time::Instant::now();
    let message = expiry(
        prepared(&registry, &config(Some(json!({ "timeout": "5s" }))))
            .with_deadline(started + Duration::from_secs(180))
            .publish(&CancellationToken::new(), Arc::new(|_| {}))
            .await,
    );
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert!(message.contains("deadline"), "{message}");
    assert_eq!(registry.calls(Call::Register), 0);
}

/// An unavailable registry under the default policy retries for 120 s from `publish`.
#[tokio::test(start_paused = true)]
async fn without_the_section_the_startup_default_applies() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let publication = prepared(&registry, &config(None));
    tokio::time::advance(Duration::from_secs(60)).await;
    let started = tokio::time::Instant::now();

    expiry(
        publication
            .publish(&CancellationToken::new(), Arc::new(|_| {}))
            .await,
    );

    assert_eq!(
        started.elapsed(),
        types_registry_sdk::STARTUP_PUBLICATION_DEADLINE
    );
}
