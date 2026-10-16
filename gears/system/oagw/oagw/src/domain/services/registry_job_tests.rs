//! The background task under a paused Tokio clock, on in-memory repositories
//! and the in-memory registry-state store (real database I/O under a paused
//! clock fires timeouts spuriously).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tenant_resolver_sdk::TenantId;
use uuid::Uuid;

use super::*;
use crate::domain::error::DomainError;
use crate::domain::model::GrpcMatch;
use crate::domain::repo::{RegistryState, RegistryStateStore, RepositoryError};
use crate::domain::services::ControlPlaneService;
use crate::domain::services::registry_reconcile::provisioning_ctx;
use crate::domain::services::registry_reconcile::tests::{in_memory_planes, registry_instances};
use crate::domain::services::registry_state::{Gate, LEASE_POLL, LEASE_TTL, content_hash};
use crate::domain::test_support::{MockTenantResolverClient, ProvisioningMetrics};
use crate::domain::type_provisioning::{ProvisionedRoute, ProvisionedUpstream};
use crate::infra::storage::registry_state_in_memory::InMemoryRegistryStateStore;

const RECHECK: Duration = Duration::from_mins(5);
/// Just past one re-check (the harness waits exactly, without jitter).
const PAST_RECHECK: Duration = Duration::from_secs(5 * 60 + 1);

/// Registry content that can change between runs, and fail on demand.
#[derive(Default)]
struct Registry {
    content: Mutex<(Vec<ProvisionedUpstream>, Vec<ProvisionedRoute>)>,
    failures_left: AtomicUsize,
    panics_left: AtomicUsize,
    hangs: std::sync::atomic::AtomicBool,
    listings: AtomicUsize,
}

#[async_trait]
impl TypeProvisioningService for Registry {
    async fn list_upstreams(&self) -> Result<Vec<ProvisionedUpstream>, DomainError> {
        self.listings.fetch_add(1, Ordering::SeqCst);
        if self.hangs.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self
            .panics_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            panic!("types-registry client bug");
        }
        if self
            .failures_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(DomainError::internal("types-registry unavailable"));
        }
        Ok(self.content.lock().unwrap().0.clone())
    }
    async fn list_routes(&self) -> Result<Vec<ProvisionedRoute>, DomainError> {
        Ok(self.content.lock().unwrap().1.clone())
    }
}

/// The in-memory state row, counting reads and writes; `on_renew` is
/// cancelled at every renewal.
struct Counted {
    inner: InMemoryRegistryStateStore,
    reads: AtomicUsize,
    writes: AtomicUsize,
    on_renew: Option<CancellationToken>,
}

impl Counted {
    fn new() -> Self {
        Self {
            inner: InMemoryRegistryStateStore::new(),
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            on_renew: None,
        }
    }

    fn counts(&self) -> (usize, usize) {
        (
            self.reads.load(Ordering::SeqCst),
            self.writes.load(Ordering::SeqCst),
        )
    }
}

#[async_trait]
impl RegistryStateStore for Counted {
    async fn read(&self) -> Result<RegistryState, RepositoryError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read().await
    }
    async fn claim(&self, generation: u64) -> Result<bool, RepositoryError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.claim(generation).await
    }
    async fn try_acquire(
        &self,
        owner: Uuid,
        ttl: Duration,
        generation: Option<u64>,
    ) -> Result<bool, RepositoryError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.try_acquire(owner, ttl, generation).await
    }
    async fn renew(&self, owner: Uuid, ttl: Duration) -> Result<bool, RepositoryError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if let Some(token) = &self.on_renew {
            token.cancel();
        }
        self.inner.renew(owner, ttl).await
    }
    async fn finish(&self, owner: Uuid, hash: Option<&str>) -> Result<bool, RepositoryError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.finish(owner, hash).await
    }
}

struct Harness {
    root: Uuid,
    registry: Arc<Registry>,
    state: Arc<Counted>,
    cp: Arc<crate::domain::services::ControlPlaneServiceImpl>,
    metrics: Arc<ProvisioningMetrics>,
    job: RegistryJob,
    status: ProvisioningState,
}

