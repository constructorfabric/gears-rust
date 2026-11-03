//! Runtime behaviour of the startup groups: the system group starts first,
//! the application group waits for the GTS admission of every manifest before
//! any `init`, and cleanup.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;
use uuid::Uuid;

use super::{DbOptions, HostRuntime};
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;
use crate::context::GearCtx;
use crate::contracts::{Gear, RunnableCapability, SystemCapability};
use crate::gts_publication::{
    DeclarationSource, GtsPublicationBackend, GtsPublicationCapability, GtsPublicationDescriptor,
    GtsPublicationRequest,
};
use crate::registry::{GearRegistry, RegistryBuilder};

/// How the fake registry answers a publisher.
#[derive(Clone)]
pub(super) enum Answer {
    Admit,
    /// Pending until notified, then admitted.
    HoldUntil(Arc<Notify>),
    Refuse,
}

/// A hub-registered fake registry, answering per publisher name.
#[derive(Default)]
pub(super) struct FakeRegistry {
    answers: Mutex<HashMap<String, Answer>>,
    /// Held publications: started, and dropped unfinished.
    held: std::sync::atomic::AtomicUsize,
    dropped: Arc<std::sync::atomic::AtomicBool>,
    /// Publisher names, in the order their publication was sent.
    sent: Mutex<Vec<String>>,
}

/// Sets its flag when dropped.
struct DropFlag(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl FakeRegistry {
    pub(super) fn with(answers: &[(&str, Answer)]) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(
                answers
                    .iter()
                    .map(|(gear, a)| ((*gear).to_owned(), a.clone()))
                    .collect(),
            ),
            ..Self::default()
        })
    }
}

fn fake_backend(hub: &ClientHub) -> anyhow::Result<GtsPublicationBackend> {
    let registry = hub.get::<FakeRegistry>()?;
    Ok(GtsPublicationBackend::new(
        move |request: GtsPublicationRequest| {
            registry
                .sent
                .lock()
                .push(request.gts_manifest().publisher_name().to_owned());
            let answer = registry
                .answers
                .lock()
                .get(request.gts_manifest().publisher_name())
                .cloned()
                .unwrap_or(Answer::Admit);
            let registry = Arc::clone(&registry);
            async move {
                match answer {
                    Answer::Admit => Ok(()),
                    Answer::HoldUntil(release) => {
                        let flag = DropFlag(Arc::clone(&registry.dropped));
                        registry
                            .held
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        request.report_pending("base type absent");
                        release.notified().await;
                        std::mem::forget(flag);
                        Ok(())
                    }
                    Answer::Refuse => anyhow::bail!("rejected"),
                }
            }
        },
    ))
}

fn one_document() -> Vec<DeclarationSource> {
    vec![DeclarationSource::new(
        "test",
        vec![("gts.cf.test.base.v1~".to_owned(), "{}".to_owned())],
    )]
}

fn other_document() -> Vec<DeclarationSource> {
    vec![DeclarationSource::new(
        "test",
        vec![("gts.cf.test.other.v1~".to_owned(), "{}".to_owned())],
    )]
}

fn no_documents() -> Vec<DeclarationSource> {
    Vec::new()
}

macro_rules! descriptor {
    ($name:ident, $gear:literal, $docs:path) => {
        static $name: GtsPublicationDescriptor = GtsPublicationDescriptor {
            gear: $gear,
            publisher_version: "1.0.0",
            declarations: $docs,
            entities: None,
            backend: fake_backend,
            provider: "fake-registry",
            wire: no_wire,
        };
    };
}

descriptor!(PUB_A, "a", one_document);
descriptor!(PUB_B, "b", other_document);
descriptor!(PUB_DUP, "dup", one_document);
descriptor!(PUB_NIL, "nil", no_documents);

/// Records lifecycle events into a shared log.
pub(super) type Events = Arc<Mutex<Vec<String>>>;

/// A runnable gear, publishing when it has a descriptor.
pub(super) struct PubGear {
    name: &'static str,
    descriptor: Option<&'static GtsPublicationDescriptor>,
    events: Events,
    fail_start: bool,
    deps: &'static [&'static str],
    /// In the system group: registers `SystemCapability`.
    system: bool,
    /// Logs its hooks: `wire` (runtime wiring, system gears), `pre_init`,
    /// `post_init` and `legacy` (legacy finalization, system gears).
    hooks: bool,
    /// The hook that fails, by its logged name.
    fail_hook: Option<&'static str>,
}

impl PubGear {
    fn new(
        name: &'static str,
        descriptor: Option<&'static GtsPublicationDescriptor>,
        events: &Events,
    ) -> Self {
        Self {
            name,
            descriptor,
            events: Arc::clone(events),
            fail_start: false,
            deps: &[],
            system: false,
            hooks: false,
            fail_hook: None,
        }
    }

