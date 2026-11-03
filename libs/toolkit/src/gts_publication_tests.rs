use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;

const WIDGET: &str = "gts.cf.test.widget.v1~";

struct Config(Option<serde_json::Value>);

impl ConfigProvider for Config {
    fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
        self.0.as_ref()
    }
}

fn config(vendor: &str) -> Config {
    Config(Some(json!({ "config": { "vendor": vendor } })))
}

fn source(origin: &str, docs: &[(&str, serde_json::Value)]) -> DeclarationSource {
    DeclarationSource::new(
        origin,
        docs.iter()
            .map(|(id, doc)| ((*id).to_owned(), doc.to_string()))
            .collect(),
    )
}

#[test]
fn a_manifest_sorts_documents_and_keeps_equal_duplicates_once() {
    let manifest = GtsManifest::build(
        "gear",
        "1.2.3",
        [
            source("sdk", &[("b~", json!({"x": 1})), ("a~", json!({}))]),
            source("crate", &[("b~", json!({"x": 1}))]),
        ],
    )
    .unwrap();

    let ids: Vec<_> = manifest
        .documents()
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(ids, ["a~", "b~"]);
    assert_eq!(manifest.publisher_name(), "gear");
    assert_eq!(manifest.publisher_version(), "1.2.3");
}

#[test]
fn a_manifest_rejects_an_id_declared_with_different_content() {
    let err = GtsManifest::build(
        "gear",
        "1.0.0",
        [
            source("sdk", &[("a~", json!({"x": 1}))]),
            source("crate", &[("a~", json!({"x": 2}))]),
        ],
    )
    .unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains("a~") && message.contains("sdk") && message.contains("crate"),
        "{message}"
    );
}

fn widget_declarations() -> Vec<DeclarationSource> {
    vec![DeclarationSource::new(
        "test-sdk",
        vec![(WIDGET.to_owned(), json!({"$id": WIDGET}).to_string())],
    )]
}

fn vendor_entity(
    inputs: &GtsPublicationInputs<'_>,
) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
    #[derive(serde::Deserialize, Default)]
    struct Cfg {
        vendor: String,
    }
    let cfg: Cfg = inputs.config_or_default()?;
    Ok(vec![(
        format!("{WIDGET}cf.test.{}.v1", cfg.vendor),
        json!({ "vendor": cfg.vendor }),
    )])
}

/// Hub-registered stand-in for a registry: counts calls and answers by mode.
struct FakeRegistry {
    calls: AtomicUsize,
    mode: Mode,
}

#[derive(Clone, Copy)]
enum Mode {
    Admit,
    Refuse,
    Hang,
}

impl FakeRegistry {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            mode,
        })
    }
}

fn fake_backend(hub: &ClientHub) -> anyhow::Result<GtsPublicationBackend> {
    let registry = hub.get::<FakeRegistry>()?;
    Ok(GtsPublicationBackend::new(
        move |request: GtsPublicationRequest| {
            let registry = Arc::clone(&registry);
            async move {
                registry.calls.fetch_add(1, Ordering::SeqCst);
                request.report_pending("dependency pending");
                match registry.mode {
                    Mode::Admit => Ok(()),
                    Mode::Refuse => anyhow::bail!("rejected: incompatible"),
                    Mode::Hang => {
                        request.cancel().cancelled().await;
                        anyhow::bail!("cancelled")
                    }
                }
            }
        },
    ))
}

fn no_declarations() -> Vec<DeclarationSource> {
    Vec::new()
}

fn unreachable_backend(_hub: &ClientHub) -> anyhow::Result<GtsPublicationBackend> {
    anyhow::bail!("an empty manifest binds no backend")
}

static WIDGETS: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "widgets",
    publisher_version: "1.0.0",
    declarations: widget_declarations,
    entities: Some(vendor_entity),
    backend: fake_backend,
    provider: "fake-registry",
    wire: no_wire,
};