impl Harness {
    fn new(generation: Option<u64>, state: Counted) -> Self {
        let root = Uuid::new_v4();
        let registry = Arc::new(Registry::default());
        *registry.content.lock().unwrap() = registry_instances(root);
        let state = Arc::new(state);
        let (cp, provisioner) = in_memory_planes();
        let metrics = Arc::new(ProvisioningMetrics::default());
        let job = RegistryJob {
            cp: provisioner,
            provisioning: registry.clone(),
            tenant_resolver: Arc::new(MockTenantResolverClient::with_siblings(
                TenantId(root),
                vec![],
            )),
            metrics: metrics.clone(),
            gate: Some(Gate {
                state: state.clone(),
                owner: Uuid::new_v4(),
                generation,
                lease_ttl: LEASE_TTL,
                poll: LEASE_POLL,
                jitter: |d| d,
            }),
            // Exact waits, so each re-check lands in its own window.
            jitter: |d| d,
        };
        Self {
            root,
            registry,
            state,
            cp,
            metrics,
            job,
            status: ProvisioningState::default(),
        }
    }

    /// The run outcomes recorded so far, in order.
    fn runs(&self) -> Vec<String> {
        self.metrics.runs.lock().unwrap().clone()
    }

    fn start(&self, cancel: &CancellationToken) -> tokio::task::JoinHandle<()> {
        tokio::spawn(run_registry_loop(
            self.job.clone(),
            RECHECK,
            self.status.clone(),
            cancel.clone(),
        ))
    }

    async fn tags(&self) -> Vec<String> {
        let id = self.registry.content.lock().unwrap().0[0]
            .request
            .id
            .unwrap();
        self.cp
            .get_upstream(&provisioning_ctx(self.root).unwrap(), id)
            .await
            .map(|u| u.tags)
            .unwrap_or_default()
    }

    fn hash(&self) -> String {
        let (upstreams, routes) = self.registry.content.lock().unwrap().clone();
        content_hash(&upstreams, &routes, self.root)
    }
}

/// Lets the spawned task run up to its next sleep.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// The jitter stays within ±20% and spreads across that range: 1,000 draws
/// reach both below -10% and above +10%.
///
/// ADR-0018: PERS-LOOP-004.
#[test]
fn jitter_stays_within_twenty_percent() {
    let waits: Vec<Duration> = (0..1_000).map(|_| jitter(RECHECK)).collect();
    for wait in &waits {
        assert!(
            *wait >= RECHECK.mul_f64(0.8) && *wait <= RECHECK.mul_f64(1.2),
            "{wait:?}"
        );
    }
    let (low, high) = (waits.iter().min().unwrap(), waits.iter().max().unwrap());
    assert!(
        *low < RECHECK.mul_f64(0.9) && *high > RECHECK.mul_f64(1.1),
        "{low:?}..{high:?}"
    );
}

/// ADR-0018: PERS-LOOP-004.
#[test]
fn backoff_doubles_from_one_second_to_five_minutes_and_resets() {
    let mut backoff = Backoff::new();
    let waits: Vec<u64> = (0..12).map(|_| backoff.next().as_secs()).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300, 300]);
    backoff.reset();
    assert_eq!(backoff.next(), BACKOFF_MIN);
}

/// After the first run the task checks again every interval, forever: an
/// unchanged state costs one read and no write.
///
/// ADR-0018: PERS-LOOP-001.
#[tokio::test(start_paused = true)]
async fn the_task_rechecks_every_interval_with_one_read() {
    let h = Harness::new(None, Counted::new());
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);
    settle().await;
    assert_eq!(h.status.get(), ProvisioningStatus::Settled);
    assert_eq!(
        h.state.inner.read().await.unwrap().content_hash,
        Some(h.hash())
    );

    for _ in 0..3 {
        let before = h.state.counts();
        tokio::time::sleep(PAST_RECHECK).await;
        let after = h.state.counts();
        assert_eq!((after.0 - before.0, after.1 - before.1), (1, 0));
    }
    assert!(!task.is_finished(), "the task never ends on its own");
    cancel.cancel();
    task.await.unwrap();
}

/// A state another writer changed (an older pod without a generation) is
/// reconciled again at the next check.
#[tokio::test(start_paused = true)]
async fn a_reverted_state_is_reconciled_at_the_next_check() {
    let h = Harness::new(None, Counted::new());
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);
    settle().await;
    h.state.inner.set_content_hash(Some("someone-else"));
    let writes = h.state.counts().1;

    tokio::time::sleep(PAST_RECHECK).await;
    assert!(h.state.counts().1 > writes, "the run took the lease again");
    assert_eq!(
        h.state.inner.read().await.unwrap().content_hash,
        Some(h.hash())
    );
    assert_eq!(h.status.get(), ProvisioningStatus::Settled);
    cancel.cancel();
    task.await.unwrap();
}