    /// The same gear logging its hooks, with `deps`.
    fn with_hooks(self, deps: &'static [&'static str]) -> Self {
        Self {
            hooks: true,
            deps,
            ..self
        }
    }

    fn hook(&self, hook: &str) -> anyhow::Result<()> {
        if !self.hooks {
            return Ok(());
        }
        self.log(hook);
        anyhow::ensure!(self.fail_hook != Some(hook), "{hook} refused");
        Ok(())
    }

    /// A gear of the system group.
    fn system_gear(name: &'static str, events: &Events) -> Self {
        Self {
            system: true,
            ..Self::new(name, None, events)
        }
    }

    fn log(&self, event: &str) {
        self.events.lock().push(format!("{event}:{}", self.name));
    }
}

#[async_trait]
impl Gear for PubGear {
    async fn pre_init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Nothing is published before the migrations.
        anyhow::ensure!(
            ctx.gts_publication_manifest().is_err(),
            "an admitted manifest before the migrations"
        );
        self.hook("pre_init")
    }

    async fn post_init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        self.hook("post_init")
    }

    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        self.log("init");
        if self.descriptor.is_some() {
            // The admitted manifest is there, before `init`.
            let manifest = ctx.gts_publication_manifest()?;
            self.log(&format!("manifest{}", manifest.documents().len()));
        }
        Ok(())
    }
}

impl GtsPublicationCapability for PubGear {
    fn descriptor(&self) -> &'static GtsPublicationDescriptor {
        self.descriptor.expect("a publishing gear")
    }
}

#[async_trait]
impl SystemCapability for PubGear {
    fn wire_runtime(&self, _sys: &crate::runtime::SystemContext) -> anyhow::Result<()> {
        self.hook("wire")
    }

    async fn legacy_finalize_after_application_init(
        &self,
        _sys: &crate::runtime::SystemContext,
    ) -> anyhow::Result<()> {
        self.hook("legacy")
    }
}

#[async_trait]
impl RunnableCapability for PubGear {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        self.log("start");
        anyhow::ensure!(!self.fail_start, "start refused");
        Ok(())
    }

    async fn stop(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        self.log("stop");
        Ok(())
    }
}

pub(super) fn register(builder: &mut RegistryBuilder, gear: PubGear) {
    let name = gear.name;
    let publishes = gear.descriptor.is_some();
    let deps = gear.deps;
    let system = gear.system;
    let gear = Arc::new(gear);
    builder.register_core_with_meta(name, deps, gear.clone() as Arc<dyn Gear>);
    builder.register_stateful_with_meta(name, gear.clone() as Arc<dyn RunnableCapability>);
    if system {
        builder.register_system_with_meta(name, gear.clone() as Arc<dyn SystemCapability>);
    }
    if publishes {
        builder.register_gts_publication_with_meta(name, gear as Arc<dyn GtsPublicationCapability>);
    }
}

struct NoConfig;
impl ConfigProvider for NoConfig {
    fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
        None
    }
}

pub(super) fn runtime(
    registry: GearRegistry,
    fake: &Arc<FakeRegistry>,
    cancel: &CancellationToken,
) -> HostRuntime {
    let hub = Arc::new(ClientHub::new());
    hub.register::<FakeRegistry>(Arc::clone(fake));
    runtime_with_hub(registry, hub, cancel)
}

fn runtime_with_hub(
    registry: GearRegistry,
    hub: Arc<ClientHub>,
    cancel: &CancellationToken,
) -> HostRuntime {
    HostRuntime::new(
        registry,
        Arc::new(NoConfig) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    )
    .with_shutdown_deadline(Duration::from_secs(2))
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("timed out waiting for {what}");
}

fn has(events: &Events, event: &str) -> bool {
    events.lock().iter().any(|e| e == event)
}

fn any_app_event(events: &Events, gears: &[&str]) -> Vec<String> {
    events
        .lock()
        .iter()
        .filter(|e| gears.iter().any(|g| e.ends_with(&format!(":{g}"))))
        .cloned()
        .collect()
}

/// A pending publication holds back every application `init` — of its own
/// gear, of another publisher already admitted and of a gear publishing
/// nothing — while the system group is initialized, started and stays so.
/// The publications run at once; the admitted manifest is readable in `init`.
#[tokio::test]
async fn a_pending_publication_holds_back_the_whole_application_group() {
    let events: Events = Arc::default();
    let release_a = Arc::new(Notify::new());
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::system_gear("registry", &events));
    register(&mut builder, PubGear::new("a", Some(&PUB_A), &events));
    register(&mut builder, PubGear::new("b", Some(&PUB_B), &events));
    register(&mut builder, PubGear::new("c", None, &events));
    register(&mut builder, PubGear::new("nil", Some(&PUB_NIL), &events));
    let fake = FakeRegistry::with(&[("a", Answer::HoldUntil(Arc::clone(&release_a)))]);
    let cancel = CancellationToken::new();
    let runtime = runtime(builder.build_topo_sorted().unwrap(), &fake, &cancel);
    let checker = Arc::clone(&runtime.dep_checker);

    let running = tokio::spawn(runtime.run_gear_phases());

    until("a and b to be sent, a to wait", || {
        fake.sent.lock().len() == 2 && fake.held.load(std::sync::atomic::Ordering::SeqCst) == 1
    })
    .await;
    assert!(has(&events, "init:registry") && has(&events, "start:registry"));
    assert!(
        any_app_event(&events, &["a", "b", "c", "nil"]).is_empty(),
        "no application init before admission: {:?}",
        events.lock()
    );
    assert_eq!(
        checker.unresolved_deps(),
        [crate::runtime::PREPARATION_GATE]
    );

    release_a.notify_one();
    until("the application group to start", || {
        checker.unresolved_deps().is_empty()
    })
    .await;
    let log = events.lock().clone();
    let pos = |e: &str| log.iter().position(|x| x == e).unwrap();
    for gear in ["a", "b", "c", "nil"] {
        assert!(
            pos("start:registry") < pos(&format!("init:{gear}")),
            "{log:?}"
        );
    }
    assert!(has(&events, "manifest1:a") && has(&events, "manifest0:nil"));
    assert_eq!(
        log.iter().filter(|e| e.starts_with("init:")).count(),
        5,
        "each init once: {log:?}"
    );

    cancel.cancel();
    running.await.unwrap().unwrap();
    let log = events.lock().clone();
    for gear in ["a", "b", "c", "nil", "registry"] {
        assert!(has(&events, &format!("stop:{gear}")), "{gear} stopped");
    }
    assert_eq!(
        log.last().map(String::as_str),
        Some("stop:registry"),
        "the system group stops last: {log:?}"
    );
}

