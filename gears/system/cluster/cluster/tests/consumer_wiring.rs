//! `K3`'s central exit criterion: **a consumer resolving in its own `start` finds
//! the cluster client already in the hub** (DESIGN.md).
//!
//! This is the first test in the tree that drives a *consumer* rather than the
//! cluster gear, and it is deliberately shaped like a Profile 3 process: it links
//! `cluster-sdk` (through the `cluster` crate, whose lib brings the SDK) but names
//! **no** `cluster` item, so the cluster gear's `inventory` entry is dropped and no
//! `LocalClusterClient` is ever registered. That is what makes cluster-sdk's
//! `ConsumerRegistration` take its remote branch — the branch a deployed consumer
//! takes, and one no other test reaches.
//!
//! Concern 3 of the plan says every consumer-facing property is asserted only by
//! cluster's own tests until a real gear consumes cluster. This narrows that: the
//! *wiring* half is now asserted by a consumer, in a host runtime, through the real
//! proxy-wiring phase.
//!
//! # What it does not do
//!
//! No cluster server exists here, and none is wanted: the point is the **ordering
//! and the presence of the client**, not a round trip. The derived endpoint names a
//! Kubernetes DNS name that does not resolve, `connect_lazy` performs no I/O, and
//! `resolve()` therefore defers descriptor validation rather than failing — which is
//! precisely §4.7.1's cold-start behaviour and invariant I6's guarantee that startup
//! never blocks on cluster reachability. `T1` and `D4` are where both sides of a
//! socket meet.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::{ClusterCacheV1, ClusterClient, ClusterError, ClusterProfile};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use toolkit::runtime::{DbOptions, RunOptions, ShutdownOptions, run};
use toolkit::{ConfigProvider, GearCtx};

/// A namespace that is syntactically valid and resolves nowhere.
const NS: &str = "platform-test";

// The consumer under test

#[derive(Clone, Copy)]
struct ReservationsProfile;
impl ClusterProfile for ReservationsProfile {
    const NAME: &'static str = "reservations";
}
// The consumer's whole cluster-facing surface, per §4.9.2: this line, plus the
// `.profile()` call in `start` below. No wiring call, no endpoint, no mode flag.
cluster_sdk::register_cluster_profile!(ReservationsProfile);

/// What the consumer observed during its own `start`, read back after the runtime
/// returns. Statics rather than gear fields because `#[toolkit::gear]` builds the
/// gear itself and the test never holds the instance.
static CLIENT_WAS_ALREADY_WIRED: AtomicBool = AtomicBool::new(false);
static RESOLVE_SUCCEEDED: AtomicBool = AtomicBool::new(false);
static RESOLVE_WAS_A_PROXY: AtomicBool = AtomicBool::new(false);
/// Set last, so the test knows the observations above are complete and it is safe
/// to cancel. Cancelling *before* `run` would make the runtime return `Cancelled`
/// without executing a phase, which is how the first version of this test managed
/// to assert nothing.
static START_COMPLETED: AtomicBool = AtomicBool::new(false);

/// A minimal consumer: a `resolve()` in `start`, never in `init` (§4.9.1), and
/// **no `deps = [cluster]`**.
///
/// That omission is not a shortcut, and it is the correction §4.9.2 needs. `deps`
/// is a *hard* topo-sort edge: `RegistryBuilder` fails the whole registry build with
/// `RegistryError::UnknownDependency` when a named gear is not linked
/// (`registry.rs:565-568`), and a Profile 3 consumer by definition does not link the
/// cluster gear. So `deps = [cluster]` is not merely unnecessary here — it makes the
/// process refuse to start, which is how this was found.
///
/// Nothing is lost by omitting it, because neither thing `deps` would buy depends on
/// it:
///
/// - **Ordering.** `run_start_phase` iterates `gears_by_system_priority()`
///   (`host_runtime.rs:838`), which runs every `system`-capability gear before every
///   other one (`registry.rs:290-304`). Cluster declares `system`, so its `start`
///   precedes any application consumer's `start` in Profile 1 without any `deps`
///   edge at all.
/// - **Readiness gating.** The `/readyz` dependency on `cluster` comes from
///   cluster-sdk's `ConsumerRegistration::dep_gear`, which the wiring phase feeds to
///   the `DependencyChecker` — so a consumer gets the gate in *both* profiles, and
///   gets it without writing anything.
#[toolkit::gear(name = "reservations", capabilities = [stateful])]
#[derive(Default)]
struct Reservations {
    hub: std::sync::OnceLock<Arc<ClientHub>>,
}

#[async_trait]
impl toolkit::Gear for Reservations {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Captured, not resolved. §4.9.1 forbids resolving here: the framework's
        // phases are global, so no provider's `start` has run yet in either
        // deployment profile.
        drop(self.hub.set(ctx.client_hub()));
        Ok(())
    }
}

#[async_trait]
impl toolkit::contracts::RunnableCapability for Reservations {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        let hub = self.hub.get().expect("init ran").as_ref();

        // 1. The criterion: the client is *already* there. The proxy-wiring phase
        //    runs between `init` and `start`, so a consumer never has to wait for
        //    it and never writes a retry loop (ADR-0005).
        CLIENT_WAS_ALREADY_WIRED.store(
            hub.try_get::<dyn ClusterClient>().is_some(),
            Ordering::SeqCst,
        );
        // 2. And it is the *remote* one, because this process hosts no cluster gear.
        RESOLVE_WAS_A_PROXY.store(
            hub.has_remote_proxy::<dyn ClusterClient>(),
            Ordering::SeqCst,
        );

