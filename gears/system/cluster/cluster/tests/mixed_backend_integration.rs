//! Layer 3 — the mixed-backend routing scenario for
//! `cpt-cf-clst-fr-routing-per-primitive` (PRD §5.5, UC-004).
//!
//! `cluster/src/config_tests.rs` covers the wiring contract with a fake native
//! lock provider: fast, no Docker, and it proves the routing dispatch. What it
//! cannot prove is that a *real* plugin-shipped native backend survives the same
//! path — that operator YAML naming two genuinely different, independently-built
//! backends in one profile produces four working primitives against live
//! infrastructure. That is this file's job.
//!
//! Two profiles are covered, and the distinction matters:
//!
//! 1. `mixed_backend_profile_wires_native_lock_over_a_postgres_cache` — both
//!    primitives name `provider: postgres`. This is *not* heterogeneous: same
//!    provider name, same database. What it proves is per-primitive **dispatch**,
//!    that an explicit `lock` binding resolves through the lock registry to the
//!    plugin's standalone `PostgresLockProvider` (its own dedicated pool, plugin
//!    DESIGN.md §3.5) rather than being auto-filled with the CAS default. It is
//!    also a realistic deployment: all-Postgres, but wanting real advisory locks
//!    instead of CAS-over-cache.
//! 2. `heterogeneous_profile_binds_a_standalone_cache_with_a_native_postgres_lock`
//!    — an in-process `standalone` cache with Postgres advisory locks: two
//!    different providers on two different infrastructures in one profile. This
//!    is the closer analogue of UC-004.
//!
//! In both, leader election stays on the omit-default
//! auto-wrap over that profile's cache, so each profile exercises both routing
//! modes at once.
//!
//! Neither test rests on mutual exclusion to prove routing — the CAS default and
//! the native backend both grant then contend. Each instead asserts that no
//! `lock/{name}` cache entry exists while the lock is held, which is how the CAS
//! default (and only the CAS default) records a holder. Both were verified to
//! fail with their `lock:` binding removed.
//!
//! These are the only pairings the shipped plugins permit today: `standalone`
//! provides a cache only, and `postgres` is the sole native non-cache provider.
//! The Redis / K8s / NATS / etcd combinations the PRD enumerates become testable
//! as those plugins land.
//!
//! Gated behind `--features integration` so a default `cargo test` never needs a
//! Docker daemon. Run with
//! `cargo test -p cf-gears-cluster --features integration`.

#![cfg(feature = "integration")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

use std::sync::Arc;
use std::time::Duration;

use cluster::{
    BoundProfile, ClusterConfig, ClusterHandle, ClusterWiring, ProfileRegistry, ProviderRegistry,
};
use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::{
    CacheCapability, ClusterCacheV1, ClusterError, ClusterProfile, DistributedLockV1,
    LeaderElectionV1,
};
use postgres_cluster_plugin::{PostgresCacheProvider, PostgresLockProvider};
use standalone_cluster_plugin::StandaloneCacheProvider;
use testcontainers::ContainerAsync;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use toolkit::client_hub::ClientHub;

/// The step the gear's `start` takes after `from_config`: publish the bound set
/// and register the local cluster client, which is what makes the profiles
/// resolvable in this process (DESIGN-DEPLOYABLE-GEAR §4.9.3).
fn publish(handle: &mut ClusterHandle, bound: Vec<Arc<BoundProfile>>) {
    handle.publish(&Arc::new(ProfileRegistry::new()), bound);
}

/// The profile the YAML fixture names.
#[derive(Clone, Copy)]
struct EventBroker;
impl ClusterProfile for EventBroker {
    const NAME: &'static str = "event-broker";
}