/// A refused publication fails the application group: no application gear is
/// initialized, started or stopped, the refusal is not retried, and the
/// system group keeps running until shutdown.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_refused_publication_fails_the_application_group_only() {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::system_gear("registry", &events));
    register(&mut builder, PubGear::new("a", Some(&PUB_A), &events));
    register(&mut builder, PubGear::new("c", None, &events));
    let fake = FakeRegistry::with(&[("a", Answer::Refuse)]);
    let cancel = CancellationToken::new();
    let runtime = runtime(builder.build_topo_sorted().unwrap(), &fake, &cancel);
    let checker = Arc::clone(&runtime.dep_checker);
    let running = tokio::spawn(runtime.run_gear_phases().in_current_span());
    until("the terminal refusal to be reported", || {
        logs_contain("application group preparation failed")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!running.is_finished(), "the process keeps running");
    assert_eq!(
        checker.unresolved_deps(),
        [crate::runtime::PREPARATION_GATE]
    );
    assert_eq!(fake.sent.lock().clone(), ["a"], "not retried");
    assert!(has(&events, "start:registry"));

    cancel.cancel();
    running.await.unwrap().unwrap();
    assert!(
        any_app_event(&events, &["a", "c"]).is_empty(),
        "{:?}",
        events.lock()
    );
    assert!(has(&events, "stop:registry"));
}

/// A startup failing in a later start stops what started — the
/// system group included.
#[tokio::test]
async fn a_failed_startup_stops_what_started() {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::system_gear("registry", &events));
    register(&mut builder, PubGear::new("a", Some(&PUB_A), &events));
    let mut failing = PubGear::new("c", None, &events);
    failing.fail_start = true;
    failing.deps = &["a"];
    register(&mut builder, failing);
    let cancel = CancellationToken::new();
    let runtime = runtime(
        builder.build_topo_sorted().unwrap(),
        &FakeRegistry::with(&[]),
        &cancel,
    );

    let err = tokio::time::timeout(Duration::from_secs(10), runtime.run_gear_phases())
        .await
        .expect("startup failure returns")
        .expect_err("startup fails");

    assert!(format!("{err:#}").contains("start refused"), "{err:#}");
    for gear in ["registry", "a", "c"] {
        assert!(has(&events, &format!("stop:{gear}")), "{:?}", events.lock());
    }
    assert!(
        !cancel.is_cancelled(),
        "only the runtime-owned roots are cancelled"
    );
}

/// Dropping the runtime's lifecycle future drops a pending publication; no
/// application gear initializes afterwards.
#[tokio::test]
async fn dropping_the_runtime_ends_a_pending_publication() {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::new("a", Some(&PUB_A), &events));
    let release = Arc::new(Notify::new());
    let fake = FakeRegistry::with(&[("a", Answer::HoldUntil(Arc::clone(&release)))]);
    let runtime = runtime(
        builder.build_topo_sorted().unwrap(),
        &fake,
        &CancellationToken::new(),
    );

    let running = tokio::spawn(runtime.run_gear_phases());
    until("a to wait", || {
        fake.held.load(std::sync::atomic::Ordering::SeqCst) == 1
    })
    .await;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());

    until("the held publication to be dropped", || {
        fake.dropped.load(std::sync::atomic::Ordering::SeqCst)
    })
    .await;
    release.notify_one();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!has(&events, "init:a"));
}