        // 3. Resolving succeeds even though cluster is unreachable: the only await
        //    is the bounded descriptor, and a descriptor that does not arrive defers
        //    validation to readiness rather than failing `start` (invariant I6).
        let resolved = ClusterCacheV1::resolver(hub)
            .profile(ReservationsProfile)
            .resolve()
            .await;
        RESOLVE_SUCCEEDED.store(resolved.is_ok(), Ordering::SeqCst);

        // 4. A call against it terminates with the *typed, retryable* error §6.10
        //    specifies - not a panic, not a hang, and not a flattened opaque one.
        //
        //    The window is deliberately generous, and the reason is a measurement
        //    worth carrying: this took **~8 s** on a developer machine, against a
        //    `CONNECT_TIMEOUT` of 5 s, because no per-call deadline is set (§6.10 -
        //    `Lock` waits server-side and a watch must carry no timeout) so what
        //    bounds the call is the connector's own DNS failure plus its backoff.
        //    A consumer's request handler absorbs that latency on the first call
        //    after cluster becomes unreachable. That is exactly the gap §12.9's
        //    `PolicyStack` default unary deadline closes, and it is #4084's to
        //    supply - so the number is pinned here rather than left to be
        //    rediscovered under load.
        if let Ok(cache) = resolved {
            // A fail-fast backstop, not a measurement: the call itself is bounded
            // by the connector's DNS failure plus backoff (~8s observed). One
            // minute is generous room over that so a slow CI box cannot turn
            // latency into an `Err(Elapsed)` the match below reads as a *wrong
            // result*.
            let outcome = tokio::time::timeout(Duration::from_mins(1), cache.get("seat/12")).await;
            match outcome {
                Ok(Err(ClusterError::Provider { kind, .. })) => {
                    assert_eq!(
                        kind,
                        cluster_sdk::ProviderErrorKind::ConnectionLost,
                        "an unreachable endpoint must decode as ConnectionLost, which is \
                         retryable - a consumer's auto-restart depends on it (section 6.10)"
                    );
                }
                other => panic!(
                    "an unreachable cluster must yield Provider{{ConnectionLost}}, got {other:?}"
                ),
            }
        }
        START_COMPLETED.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self, _deadline: CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
}

// Fixture

/// No gear needs configuration here; the consumer reads none and there is no
/// cluster gear in this process to configure.
struct NoConfig;

impl ConfigProvider for NoConfig {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        None
    }
}

// The test

/// One test, because the four properties above are four observations of one
/// lifecycle and there is only one lifecycle to run in a process.
#[tokio::test(flavor = "multi_thread")]
async fn a_consumer_resolving_in_start_finds_the_wired_remote_client() {
    // Assert the premise before relying on it: no cluster gear in this process, so
    // the wiring must take its remote branch. If a future edit pulled `cluster`
    // into this target's link graph, every assertion below would still pass - for
    // the wrong reason - because the local client would satisfy them.
    let registry = toolkit::registry::GearRegistry::discover_and_build().expect("registry builds");
    let names: Vec<&str> = registry
        .gears()
        .iter()
        .map(toolkit::registry::GearEntry::name)
        .collect();
    assert!(
        names.contains(&"reservations"),
        "the consumer gear must be discovered; got {names:?}"
    );
    assert!(
        !names.contains(&"cluster"),
        "this target must NOT link the cluster gear - it is the Profile 3 shape, and a \
         co-located gear would make the wiring report Local; got {names:?}"
    );

    let cancel = CancellationToken::new();
    // `RunOptions` is `#[non_exhaustive]` upstream: build via `new` + `with_*`.
    // The four `new` args are the required fields; empty `clients`, no `oop` and
    // the default shutdown deadline are exactly what `new` already sets.
    let options = RunOptions::new(
        Arc::new(NoConfig),
        DbOptions::None,
        ShutdownOptions::Token(cancel.clone()),
        uuid::Uuid::new_v4(),
    );

    // `POD_NAMESPACE` is what `derive_endpoint` reads, and it is set only for the
    // duration of the run. A deployed pod gets it from the downward API.
    temp_env::async_with_vars(
        [(cluster_sdk::wiring::POD_NAMESPACE_ENV, Some(NS))],
        async {
            let lifecycle = tokio::spawn(run(options));

            // Wait for `start` to finish its observations, then cancel. Cancelling
            // first would return `Cancelled` from `run` before any phase executed.
            // Strictly larger than the inner `cache.get` backstop (one minute)
            // above: `START_COMPLETED` is set right after that call returns, so
            // this outer wait must dominate it rather than race it to the deadline.
            // The `is_finished` guard below still fails fast if the lifecycle returns.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
            while !START_COMPLETED.load(Ordering::SeqCst) {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the consumer's `start` never completed"
                );
                assert!(
                    !lifecycle.is_finished(),
                    "the lifecycle returned before the consumer's `start` completed"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            cancel.cancel();

            tokio::time::timeout(Duration::from_secs(30), lifecycle)
                .await
                .expect("the lifecycle should return promptly after cancellation")
                .expect("the lifecycle task should not panic")
                .expect("the consumer lifecycle should complete");
        },
    )
    .await;

    assert!(
        CLIENT_WAS_ALREADY_WIRED.load(Ordering::SeqCst),
        "the proxy-wiring phase must register `dyn ClusterClient` before any consumer's \
         `start`, so a consumer never writes wiring or retry code (ADR-0005, DESIGN 4.9.3)"
    );
    assert!(
        RESOLVE_WAS_A_PROXY.load(Ordering::SeqCst),
        "with no co-located cluster gear the wiring must have taken its remote branch"
    );
    assert!(
        RESOLVE_SUCCEEDED.load(Ordering::SeqCst),
        "resolve() must succeed against an unreachable cluster - startup never blocks on \
         cluster reachability (invariant I6)"
    );
}