/// Starts a Postgres container and returns it with a connection string to its
/// mapped host port.
///
/// Retries the whole create -> `start()` -> `get_host_port_ipv4` sequence: under
/// parallel test load the container's wait strategy passes but the follow-up
/// Docker port-inspect transiently fails, and a fresh container per attempt
/// side-steps that flake. Mirrors the Postgres plugin's `tests/common` fixture,
/// which lives in that crate's test tree and so cannot be shared here (and the
/// plugin cannot depend on this crate — the dependency runs the other way).
async fn spawn_postgres() -> (ContainerAsync<Postgres>, String) {
    let backoffs = [
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_secs(1),
        Duration::from_secs(2),
    ];
    let attempts = backoffs.len() + 1;
    let mut last_error = String::new();
    for attempt in 1..=attempts {
        let container = match Postgres::default()
            .with_db_name("cluster_test")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) => {
                last_error = format!("container start: {error}");
                if let Some(backoff) = backoffs.get(attempt - 1) {
                    tokio::time::sleep(*backoff).await;
                }
                continue;
            }
        };
        match container.get_host_port_ipv4(5432).await {
            Ok(port) => {
                let connection_string =
                    format!("postgres://postgres:postgres@127.0.0.1:{port}/cluster_test");
                return (container, connection_string);
            }
            Err(error) => {
                last_error = format!("mapped host port: {error}");
                drop(container);
                if let Some(backoff) = backoffs.get(attempt - 1) {
                    tokio::time::sleep(*backoff).await;
                }
            }
        }
    }
    panic!(
        "Postgres container acquisition failed after {attempts} attempts; last error: {last_error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_backend_profile_wires_native_lock_over_a_postgres_cache() {
    let (_container, connection_string) = spawn_postgres().await;

    // One profile, two independently-built backends, plus two omit-default
    // primitives riding the cache. The `lock` binding carries its own options
    // (its own pool) exactly as an operator would write them.
    let yaml = format!(
        "
profiles:
  event-broker:
    cache:
      provider: postgres
      connection_string: {connection_string}
      pool_max_size: 5
    lock:
      provider: postgres
      connection_string: {connection_string}
      pool_max_size: 5
"
    );
    let cfg: ClusterConfig = serde_saphyr::from_str(&yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());
    let registry = ProviderRegistry::new()
        .with_cache_provider(Arc::new(PostgresCacheProvider))
        .with_lock_provider(Arc::new(PostgresLockProvider));

    let (mut handle, bound) = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &registry)
        .await
        .expect("a mixed-backend profile must wire against live Postgres");
    publish(&mut handle, bound);

    // --- All three primitives resolve under the single profile (UC-004's
    // "consumer gears see four working primitives" clause). Capability
    // validation is unchanged and still applies: requiring `Linearizable`
    // resolves only because the Postgres cache genuinely declares it
    // (`cpt-cf-clst-nfr-capability-validation`).
    let cache = ClusterCacheV1::resolver(&hub)
        .profile(EventBroker)
        .require(CacheCapability::Linearizable)
        .resolve()
        .await
        .expect("the natively-bound Postgres cache resolves as linearizable");
    let lock = DistributedLockV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .await
        .expect("the natively-bound Postgres lock resolves");
    assert!(
        LeaderElectionV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .await
            .is_ok(),
        "leader election still rides the omit-default auto-wrap over the cache"
    );

    // --- Both natively-bound backends actually work against the database.
    cache
        .put(PutRequest {
            key: "routing-probe",
            value: b"v",
            ttl: Ttl::Of(Duration::from_secs(30)),
        })
        .await
        .expect("the Postgres cache serves a write");
    let entry = cache
        .get("routing-probe")
        .await
        .expect("the Postgres cache serves a read");
    assert_eq!(
        entry.map(|found| found.value),
        Some(b"v".to_vec()),
        "the value round-trips through the natively-bound cache"
    );

    let guard = lock
        .try_lock("shard-assignment", Duration::from_secs(10))
        .await
        .expect("the natively-bound Postgres lock grants an uncontended lock");
    // Mutual exclusion holds while the guard is alive.
    assert!(
        matches!(
            lock.try_lock("shard-assignment", Duration::from_secs(10))
                .await,
            Err(ClusterError::LockContended { .. })
        ),
        "the held lock must report contention, not some other failure"
    );

    // The assertion that actually pins the *routing*. Mutual exclusion alone
    // proves nothing here: the CAS default layered over this same Postgres cache
    // would grant then contend identically, so this test passed even with the
    // `lock:` binding deleted until this check existed.
    //
    // The two implementations are distinguishable by where the holder lives.
    // `CasBasedDistributedLockBackend` represents a held lock as a cache entry at
    // `lock/{name}` (`defaults::LOCK_KEY_PREFIX`), whereas the native Postgres
    // backend takes a `pg_advisory_lock` and writes nothing to the cache. So with
    // the native backend bound, that key must be absent while the lock is held —
    // and its presence would mean the omit-default CAS lock is serving this
    // profile instead of the provider the operator named.
    let cas_holder_key = cache
        .get("lock/shard-assignment")
        .await
        .expect("probing the cache for a CAS-default lock entry must succeed");
    assert!(
        cas_holder_key.is_none(),
        "a `lock/` cache entry means the CAS default served this lock, so the \
         explicit `lock: {{ provider: postgres }}` binding was not routed to the \
         native backend"
    );

    drop(guard);

    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn heterogeneous_profile_binds_a_standalone_cache_with_a_native_postgres_lock() {
    // The genuinely heterogeneous shape, and the closer analogue of UC-004: two
    // *different* providers backed by two *different* infrastructures in one
    // profile — an in-process `standalone` cache alongside Postgres advisory
    // locks. The sibling test above binds `postgres` to both primitives, which
    // exercises per-primitive dispatch (the `lock` registry is consulted, not the
    // omit-default auto-wrap) but not heterogeneity: same provider name, same
    // database. This one crosses a real backend boundary.
    //
    // These two are the only heterogeneous pairing the shipped plugins allow
    // today — `standalone` provides a cache only, `postgres` is the sole native
    // non-cache provider. The Redis / K8s / NATS / etcd combinations the PRD
    // enumerates become testable as those plugins land.
    let (_container, connection_string) = spawn_postgres().await;

    let yaml = format!(
        "
profiles:
  event-broker:
    cache:
      provider: standalone
    lock:
      provider: postgres
      connection_string: {connection_string}
      pool_max_size: 5
"
    );
    let cfg: ClusterConfig = serde_saphyr::from_str(&yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());
    let registry = ProviderRegistry::new()
        .with_cache_provider(Arc::new(StandaloneCacheProvider))
        .with_lock_provider(Arc::new(PostgresLockProvider));

    let (mut handle, bound) = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &registry)
        .await
        .expect("a heterogeneous profile must wire");
    publish(&mut handle, bound);

    let cache = ClusterCacheV1::resolver(&hub)
        .profile(EventBroker)
        .require(CacheCapability::Linearizable)
        .resolve()
        .await
        .expect("the standalone cache resolves as linearizable");
    let lock = DistributedLockV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .await
        .expect("the Postgres lock resolves alongside a non-Postgres cache");
    assert!(
        LeaderElectionV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .await
            .is_ok(),
        "leader election rides the omit-default auto-wrap over the standalone cache"
    );

    // The cache is in-process and knows nothing of Postgres.
    cache
        .put(PutRequest {
            key: "routing-probe",
            value: b"v",
            ttl: Ttl::Of(Duration::from_secs(30)),
        })
        .await
        .expect("the standalone cache serves a write");
    assert_eq!(
        cache
            .get("routing-probe")
            .await
            .expect("the standalone cache serves a read")
            .map(|found| found.value),
        Some(b"v".to_vec()),
        "the value round-trips through the standalone cache"
    );

    // The lock reaches Postgres, across the backend boundary from that cache.
    let guard = lock
        .try_lock("shard-assignment", Duration::from_secs(10))
        .await
        .expect("the Postgres lock grants an uncontended lock");
    assert!(
        matches!(
            lock.try_lock("shard-assignment", Duration::from_secs(10))
                .await,
            Err(ClusterError::LockContended { .. })
        ),
        "the held Postgres lock must report contention"
    );

    // Same routing distinguisher as above, and here it also proves the two
    // primitives really are on separate infrastructure: had the `lock` binding
    // been dropped, the CAS default would have recorded its holder in *this*
    // in-memory standalone cache.
    assert!(
        cache
            .get("lock/shard-assignment")
            .await
            .expect("probing the standalone cache must succeed")
            .is_none(),
        "a `lock/` entry in the standalone cache means the CAS default served the \
         lock, so the `lock: {{ provider: postgres }}` binding was not routed"
    );

    drop(guard);

    handle.stop().await;
}
