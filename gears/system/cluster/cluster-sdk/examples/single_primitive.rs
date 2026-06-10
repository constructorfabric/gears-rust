// Created: 2026-06-11 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-showcase-audit-examples:p1
//! Showcase: single-primitive usage.
//!
//! The minimal consumer shape — bind one backend under a typed profile, resolve
//! one primitive (the cache) with the capabilities the workload needs, and use
//! it: versioned writes, atomic create, compare-and-swap, and a reactive watch.
//!
//! Run with: `cargo run --example single_primitive`

mod common;

use cluster_sdk::cache::{
    CacheCapability, CacheConsistency, CacheEvent, CacheWatchEvent, ClusterCacheV1,
};
use cluster_sdk::error::ClusterError;
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::registration::register_cache_backend;
use common::MemCacheBackend;
use toolkit::client_hub::ClientHub;

/// The typed profile this app binds its cluster backend under. The marker
/// removes magic-string profile names from resolution sites.
#[derive(Clone, Copy)]
struct AppProfile;

impl ClusterProfile for AppProfile {
    const NAME: &'static str = "app";
}

#[tokio::main]
async fn main() -> Result<(), ClusterError> {
    // 1. Bind a backend under the profile. In production the wiring crate does
    //    this from operator config; here we register the in-memory fixture.
    let hub = ClientHub::new();
    register_cache_backend(&hub, AppProfile::NAME, MemCacheBackend::linearizable())?;

    // 2. Resolve the cache, declaring the capabilities the workload requires.
    //    Resolution fails fast (at startup) if the bound backend cannot meet them
    //    — see the `capability_mismatch` example.
    // @cpt-begin:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-single
    let cache = ClusterCacheV1::resolver(&hub)
        .profile(AppProfile)
        .require(CacheCapability::Linearizable)
        .require(CacheCapability::PrefixWatch)
        .resolve()?;
    // @cpt-end:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-single
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
    cache.put("config/region", b"us-east", None).await?;
    if let Some(entry) = cache.get("config/region").await? {
        let value = String::from_utf8_lossy(&entry.value);
        println!("config/region = {value} (version {})", entry.version);
    }

    // 4. Atomic create-if-absent, then a version-checked compare-and-swap — the
    //    universal CAS building block every other primitive is built on.
    let Some(created) = cache.put_if_absent("counter", b"1", None).await? else {
        return Err(ClusterError::InvalidConfig {
            reason: "counter unexpectedly already present in a fresh cache".to_owned(),
        });
    };
    let swapped = cache
        .compare_and_swap("counter", created.version, b"2", None)
        .await?;
    println!(
        "counter advanced version {} -> {}",
        created.version, swapped.version
    );

    // 5. A reactive watch: subscribe, then observe the next mutation as a
    //    lightweight key-only event (the consumer re-reads the value via `get`).
    let mut watch = cache.watch("config/region").await?;
    cache.put("config/region", b"eu-west", None).await?;
    if let Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) = watch.recv().await {
        // @cpt-begin:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-reread
        // @cpt-begin:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-recover
        let current = cache.get(&key).await?;
        // @cpt-end:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-recover
        // @cpt-end:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-reread
        let value = current.map_or_else(
            || "<deleted>".to_owned(),
            |entry| String::from_utf8_lossy(&entry.value).into_owned(),
        );
        println!("watch observed change to {key}; current value = {value}");
    }

    Ok(())
}