static NOTHING: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "nothing",
    publisher_version: "1.0.0",
    declarations: no_declarations,
    entities: None,
    backend: unreachable_backend,
    provider: "fake-registry",
    wire: no_wire,
};

#[test]
fn equal_effective_configuration_gives_an_equal_manifest() {
    let one = WIDGETS.build_gts_manifest(&config("acme")).unwrap();
    let two = WIDGETS.build_gts_manifest(&config("acme")).unwrap();
    let other = WIDGETS.build_gts_manifest(&config("other")).unwrap();

    assert_eq!(one, two);
    assert_ne!(one, other);
    let ids: Vec<_> = one.documents().iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, [WIDGET, "gts.cf.test.widget.v1~cf.test.acme.v1"]);
}

#[test]
fn a_bad_configuration_fails_the_manifest() {
    let err = WIDGETS
        .build_gts_manifest(&Config(Some(json!({ "config": { "vendor": 7 } }))))
        .unwrap_err();
    assert!(format!("{err:#}").contains("widgets"), "{err:#}");
}

fn hub_with(registry: &Arc<FakeRegistry>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<FakeRegistry>(Arc::clone(registry));
    hub
}

fn progress() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|_: &str| {})
}

fn prepared(
    descriptor: &'static GtsPublicationDescriptor,
    hub: &ClientHub,
) -> PreparedGtsPublication {
    PreparedGtsPublication::prepare(descriptor, &config("acme"), hub).unwrap()
}

#[tokio::test]
async fn a_publication_reports_pending_until_admitted() {
    let registry = FakeRegistry::new(Mode::Admit);
    let hub = hub_with(&registry);
    let pending = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let record: Arc<dyn Fn(&str) + Send + Sync> = {
        let pending = Arc::clone(&pending);
        Arc::new(move |d: &str| pending.lock().push(d.to_owned()))
    };

    prepared(&WIDGETS, &hub)
        .publish(&CancellationToken::new(), record)
        .await
        .unwrap();

    assert_eq!(registry.calls.load(Ordering::SeqCst), 1);
    assert_eq!(pending.lock().clone(), ["dependency pending"]);
}

#[tokio::test]
async fn a_refusal_is_terminal() {
    let registry = FakeRegistry::new(Mode::Refuse);

    let err = prepared(&WIDGETS, &hub_with(&registry))
        .publish(&CancellationToken::new(), progress())
        .await
        .unwrap_err();

    assert!(
        matches!(
            err,
            GtsPublicationError::Refused {
                gear: "widgets",
                ..
            }
        ),
        "{err}"
    );
    assert!(err.to_string().contains("incompatible"), "{err}");
}

#[tokio::test]
async fn cancellation_ends_a_pending_publication() {
    let registry = FakeRegistry::new(Mode::Hang);
    let cancel = CancellationToken::new();
    let publication = prepared(&WIDGETS, &hub_with(&registry));

    let running = publication.publish(&cancel, progress());
    tokio::pin!(running);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut running)
            .await
            .is_err(),
        "pending until cancelled"
    );
    cancel.cancel();
    let err = running.await.unwrap_err();

    assert!(
        matches!(err, GtsPublicationError::Cancelled { gear: "widgets" }),
        "{err}"
    );
}

#[tokio::test]
async fn an_empty_manifest_needs_no_registry() {
    let hub = ClientHub::new();

    prepared(&NOTHING, &hub)
        .publish(&CancellationToken::new(), progress())
        .await
        .unwrap();
}

#[tokio::test]
async fn an_already_cancelled_empty_publication_is_cancelled() {
    let cancel = CancellationToken::new();
    cancel.cancel();

    let err = prepared(&NOTHING, &ClientHub::new())
        .publish(&cancel, progress())
        .await
        .unwrap_err();

    assert!(
        matches!(err, GtsPublicationError::Cancelled { .. }),
        "{err}"
    );
}

