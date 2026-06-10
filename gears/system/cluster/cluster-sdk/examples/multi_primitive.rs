// Created: 2026-06-11 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-showcase-audit-examples:p1
//! Showcase: multi-primitive usage over a single backend.
//!
//! One cache backend, bound under one profile, yields all four coordination
//! primitives — cache, leader election, distributed lock, and service discovery
//! — via the SDK default backends (`CasBased*` / `CacheBased*`). This is the
//! "implement cache only, get all four primitives" guarantee in action.
//!
//! Run with: `cargo run --example multi_primitive`

mod common;

use std::collections::HashMap;
use std::time::Duration;

use cluster_sdk::discovery::{DiscoveryFilter, ServiceRegistration};
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{LeaderStatus, LeaderWatch, LeaderWatchEvent};
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::{ClusterCacheV1, DistributedLockV1, LeaderElectionV1, ServiceDiscoveryV1};
use common::{MemCacheBackend, register_cache_and_siblings};
use toolkit::client_hub::ClientHub;

/// The single profile all four primitives resolve under.
#[derive(Clone, Copy)]
struct AppProfile;

impl ClusterProfile for AppProfile {
    const NAME: &'static str = "app";
}

#[tokio::main]
async fn main() -> Result<(), ClusterError> {
    // Bind one cache plus its three derived default backends under the profile.
    let hub = ClientHub::new();
    register_cache_and_siblings(&hub, AppProfile::NAME, MemCacheBackend::linearizable())?;

    // @cpt-begin:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-multi
    cache_demo(&hub).await?;
    leader_demo(&hub).await?;
    lock_demo(&hub).await?;
    discovery_demo(&hub).await?;
    // @cpt-end:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-multi
    Ok(())
}

/// Shared state behind a versioned key.
async fn cache_demo(hub: &ClientHub) -> Result<(), ClusterError> {
    let cache = ClusterCacheV1::resolver(hub)
        .profile(AppProfile)
        .resolve()?;
    cache.put("epoch", b"0", None).await?;
    println!("[cache] stored epoch=0");
    Ok(())
}

/// Single-leader election: one candidate enrolls and observes itself as leader.
async fn leader_demo(hub: &ClientHub) -> Result<(), ClusterError> {
    let leader = LeaderElectionV1::resolver(hub)
        .profile(AppProfile)
        .resolve()?;
    // @cpt-begin:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-join
    let mut watch = leader.elect("scheduler").await?;
    // @cpt-end:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-join
    match first_status(&mut watch).await? {
        // @cpt-begin:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-resolved
        // @cpt-begin:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-resume
        LeaderStatus::Leader => println!("[leader] this node is the scheduler leader"),
        LeaderStatus::Follower => println!("[leader] another node leads; this node follows"),
        // @cpt-end:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-resume
        // @cpt-end:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-resolved
        // @cpt-begin:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-lost
        LeaderStatus::Lost => println!("[leader] leadership lost (transient)"),
        // @cpt-end:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-lost
    }
    // Step down gracefully so the claim is released promptly.
    watch.resign().await?;
    println!("[leader] resigned");
    Ok(())
}

/// Awaits the watch's first leadership status, skipping non-status signals.
/// Bounded by a timeout so the example never hangs if no status arrives.
async fn first_status(watch: &mut LeaderWatch) -> Result<LeaderStatus, ClusterError> {
    let deadline = Duration::from_secs(5);
    let wait = async {
        // @cpt-begin:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-observe
        loop {
            match watch.changed().await {
                LeaderWatchEvent::Status(status) => return Ok(status),
                LeaderWatchEvent::Closed(err) => return Err(err),
                // Lagged / Reset: keep waiting for the next status.
                _ => {}
            }
        }
        // @cpt-end:cpt-cf-clst-flow-leader-election-participate-observe:p1:inst-le-observe
    };
    match tokio::time::timeout(deadline, wait).await {
        Ok(result) => result,
        Err(_elapsed) => Err(ClusterError::InvalidConfig {
            reason: "no leadership status within the demo deadline".to_owned(),
        }),
    }
}

/// TTL-bounded mutual exclusion: acquire, do local-only work, release.
async fn lock_demo(hub: &ClientHub) -> Result<(), ClusterError> {
    let lock = DistributedLockV1::resolver(hub)
        .profile(AppProfile)
        .resolve()?;
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-try
    let guard = lock
        .try_lock("rebuild-index", Duration::from_secs(30))
        .await?;
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-try
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-local
    println!("[lock] acquired '{}'", guard.name());
    // Critical-section rule (ADR-002): no remote I/O while holding the guard —
    // only local, bounded work belongs here.
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-local
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-release
    guard.release().await?;
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-release
    println!("[lock] released 'rebuild-index'");
    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-return
    Ok(())
    // @cpt-end:cpt-cf-clst-flow-distributed-lock-try-critical:p1:inst-tc-return
}

/// Register an instance, discover it, then deregister via the handle.
async fn discovery_demo(hub: &ClientHub) -> Result<(), ClusterError> {
    let discovery = ServiceDiscoveryV1::resolver(hub)
        .profile(AppProfile)
        .resolve()?;

    let mut metadata = HashMap::new();
    metadata.insert("region".to_owned(), "us-east".to_owned());
    // @cpt-begin:cpt-cf-clst-flow-service-discovery-register:p1:inst-rg-submit
    let handle = discovery
        .register(ServiceRegistration {
            name: "api".to_owned(),
            instance_id: None,
            address: "10.0.0.1:8080".to_owned(),
            metadata,
        })
        .await?;
    // @cpt-end:cpt-cf-clst-flow-service-discovery-register:p1:inst-rg-submit
    println!("[discovery] registered instance {}", handle.instance_id());

    // @cpt-begin:cpt-cf-clst-flow-service-discovery-discover:p1:inst-ds-call
    // @cpt-begin:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-reread
    let instances = discovery
        .discover("api", DiscoveryFilter::default())
        .await?;
    // @cpt-end:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-reread
    // @cpt-end:cpt-cf-clst-flow-service-discovery-discover:p1:inst-ds-call
    // @cpt-begin:cpt-cf-clst-flow-service-discovery-discover:p1:inst-ds-order
    // @cpt-begin:cpt-cf-clst-flow-service-discovery-discover:p1:inst-ds-sort
    for instance in &instances {
        println!(
            "[discovery] discovered {} at {}",
            instance.instance_id, instance.address
        );
    }
    // @cpt-end:cpt-cf-clst-flow-service-discovery-discover:p1:inst-ds-sort
    // @cpt-end:cpt-cf-clst-flow-service-discovery-discover:p1:inst-ds-order

    handle.deregister().await?;
    println!("[discovery] deregistered");
    Ok(())
}
