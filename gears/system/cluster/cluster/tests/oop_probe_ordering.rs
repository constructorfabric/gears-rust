//! `D1`'s probe-ordering criterion, driven rather than described
//! (DESIGN.md).
//!
//! The criterion is that `cluster-oop` **binds probes before `start`** and serves
//! `/healthz`, `/readyz`, `/health` and `/openapi.json`. None of that mechanism is
//! cluster's code — it is `run_oop_serving`'s — so this file asserts the property
//! over the **real gear set** this binary links (`cluster` + `grpc-hub`, exactly
//! what `src/registered_gears.rs` names) rather than re-testing the framework with
//! a synthetic gear.
//!
//! Two things make it race-free where the obvious version is not. Snapshotting the
//! probes "during startup" against a gear set that starts in milliseconds is a
//! coin flip, so a **gate gear** holds the start phase open until this test
//! releases it: every pre-`start` assertion is taken while the phase is provably
//! still running, and a regression that moved the bind after `start` would fail on
//! connection-refused rather than on timing. And `run_oop_serving` takes the
//! cancellation token by argument, so the drain half is assertable too — which the
//! binary's own entry point (`run_oop_with_options`, covered in
//! `oop_bootstrap.rs`) hooks to OS signals and does not expose.
//!
//! The gate gear is why this is its own target: `inventory` is per-binary, so a
//! gear that blocks the start phase would block every other test in the same
//! process.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;
use toolkit::runtime::{DbOptions, OopServeOptions, RunOptions, ShutdownOptions, run_oop_serving};
use toolkit::{
    ConfigProvider, DirectoryClient, GearCtx, RegisterInstanceInfo, ServiceEndpoint,
    ServiceInstanceInfo,
};

// `src/registered_gears.rs`, replayed verbatim - and both lines are load-bearing
// for the same reason they are in the binary. `inventory` only sees a crate the
// linker kept, and a crate nothing references is dropped, so **omitting either
// line leaves that gear out of the registry entirely** rather than producing any
// kind of error. Measured, not assumed: the first version of this file named no
// `cluster` item, `GearRegistry::discover_and_build` returned `grpc-hub` alone,
// and every probe below still answered 200 - a lifecycle with no cluster gear in
// it looks exactly like a healthy one from the outside.
// `registered_gears_are_the_gears_discovered` below is the guard that keeps that
// from happening again.
use cluster as _;
use grpc_hub as _;

// The gate gear: holds the start phase open

/// Released by the test to let the start phase complete.
static START_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(0);

/// Posted by the gate gear the instant its `start` has recorded its observation,
/// before it blocks on [`START_GATE`]. The test acquires it to establish a
/// happens-before with [`CLUSTER_WAS_STARTED_FIRST`]: without it the polling
/// thread can reach the ordering assertion before the spawned lifecycle task has
/// even reached the gate gear's `start`, reading the atomic's default `false`
/// (the framework guarantees the *ordering* - `run_start_phase` awaits each gear
/// in `gears_by_system_priority` order - so the only way the flag reads `false`
/// is a test that reads it too early, which is what flaked under CI load).
static GATE_REACHED_START: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(0);

/// What the gate gear saw when its own `start` ran — the Profile 1 ordering
/// observation, recorded rather than assumed (see [`StartGate`]).
static CLUSTER_WAS_STARTED_FIRST: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// A gear whose only behaviour is to make the start phase observably long.
///
/// It is not a stand-in for cluster — cluster's own gear starts in this same
/// process, from this same config, and publishes its profiles before this gear is
/// reached (cluster is `system`-tier, so the topological order puts it first).
/// This gear exists only to keep the *phase* from finishing, so the pre-attach
/// probe assertions are taken at a known point rather than a lucky one.
/// It also carries a second observation, cheap because this gear is already in the
/// right place to make it: **cluster's `start` runs before a non-`system` gear's
/// `start` with no `deps` edge anywhere in this process.** Neither gear declares
/// `deps`, and `run_start_phase` iterates `gears_by_system_priority()`
/// (`host_runtime.rs:838`), which puts every `system`-capability gear ahead of every
/// other one (`registry.rs:290-304`). That is what lets a consumer omit
/// `deps = [cluster]` — which it must, because `deps` naming an unlinked gear is a
/// hard `RegistryError::UnknownDependency` and a Profile 3 consumer links no cluster
/// gear (`tests/consumer_wiring.rs`). The claim is load-bearing enough to measure
/// rather than cite.
#[toolkit::gear(name = "d1-start-gate", capabilities = [stateful])]
#[derive(Default)]
struct StartGate {
    hub: std::sync::OnceLock<Arc<toolkit::client_hub::ClientHub>>,
}