/// Cancelling while the barrier is pending ends startup cleanly: no
/// application init, the system group stopped.
#[tokio::test]
async fn cancelling_a_pending_barrier_stops_the_system_group() {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::system_gear("registry", &events));
    register(&mut builder, PubGear::new("a", Some(&PUB_A), &events));
    let fake = FakeRegistry::with(&[("a", Answer::HoldUntil(Arc::new(Notify::new())))]);
    let cancel = CancellationToken::new();
    let runtime = runtime(builder.build_topo_sorted().unwrap(), &fake, &cancel);
    let checker = Arc::clone(&runtime.dep_checker);

    let running = tokio::spawn(runtime.run_gear_phases());
    until("a to wait", || {
        fake.held.load(std::sync::atomic::Ordering::SeqCst) == 1
    })
    .await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("ends")
        .unwrap()
        .unwrap();

    assert_eq!(
        checker.unresolved_deps(),
        [crate::runtime::PREPARATION_GATE]
    );
    assert!(!has(&events, "init:a"));
    assert!(has(&events, "stop:registry"));
}

/// A manifest error is local and deterministic: it fails startup, named,
/// before any gear initializes.
#[tokio::test]
async fn a_manifest_error_fails_startup_before_any_init() {
    fn conflicting() -> Vec<DeclarationSource> {
        vec![
            DeclarationSource::new("one", vec![("x~".to_owned(), "{\"a\":1}".to_owned())]),
            DeclarationSource::new("two", vec![("x~".to_owned(), "{\"a\":2}".to_owned())]),
        ]
    }
    descriptor!(PUB_BAD, "bad", conflicting);
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::system_gear("registry", &events));
    register(&mut builder, PubGear::new("bad", Some(&PUB_BAD), &events));
    let runtime = runtime(
        builder.build_topo_sorted().unwrap(),
        &FakeRegistry::with(&[]),
        &CancellationToken::new(),
    );

    let err = runtime.run_gear_phases().await.unwrap_err();

    let message = format!("{err:#}");
    assert!(
        message.contains("bad") && message.contains("x~"),
        "{message}"
    );
    assert!(events.lock().is_empty(), "{:?}", events.lock());
}

/// Two publishers of one GTS id fail startup before anything runs, even
/// with equal content.
#[tokio::test]
async fn two_publishers_of_one_id_fail_startup() {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::new("a", Some(&PUB_A), &events));
    register(&mut builder, PubGear::new("dup", Some(&PUB_DUP), &events));
    let fake = FakeRegistry::with(&[]);
    let runtime = runtime(
        builder.build_topo_sorted().unwrap(),
        &fake,
        &CancellationToken::new(),
    );

    let err = runtime.run_gear_phases().await.unwrap_err();

    let message = format!("{err:#}");
    assert!(
        message.contains("gts.cf.test.base.v1~ is published by a and dup"),
        "{message}"
    );
    assert!(fake.sent.lock().is_empty(), "nothing was sent");
    assert!(events.lock().is_empty());
}

/// Without a non-empty manifest no registry is needed: the hub has none.
#[tokio::test]
async fn empty_manifests_need_no_registry() {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::new("nil", Some(&PUB_NIL), &events));
    register(&mut builder, PubGear::new("c", None, &events));
    let cancel = CancellationToken::new();
    let runtime = runtime_with_hub(
        builder.build_topo_sorted().unwrap(),
        Arc::new(ClientHub::new()),
        &cancel,
    );
    let checker = Arc::clone(&runtime.dep_checker);

    let running = tokio::spawn(runtime.run_gear_phases());
    until("both to start", || checker.unresolved_deps().is_empty()).await;
    assert!(has(&events, "start:nil") && has(&events, "start:c"));

    cancel.cancel();
    running.await.unwrap().unwrap();
}

/// Migration-only mode runs the runtime wiring and `pre_init` of every gear
/// once, the system group first, and nothing else: no `init`, publication,
/// `post_init`, legacy finalization or `start`.
#[tokio::test]
async fn migrate_only_runs_no_init_and_no_publication() {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(
        &mut builder,
        PubGear::system_gear("registry", &events).with_hooks(&[]),
    );
    register(
        &mut builder,
        PubGear::new("a", Some(&PUB_A), &events).with_hooks(&["registry"]),
    );
    let fake = FakeRegistry::with(&[]);
    let runtime = runtime(
        builder.build_topo_sorted().unwrap(),
        &fake,
        &CancellationToken::new(),
    );

    runtime.run_migration_phases().await.unwrap();

    assert_eq!(
        *events.lock(),
        ["wire:registry", "pre_init:registry", "pre_init:a"]
    );
    assert!(fake.sent.lock().is_empty());
}

/// A system gear initializes before the barrier: it cannot publish.
#[tokio::test]
async fn a_system_publisher_is_a_composition_error() {
    let events: Events = Arc::default();
    let mut gear = PubGear::new("registry", Some(&PUB_A), &events).with_hooks(&[]);
    gear.system = true;
    let mut builder = RegistryBuilder::default();
    register(&mut builder, gear);
    let runtime = runtime(
        builder.build_topo_sorted().unwrap(),
        &FakeRegistry::with(&[]),
        &CancellationToken::new(),
    );

    let err = runtime.run_gear_phases().await.unwrap_err();

    assert!(format!("{err:#}").contains("a system gear"), "{err:#}");
    assert!(events.lock().is_empty(), "no hook ran: {:?}", events.lock());
}

