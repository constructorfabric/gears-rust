//! Showcase: single-primitive usage.
//!
//! The minimal consumer shape — bind one backend under a typed profile, resolve
//! one primitive (the cache) with the capabilities the workload needs, and use
//! it: versioned writes, atomic create, compare-and-swap, and a reactive watch.
//!
//! Run with: `cargo run --example single_primitive`

mod common;

use std::sync::Arc;

use cluster_sdk::cache::{
    CacheCapability, CacheConsistency, CacheEvent, CacheWatchEvent, ClusterCacheV1, PutRequest, Ttl,
};
use cluster_sdk::error::ClusterError;
use cluster_sdk::profile::ClusterProfile;
use common::{MemCacheBackend, cache_profile, wire};
use toolkit::client_hub::ClientHub;

/// The typed profile this app binds its cluster backend under. The marker
/// removes magic-string profile names from resolution sites.
#[derive(Clone, Copy)]
struct AppProfile;

impl ClusterProfile for AppProfile {
    const NAME: &'static str = "app";
}

// Declare the marker to the process. One line, at module scope, and it is the
// *entire* difference between this file and the same file in a deployed consumer:
// it tells cluster's consumer wiring and readiness contributor which profiles this
// process expects, so a profile the cluster gear does not bind is named at startup
// rather than at the first coordination call.
//
// It is not a prerequisite for resolving - every `resolve()` below would work
// without it. Omitting it costs the diagnostic, not the function.
cluster_sdk::register_cluster_profile!(AppProfile);

#[tokio::main]
async fn main() -> Result<(), ClusterError> {
    // 1. Wire a backend under the profile. In production the gear does this from
    //    operator config; here we wire the in-memory fixture. The wiring is also
    //    what registers the process's cluster client, which is what a facade
    //    resolves through.
    let hub = Arc::new(ClientHub::new());
    let handle = wire(
        &hub,
        vec![(
            AppProfile::NAME,
            cache_profile(MemCacheBackend::linearizable()),
        )],
    )?;

    // 2. Resolve the cache, declaring the capabilities the workload requires.
    //    Resolution fails fast (at startup) if the bound backend cannot meet them
    //    — see the `capability_mismatch` example.
    let cache = ClusterCacheV1::resolver(&hub)
        .profile(AppProfile)
        .require(CacheCapability::Linearizable)
        .require(CacheCapability::PrefixWatch)
        .resolve()
        .await?;
    let consistency = match cache.consistency() {
        CacheConsistency::Linearizable => "linearizable",
        CacheConsistency::EventuallyConsistent => "eventually-consistent",
        // `CacheConsistency` is `#[non_exhaustive]`; tolerate future classes.
        _ => "other",
    };
    println!(
        "resolved cache: consistency={consistency}, prefix_watch={}",
        cache.features().prefix_watch
    );

    // 3. Versioned writes and reads.
    cache
        .put(PutRequest {
            key: "config/region",
            value: b"us-east",
            ttl: Ttl::Indefinite,
        })
        .await?;
    if let Some(entry) = cache.get("config/region").await? {
        let value = String::from_utf8_lossy(&entry.value);
        println!("config/region = {value} (version {})", entry.version);
    }

    // 4. Atomic create-if-absent, then a version-checked compare-and-swap — the
    //    universal CAS building block every other primitive is built on.
    let Some(created) = cache
        .put_if_absent(PutRequest {
            key: "counter",
            value: b"1",
            ttl: Ttl::Indefinite,
        })
        .await?
    else {
        return Err(ClusterError::InvalidConfig {
            reason: "counter unexpectedly already present in a fresh cache".to_owned(),
        });
    };
    let swapped = cache
        .compare_and_swap("counter", created.version, b"2", Ttl::Indefinite)
        .await?;
    println!(
        "counter advanced version {} -> {}",
        created.version, swapped.version
    );

    // 5. A reactive watch: subscribe, then observe the next mutation as a
    //    lightweight key-only event (the consumer re-reads the value via `get`).
    let mut watch = cache.watch("config/region").await?;
    cache
        .put(PutRequest {
            key: "config/region",
            value: b"eu-west",
            ttl: Ttl::Indefinite,
        })
        .await?;
    if let Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) = watch.recv().await {
        let current = cache.get(&key).await?;
        let value = current.map_or_else(
            || "<deleted>".to_owned(),
            |entry| String::from_utf8_lossy(&entry.value).into_owned(),
        );
        println!("watch observed change to {key}; current value = {value}");
    }

    handle.stop().await;

    Ok(())
}