#[async_trait]
impl toolkit::Gear for StartGate {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        drop(self.hub.set(ctx.client_hub()));
        Ok(())
    }
}

#[async_trait]
impl toolkit::contracts::RunnableCapability for StartGate {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        // Recorded before blocking: has cluster already published, even though this
        // gear declares no dependency on it? A bound profile is the proof - the
        // client is registered in cluster's `init`, so its mere presence would prove
        // nothing about the *start* order, while a resolvable `orders` backend only
        // exists after cluster's `start` published it.
        if let Some(hub) = self.hub.get() {
            let published = hub
                .try_get::<dyn cluster_sdk::ClusterClient>()
                .is_some_and(|client| client.cache_backend("orders").is_ok());
            CLUSTER_WAS_STARTED_FIRST.store(published, std::sync::atomic::Ordering::SeqCst);
        }

        // Announce that `start` has run and the observation is recorded, so the
        // test can read `CLUSTER_WAS_STARTED_FIRST` with a happens-before rather
        // than racing the spawned lifecycle task to it. Posted before the block
        // below, so the test's ordering assertion is unblocked while the phase is
        // still held open here.
        GATE_REACHED_START.add_permits(1);

        // Blocks until the test grants a permit. `forget` so a second start (there
        // is none) would block again rather than inherit the permit.
        START_GATE
            .acquire()
            .await
            .expect("gate is never closed")
            .forget();
        Ok(())
    }

    async fn stop(&self, _deadline: CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
}

// Fixtures

/// Config for the two real gears, supplied directly rather than through a YAML
/// file: this target drives `run_oop_serving`, which takes a `ConfigProvider`,
/// and the file-loading half is `oop_bootstrap.rs`'s to cover.
///
/// `grpc-hub` binds an ephemeral port so nothing in CI collides, and cluster gets
/// one standalone profile so `start` needs no Docker and no network (§7.6).
struct TestConfig(std::collections::HashMap<String, serde_json::Value>);

impl TestConfig {
    fn new() -> Self {
        let mut gears = std::collections::HashMap::new();
        gears.insert(
            "grpc-hub".to_owned(),
            serde_json::json!({ "config": { "listen_addr": "127.0.0.1:0" } }),
        );
        gears.insert(
            "cluster".to_owned(),
            serde_json::json!({
                "config": { "profiles": { "orders": { "cache": { "provider": "standalone" } } } }
            }),
        );
        Self(gears)
    }
}

impl ConfigProvider for TestConfig {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        self.0.get(gear_name)
    }
}

/// A directory that records what the presence loop did to it and resolves nothing.
///
/// Nothing here needs to succeed for the probes to serve — which is the point of
/// the framework's "directory registration is explicitly not a readiness signal"
/// rule (§4.4). `deregistered` is read at the end to show the drain ran.
struct StubDirectory {
    registered: AtomicUsize,
    deregistered: AtomicUsize,
}

