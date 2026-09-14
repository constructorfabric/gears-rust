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

use std::sync::Arc;

use cluster_sdk::ClusterCacheV1;
use cluster_sdk::cache::{CacheCapability, CacheConsistency, PutRequest, Ttl};
use cluster_sdk::error::ClusterError;
use cluster_sdk::profile::ClusterProfile;
use common::{MemCacheBackend, cache_profile, wire};
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
    // Each profile binds its own, independent backend - wired together, since one
    // cluster client serves every profile in the process.
    let hub = Arc::new(ClientHub::new());
    let handle = wire(
        &hub,
        vec![
            (
                PrimaryProfile::NAME,
                cache_profile(MemCacheBackend::linearizable()),
            ),
            (
                AnalyticsProfile::NAME,
                cache_profile(MemCacheBackend::eventually_consistent()),
            ),
        ],
    )?;

    // The primary profile requires linearizable CAS; the analytics profile does
    // not, so it resolves against the weaker backend without a mismatch.
    let primary = ClusterCacheV1::resolver(&hub)
        .profile(PrimaryProfile)
        .require(CacheCapability::Linearizable)
        .resolve()
        .await?;
    let analytics = ClusterCacheV1::resolver(&hub)
        .profile(AnalyticsProfile)
        .resolve()
        .await?;

    // Write the same key in each profile; the namespaces are independent.
    primary
        .put(PutRequest {
            key: "cursor",
            value: b"primary-1",
            ttl: Ttl::Indefinite,
        })
        .await?;
    analytics
        .put(PutRequest {
            key: "cursor",
            value: b"analytics-1",
            ttl: Ttl::Indefinite,
        })
        .await?;

    // Each profile observes only its own value — no cross-talk.
    print_cursor("primary", &primary).await?;
    print_cursor("analytics", &analytics).await?;

    println!(
        "primary consistency={}, analytics consistency={}",
        consistency_label(&primary),
        consistency_label(&analytics)
    );

    handle.stop().await;
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
