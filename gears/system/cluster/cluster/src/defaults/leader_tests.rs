use std::sync::Arc;
use std::time::Duration;

use super::CasBasedLeaderElectionBackend;
use crate::defaults::ShutdownRevoke;
use crate::defaults::test_cache::MemoryCache;
use cluster_sdk::cache::ClusterCacheBackend;
use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{LeaderElectionBackend, LeaderStatus, LeaderWatchEvent};

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn new_rejects_eventually_consistent_cache() {
    let cache = MemoryCache::eventually_consistent();
    assert!(matches!(
        CasBasedLeaderElectionBackend::new(cache),
        Err(ClusterError::InvalidConfig { .. })
    ));
}

#[tokio::test]
async fn new_accepts_linearizable_cache() {
    let cache = MemoryCache::linearizable();
    assert!(CasBasedLeaderElectionBackend::new(cache).is_ok());
}

#[tokio::test]
async fn weak_consistency_constructor_always_succeeds_and_features_track_cache() {
    let weak = CasBasedLeaderElectionBackend::new_allow_weak_consistency(
        MemoryCache::eventually_consistent(),
    );
    assert!(!weak.features().linearizable);
    let strong =
        CasBasedLeaderElectionBackend::new_allow_weak_consistency(MemoryCache::linearizable());
    assert!(strong.features().linearizable);
}

#[tokio::test]
async fn graceful_shutdown_revokes_leader_then_closes_terminally() {
    let cache = MemoryCache::linearizable();
    let Ok(backend) = CasBasedLeaderElectionBackend::new(cache) else {
        panic!("linearizable cache must construct");
    };
    let Ok(mut watch) = backend.elect("primary").await else {
        panic!("election must join");
    };
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(watch.is_leader());

    // Graceful cluster shutdown. `revoke` awaits the election task's revocation
    // emit, so the leader has observed loss by the time it returns.
    backend.revoke().await;

    // Loss is observed before the terminal close (cpt-cf-clst-fr-shutdown-revoke),
    // and the synchronous snapshot no longer reports leadership.
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Lost)
    ));
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
    assert_eq!(watch.status(), LeaderStatus::Lost);
    assert!(!watch.is_leader());
}

#[tokio::test]
async fn single_candidate_becomes_leader() {
    let cache = MemoryCache::linearizable();
    let Ok(backend) = CasBasedLeaderElectionBackend::new(cache) else {
        panic!("linearizable cache must construct");
    };
    let Ok(mut watch) = backend.elect("primary").await else {
        panic!("election must join");
    };
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(watch.is_leader());
}

#[tokio::test]
async fn second_candidate_is_follower() {
    let cache = MemoryCache::linearizable();
    let Ok(a) = CasBasedLeaderElectionBackend::new(Arc::clone(&cache) as _) else {
        panic!("construct a");
    };
    let Ok(b) = CasBasedLeaderElectionBackend::new(cache as _) else {
        panic!("construct b");
    };
    let Ok(mut watch_a) = a.elect("primary").await else {
        panic!("a joins");
    };
    assert!(matches!(
        watch_a.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    let Ok(mut watch_b) = b.elect("primary").await else {
        panic!("b joins");
    };
    assert!(matches!(
        watch_b.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Follower)
    ));
    assert!(!watch_b.is_leader());
}

#[tokio::test]
async fn foreign_takeover_emits_lost_then_resolves() {
    let cache = MemoryCache::linearizable();
    let Ok(backend) = CasBasedLeaderElectionBackend::new(Arc::clone(&cache) as _) else {
        panic!("construct");
    };
    let Ok(mut watch) = backend.elect("primary").await else {
        panic!("join");
    };
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    // A foreign holder overwrites the claim — split-brain takeover.
    assert!(
        cache
            .put(PutRequest {
                key: "election/primary",
                value: b"intruder",
                ttl: Ttl::Of(Duration::from_secs(30)),
            })
            .await
            .is_ok()
    );
    // The watch observes the loss, then resolves to follower.
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Lost)
    ));
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Follower)
    ));
    // The intruder leaves; the watch auto-reenrolls back to leader. Bounded so a
    // re-election regression fails the test fast instead of hanging CI.
    assert!(cache.delete("election/primary").await.is_ok());
    let reenroll = async {
        loop {
            match watch.changed().await {
                LeaderWatchEvent::Status(LeaderStatus::Leader) => break,
                LeaderWatchEvent::Status(_) | LeaderWatchEvent::Reset => {}
                other => panic!("unexpected event while reenrolling: {other:?}"),
            }
        }
    };
    if tokio::time::timeout(Duration::from_secs(5), reenroll)
        .await
        .is_err()
    {
        panic!("timed out waiting for re-enrollment to leader");
    }
}

