//! Tests for the composite readiness healthcheck.
//!
//! Two things shape them. First, the registry is driven by the **real wiring**, as
//! in `registry_tests.rs`: the bound-profile set is the check's only data source,
//! and a fabricated `BoundProfile` would not carry the wrapper stack a deployed
//! one does. Second, the degrading backend is wrapped in
//! [`InstrumentedCache`] exactly as both real plugins wrap theirs — without that
//! the tests would pass against a `probe()` the decorator never forwards, which is
//! the one way this feature can be wrong and still look right.
//!
//! The `/readyz` assertions go through the framework's own
//! [`ReadinessState`](toolkit::runtime::ReadinessState) rather than restating its
//! mapping, so the bodies asserted are the bodies it renders.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::{
    CacheConsistency, CacheEntry, CacheFeatures, CacheWatch, ClusterCacheBackend,
    ClusterCacheProvider, ClusterError, InstrumentedCache, NoopMetrics, ProfileHealth,
    ProviderErrorKind, StopHook,
};
use toolkit::client_hub::ClientHub;
use toolkit::runtime::{ReadinessLifecycle, ReadinessState};
use toolkit::{Healthcheck, HealthcheckResult, HealthcheckStatus, RestHealthcheckRegistry};

use super::ClusterReadiness;
use crate::domain::registry::{BoundProfile, ProfileRegistry};
use crate::{ClusterConfig, ClusterHandle, ClusterWiring, ProviderRegistry};
use standalone_cluster_plugin::StandaloneCacheProvider;

// ---------------------------------------------------------------------------
// A cache whose `probe()` is under the test's control
// ---------------------------------------------------------------------------

/// How the stub's `probe()` answers.
#[derive(Clone, Copy)]
enum ProbeBehaviour {
    Ok,
    Fail,
    /// Never returns — the hang the budget exists to bound.
    Hang,
}

/// A cache backend that exists to answer `probe()`. Every data method is
/// unreachable in these tests and says so rather than pretending to work.
///
/// `consistency()` is `Linearizable` because the wiring refuses to auto-fill the
/// lock and leader-election defaults over anything weaker — this stub has to get
/// through the real wiring, not around it.
struct ProbeCache {
    behaviour: ProbeBehaviour,
    probes: Arc<AtomicUsize>,
}

impl ProbeCache {
    fn unreachable<T>() -> Result<T, ClusterError> {
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            message: "ProbeCache serves probes only".to_owned(),
        })
    }
}

#[async_trait]
impl ClusterCacheBackend for ProbeCache {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(false)
    }

    fn provider_name(&self) -> &'static str {
        "probe-stub"
    }

    async fn get(&self, _key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        Self::unreachable()
    }

    async fn put(&self, _req: PutRequest<'_>) -> Result<(), ClusterError> {
        Self::unreachable()
    }

    async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
        Self::unreachable()
    }

    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        Self::unreachable()
    }

    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        Self::unreachable()
    }

    async fn compare_and_swap(
        &self,
        _key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        Self::unreachable()
    }

    async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
        Self::unreachable()
    }

    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        Self::unreachable()
    }

    async fn probe(&self) -> Result<(), ClusterError> {
        self.probes.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            ProbeBehaviour::Ok => Ok(()),
            ProbeBehaviour::Fail => Err(ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                // Deliberately DSN-shaped: the check must not put this on
                // `/health`, and the sanitizer collapsing it would hide the fact
                // that the check leaked it at all.
                message: "connection to postgres://user:pw@db/orders refused".to_owned(),
            }),
            ProbeBehaviour::Hang => std::future::pending().await,
        }
    }
}

/// A provider handing out [`ProbeCache`] wrapped the way a real plugin wraps its
/// cache — see the module docs for why the decorator is in the path.
struct ProbeCacheProvider {
    behaviour: ProbeBehaviour,
    probes: Arc<AtomicUsize>,
}

#[async_trait]
impl ClusterCacheProvider for ProbeCacheProvider {
    fn provider(&self) -> &'static str {
        "probe-stub"
    }

    async fn build_cache(
        &self,
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(Arc<dyn ClusterCacheBackend>, StopHook), ClusterError> {
        let inner: Arc<dyn ClusterCacheBackend> = Arc::new(ProbeCache {
            behaviour: self.behaviour,
            probes: Arc::clone(&self.probes),
        });
        let cache: Arc<dyn ClusterCacheBackend> = Arc::new(InstrumentedCache::new(
            inner,
            "probe-stub",
            Arc::new(NoopMetrics),
        ));
        Ok((cache, Box::new(|| Box::pin(async {}))))
    }
}