/// A newer generation owns the registry: this pod's changed content is not
/// applied, and nothing is left for it to do.
///
/// ADR-0018: PERS-LOOP-002.
#[tokio::test(start_paused = true)]
async fn an_older_generation_does_not_apply_its_content() {
    let h = Harness::new(Some(1), Counted::new());
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);
    settle().await;
    assert!(h.state.inner.claim(2).await.unwrap());
    h.registry.content.lock().unwrap().0[0].request.tags = vec!["old-release".into()];
    let writes = h.state.counts().1;

    tokio::time::sleep(PAST_RECHECK).await;
    assert_eq!(h.state.counts().1, writes, "a stale pod writes nothing");
    assert!(h.tags().await.is_empty());
    assert_eq!(h.status.get(), ProvisioningStatus::Settled);
    assert_eq!(h.runs(), ["clean", "stale"]);
    cancel.cancel();
    task.await.unwrap();
}

/// A run that skips an invalid instance is not clean: the pod reports it
/// and checks again every interval, and settles once the content is fixed.
///
/// ADR-0018: PERS-LOOP-003.
#[tokio::test(start_paused = true)]
async fn an_invalid_instance_is_not_clean_until_its_content_is_fixed() {
    let h = Harness::new(None, Counted::new());
    // A route matches by exactly one of HTTP and gRPC.
    let set_grpc = |grpc: Option<GrpcMatch>| {
        h.registry.content.lock().unwrap().1[0]
            .request
            .match_rules
            .grpc = grpc;
    };
    set_grpc(Some(GrpcMatch {
        service: "x.v1.Svc".into(),
        method: "Call".into(),
    }));
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);
    settle().await;
    assert_eq!(h.status.get(), ProvisioningStatus::NotClean);
    assert_eq!(h.state.inner.read().await.unwrap().content_hash, None);

    set_grpc(None);
    tokio::time::sleep(PAST_RECHECK).await;
    assert_eq!(h.status.get(), ProvisioningStatus::Settled);
    assert_eq!(
        h.state.inner.read().await.unwrap().content_hash,
        Some(h.hash())
    );
    assert_eq!(h.runs(), ["not_clean", "clean"]);
    cancel.cancel();
    task.await.unwrap();
}

/// A run that fails on an infrastructure error is retried after 1 s, 2 s,
/// 4 s…; the pod keeps serving, and the status says the run failed.
///
/// ADR-0018: PERS-LOOP-004.
#[tokio::test(start_paused = true)]
async fn a_failing_run_is_retried_with_backoff() {
    let h = Harness::new(None, Counted::new());
    h.registry.failures_left.store(3, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);
    let attempts = || h.registry.listings.load(Ordering::SeqCst);

    settle().await;
    assert_eq!(
        (attempts(), h.status.get()),
        (1, ProvisioningStatus::Failed)
    );
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert_eq!(attempts(), 2, "retried after 1 s");
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    assert_eq!(attempts(), 3, "then after 2 s");
    assert_eq!(h.status.get(), ProvisioningStatus::Failed);
    tokio::time::sleep(Duration::from_millis(4_000)).await;
    assert_eq!(attempts(), 4, "then after 4 s");
    assert_eq!(h.status.get(), ProvisioningStatus::Settled);
    assert_eq!(h.runs(), ["failed", "failed", "failed", "clean"]);
    cancel.cancel();
    task.await.unwrap();
}

/// The task spreads both its waits with its jitter: under a fixed ×1.2, the
/// retry after 1 s comes at 1.2 s and the next check at 6 min.
///
/// ADR-0018: PERS-LOOP-004.
#[tokio::test(start_paused = true)]
async fn a_retry_is_jittered_around_its_backoff() {
    let mut h = Harness::new(None, Counted::new());
    h.job.jitter = |d| d.mul_f64(1.2);
    h.registry.failures_left.store(1, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);
    let attempts = || h.registry.listings.load(Ordering::SeqCst);

    settle().await;
    assert_eq!(attempts(), 1);
    tokio::time::sleep(Duration::from_millis(1_197)).await;
    assert_eq!(attempts(), 1, "not before 1.2 s");
    tokio::time::sleep(Duration::from_millis(3)).await;
    assert_eq!(attempts(), 2, "at 1.2 s");
    assert_eq!(h.status.get(), ProvisioningStatus::Settled);

    tokio::time::sleep(RECHECK.mul_f64(1.2) - Duration::from_millis(2)).await;
    assert_eq!(attempts(), 2, "not before 6 min");
    tokio::time::sleep(Duration::from_millis(3)).await;
    assert_eq!(attempts(), 3, "at 6 min");
    cancel.cancel();
    task.await.unwrap();
}

