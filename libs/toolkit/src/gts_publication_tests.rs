use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;

const WIDGET: &str = "gts.cf.test.publication.widget.v1~";
const A: &str = "gts.cf.test.publication.a.v1~";
const B: &str = "gts.cf.test.publication.b.v1~";
const C: &str = "gts.cf.test.publication.c.v1~";

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
            source("sdk", &[(B, json!({"x": 1})), (A, json!({}))]),
            source("crate", &[(B, json!({"x": 1}))]),
        ],
    )
    .unwrap();

    let ids: Vec<_> = manifest.documents().iter().map(|(id, _)| id.id()).collect();
    assert_eq!(ids, [A, B]);
    assert_eq!(manifest.publisher_name(), "gear");
    assert_eq!(manifest.publisher_version(), "1.2.3");
}

#[test]
fn a_manifest_rejects_an_id_declared_with_different_content() {
    let err = GtsManifest::build(
        "gear",
        "1.0.0",
        [
            source("sdk", &[(A, json!({"x": 1}))]),
            source("crate", &[(A, json!({"x": 2}))]),
        ],
    )
    .unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains(A) && message.contains("sdk") && message.contains("crate"),
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
) -> anyhow::Result<Vec<(gts::GtsId, serde_json::Value)>> {
    #[derive(serde::Deserialize, Default)]
    struct Cfg {
        vendor: String,
    }
    let cfg: Cfg = inputs.config_or_default()?;
    Ok(vec![(
        gts::GtsId::try_new(&format!("{WIDGET}cf.test.publication.{}.v1", cfg.vendor))?,
        json!({ "vendor": cfg.vendor }),
    )])
}

/// Hub-registered stand-in for a registry: counts calls and answers by mode.
struct FakeRegistry {
    calls: AtomicUsize,
    mode: Mode,
    options: parking_lot::Mutex<Vec<GtsPublicationOptions>>,
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
            options: parking_lot::Mutex::default(),
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
                registry.options.lock().push(request.options());
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
    let ids: Vec<_> = one.documents().iter().map(|(id, _)| id.id()).collect();
    assert_eq!(
        ids,
        [
            WIDGET,
            "gts.cf.test.publication.widget.v1~cf.test.publication.acme.v1"
        ]
    );
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
        [source("s", &[(A, json!({})), (B, json!({}))])],
    )
    .unwrap();
    let two = GtsManifest::build("two", "1.0.0", [source("s", &[(B, json!({}))])]).unwrap();
    let three = GtsManifest::build("three", "1.0.0", [source("s", &[(C, json!({}))])]).unwrap();

    check_ownership([&one, &three]).unwrap();
    let err = check_ownership([&one, &two, &three]).unwrap_err();

    assert_eq!(
        err.to_string(),
        format!("GTS ids with more than one publisher:\n- {B} is published by one and two")
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

// ========== `gears.<gear>.config.gts_publication` ==========

fn options_of(config: &serde_json::Value) -> anyhow::Result<GtsPublicationOptions> {
    GtsPublicationOptions::from_config("widgets", &Config(Some(json!({ "config": config }))))
}

#[test]
fn without_a_section_the_options_are_the_startup_defaults() {
    for config in [
        Config(None),
        Config(Some(json!({}))),
        Config(Some(json!({ "config": { "vendor": "acme" } }))),
    ] {
        let options = GtsPublicationOptions::from_config("widgets", &config).unwrap();
        assert_eq!(options, GtsPublicationOptions::default());
        assert_eq!(options.timeout(), None);
        assert_eq!(options.retry_backoff(), Duration::from_secs(1));
        assert_eq!(options.retry_backoff_max(), Duration::from_secs(30));
        assert_eq!(options.batch_size().get(), 100);
    }
}

#[test]
fn the_section_sets_every_field() {
    let options = options_of(&json!({ "gts_publication": {
        "timeout": "2m",
        "retry_backoff": "250ms",
        "retry_backoff_max": "4s",
        "batch_size": 7,
    }}))
    .unwrap();
    assert_eq!(options.timeout(), Some(Duration::from_secs(120)));
    assert_eq!(options.retry_backoff(), Duration::from_millis(250));
    assert_eq!(options.retry_backoff_max(), Duration::from_secs(4));
    assert_eq!(options.batch_size().get(), 7);
}

#[test]
fn an_absent_field_keeps_its_default() {
    let options = options_of(&json!({ "gts_publication": { "batch_size": 3 } })).unwrap();
    assert_eq!(
        (
            options.timeout(),
            options.retry_backoff(),
            options.retry_backoff_max()
        ),
        (None, Duration::from_secs(1), Duration::from_secs(30))
    );
    assert_eq!(options.batch_size().get(), 3);
    // An empty mapping is the default policy.
    assert_eq!(
        options_of(&json!({ "gts_publication": {} })).unwrap(),
        GtsPublicationOptions::default()
    );
}

#[test]
fn an_invalid_section_is_an_error_naming_the_gear_and_the_path() {
    const PATH: &str = "gears.widgets.config.gts_publication";
    let cases = [
        (json!(null), PATH, "expected a mapping"),
        (json!("120s"), PATH, "expected a mapping"),
        (json!([]), PATH, "expected a mapping"),
        (
            json!({ "retry_backof": "1s" }),
            "gts_publication.retry_backof",
            "unknown field",
        ),
        (
            json!({ "timeout": 120 }),
            "gts_publication.timeout",
            "expected a duration",
        ),
        (
            json!({ "timeout": null }),
            "gts_publication.timeout",
            "expected a duration",
        ),
        (
            json!({ "timeout": "soon" }),
            "gts_publication.timeout",
            "expected a duration",
        ),
        (
            json!({ "timeout": "0s" }),
            "gts_publication.timeout",
            "greater than zero",
        ),
        (
            json!({ "retry_backoff": "0ms" }),
            "gts_publication.retry_backoff",
            "greater than zero",
        ),
        (
            json!({ "retry_backoff_max": "0s" }),
            "gts_publication.retry_backoff_max",
            "greater than zero",
        ),
        (
            json!({ "batch_size": 0 }),
            "gts_publication.batch_size",
            "positive integer",
        ),
        (
            json!({ "batch_size": -1 }),
            "gts_publication.batch_size",
            "positive integer",
        ),
        (
            json!({ "batch_size": 1.5 }),
            "gts_publication.batch_size",
            "positive integer",
        ),
        (
            json!({ "batch_size": "100" }),
            "gts_publication.batch_size",
            "positive integer",
        ),
        (
            json!({ "batch_size": null }),
            "gts_publication.batch_size",
            "positive integer",
        ),
        (
            json!({ "retry_backoff": "10s", "retry_backoff_max": "5s" }),
            PATH,
            "retry_backoff_max (5s) is less than retry_backoff (10s)",
        ),
        // Checked against the default maximum too: no silent clamp to 30s.
        (
            json!({ "retry_backoff": "1m" }),
            PATH,
            "retry_backoff_max (30s) is less than retry_backoff (60s)",
        ),
        (
            json!({ "timeout": "500000000000years" }),
            "gts_publication.timeout",
            "too large to be a deadline",
        ),
    ];
    for (section, path, reason) in cases {
        let err = options_of(&json!({ "gts_publication": section.clone() })).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("gears.widgets.config.")
                && message.contains(path)
                && message.contains(reason),
            "{section}: {message}"
        );
    }
}