// ---------------------------------------------------------------------------
// Wiring helpers
// ---------------------------------------------------------------------------

/// Wires `standalone` and `probe-stub` profiles through the real config path.
async fn wire(
    standalone: &[&str],
    probing: &[&str],
    behaviour: ProbeBehaviour,
    probes: &Arc<AtomicUsize>,
) -> (ClusterHandle, Vec<Arc<BoundProfile>>) {
    let mut yaml = String::from("profiles:\n");
    for name in standalone {
        writeln!(yaml, "  {name}:\n    cache: {{ provider: standalone }}")
            .expect("writing to a String cannot fail");
    }
    for name in probing {
        writeln!(yaml, "  {name}:\n    cache: {{ provider: probe-stub }}")
            .expect("writing to a String cannot fail");
    }
    let cfg: ClusterConfig = serde_saphyr::from_str(&yaml).expect("config parses");
    let providers = ProviderRegistry::new()
        .with_cache_provider(Arc::new(StandaloneCacheProvider))
        .with_cache_provider(Arc::new(ProbeCacheProvider {
            behaviour,
            probes: Arc::clone(probes),
        }));
    ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
        .await
        .expect("wiring starts")
}

/// A published registry over the wired set, plus the handle to stop.
async fn published(
    standalone: &[&str],
    probing: &[&str],
    behaviour: ProbeBehaviour,
) -> (ClusterHandle, Arc<ProfileRegistry>, Arc<AtomicUsize>) {
    let probes = Arc::new(AtomicUsize::new(0));
    let (handle, bound) = wire(standalone, probing, behaviour, &probes).await;
    let registry = Arc::new(ProfileRegistry::new());
    registry.publish(bound);
    (handle, registry, probes)
}

/// The check over `registry`, expecting `configured`, with a budget short enough
/// that a hung probe is outrun without the test waiting on the real one.
fn check_for(registry: &Arc<ProfileRegistry>, configured: &[&str]) -> ClusterReadiness {
    ClusterReadiness::new(Arc::clone(registry), configured.iter().copied())
        .with_budget(Duration::from_millis(50))
}

// ---------------------------------------------------------------------------
// The health verdict
// ---------------------------------------------------------------------------

#[tokio::test]
async fn before_start_publishes_the_check_is_unhealthy() {
    // The `init` -> `start` window. Generation 0 is the whole signal: `publish`
    // always bumps it, so an empty set at a later generation is a deliberate
    // empty configuration rather than an unfinished startup.
    let registry = Arc::new(ProfileRegistry::new());
    let check = check_for(&registry, &["orders"]);

    let result = check.check().await;
    assert_eq!(result.status, HealthcheckStatus::Unhealthy);
    assert_eq!(result.code.as_deref(), Some("starting"));
}

#[tokio::test]
async fn an_empty_configuration_is_ready_once_published() {
    // Nothing configured and nothing bound is not a half-started pod: there is no
    // profile that could fail to serve. Distinguishing this from the case above is
    // the reason the check reads `generation` rather than emptiness.
    let (handle, registry, _) = published(&[], &[], ProbeBehaviour::Ok).await;
    let check = check_for(&registry, &[]);

    assert_eq!(check.check().await.status, HealthcheckStatus::Healthy);
    handle.stop().await;
}

#[tokio::test]
async fn every_configured_profile_bound_and_probing_is_healthy() {
    let (handle, registry, _) = published(&["orders", "billing"], &[], ProbeBehaviour::Ok).await;
    let check = check_for(&registry, &["orders", "billing"]);

    let result = check.check().await;
    assert_eq!(result.status, HealthcheckStatus::Healthy);
    assert!(result.message.is_none());
    handle.stop().await;
}

#[tokio::test]
async fn a_configured_profile_missing_from_the_registry_is_unhealthy() {
    // The `Starting` row's second clause: the registry has published, but not
    // everything configuration declares. A registry-only check cannot see this at
    // all, which is why the configured set is captured alongside it.
    let (handle, registry, _) = published(&["orders"], &[], ProbeBehaviour::Ok).await;
    let check = check_for(&registry, &["orders", "billing"]);

    let result = check.check().await;
    assert_eq!(result.status, HealthcheckStatus::Unhealthy);
    assert_eq!(result.code.as_deref(), Some("profiles_unbound"));
    let message = result.message.expect("an unbound profile is named");
    assert!(
        message.contains("billing") && !message.contains("orders"),
        "only the unbound profile is named, got: {message}"
    );
    handle.stop().await;
}

