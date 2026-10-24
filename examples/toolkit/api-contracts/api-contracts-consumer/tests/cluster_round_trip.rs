//! The status probe against a bound cache, a cache that never answers, and no
//! cluster at all. The cache is bound through `cluster-sdk`'s stub client, the
//! way a consumer's own tests should: no cluster gear is linked.

use std::sync::Arc;

use async_trait::async_trait;
use cf_api_contracts_consumer::cluster::AuditProfile;
use cf_api_contracts_consumer::domain::{ChargeProxyService, ROUND_TRIP_DEADLINE};
use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheFeatures, CacheWatch, PutRequest, Ttl,
};
use cluster_sdk::test_support::{StubClusterClient, unbound_leader_election, unbound_lock};
use cluster_sdk::{ClusterCacheBackend, ClusterCacheProvider as _, ClusterError, ClusterProfile};
use toolkit::ClientHub;

fn bind(hub: &ClientHub, cache: Arc<dyn ClusterCacheBackend>) {
    StubClusterClient::for_profile(AuditProfile::NAME)
        .with_cache(cache)
        .with_lock(unbound_lock(AuditProfile::NAME))
        .with_leader_election(unbound_leader_election(AuditProfile::NAME))
        .register(hub);
}

fn problem(err: toolkit_canonical_errors::CanonicalError) -> toolkit_canonical_errors::Problem {
    toolkit_canonical_errors::Problem::from(err)
}

#[tokio::test]
async fn the_probe_writes_then_reads_back_through_the_bound_cache() {
    let hub = Arc::new(ClientHub::new());
    let (cache, stop) = standalone_cluster_plugin::StandaloneCacheProvider
        .build_cache(&serde_json::Map::new())
        .await
        .expect("the standalone cache builds");
    bind(&hub, cache);
    let service = ChargeProxyService::new(Arc::clone(&hub));
    let tenant = uuid::Uuid::now_v7();

    let first = service
        .cluster_round_trip(tenant)
        .await
        .expect("round trip succeeds");
    let second = service
        .cluster_round_trip(tenant)
        .await
        .expect("round trip succeeds");
    assert!(second > first, "each write produces a newer version");

    stop().await;
}

/// A cache whose every call waits forever: a live server that stopped answering.
struct Silent;

#[async_trait]
impl ClusterCacheBackend for Silent {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }
    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(false)
    }
    async fn get(&self, _key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        std::future::pending().await
    }
    async fn put(&self, _req: PutRequest<'_>) -> Result<(), ClusterError> {
        std::future::pending().await
    }
    async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
        std::future::pending().await
    }
    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        std::future::pending().await
    }
    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        std::future::pending().await
    }
    async fn compare_and_swap(
        &self,
        _key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        std::future::pending().await
    }
    async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
        std::future::pending().await
    }
    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn a_cache_that_never_answers_is_unavailable_at_the_deadline() {
    let hub = Arc::new(ClientHub::new());
    bind(&hub, Arc::new(Silent));
    let service = ChargeProxyService::new(Arc::clone(&hub));
    let started = tokio::time::Instant::now();

    let err = service
        .cluster_round_trip(uuid::Uuid::now_v7())
        .await
        .expect_err("the cache never answers");

    assert_eq!(started.elapsed(), ROUND_TRIP_DEADLINE);
    let problem = problem(err);
    assert_eq!(problem.status, Some(503));
    assert_eq!(problem.detail, "cluster round trip timed out");
}

#[tokio::test]
async fn without_a_cluster_the_probe_is_unavailable_and_names_no_cause() {
    let service = ChargeProxyService::new(Arc::new(ClientHub::new()));
    let err = service
        .cluster_round_trip(uuid::Uuid::now_v7())
        .await
        .expect_err("nothing is wired");
    let problem = problem(err);
    assert_eq!(problem.status, Some(503));
    // Which step fails first depends on how lazily the facade resolves; what
    // matters is that the detail is the fixed step name, with no cause text.
    assert!(
        ["cluster cache not resolvable", "cluster put failed"].contains(&problem.detail.as_str()),
        "unexpected detail: {}",
        problem.detail
    );
}
