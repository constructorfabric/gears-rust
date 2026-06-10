// Created: 2026-06-11 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-showcase-audit-examples:p1
//! Showcase: capability-mismatch startup failure and its resolution.
//!
//! A consumer declares the capabilities its workload needs at the resolution
//! site. If the bound backend cannot meet a declared capability, resolution
//! fails **at startup** — not at the first operation — with a
//! [`ClusterError::CapabilityNotMet`] that names the primitive, the unmet
//! capability, and the concrete provider. This turns a subtle runtime
//! correctness bug into an obvious, immediate configuration error.
//!
//! The fix is operational: bind a backend that satisfies the requirement.
//!
//! Run with: `cargo run --example capability_mismatch`

mod common;

use cluster_sdk::ClusterCacheV1;
use cluster_sdk::cache::CacheCapability;
use cluster_sdk::error::ClusterError;
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::registration::register_cache_backend;
use common::MemCacheBackend;
use toolkit::client_hub::ClientHub;

/// The profile whose cache must provide linearizable CAS.
#[derive(Clone, Copy)]
struct AppProfile;

impl ClusterProfile for AppProfile {
    const NAME: &'static str = "app";
}

#[tokio::main]
async fn main() -> Result<(), ClusterError> {
    show_mismatch();
    show_resolution()?;
    Ok(())
}

/// Bind an eventually-consistent cache, then require linearizable CAS — the
/// requirement is unmet, so resolution fails at startup with a precise error.
fn show_mismatch() {
    let hub = ClientHub::new();
    // A misconfiguration: an eventually-consistent backend bound where the
    // workload needs linearizable CAS.
    let registered = register_cache_backend(
        &hub,
        AppProfile::NAME,
        MemCacheBackend::eventually_consistent(),
    );
    if registered.is_err() {
        println!("[mismatch] unexpected registration failure");
        return;
    }

    // @cpt-begin:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-unmet
    let outcome = ClusterCacheV1::resolver(&hub)
        .profile(AppProfile)
        .require(CacheCapability::Linearizable)
        .resolve();
    // @cpt-end:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-unmet

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
}

/// The corrected binding: a linearizable backend meets the requirement, so the
/// same resolution succeeds.
fn show_resolution() -> Result<(), ClusterError> {
    let hub = ClientHub::new();
    register_cache_backend(&hub, AppProfile::NAME, MemCacheBackend::linearizable())?;

    // @cpt-begin:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-fix
    let cache = ClusterCacheV1::resolver(&hub)
        .profile(AppProfile)
        .require(CacheCapability::Linearizable)
        .resolve()?;
    // @cpt-end:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-fix
    println!(
        "[resolved] cache resolved; linearizable requirement met (prefix_watch={})",
        cache.features().prefix_watch
    );
    Ok(())
}