/// A dependency that never answers fails the run after the call timeout,
/// not before; the task retries, and settles once the dependency answers
/// again.
///
/// ADR-0018: PERS-LOOP-005.
#[tokio::test(start_paused = true)]
async fn a_hung_dependency_fails_the_run_after_the_call_timeout() {
    use crate::domain::services::registry_reconcile::CALL_TIMEOUT;

    let h = Harness::new(None, Counted::new());
    h.registry.hangs.store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);
    settle().await;
    assert_eq!(h.status.get(), ProvisioningStatus::Pending);

    tokio::time::sleep(CALL_TIMEOUT - Duration::from_millis(2)).await;
    assert_eq!(h.status.get(), ProvisioningStatus::Pending, "not before");
    tokio::time::sleep(Duration::from_millis(2)).await;
    settle().await;
    assert_eq!(h.status.get(), ProvisioningStatus::Failed);
    h.registry.hangs.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert_eq!(h.status.get(), ProvisioningStatus::Settled);
    cancel.cancel();
    task.await.unwrap();
}

/// A stop while a dependency hangs ends the task at once.
///
/// ADR-0018: PERS-LOOP-006.
#[tokio::test(start_paused = true)]
async fn a_stop_during_a_hung_call_ends_the_task() {
    let h = Harness::new(None, Counted::new());
    h.registry.hangs.store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);
    settle().await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_millis(10), task)
        .await
        .expect("the task does not wait for the hung call")
        .unwrap();
}

/// A run that panics is a failed run: recorded `Failed`, retried with
/// backoff, and the task lives on.
#[tokio::test(start_paused = true)]
async fn a_panicking_run_fails_and_is_retried() {
    let h = Harness::new(None, Counted::new());
    h.registry.panics_left.store(1, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);
    let attempts = || h.registry.listings.load(Ordering::SeqCst);

    settle().await;
    assert_eq!(
        (attempts(), h.status.get()),
        (1, ProvisioningStatus::Failed)
    );
    assert!(!task.is_finished(), "a panic does not end the task");
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert_eq!(attempts(), 2, "retried after 1 s");
    assert_eq!(h.status.get(), ProvisioningStatus::Settled);
    cancel.cancel();
    task.await.unwrap();
}

/// A stop that arrives mid-run ends the run at the next check between
/// writes, releases the lease, and ends the task.
///
/// ADR-0018: PERS-LOOP-006.
#[tokio::test(start_paused = true)]
async fn a_stop_mid_run_ends_between_writes_and_releases_the_lease() {
    let cancel = CancellationToken::new();
    let mut state = Counted::new();
    state.on_renew = Some(cancel.clone());
    let h = Harness::new(None, state);
    let task = h.start(&cancel);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("the task ends on stop")
        .unwrap();
    assert_eq!(h.state.inner.lease_owner(), None, "the lease is released");
    assert_eq!(h.state.inner.read().await.unwrap().content_hash, None);
    assert_eq!(h.status.get(), ProvisioningStatus::Pending);
}

/// A pod that lost the lease polls until the winner records the hash of
/// the same content, then has nothing to do.
///
/// ADR-0018: PERS-GATE-002.
#[tokio::test(start_paused = true)]
async fn a_losing_pod_polls_until_the_winners_hash_appears() {
    let h = Harness::new(None, Counted::new());
    let winner = Uuid::new_v4();
    assert!(
        h.state
            .inner
            .try_acquire(winner, LEASE_TTL, None)
            .await
            .unwrap()
    );
    let cancel = CancellationToken::new();
    let task = h.start(&cancel);

    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(h.status.get(), ProvisioningStatus::Pending, "still waiting");
    assert!(h.state.counts().0 >= 10, "polled about once a second");

    assert!(h.state.inner.finish(winner, Some(&h.hash())).await.unwrap());
    tokio::time::sleep(LEASE_POLL * 2).await;
    assert_eq!(h.status.get(), ProvisioningStatus::Settled);
    assert!(h.tags().await.is_empty() && h.state.inner.lease_owner().is_none());
    cancel.cancel();
    task.await.unwrap();
}