/// A system gear depending on an application gear is refused when the
/// registry is built; on another system gear it is fine.
#[test]
fn a_system_gear_cannot_depend_on_an_application_gear() {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::new("app", None, &events));
    register(
        &mut builder,
        PubGear::system_gear("sys", &events).with_hooks(&["app"]),
    );

    let err = builder.build_topo_sorted().unwrap_err();

    assert!(
        matches!(
            err,
            crate::registry::RegistryError::SystemDependsOnApplication { .. }
        ),
        "{err}"
    );

    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::system_gear("hub", &events));
    register(
        &mut builder,
        PubGear::system_gear("sys", &events).with_hooks(&["hub"]),
    );
    register(
        &mut builder,
        PubGear::new("app", None, &events).with_hooks(&["sys"]),
    );
    builder.build_topo_sorted().unwrap();
}

/// A pre-cancelled runtime runs no runtime wiring and no `pre_init`.
#[tokio::test]
async fn a_cancelled_pre_init_phase_runs_no_hook() {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(
        &mut builder,
        PubGear::system_gear("registry", &events).with_hooks(&[]),
    );
    register(
        &mut builder,
        PubGear::new("a", None, &events).with_hooks(&["registry"]),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    let runtime = runtime(
        builder.build_topo_sorted().unwrap(),
        &FakeRegistry::with(&[]),
        &cancel,
    );

    let err = runtime.run_pre_init_phase().await.unwrap_err();

    assert!(
        matches!(err, crate::registry::RegistryError::Cancelled),
        "{err}"
    );
    assert!(events.lock().is_empty(), "{:?}", events.lock());
}

static EARLY: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
/// Held by every test that counts [`EARLY`]: the tests run in parallel.
static EARLY_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[allow(
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value,
    reason = "must match the WireFn signature the gear macro emits"
)]
fn remote_wire(
    _hub: &ClientHub,
    _resolver: Arc<dyn crate::discovery::EndpointResolver>,
    tuning: toolkit_contract::wiring::ClientTuning,
) -> anyhow::Result<crate::discovery::WireOutcome> {
    EARLY
        .lock()
        .unwrap()
        .push(if tuning.internal_token_provider.is_some() {
            "with-credential"
        } else {
            "no-credential"
        });
    Ok(crate::discovery::WireOutcome::Remote)
}

/// A publication whose registry client resolves remotely.
static PUB_REMOTE: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "a",
    publisher_version: "1.0.0",
    declarations: one_document,
    entities: None,
    backend: fake_backend,
    provider: "fake-registry",
    wire: remote_wire,
};

/// The same publisher with nothing to publish.
static PUB_REMOTE_EMPTY: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "a",
    publisher_version: "1.0.0",
    declarations: no_documents,
    entities: None,
    backend: fake_backend,
    provider: "fake-registry",
    wire: remote_wire,
};

/// A consumed second contract of the registry (a tenant API, say).
static TENANT: crate::discovery::ConsumerRegistration = crate::discovery::ConsumerRegistration {
    owner_gear: "a",
    dep_gear: "fake-registry",
    wire: remote_wire,
};
/// A consumed contract of another provider.
static OTHER: crate::discovery::ConsumerRegistration = crate::discovery::ConsumerRegistration {
    owner_gear: "a",
    dep_gear: "billing",
    wire: remote_wire,
};

fn runtime_publishing(descriptor: &'static GtsPublicationDescriptor) -> HostRuntime {
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::new("a", Some(descriptor), &events));
    runtime(
        builder.build_topo_sorted().unwrap(),
        &FakeRegistry::with(&[]),
        &CancellationToken::new(),
    )
}

/// The registry clients of the publications are wired early, each once: the
/// publisher's, and every consumed contract of the same provider as a
/// registration of its own. A later wiring of every registration skips them;
/// registrations of other providers wait for it.
#[tokio::test]
async fn the_registry_clients_are_wired_early_and_once() {
    let _serial = EARLY_TESTS.lock().await;
    let runtime = runtime_publishing(&PUB_REMOTE);
    let manifests = runtime.build_gts_manifests().unwrap();
    EARLY.lock().unwrap().clear();

    runtime
        .wire_registry_clients_over(&manifests, &[&TENANT, &OTHER])
        .unwrap();
    assert_eq!(
        EARLY.lock().unwrap().len(),
        2,
        "the publisher and the tenant contract, not the other provider's"
    );
    assert_eq!(
        runtime.dep_checker.unresolved_deps(),
        ["application/preparation", "fake-registry"]
    );

    let late = runtime
        .wire_consumer_registrations(&[&TENANT, &OTHER])
        .unwrap();
    assert_eq!(late.len(), 1, "the registry clients are not wired again");
    assert_eq!(EARLY.lock().unwrap().len(), 3);
}