#[test]
fn the_options_are_not_part_of_the_manifest() {
    let plain = WIDGETS.build_gts_manifest(&config("acme")).unwrap();
    let with_policy = WIDGETS
        .build_gts_manifest(&Config(Some(json!({ "config": {
            "vendor": "acme",
            "gts_publication": { "timeout": "5s", "batch_size": 1 },
        }}))))
        .unwrap();
    assert_eq!(plain, with_policy);
}

/// An `entities` function over a strict gear config, as the real gears have.
fn strict_vendor_entity(
    inputs: &GtsPublicationInputs<'_>,
) -> anyhow::Result<Vec<(gts::GtsId, serde_json::Value)>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Cfg {
        vendor: String,
    }
    let cfg: Cfg = inputs.config()?;
    Ok(vec![(
        gts::GtsId::try_new(&format!("{WIDGET}cf.test.publication.{}.v1", cfg.vendor))?,
        json!({ "vendor": cfg.vendor }),
    )])
}

static STRICT_WIDGETS: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "widgets",
    publisher_version: "1.0.0",
    declarations: widget_declarations,
    entities: Some(strict_vendor_entity),
    backend: fake_backend,
    provider: "fake-registry",
    wire: no_wire,
};

#[test]
fn a_strict_entities_config_accepts_the_section_and_rejects_a_business_typo() {
    let manifest = STRICT_WIDGETS
        .build_gts_manifest(&Config(Some(json!({ "config": {
            "vendor": "acme",
            "gts_publication": { "batch_size": 1 },
        }}))))
        .unwrap();
    assert_eq!(
        manifest,
        STRICT_WIDGETS.build_gts_manifest(&config("acme")).unwrap()
    );

    let err = STRICT_WIDGETS
        .build_gts_manifest(&Config(Some(json!({ "config": {
            "vendor": "acme",
            "gts_publication": { "batch_size": 1 },
            "vendro": "acme",
        }}))))
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("unknown field `vendro`"),
        "{err:#}"
    );
}