#[tokio::test]
async fn one_unreachable_profile_is_degraded_not_unhealthy() {
    // The verdict this whole module exists to get right. `Unhealthy` would take
    // the pod out of rotation and with it coordination for the healthy profile.
    let (handle, registry, probes) =
        published(&["orders"], &["billing"], ProbeBehaviour::Fail).await;
    let check = check_for(&registry, &["orders", "billing"]);

    let result = check.check().await;
    assert_eq!(
        result.status,
        HealthcheckStatus::Degraded,
        "one unreachable profile is Degraded, never Unhealthy"
    );
    assert_eq!(result.code.as_deref(), Some("profile_degraded"));
    assert!(
        probes.load(Ordering::SeqCst) > 0,
        "the decorator must forward probe() to the backend under it"
    );

    // And the per-profile consequence: only the failing profile's descriptor
    // degrades, which is what pulls its consumers - and only its consumers - out
    // of rotation.
    let snapshot = registry.snapshot();
    assert_eq!(
        snapshot.profiles["billing"].descriptor().health,
        ProfileHealth::Degraded
    );
    assert_eq!(
        snapshot.profiles["orders"].descriptor().health,
        ProfileHealth::Serving
    );
    handle.stop().await;
}

#[tokio::test]
async fn the_failure_detail_never_reaches_the_health_message() {
    // `/health` is unauthenticated. A provider error can carry a DSN, so the text
    // stays in the log and the message names profiles only. The framework's
    // sanitizer would collapse a leak to "health check failed" - which is why this
    // asserts on the message the check produced, before sanitization.
    let (handle, registry, _) = published(&[], &["billing"], ProbeBehaviour::Fail).await;
    let check = check_for(&registry, &["billing"]);

    let message = check
        .check()
        .await
        .message
        .expect("a degraded profile is named");
    assert!(
        !message.contains("postgres") && !message.contains("pw"),
        "the probe error must not reach the health message, got: {message}"
    );
    assert!(message.contains("billing"));
    handle.stop().await;
}

#[tokio::test]
async fn every_profile_unreachable_is_still_degraded() {
    // The case DESIGN section 4.4's table leaves open. Still `Degraded`: a 503
    // removes the pod from its Service endpoints, taking the gRPC port with it -
    // and `DescribeProfiles` is how a consumer learns its profile is degraded. No
    // restart fixes an unreachable database, so nothing is bought by hiding the
    // signal.
    let (handle, registry, _) = published(&[], &["orders", "billing"], ProbeBehaviour::Fail).await;
    let check = check_for(&registry, &["orders", "billing"]);

    let result = check.check().await;
    assert_eq!(result.status, HealthcheckStatus::Degraded);
    let message = result.message.expect("both profiles are named");
    assert!(message.contains("orders") && message.contains("billing"));
    handle.stop().await;
}

#[tokio::test]
async fn a_hung_probe_reports_degraded_inside_the_budget() {
    // The framework maps an overrunning check to `Unhealthy`, so a probe that
    // hangs would report the pod `starting`/503. The check's own budget is what
    // converts the hang into the right verdict.
    let (handle, registry, _) = published(&["orders"], &["billing"], ProbeBehaviour::Hang).await;
    let budget = Duration::from_millis(50);
    let check =
        ClusterReadiness::new(Arc::clone(&registry), ["orders", "billing"]).with_budget(budget);

    // Bounded well under the framework's 500 ms so a regression fails here rather
    // than hanging the suite.
    let result = tokio::time::timeout(Duration::from_millis(400), check.check())
        .await
        .expect("the check returns rather than hanging");
    assert_eq!(
        result.status,
        HealthcheckStatus::Degraded,
        "a hung probe is Degraded, not the Unhealthy the framework would infer"
    );
    let message = result.message.expect("the hung profile is named");
    assert!(message.contains("billing") && !message.contains("orders"));
    handle.stop().await;
}

#[tokio::test]
async fn health_moves_without_bumping_the_generation() {
    // Why health is a cell on the profile and not a value in the snapshot: a
    // client reads `generation` to detect that the profile *set* changed, so
    // routing a flapping backend through `publish` would look like continuous
    // reconfiguration and invalidate every cached descriptor on every flap.
    let (handle, registry, _) = published(&[], &["billing"], ProbeBehaviour::Fail).await;
    let check = check_for(&registry, &["billing"]);
    let before = registry.generation();

    assert_eq!(check.check().await.status, HealthcheckStatus::Degraded);

    assert_eq!(
        registry.generation(),
        before,
        "a health change must not bump the registry generation"
    );
    handle.stop().await;
}