#[test]
fn a_missing_registry_client_fails_preparation() {
    let err = PreparedGtsPublication::prepare(&WIDGETS, &config("acme"), &ClientHub::new())
        .err()
        .expect("no registry client");
    assert!(
        matches!(
            err,
            GtsPublicationError::Backend {
                gear: "widgets",
                ..
            }
        ),
        "{err}"
    );
}

/// A pending publication does not hold back another one: both run at once,
/// and the batch reports each in the order given.
#[tokio::test]
async fn a_batch_runs_its_publications_concurrently() {
    let hanging = FakeRegistry::new(Mode::Hang);
    let admitting = FakeRegistry::new(Mode::Admit);
    let batch = [
        prepared(&WIDGETS, &hub_with(&hanging)),
        prepared(&WIDGETS, &hub_with(&admitting)),
    ];
    let cancel = CancellationToken::new();
    let reports = Arc::new(parking_lot::Mutex::new(Vec::<(&str, String)>::new()));
    let record: GtsPublicationProgress = {
        let reports = Arc::clone(&reports);
        Arc::new(move |gear, detail: &str| reports.lock().push((gear, detail.to_owned())))
    };

    let running = publish_all(&batch, &cancel, record, true);
    tokio::pin!(running);
    tokio::time::timeout(std::time::Duration::from_millis(50), &mut running)
        .await
        .expect_err("the hanging publication keeps the batch pending");
    assert_eq!(
        admitting.calls.load(Ordering::SeqCst),
        1,
        "admitted beside it"
    );
    assert_eq!(reports.lock().len(), 2, "both reported pending");

    cancel.cancel();
    let results = running.await;
    assert!(matches!(
        results[0].1,
        Err(GtsPublicationError::Cancelled { .. })
    ));
    assert!(results[1].1.is_ok());
}

/// The first refusal fails the batch: the publications still pending end.
#[tokio::test]
async fn a_refusal_ends_the_rest_of_a_stopping_batch() {
    let hanging = FakeRegistry::new(Mode::Hang);
    let refusing = FakeRegistry::new(Mode::Refuse);
    let batch = [
        prepared(&WIDGETS, &hub_with(&hanging)),
        prepared(&WIDGETS, &hub_with(&refusing)),
    ];

    let results = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        publish_all(
            &batch,
            &CancellationToken::new(),
            Arc::new(|_, _: &str| {}),
            true,
        ),
    )
    .await
    .expect("a refusal ends the batch");

    assert!(matches!(
        results[0].1,
        Err(GtsPublicationError::Cancelled { .. })
    ));
    assert!(matches!(
        results[1].1,
        Err(GtsPublicationError::Refused { .. })
    ));
}

#[test]
fn an_id_in_two_manifests_is_an_ownership_conflict() {
    let one = GtsManifest::build(
        "one",
        "1.0.0",
        [source("s", &[("a~", json!({})), ("b~", json!({}))])],
    )
    .unwrap();
    let two = GtsManifest::build("two", "1.0.0", [source("s", &[("b~", json!({}))])]).unwrap();
    let three = GtsManifest::build("three", "1.0.0", [source("s", &[("c~", json!({}))])]).unwrap();

    check_ownership([&one, &three]).unwrap();
    let err = check_ownership([&one, &two, &three]).unwrap_err();

    assert_eq!(
        err.to_string(),
        "GTS ids with more than one publisher:\n- b~ is published by one and two"
    );
}

#[allow(
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value,
    reason = "must match the WireFn signature the gear macro emits"
)]
fn no_wire(
    _hub: &ClientHub,
    _resolver: Arc<dyn crate::discovery::EndpointResolver>,
    _tuning: toolkit_contract::wiring::ClientTuning,
) -> anyhow::Result<crate::discovery::WireOutcome> {
    Ok(crate::discovery::WireOutcome::Local)
}
