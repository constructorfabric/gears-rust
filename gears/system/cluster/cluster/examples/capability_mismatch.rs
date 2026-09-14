//! Showcase: capability-mismatch startup failure and its resolution.
//!
//! A consumer declares the capabilities its workload needs at the resolution
//! site. If the bound backend cannot meet a declared capability, resolution
//! fails with a [`ClusterError::CapabilityNotMet`] that names the primitive, the
//! unmet capability, and the concrete provider — not a subtle runtime
//! correctness bug at the first operation, but an obvious configuration error.
//!
//! The fix is operational: bind a backend that satisfies the requirement.
//!
//! # Where the failure surfaces
//!
//! **In this example, and in every in-process deployment, it surfaces right here
//! at `resolve().await`.** The bound object *is* the real backend, so what it
//! declares is known immediately and validation is inline. That is the only case
//! an embedded consumer has.
//!
//! Against a *remote* cluster the guarantee is the same and the delivery is not
//! always. `resolve()` awaits the profile's descriptor under a bounded timeout; if
//! cluster is not reachable in time — a platform cold start, where cluster and its
//! consumers come up together — `resolve()` returns `Ok` and validation is deferred
//! to the SDK's readiness contributor, which reports the *same* triple and holds
//! the pod at 503. What never happens is a consumer serving traffic against an
//! unmet requirement.
//!
//! The consequence worth knowing before you write the `match` below: **a consumer
//! that branches on `CapabilityNotMet` is correct in the inline case and silently
//! skipped in the deferred one.** The fallback arm does not run; the pod stays
//! not-ready instead. Which path a startup took is logged at `info`.
//!
//! This is also why `resolve()` is `async`: awaiting a descriptor is I/O, and a
//! sync signature could not do it. It is the only SDK signature the remote model
//! changes, and it costs a consumer one `.await` — facades are resolved in a
//! gear's `start`, which is already `async fn`.
//!
//! Run with: `cargo run --example capability_mismatch`

mod common;

use cluster_sdk::ClusterCacheV1;
use std::sync::Arc;

use cluster_sdk::cache::CacheCapability;
use cluster_sdk::error::ClusterError;
use cluster_sdk::profile::ClusterProfile;
use common::{MemCacheBackend, cache_profile, wire};
use toolkit::client_hub::ClientHub;

/// The profile whose cache must provide linearizable CAS.
#[derive(Clone, Copy)]
struct AppProfile;

impl ClusterProfile for AppProfile {
    const NAME: &'static str = "app";
}

#[tokio::main]
async fn main() -> Result<(), ClusterError> {
    show_mismatch().await;
    show_resolution().await?;
    Ok(())
}

/// Bind an eventually-consistent cache, then require linearizable CAS — the
/// requirement is unmet, so resolution fails at startup with a precise error.
async fn show_mismatch() {
    let hub = Arc::new(ClientHub::new());
    // A misconfiguration: an eventually-consistent backend wired where the
    // workload needs linearizable CAS.
    let Ok(handle) = wire(
        &hub,
        vec![(
            AppProfile::NAME,
            cache_profile(MemCacheBackend::eventually_consistent()),
        )],
    ) else {
        println!("[mismatch] unexpected wiring failure");
        return;
    };

    let outcome = ClusterCacheV1::resolver(&hub)
        .profile(AppProfile)
        .require(CacheCapability::Linearizable)
        .resolve()
        .await;

    match outcome {
        Ok(_) => println!("[mismatch] unexpectedly resolved against a weaker backend"),
        Err(ClusterError::CapabilityNotMet {
            primitive,
            capability,
            provider,
        }) => {
            // The error names exactly what to fix and where.
            println!(
                "[mismatch] startup failed: {primitive} requires capability \
                 '{capability}', but the bound provider '{provider}' does not \
                 declare it"
            );
            println!("[mismatch] fix: bind a backend whose consistency is linearizable");
        }
        Err(other) => println!("[mismatch] resolution failed with another error: {other}"),
    }

    handle.stop().await;
}

/// The corrected binding: a linearizable backend meets the requirement, so the
/// same resolution succeeds.
async fn show_resolution() -> Result<(), ClusterError> {
    let hub = Arc::new(ClientHub::new());
    let handle = wire(
        &hub,
        vec![(
            AppProfile::NAME,
            cache_profile(MemCacheBackend::linearizable()),
        )],
    )?;

    let cache = ClusterCacheV1::resolver(&hub)
        .profile(AppProfile)
        .require(CacheCapability::Linearizable)
        .resolve()
        .await?;
    println!(
        "[resolved] cache resolved; linearizable requirement met (prefix_watch={})",
        cache.features().prefix_watch
    );

    handle.stop().await;
    Ok(())
}