#[tokio::test]
async fn a_profile_returns_to_serving_without_a_restart() {
    // The recovery half of the per-profile gate: a degraded profile must come back
    // on a later poll, with no republish and no process restart.
    let flaky = Arc::new(FlakyProbe {
        fail: std::sync::atomic::AtomicBool::new(true),
    });
    let (handle, bound) = wire_with_cache(Arc::clone(&flaky) as Arc<dyn ClusterCacheBackend>).await;
    let registry = Arc::new(ProfileRegistry::new());
    registry.publish(bound);
    let check = check_for(&registry, &["orders"]);

    assert_eq!(check.check().await.status, HealthcheckStatus::Degraded);
    assert_eq!(
        registry.snapshot().profiles["orders"].descriptor().health,
        ProfileHealth::Degraded
    );

    flaky.fail.store(false, Ordering::SeqCst);
    assert_eq!(check.check().await.status, HealthcheckStatus::Healthy);
    assert_eq!(
        registry.snapshot().profiles["orders"].descriptor().health,
        ProfileHealth::Serving,
        "recovery is observable on the descriptor without a republish"
    );
    handle.stop().await;
}

#[tokio::test]
async fn one_instance_is_probed_once_per_round() {
    // Dedup is by instance, not by (profile, primitive): the lock and
    // leader-election defaults ride the profile's own cache, and probing that
    // cache three times per round would triple readiness traffic against every
    // deployed backend for no extra information.
    let (handle, registry, probes) = published(&[], &["orders"], ProbeBehaviour::Ok).await;
    let check = check_for(&registry, &["orders"]);

    assert_eq!(check.check().await.status, HealthcheckStatus::Healthy);
    assert_eq!(
        probes.load(Ordering::SeqCst),
        1,
        "the profile's single cache instance is probed once, not once per primitive"
    );
    handle.stop().await;
}

// ---------------------------------------------------------------------------
// A cache that can be flipped between failing and serving
// ---------------------------------------------------------------------------

struct FlakyProbe {
    fail: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl ClusterCacheBackend for FlakyProbe {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(false)
    }

    fn provider_name(&self) -> &'static str {
        "flaky-probe"
    }

    async fn get(&self, _key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        ProbeCache::unreachable()
    }

    async fn put(&self, _req: PutRequest<'_>) -> Result<(), ClusterError> {
        ProbeCache::unreachable()
    }

    async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
        ProbeCache::unreachable()
    }

    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        ProbeCache::unreachable()
    }

    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        ProbeCache::unreachable()
    }

    async fn compare_and_swap(
        &self,
        _key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        ProbeCache::unreachable()
    }

    async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
        ProbeCache::unreachable()
    }

    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        ProbeCache::unreachable()
    }

    async fn probe(&self) -> Result<(), ClusterError> {
        if self.fail.load(Ordering::SeqCst) {
            Err(ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message: "backend down".to_owned(),
            })
        } else {
            Ok(())
        }
    }
}

/// Wires a single `orders` profile over `cache`, through the programmatic builder
/// so a caller-owned backend instance can be flipped mid-test.
async fn wire_with_cache(
    cache: Arc<dyn ClusterCacheBackend>,
) -> (ClusterHandle, Vec<Arc<BoundProfile>>) {
    struct Fixed(Arc<dyn ClusterCacheBackend>);
    #[async_trait]
    impl ClusterCacheProvider for Fixed {
        fn provider(&self) -> &'static str {
            "flaky-probe"
        }
        async fn build_cache(
            &self,
            _options: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<(Arc<dyn ClusterCacheBackend>, StopHook), ClusterError> {
            // Wrapped as a real plugin wraps it, so the decorator is in the path
            // here too.
            let cache: Arc<dyn ClusterCacheBackend> = Arc::new(InstrumentedCache::new(
                Arc::clone(&self.0),
                "flaky-probe",
                Arc::new(NoopMetrics),
            ));
            Ok((cache, Box::new(|| Box::pin(async {}))))
        }
    }

    let cfg: ClusterConfig =
        serde_saphyr::from_str("profiles:\n  orders:\n    cache: { provider: flaky-probe }\n")
            .expect("config parses");
    let providers = ProviderRegistry::new().with_cache_provider(Arc::new(Fixed(cache)));
    ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
        .await
        .expect("wiring starts")
}

// ---------------------------------------------------------------------------
// `/readyz` — the framework's mapping and its exact bodies
// ---------------------------------------------------------------------------