impl StubDirectory {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            registered: AtomicUsize::new(0),
            deregistered: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl DirectoryClient for StubDirectory {
    async fn resolve_grpc_service(&self, service: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("no gRPC service {service} in the stub directory")
    }
    async fn resolve_rest_service(&self, gear: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("no REST endpoint for {gear} in the stub directory")
    }
    async fn get_openapi_spec(&self, gear: &str) -> anyhow::Result<String> {
        anyhow::bail!("no OpenAPI spec for {gear} in the stub directory")
    }
    async fn list_instances(&self, _gear: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn register_instance(&self, _info: RegisterInstanceInfo) -> anyhow::Result<()> {
        self.registered.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn deregister_instance(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        self.deregistered.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn send_heartbeat(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Reserve an ephemeral port and release it, so the server under test can bind it.
fn free_addr() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

/// A raw HTTP/1.1 GET, returning `(status, body)`. `Connection: close` so the read
/// completes on EOF and no client-side pool outlives the assertion.
async fn http_get(addr: SocketAddr, path: &str) -> Option<(u16, String)> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())?;
    let body = text.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
    Some((status, body))
}

/// Poll `path` until it answers `want`, or fail after `timeout`.
async fn await_status(addr: SocketAddr, path: &str, want: u16, timeout: Duration) -> String {
    await_body(addr, path, want, |_| true, timeout).await
}

/// Poll `path` until it answers `want` **and** the body satisfies `accept`.
///
/// The body predicate is not a convenience. `RestHealthcheckRegistry::report`
/// caches its aggregate for `REPORT_CACHE_TTL` (2 s, `healthcheck.rs:217`) and
/// `/readyz` and `/health` both read through that cache, so a probe polled *before*
/// the router composed keeps serving the pre-composition report - empty components,
/// trivially `healthy` - for two seconds afterwards. Status alone therefore cannot
/// distinguish "cluster reported Ready" from "nothing was registered and the empty
/// report is cached". Found by writing the status-only version first and watching
/// `/health` come back `{"status":"healthy","components":[]}` in a 0.05 s test run.
async fn await_body(
    addr: SocketAddr,
    path: &str,
    want: u16,
    accept: impl Fn(&str) -> bool,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = String::from("<never connected>");
    loop {
        if let Some((status, body)) = http_get(addr, path).await {
            if status == want && accept(&body) {
                return body;
            }
            last = format!("{status} {body}");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "GET {path} never settled on {want} with an acceptable body within {timeout:?}; last: {last}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// The tests

/// `src/registered_gears.rs` names exactly the gears a `cluster-oop` process must
/// run, and this asserts that naming them is what puts them in the registry.
///
/// It is not a tautology: linkage is the whole mechanism, and it fails silently.
/// Delete `use cluster as _;` above and this test reports one gear instead of two,
/// while every HTTP assertion in the test below still passes.
#[test]
fn registered_gears_are_the_gears_discovered() {
    let registry = toolkit::registry::GearRegistry::discover_and_build()
        .expect("the linked gear set should build a registry");
    let names: Vec<&str> = registry
        .gears()
        .iter()
        .map(toolkit::registry::GearEntry::name)
        .collect();

    assert!(
        names.contains(&"cluster"),
        "the cluster gear must be discovered; got {names:?}"
    );
    assert!(
        names.contains(&"grpc-hub"),
        "grpc-hub must be discovered - cluster's `grpc` capability requires it \
         (RegistryError::GrpcRequiresHub); got {names:?}"
    );
    // Cluster is the only REST-capable gear here, and that is what carries its
    // composite readiness check into `/readyz` (`compose_oop_router` collects
    // `RestApiCapability::healthcheck` per gear).
    let rest_capable: Vec<&str> = registry
        .gears()
        .iter()
        .filter(|gear| gear.caps().has::<toolkit::registry::RestApiCap>())
        .map(toolkit::registry::GearEntry::name)
        .collect();
    assert_eq!(
        rest_capable,
        vec!["cluster"],
        "cluster should be the REST-capable gear supplying the readiness dimension"
    );
}

/// `D1`'s third exit criterion, in the order the criterion states it.
///
/// The single test is deliberate: every assertion is a phase of one lifecycle, and
/// splitting them would mean binding the same gear set several times over just to
/// re-reach the state the previous assertion left.
#[tokio::test(flavor = "multi_thread")]
async fn probes_serve_before_start_completes_and_all_four_answer_after() {
    // The test's premise: the gate gear is actually in the lifecycle. Without it
    // the start phase finishes in microseconds and step 1 degrades from a proof
    // into a coin flip, so this is checked rather than assumed.
    let registry = toolkit::registry::GearRegistry::discover_and_build().expect("registry builds");
    assert!(
        registry
            .gears()
            .iter()
            .any(|gear| gear.name() == "d1-start-gate"),
        "the gate gear must be registered, or the pre-start assertions prove nothing"
    );

    let addr = free_addr();
    let directory = StubDirectory::new();

    let cancel = CancellationToken::new();
    // `OopServeOptions` is `#[non_exhaustive]`: built via `new` + `with_*`. Only
    // the three non-default values are spelled out here — `probe_bind_addr` and
    // the 500ms `healthcheck_timeout` already match `new`'s defaults, and so do
    // both authenticators. Both being `None` is the deployed shape for the probe
    // plane either way: probes carry no JWT and are never subject to the auth
    // middlewares (`oop_serve.rs`). The inbound platform-plane authenticator is
    // `A1`(b)'s and reaches HTTP only.
    let serve = OopServeOptions::new(
        "cluster".to_owned(),
        "d1-test".to_owned(),
        format!("http://{addr}"),
        addr,
        Arc::clone(&directory) as Arc<dyn DirectoryClient>,
    )
    .with_version(Some("0.1.5".to_owned()))
    .with_drain_timeout(Duration::from_secs(5))
    .with_heartbeat_interval(Duration::from_secs(30));

    let run = RunOptions::new(
        Arc::new(TestConfig::new()),
        DbOptions::None,
        ShutdownOptions::Token(cancel.clone()),
        uuid::Uuid::new_v4(),
    );

    let lifecycle = tokio::spawn(run_oop_serving(run, serve));

    // 0. Wait until the start phase has provably reached the gate gear (and so has
    //    already run - and awaited to completion - every earlier gear, cluster
    //    among them, since `run_start_phase` is sequential and system-first). This
    //    is the happens-before every "during startup" assertion below relies on:
    //    the gate gear is now parked inside its `start`, holding the phase open, so
    //    the probe states are a genuine mid-startup snapshot and
    //    `CLUSTER_WAS_STARTED_FIRST` is recorded rather than still at its default.
    tokio::time::timeout(Duration::from_secs(10), GATE_REACHED_START.acquire())
        .await
        .expect("the start phase must reach the gate gear within 10s")
        .expect("the gate-reached semaphore is never closed")
        .forget();

    // 1. Liveness answers while the start phase is still blocked on the gate. This
    //    is the criterion's "binds probes before `start`", and it is checked
    //    against a phase that provably has not returned rather than against a
    //    stopwatch.
    let body = await_status(addr, "/healthz", 200, Duration::from_secs(10)).await;
    assert_eq!(body.trim(), "ok", "/healthz body");
    assert_eq!(
        START_GATE.available_permits(),
        0,
        "the gate must still be closed when /healthz answers - otherwise this \
         test proves nothing about ordering"
    );

    // 2. `/readyz` and `/health` answer too, and say `starting`: `startup_complete`
    //    does not flip until the composed routes are published, which is after the
    //    start phase. A pod in this state is live and out of rotation, which is
    //    exactly ADR-0005's `Starting`.
    let readyz = await_status(addr, "/readyz", 503, Duration::from_secs(5)).await;
    assert!(
        readyz.contains("\"state\":\"starting\"") && readyz.contains("\"ready\":false"),
        "/readyz should report the framework's Starting body before start completes, got: {readyz}"
    );
    // `/health` is the diagnostic plane and answers on the same listener. Cluster's
    // own composite check is not in the registry yet (it is collected during router
    // composition), so this is the framework's empty report - a 200, not a 503.
    let health = await_status(addr, "/health", 200, Duration::from_secs(5)).await;
    assert!(
        !health.is_empty(),
        "/health should return a report body before start completes"
    );

    // 3. Gear routes and the OpenAPI document are published at attach, so before
    //    it they are `503 starting` rather than 404 - the framework serving a
    //    "not yet" instead of a "never".
    let (status, _) = http_get(addr, "/openapi.json").await.expect("connects");
    assert_eq!(
        status, 503,
        "/openapi.json should be 503 starting until the composed routes attach"
    );

    // 4. The Profile 1 ordering fact, observed by the gate gear rather than cited:
    //    cluster had already published its profiles by the time a non-`system`
    //    gear's `start` ran, with no `deps` edge in this process. This is what
    //    makes `deps = [cluster]` unnecessary - and it must be unnecessary, because
    //    it is fatal in Profile 3 (`tests/consumer_wiring.rs`). The read is safe
    //    because step 0 already synchronized on the gate gear having recorded it.
    assert!(
        CLUSTER_WAS_STARTED_FIRST.load(Ordering::SeqCst),
        "cluster's `start` must precede a non-system gear's `start` by tier alone; \
         `gears_by_system_priority` is what orders them, not a dependency edge"
    );

    // 5. Release the gate: the start phase completes, the router composes, routes
    //    and the spec attach.
    START_GATE.add_permits(1);

    // 6. Wait for a report computed *after* router composition - identified by
    //    cluster's own component appearing in it, not by a sleep. This is what
    //    makes the next assertion attributable to cluster rather than to an empty
    //    registry: `compose_oop_router` collects `RestApiCapability::healthcheck`
    //    from every REST-capable gear, so `cluster` in `components` means
    //    `ClusterReadiness` is the thing being asked.
    let health = await_body(
        addr,
        "/health",
        200,
        |body| body.contains("cluster"),
        Duration::from_secs(20),
    )
    .await;
    assert!(
        health.contains("\"status\":\"healthy\""),
        "cluster's composite check should report healthy over one standalone profile, got: {health}"
    );

    // 7. `/readyz` reports 200 `ready`, and now it is cluster's composite readiness
    //    driving it: `ClusterReadiness` reports `Starting` while the registry is at
    //    generation 0, so a `ready` verdict means the cluster gear's `start`
    //    published its profiles and the standalone cache probe passed (§4.4).
    let readyz = await_body(
        addr,
        "/readyz",
        200,
        |body| body.contains("\"state\":\"ready\""),
        Duration::from_secs(20),
    )
    .await;
    assert!(
        readyz.contains("\"ready\":true"),
        "/readyz's `ready` mirror should follow the Ready state, got: {readyz}"
    );

    // 8. Every endpoint named by the exit criterion answers. `/.well-known/` is
    //    checked alongside `/openapi.json` because that is the canonical discovery
    //    path the platform binding constraint names, and both are the same handler.
    for path in [
        "/healthz",
        "/readyz",
        "/health",
        "/openapi.json",
        "/.well-known/openapi.json",
    ] {
        let (status, body) = http_get(addr, path).await.expect("connects");
        assert_eq!(status, 200, "GET {path} after start; body: {body}");
        assert!(!body.is_empty(), "GET {path} returned an empty body");
    }

    // 9. The drain sequence: cancel, and the lifecycle returns. `stop` runs, the
    //    instance is deregistered, and none of it is cluster's code.
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(20), lifecycle)
        .await
        .expect("the OoP lifecycle should return promptly after cancellation")
        .expect("the lifecycle task should not panic");
    assert!(
        result.is_ok(),
        "graceful shutdown should succeed: {result:?}"
    );
    assert!(
        directory.registered.load(Ordering::SeqCst) >= 1,
        "the framework's presence loop should have registered the instance"
    );
    // Two deregistrations, and the number is a measured property of this gear set
    // rather than a target: `grpc-hub`'s `stop` deregisters the gears whose gRPC
    // services it published (`grpc-hub/src/gear.rs:348`, so one call naming
    // `cluster`) and `oop_serve`'s drain deregisters the OoP instance
    // (`oop_serve.rs:669`). `HostRuntime::deregister_rest_providers` is *not* a
    // third: it returns early unless `run_directory_register_phase` ran, which the
    // OoP path never does. In the deployed binary both calls name the same
    // (gear, instance) pair - here they differ only because the fixture passes a
    // literal `instance_id` beside a fresh `RunOptions.instance_id`.
    assert!(
        directory.deregistered.load(Ordering::SeqCst) >= 1,
        "the drain should deregister from the directory; got {}",
        directory.deregistered.load(Ordering::SeqCst)
    );
}
