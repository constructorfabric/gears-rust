// Created: 2026-06-11 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-showcase-audit-examples:p1
//! Showcase: multi-profile usage.
//!
//! A single process can coordinate against more than one cluster by binding a
//! distinct backend under each typed profile. Profiles are independent
//! namespaces: each resolves its own backend with its own characteristics, and
//! coordination state never crosses between them.
//!
//! Here a `primary` profile is backed by a linearizable cache (for
//! correctness-sensitive coordination) and an `analytics` profile by an
//! eventually-consistent cache (cheaper, best-effort). Each is resolved with the
//! capabilities appropriate to its backend.
//!
//! Run with: `cargo run --example multi_profile`

mod common;

use cluster_sdk::ClusterCacheV1;
use cluster_sdk::cache::{CacheCapability, CacheConsistency};
use cluster_sdk::error::ClusterError;
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::registration::register_cache_backend;
use common::MemCacheBackend;
use toolkit::client_hub::ClientHub;

/// Correctness-sensitive coordination — bound to a linearizable backend.
#[derive(Clone, Copy)]
struct PrimaryProfile;

impl ClusterProfile for PrimaryProfile {
    const NAME: &'static str = "primary";
}

/// Best-effort analytics coordination — bound to an eventually-consistent
/// backend that is cheaper but cannot promise linearizable CAS.
#[derive(Clone, Copy)]
struct AnalyticsProfile;

impl ClusterProfile for AnalyticsProfile {
    const NAME: &'static str = "analytics";
}

#[tokio::main]
async fn main() -> Result<(), ClusterError> {
    // Each profile binds its own, independent backend.
    let hub = ClientHub::new();
    register_cache_backend(&hub, PrimaryProfile::NAME, MemCacheBackend::linearizable())?;
    register_cache_backend(
        &hub,
        AnalyticsProfile::NAME,
        MemCacheBackend::eventually_consistent(),
    )?;

    // The primary profile requires linearizable CAS; the analytics profile does
    // not, so it resolves against the weaker backend without a mismatch.
    let primary = ClusterCacheV1::resolver(&hub)
        .profile(PrimaryProfile)
        .require(CacheCapability::Linearizable)
        .resolve()?;
    let analytics = ClusterCacheV1::resolver(&hub)
        .profile(AnalyticsProfile)
        .resolve()?;

    // Write the same key in each profile; the namespaces are independent.
    primary.put("cursor", b"primary-1", None).await?;
    analytics.put("cursor", b"analytics-1", None).await?;

    // Each profile observes only its own value — no cross-talk.
    print_cursor("primary", &primary).await?;
    print_cursor("analytics", &analytics).await?;

    println!(
        "primary consistency={}, analytics consistency={}",
        consistency_label(&primary),
        consistency_label(&analytics)
    );
    Ok(())
}

/// A human-readable label for a cache's declared consistency class.
fn consistency_label(cache: &ClusterCacheV1) -> &'static str {
    match cache.consistency() {
        CacheConsistency::Linearizable => "linearizable",
        CacheConsistency::EventuallyConsistent => "eventually-consistent",
        // `CacheConsistency` is `#[non_exhaustive]`; tolerate future classes.
        _ => "other",
    }
}

/// Reads and prints the `cursor` key for a resolved profile cache.
async fn print_cursor(label: &str, cache: &ClusterCacheV1) -> Result<(), ClusterError> {
    let value = cache.get("cursor").await?.map_or_else(
        || "<absent>".to_owned(),
        |entry| String::from_utf8_lossy(&entry.value).into_owned(),
    );
    println!("[{label}] cursor = {value}");
    Ok(())
}