/// A `ReadinessState` with `check` registered, startup marked complete and no
/// critical deps, so the only input to the verdict is cluster's own health.
///
/// A fresh registry per call on purpose: `RestHealthcheckRegistry` caches its
/// report for two seconds, so reusing one across states would assert the first
/// verdict three times.
fn readiness_over(check: ClusterReadiness) -> Arc<ReadinessState> {
    let registry = Arc::new(RestHealthcheckRegistry::new());
    registry.register("cluster", Arc::new(check));
    let state = ReadinessState::with_check_timeout(
        Vec::<String>::new(),
        registry,
        // The framework's own default, so the budget relationship under test is
        // the real one.
        toolkit::runtime::DEFAULT_HEALTHCHECK_TIMEOUT,
    );
    state.mark_startup_complete();
    state
}

/// The `/readyz` body, as the probe endpoint serialises it.
async fn readyz_body(state: &ReadinessState) -> String {
    serde_json::to_string(&state.evaluate().await).expect("the report serializes")
}

#[tokio::test]
async fn readyz_reports_starting_before_start_publishes() {
    let registry = Arc::new(ProfileRegistry::new());
    let state = readiness_over(check_for(&registry, &["orders"]));

    let report = state.evaluate().await;
    assert_eq!(report.state, ReadinessLifecycle::Starting);
    assert!(!report.ready);
    // `unresolved_deps` is omitted when empty, so this is the whole body.
    assert_eq!(
        readyz_body(&state).await,
        r#"{"state":"starting","ready":false}"#
    );
}

#[tokio::test]
async fn readyz_reports_ready_when_every_profile_probes() {
    let (handle, registry, _) = published(&["orders"], &[], ProbeBehaviour::Ok).await;
    let state = readiness_over(check_for(&registry, &["orders"]));

    let report = state.evaluate().await;
    assert_eq!(report.state, ReadinessLifecycle::Ready);
    assert!(report.ready);
    assert_eq!(
        readyz_body(&state).await,
        r#"{"state":"ready","ready":true}"#
    );
    handle.stop().await;
}

#[tokio::test]
async fn readyz_reports_degraded_and_stays_in_rotation_on_one_bad_profile() {
    // The end-to-end statement of the verdict: 200 and `ready: true`, so the pod
    // keeps serving the healthy profile - and keeps answering `DescribeProfiles`
    // for the degraded one.
    let (handle, registry, _) = published(&["orders"], &["billing"], ProbeBehaviour::Fail).await;
    let state = readiness_over(check_for(&registry, &["orders", "billing"]));

    let report = state.evaluate().await;
    assert_eq!(report.state, ReadinessLifecycle::Degraded);
    assert!(
        report.ready,
        "Degraded keeps the pod in rotation - that is the point of choosing it"
    );
    assert_eq!(
        readyz_body(&state).await,
        r#"{"state":"degraded","ready":true}"#
    );
    handle.stop().await;
}

#[tokio::test]
async fn readyz_reports_draining_regardless_of_health() {
    // Draining is the framework's, not cluster's, and it outranks every health
    // verdict (DESIGN section 4.8).
    let (handle, registry, _) = published(&["orders"], &[], ProbeBehaviour::Ok).await;
    let state = readiness_over(check_for(&registry, &["orders"]));
    state.set_draining(true);

    let report = state.evaluate().await;
    assert_eq!(report.state, ReadinessLifecycle::Draining);
    assert!(!report.ready);
    assert_eq!(
        readyz_body(&state).await,
        r#"{"state":"draining","ready":false}"#
    );
    handle.stop().await;
}

#[test]
fn the_probe_budget_leaves_the_framework_timeout_room() {
    // The relationship the budget depends on. If the framework's default ever
    // drops to or below the budget, its timeout fires first and reports
    // `Unhealthy`, silently inverting the verdict this module is built around.
    assert!(
        super::READINESS_PROBE_BUDGET < toolkit::runtime::DEFAULT_HEALTHCHECK_TIMEOUT,
        "the probe budget must expire before the framework's per-check timeout"
    );
}

#[tokio::test]
async fn the_check_name_is_an_explicit_id() {
    // Exposed verbatim on `/health`, so it must not be a type path.
    let registry = Arc::new(ProfileRegistry::new());
    let check = check_for(&registry, &[]);
    assert_eq!(check.name(), "cluster-readiness");
}

#[test]
fn a_healthy_result_carries_no_message() {
    // Guards the shape the `/health` component report is built from.
    let result = HealthcheckResult::healthy();
    assert_eq!(result.status, HealthcheckStatus::Healthy);
    assert!(result.message.is_none());
}