/// An empty manifest needs no registry: its publisher client is not wired and
/// nothing waits for the provider. A contract of that provider the gear
/// consumes for its own use still is, and still gates readiness.
#[tokio::test]
async fn an_empty_manifest_wires_no_registry_but_an_explicit_consumer_still_gates() {
    let _serial = EARLY_TESTS.lock().await;
    let runtime = runtime_publishing(&PUB_REMOTE_EMPTY);
    let manifests = runtime.build_gts_manifests().unwrap();
    EARLY.lock().unwrap().clear();

    runtime
        .wire_registry_clients_over(&manifests, &[&TENANT])
        .unwrap();
    assert!(EARLY.lock().unwrap().is_empty(), "nothing wired early");
    assert_eq!(
        runtime.dep_checker.unresolved_deps(),
        ["application/preparation"]
    );

    runtime.wire_consumer_registrations(&[&TENANT]).unwrap();
    assert_eq!(
        runtime.dep_checker.unresolved_deps(),
        ["application/preparation", "fake-registry"],
        "the explicit consumer gates"
    );
}

/// A provider gear in this process that advertises itself but does not
/// provide the registry contract would make a publication call itself: a
/// configuration error, before any publication.
#[tokio::test]
async fn a_publication_that_would_call_this_process_is_refused() {
    struct Advertised;
    #[async_trait]
    impl Gear for Advertised {
        async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
            Ok(())
        }
    }
    impl SystemCapability for Advertised {}
    impl crate::contracts::RestApiCapability for Advertised {
        fn register_rest(
            &self,
            _ctx: &GearCtx,
            router: axum::Router,
            _openapi: &dyn crate::contracts::OpenApiRegistry,
        ) -> anyhow::Result<axum::Router> {
            Ok(router)
        }
    }
    let _serial = EARLY_TESTS.lock().await;
    let events: Events = Arc::default();
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::new("a", Some(&PUB_REMOTE), &events));
    let provider = Arc::new(Advertised);
    let provider_system = Arc::clone(&provider);
    builder.register_core_with_meta("fake-registry", &[], provider.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(
        "fake-registry",
        provider as Arc<dyn crate::contracts::RestApiCapability>,
    );
    builder.register_system_with_meta(
        "fake-registry",
        provider_system as Arc<dyn SystemCapability>,
    );
    let runtime = runtime(
        builder.build_topo_sorted().unwrap(),
        &FakeRegistry::with(&[]),
        &CancellationToken::new(),
    );
    let manifests = runtime.build_gts_manifests().unwrap();

    let err = runtime
        .wire_registry_clients_over(&manifests, &[])
        .unwrap_err();

    let err = anyhow::Error::from(err);
    assert!(format!("{err:#}").contains("call this process"), "{err:#}");
}

/// Records the children it spawns and whether it stopped them.
#[derive(Default)]
struct FakeBackend {
    spawned: Mutex<Vec<String>>,
    stopped: std::sync::atomic::AtomicBool,
    fail: Option<&'static str>,
}