#[tokio::test]
async fn the_prepared_options_reach_the_backend() {
    let registry = FakeRegistry::new(Mode::Admit);
    let hub = hub_with(&registry);
    let config = Config(Some(json!({ "config": {
        "vendor": "acme",
        "gts_publication": { "timeout": "9s", "retry_backoff": "2s", "batch_size": 4 },
    }})));
    let publication = PreparedGtsPublication::prepare(&WIDGETS, &config, &hub).unwrap();
    let expected = GtsPublicationOptions::from_config("widgets", &config).unwrap();
    assert_eq!(publication.options(), expected);

    publication
        .publish(&CancellationToken::new(), progress())
        .await
        .unwrap();

    assert_eq!(registry.options.lock().clone(), [expected]);
    assert_eq!(expected.timeout(), Some(Duration::from_secs(9)));
    assert_eq!(expected.batch_size().get(), 4);
}

#[test]
fn invalid_options_fail_preparation_before_binding() {
    let err = PreparedGtsPublication::prepare(
        &WIDGETS,
        &Config(Some(
            json!({ "config": { "vendor": "acme", "gts_publication": { "batch_size": 0 } } }),
        )),
        // No registry client: the options fail first.
        &ClientHub::new(),
    )
    .err()
    .unwrap();
    assert!(
        matches!(
            err,
            GtsPublicationError::Options {
                gear: "widgets",
                ..
            }
        ),
        "{err}"
    );
    let message = format!("{err:#}");
    assert!(
        message.starts_with(
            "widgets: GTS publication options: gears.widgets.config.gts_publication.batch_size"
        ),
        "{message}"
    );
}

fn static_and_entity_declarations() -> Vec<DeclarationSource> {
    vec![
        source(
            "test-sdk",
            &[(C, json!({ "n": 1.0, "k": "v" })), (A, json!({}))],
        ),
        source("test-gear", &[(WIDGET, json!({}))]),
    ]
}

/// The static content again, keys reordered, and one document of its own.
fn typed_entities(
    _inputs: &GtsPublicationInputs<'_>,
) -> anyhow::Result<Vec<(gts::GtsId, serde_json::Value)>> {
    Ok(vec![
        (gts::GtsId::try_new(B)?, json!({ "b": 1 })),
        (gts::GtsId::try_new(C)?, json!({ "k": "v", "n": 1.0 })),
    ])
}

fn conflicting_entities(
    _inputs: &GtsPublicationInputs<'_>,
) -> anyhow::Result<Vec<(gts::GtsId, serde_json::Value)>> {
    Ok(vec![(gts::GtsId::try_new(C)?, json!({ "n": -1.0 }))])
}

static TYPED: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "typed",
    publisher_version: "1.0.0",
    declarations: static_and_entity_declarations,
    entities: Some(typed_entities),
    backend: unreachable_backend,
    provider: "fake-registry",
    wire: no_wire,
};

static CONFLICTING: GtsPublicationDescriptor = GtsPublicationDescriptor {
    entities: Some(conflicting_entities),
    ..TYPED
};

/// Typed `entities` documents merge with the static ones into one sorted manifest,
/// an equal duplicate kept once; a different content names both origins.
#[test]
fn typed_entities_merge_with_the_static_declarations() {
    let manifest = TYPED.build_gts_manifest(&Config(None)).unwrap();

    assert_eq!(
        manifest.documents(),
        [
            (gts::GtsId::try_new(A).unwrap(), json!({})),
            (gts::GtsId::try_new(B).unwrap(), json!({ "b": 1 })),
            (
                gts::GtsId::try_new(C).unwrap(),
                json!({ "k": "v", "n": 1.0 })
            ),
            (gts::GtsId::try_new(WIDGET).unwrap(), json!({})),
        ]
    );

    let message = format!(
        "{:#}",
        CONFLICTING.build_gts_manifest(&Config(None)).unwrap_err()
    );
    assert!(
        message.contains(&format!(
            "{C} is declared with different content by test-sdk and entities"
        )),
        "{message}"
    );
}

fn misspelled_declarations() -> Vec<DeclarationSource> {
    vec![
        source("test-sdk", &[(A, json!({}))]),
        source("test-gear", &[("gts.cf.test.publication.B.v1~", json!({}))]),
    ]
}

static MISSPELLED: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "misspelled",
    declarations: misspelled_declarations,
    entities: None,
    ..TYPED
};

/// An invalid static id fails preparation, named with its origin, before the backend
/// is bound: binding it would fail differently.
#[test]
fn an_invalid_static_id_fails_preparation_before_binding() {
    let err = PreparedGtsPublication::prepare(&MISSPELLED, &Config(None), &ClientHub::new())
        .err()
        .unwrap();

    assert!(
        matches!(
            err,
            GtsPublicationError::GtsManifest {
                gear: "misspelled",
                ..
            }
        ),
        "{err}"
    );
    let message = format!("{err:#}");
    assert!(
        message.contains("gts.cf.test.publication.B.v1~ from test-gear: invalid GTS id"),
        "{message}"
    );
}