#[tokio::test]
async fn resign_releases_the_claim() {
    let cache = MemoryCache::linearizable();
    let Ok(backend) = CasBasedLeaderElectionBackend::new(Arc::clone(&cache) as _) else {
        panic!("construct");
    };
    let Ok(mut watch) = backend.elect("primary").await else {
        panic!("join");
    };
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(watch.resign().await.is_ok());
    settle().await;
    let Ok(after) = cache.get("election/primary").await else {
        panic!("get must succeed");
    };
    assert!(after.is_none(), "resign must release the claim");
}

#[tokio::test]
async fn dropping_watch_releases_claim_best_effort() {
    let cache = MemoryCache::linearizable();
    let Ok(backend) = CasBasedLeaderElectionBackend::new(Arc::clone(&cache) as _) else {
        panic!("construct");
    };
    let Ok(mut watch) = backend.elect("primary").await else {
        panic!("join");
    };
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    drop(watch);
    settle().await;
    let Ok(after) = cache.get("election/primary").await else {
        panic!("get must succeed");
    };
    assert!(
        after.is_none(),
        "dropping the watch best-effort releases the claim"
    );
}

#[tokio::test]
async fn compare_and_delete_is_guarded_by_value_not_version() {
    // The primitive the guarded release (`release_if_holder`) relies on, exercised
    // against the spurious-flap race: an owner re-claims a key after a successor
    // already took it over, so both claims sit at version 1 (a fresh
    // `put_if_absent` resets the version). A value guard distinguishes them where
    // a version guard would alias and wipe the successor's claim.
    let cache = MemoryCache::linearizable();
    // Owner A claims (fresh entry, version 1).
    assert!(matches!(
        cache
            .put_if_absent(PutRequest {
                key: "k",
                value: b"owner-a",
                ttl: Ttl::Indefinite,
            })
            .await,
        Ok(Some(_))
    ));
    // A's claim lapses and successor B re-claims — also a fresh entry at version 1.
    assert!(cache.delete("k").await.is_ok());
    assert!(matches!(
        cache
            .put_if_absent(PutRequest {
                key: "k",
                value: b"owner-b",
                ttl: Ttl::Indefinite,
            })
            .await,
        Ok(Some(_))
    ));

    // A's late release must NOT wipe B's claim: the value no longer matches.
    let Ok(deleted) = cache.compare_and_delete("k", b"owner-a").await else {
        panic!("compare_and_delete must succeed");
    };
    assert!(
        !deleted,
        "a value mismatch must not delete the successor's claim"
    );
    let Ok(Some(entry)) = cache.get("k").await else {
        panic!("the successor's claim must survive");
    };
    assert_eq!(entry.value, b"owner-b".to_vec());

    // B releasing its own claim deletes it.
    let Ok(deleted) = cache.compare_and_delete("k", b"owner-b").await else {
        panic!("compare_and_delete must succeed");
    };
    assert!(deleted, "the matching owner must delete its own claim");
    assert!(matches!(cache.get("k").await, Ok(None)));
}

#[tokio::test(start_paused = true)]
async fn renewal_extends_the_lease_beyond_the_initial_ttl() {
    let cache = MemoryCache::linearizable();
    let Ok(backend) = CasBasedLeaderElectionBackend::new(Arc::clone(&cache) as _) else {
        panic!("construct");
    };
    // Default config: ttl 30s, renewal interval 10s.
    let Ok(mut watch) = backend.elect("primary").await else {
        panic!("join");
    };
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    // Advance well past the initial 30s TTL in renewal-sized steps; the
    // renewal CAS must keep extending the lease so leadership never lapses.
    for _ in 0..6 {
        tokio::time::advance(Duration::from_secs(11)).await;
        settle().await;
    }
    assert!(
        watch.is_leader(),
        "renewal must preserve leadership past the initial TTL"
    );
    let Ok(entry) = cache.get("election/primary").await else {
        panic!("get must succeed");
    };
    assert!(entry.is_some(), "the renewed claim must still be present");
}
