//! Showcase: multi-primitive usage over a single backend.
//!
//! One cache backend, bound under one profile, yields all three coordination
//! primitives — cache, leader election, and distributed lock — via the SDK
//! default backends (`CasBased*`). This is the "implement cache only, get all
//! three primitives" guarantee in action.
//!
//! Run with: `cargo run --example multi_primitive`

mod common;

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{LeaderStatus, LeaderWatch, LeaderWatchEvent};
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::{ClusterCacheV1, DistributedLockV1, LeaderElectionV1};
use common::{MemCacheBackend, cache_profile, wire};
use toolkit::client_hub::ClientHub;

/// The single profile all three primitives resolve under.
#[derive(Clone, Copy)]
struct AppProfile;

impl ClusterProfile for AppProfile {
    const NAME: &'static str = "app";
}

#[tokio::main]
async fn main() -> Result<(), ClusterError> {
    // Wire one cache; the omit-default auto-wrap supplies leader election and the
    // lock over it, and the wiring registers the cluster client consumers resolve
    // through.
    let hub = Arc::new(ClientHub::new());
    let handle = wire(
        &hub,
        vec![(
            AppProfile::NAME,
            cache_profile(MemCacheBackend::linearizable()),
        )],
    )?;

    cache_demo(&hub).await?;
    leader_demo(&hub).await?;
    lock_demo(&hub).await?;

    handle.stop().await;
    Ok(())
}

/// Shared state behind a versioned key.
async fn cache_demo(hub: &ClientHub) -> Result<(), ClusterError> {
    let cache = ClusterCacheV1::resolver(hub)
        .profile(AppProfile)
        .resolve()
        .await?;
    cache
        .put(PutRequest {
            key: "epoch",
            value: b"0",
            ttl: Ttl::Indefinite,
        })
        .await?;
    println!("[cache] stored epoch=0");
    Ok(())
}

/// Single-leader election: one candidate enrolls and observes itself as leader.
async fn leader_demo(hub: &ClientHub) -> Result<(), ClusterError> {
    let leader = LeaderElectionV1::resolver(hub)
        .profile(AppProfile)
        .resolve()
        .await?;
    let mut watch = leader.elect("scheduler").await?;
    match first_status(&mut watch).await? {
        LeaderStatus::Leader => println!("[leader] this node is the scheduler leader"),
        LeaderStatus::Follower => println!("[leader] another node leads; this node follows"),
        LeaderStatus::Lost => println!("[leader] leadership lost (transient)"),
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
        loop {
            match watch.changed().await {
                LeaderWatchEvent::Status(status) => return Ok(status),
                LeaderWatchEvent::Closed(err) => return Err(err),
                // Lagged / Reset: keep waiting for the next status.
                _ => {}
            }
        }
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
        .resolve()
        .await?;
    let guard = lock
        .try_lock("rebuild-index", Duration::from_secs(30))
        .await?;
    println!("[lock] acquired '{}'", guard.name());
    // Critical-section rule (ADR-002): no remote I/O while holding the guard —
    // only local, bounded work belongs here.
    guard.release().await?;
    println!("[lock] released 'rebuild-index'");
    Ok(())
}