#[async_trait]
impl crate::backends::OopBackend for Arc<FakeBackend> {
    async fn spawn(&self, config: crate::backends::OopSpawnConfig) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.fail != Some(config.gear_name.as_str()),
            "{} did not spawn",
            config.gear_name
        );
        self.spawned.lock().push(config.gear_name);
        Ok(())
    }

    async fn shutdown_all(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

fn child(name: &str) -> crate::runtime::OopGearSpawnConfig {
    crate::runtime::OopGearSpawnConfig {
        gear_name: name.to_owned(),
        binary: std::path::PathBuf::from("/bin/true"),
        args: Vec::new(),
        env: HashMap::new(),
        working_directory: None,
        rendered_config_json: "{}".to_owned(),
    }
}

/// Children spawn before the application group; when its startup then fails
/// — an `init` error, or a later child that does not spawn — the spawned
/// children are stopped before the error returns, without cancelling the
/// caller's token.
#[tokio::test]
async fn a_failed_startup_stops_the_children_it_spawned() {
    for (fail_child, fail_init) in [(None, true), (Some("second"), false)] {
        struct Failing;
        #[async_trait]
        impl Gear for Failing {
            async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
                anyhow::bail!("init refused")
            }
        }
        let backend = Arc::new(FakeBackend {
            fail: fail_child,
            ..FakeBackend::default()
        });
        let mut builder = RegistryBuilder::default();
        if fail_init {
            builder.register_core_with_meta("app", &[], Arc::new(Failing) as Arc<dyn Gear>);
        }
        let cancel = CancellationToken::new();
        let hub = Arc::new(ClientHub::new());
        let runtime = HostRuntime::new(
            builder.build_topo_sorted().unwrap(),
            Arc::new(NoConfig) as Arc<dyn ConfigProvider>,
            DbOptions::None,
            hub,
            cancel.clone(),
            Uuid::new_v4(),
            Some(crate::runtime::OopSpawnOptions {
                gears: vec![child("first"), child("second")],
                backend: Box::new(Arc::clone(&backend)),
            }),
        );

        let err = runtime.run_gear_phases().await.unwrap_err();

        let expected = if fail_init { "init refused" } else { "second" };
        assert!(format!("{err:#}").contains(expected), "{err:#}");
        assert!(backend.spawned.lock().contains(&"first".to_owned()));
        assert!(
            backend.stopped.load(std::sync::atomic::Ordering::SeqCst),
            "spawned children stopped before the error returned"
        );
        assert!(
            !cancel.is_cancelled(),
            "the caller's token is not cancelled"
        );
    }
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

/// The gears of the group order tests, logging their hooks: the system gear
/// `registry`, then the application publisher `a` and `c`, in this
/// topological order.
pub(super) fn grouped_gears(events: &Events) -> RegistryBuilder {
    let mut builder = RegistryBuilder::default();
    register(
        &mut builder,
        PubGear::system_gear("registry", events).with_hooks(&[]),
    );
    register(
        &mut builder,
        PubGear::new("a", Some(&PUB_A), events).with_hooks(&["registry"]),
    );
    register(
        &mut builder,
        PubGear::new("c", None, events).with_hooks(&["a"]),
    );
    builder
}

/// The startup of [`grouped_gears`]: the system group complete (runtime
/// wiring first), then the application group's `pre_init` before its
/// barrier, its `init`, the legacy finalization of the system gear once, its
/// `post_init`, `start`. Each hook runs once, within its gear's group.
pub(super) const GROUPED_STARTUP: [&str; 15] = [
    "wire:registry",
    "pre_init:registry",
    "init:registry",
    "post_init:registry",
    "start:registry",
    "pre_init:a",
    "pre_init:c",
    "init:a",
    "manifest1:a",
    "init:c",
    "legacy:registry",
    "post_init:a",
    "post_init:c",
    "start:a",
    "start:c",
];

/// Each group runs its own hooks, the system group's before anything of the
/// application group; no application `init` runs while a manifest is pending;
/// the legacy finalization runs once, between the application `init` and its
/// `post_init`.
#[tokio::test]
async fn each_group_runs_its_own_hooks_and_the_legacy_finalization_once() {
    let events: Events = Arc::default();
    let release = Arc::new(Notify::new());
    let fake = FakeRegistry::with(&[("a", Answer::HoldUntil(Arc::clone(&release)))]);
    let cancel = CancellationToken::new();
    let runtime = runtime(
        grouped_gears(&events).build_topo_sorted().unwrap(),
        &fake,
        &cancel,
    );
    let checker = Arc::clone(&runtime.dep_checker);

    let running = tokio::spawn(runtime.run_gear_phases());

    until("a's publication to wait", || {
        fake.held.load(std::sync::atomic::Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(
        *events.lock(),
        GROUPED_STARTUP[..7],
        "no application init while a manifest is pending"
    );

    release.notify_one();
    until("the application group to start", || {
        checker.unresolved_deps().is_empty()
    })
    .await;
    assert_eq!(*events.lock(), GROUPED_STARTUP);

    cancel.cancel();
    running.await.unwrap().unwrap();
}

/// A failing group hook fails startup like any other phase, and what started
/// is stopped: a system gear's runtime wiring, `pre_init` or `post_init`
/// before anything started or the application group began; an application
/// `pre_init` or `post_init`, or the legacy finalization, after the system
/// group started.
#[tokio::test]
async fn a_failing_group_hook_fails_startup_and_stops_what_started() {
    for (gear, hook, system_started, app_pre_init, app_init) in [
        ("registry", "wire", false, false, false),
        ("registry", "pre_init", false, false, false),
        ("registry", "post_init", false, false, false),
        ("a", "pre_init", true, true, false),
        ("registry", "legacy", true, true, true),
        ("a", "post_init", true, true, true),
    ] {
        let events: Events = Arc::default();
        let mut registry = PubGear::system_gear("registry", &events).with_hooks(&[]);
        let mut app = PubGear::new("a", None, &events).with_hooks(&["registry"]);
        for candidate in [&mut registry, &mut app] {
            if candidate.name == gear {
                candidate.fail_hook = Some(hook);
            }
        }
        let mut builder = RegistryBuilder::default();
        register(&mut builder, registry);
        register(&mut builder, app);
        let cancel = CancellationToken::new();
        let runtime = runtime(
            builder.build_topo_sorted().unwrap(),
            &FakeRegistry::with(&[]),
            &cancel,
        );

        let err = runtime.run_gear_phases().await.unwrap_err();

        let log = events.lock().clone();
        assert!(
            format!("{err:#}").contains(&format!("{hook} refused")),
            "{err:#}"
        );
        assert_eq!(has(&events, "start:registry"), system_started, "{log:?}");
        assert_eq!(
            has(&events, "stop:registry"),
            system_started,
            "what started is stopped: {log:?}"
        );
        assert_eq!(has(&events, "pre_init:a"), app_pre_init, "{log:?}");
        assert_eq!(has(&events, "init:a"), app_init, "{log:?}");
        assert!(!has(&events, "start:a"), "{log:?}");
        assert!(
            !cancel.is_cancelled(),
            "the caller's token is not cancelled"
        );
    }
}

/// A system consumer of a provider outside this process.
static SYSTEM_CONSUMES_EXTERNAL: crate::discovery::ConsumerRegistration =
    crate::discovery::ConsumerRegistration {
        owner_gear: "sys",
        dep_gear: "billing",
        wire: remote_wire,
    };
/// An SDK-submitted registration (no owner gear) of a system provider.
static SDK_CONSUMES_SYSTEM: crate::discovery::ConsumerRegistration =
    crate::discovery::ConsumerRegistration {
        owner_gear: "cluster-sdk",
        dep_gear: "sys",
        wire: remote_wire,
    };
/// An SDK-submitted registration of a provider outside this process.
static SDK_CONSUMES_EXTERNAL: crate::discovery::ConsumerRegistration =
    crate::discovery::ConsumerRegistration {
        owner_gear: "cluster-sdk",
        dep_gear: "remote-only",
        wire: remote_wire,
    };
/// An application consumer of an application provider of this process.
static APP_CONSUMES_APP: crate::discovery::ConsumerRegistration =
    crate::discovery::ConsumerRegistration {
        owner_gear: "app",
        dep_gear: "app-provider",
        wire: |hub, resolver, tuning| {
            remote_wire(hub, resolver, tuning).map(|_| crate::discovery::WireOutcome::Local)
        },
    };
/// A system consumer of an application provider of this process.
static SYSTEM_CONSUMES_APP: crate::discovery::ConsumerRegistration =
    crate::discovery::ConsumerRegistration {
        owner_gear: "sys",
        dep_gear: "app-provider",
        wire: remote_wire,
    };

/// A publication to an application gear of this process.
static PUB_TO_APP: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "a",
    publisher_version: "1.0.0",
    declarations: one_document,
    entities: None,
    backend: fake_backend,
    provider: "app-provider",
    wire: remote_wire,
};
/// The same publication with nothing to publish.
static PUB_TO_APP_EMPTY: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "a",
    publisher_version: "1.0.0",
    declarations: no_documents,
    entities: None,
    backend: fake_backend,
    provider: "app-provider",
    wire: remote_wire,
};

/// `sys` (system), `app` and `app-provider` (application), and `a`
/// publishing with `publication` when given.
fn wiring_runtime(
    events: &Events,
    publication: Option<&'static GtsPublicationDescriptor>,
) -> HostRuntime {
    let mut builder = RegistryBuilder::default();
    register(&mut builder, PubGear::system_gear("sys", events));
    register(&mut builder, PubGear::new("app", None, events));
    register(&mut builder, PubGear::new("app-provider", None, events));
    if let Some(publication) = publication {
        register(&mut builder, PubGear::new("a", Some(publication), events));
    }
    runtime(
        builder.build_topo_sorted().unwrap(),
        &FakeRegistry::with(&[]),
        &CancellationToken::new(),
    )
}

fn sorted(mut deps: Vec<String>) -> Vec<String> {
    deps.sort();
    deps
}

/// With the system group, the clients a system gear owns or provides are
/// wired — an SDK registration without an owner gear by its provider; every
/// other one after the application `init`, each once, so a local application
/// provider is not shadowed.
#[tokio::test]
async fn system_clients_are_wired_with_their_group_the_rest_after_application_init() {
    let _serial = EARLY_TESTS.lock().await;
    let runtime = wiring_runtime(&Events::default(), None);
    let all = [
        &SYSTEM_CONSUMES_EXTERNAL,
        &SDK_CONSUMES_SYSTEM,
        &SDK_CONSUMES_EXTERNAL,
        &APP_CONSUMES_APP,
    ];
    EARLY.lock().unwrap().clear();

    runtime.wire_system_clients(&all).unwrap();
    assert_eq!(EARLY.lock().unwrap().len(), 2);
    assert_eq!(
        sorted(runtime.dep_checker.unresolved_deps()),
        ["application/preparation", "billing", "sys"]
    );

    runtime.wire_consumer_registrations(&all).unwrap();
    assert_eq!(EARLY.lock().unwrap().len(), 4, "the other two, once each");
    assert_eq!(
        sorted(runtime.dep_checker.unresolved_deps()),
        ["application/preparation", "billing", "remote-only", "sys"]
    );
}

/// A client the runtime would wire before its application provider of this
/// process initialized is refused before any gear runs: a system consumer
/// of it, or a non-empty publication to it. An empty publication wires
/// nothing, and other group combinations are fine.
#[tokio::test]
async fn a_client_wired_before_its_application_provider_is_refused() {
    let runtime = wiring_runtime(&Events::default(), None);
    let err = runtime
        .check_wiring_groups(&[], &[&SYSTEM_CONSUMES_APP])
        .unwrap_err();
    assert!(
        format!("{:#}", anyhow::Error::from(err)).contains("an application gear of this process")
    );
    runtime
        .check_wiring_groups(
            &[],
            &[
                &SYSTEM_CONSUMES_EXTERNAL,
                &SDK_CONSUMES_SYSTEM,
                &SDK_CONSUMES_EXTERNAL,
                &APP_CONSUMES_APP,
            ],
        )
        .unwrap();

    let empty = wiring_runtime(&Events::default(), Some(&PUB_TO_APP_EMPTY));
    let manifests = empty.build_gts_manifests().unwrap();
    empty.check_wiring_groups(&manifests, &[]).unwrap();

    let events: Events = Arc::default();
    let err = wiring_runtime(&events, Some(&PUB_TO_APP))
        .run_gear_phases()
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("an application gear of this process"),
        "{err:#}"
    );
    assert!(events.lock().is_empty(), "no gear ran: {:?}", events.lock());
}
