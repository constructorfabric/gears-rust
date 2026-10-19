//! Feature 01 §3.3–§3.4 and §6: advisory leadership over `LeaderElectionV1` (ADR-0008) with
//! `FakeClusterLeaders`, and the exception sweeper it runs.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bss_rating::authz::ServiceGrant;
use bss_rating::domain::error::DomainError;
use bss_rating::domain::exception::{
    ExceptionKey, ExceptionRecord, ExceptionRetrier, ExceptionSweeper, SWEEP_BATCH, SubjectKind,
};
use bss_rating::domain::inbox::{AcceptOutcome, InboxTransition};
use bss_rating::domain::leadership::{LeaderWork, LeadershipName, LeaseProvider};
use bss_rating::domain::ports::TenantGrant;
use bss_rating::infra::cluster::{ClusterLeaseProvider, ElectionTiming, resolve_election};
use bss_rating::infra::storage::repo::exception_repo::OrmExceptionRepo;
use bss_rating::infra::storage::repo::inbox_repo;
use bss_rating::test_providers::{FakeClusterLeaders, FakePdp};
use cluster_sdk::ElectionConfig;
use common::{
    LeadersWiring, exception_key, exception_register, fact, fixture, migrated_db, record_exception,
};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use toolkit_db::Db;
use toolkit_security::AccessScope;
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0xA);
const SWEEP: &str = "bss-rating/exception-sweep";

/// The next event, or a failure after five seconds instead of a hung test.
async fn next<T>(events: &mut mpsc::UnboundedReceiver<T>) -> Option<T> {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("an event arrives in time")
}

/// A provider over `leaders`, as one gear instance resolves it from its own hub.
async fn provider(leaders: &FakeClusterLeaders) -> (ClusterLeaseProvider, LeadersWiring) {
    let hub = Arc::new(ClientHub::new());
    let wiring = LeadersWiring::bind(leaders, &hub);
    let election = resolve_election(&hub).await.unwrap();
    let provider = ClusterLeaseProvider::new(
        election,
        ElectionTiming::new(
            ElectionConfig::new(Duration::from_secs(30), 2).unwrap(),
            Duration::from_secs(5),
        ),
    );
    (provider, wiring)
}

#[tokio::test]
async fn leadership_is_resolved_through_ratings_profile_scoped_bss_rating() {
    let leaders = FakeClusterLeaders::new();
    let (provider, wiring) = provider(&leaders).await;
    let shutdown = CancellationToken::new();
    let idle: LeaderWork =
        Arc::new(|term: CancellationToken| Box::pin(async move { term.cancelled().await }));
    let task = tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            provider
                .run_while_leader(LeadershipName::Scheduler(3), shutdown, idle)
                .await
        }
    });
    leaders
        .wait_for_candidates("bss-rating/scheduler-3", 1)
        .await;
    shutdown.cancel();
    task.await.unwrap().unwrap();
    wiring.stop().await;
}

#[tokio::test]
async fn an_unbound_profile_is_a_coordination_error() {
    // With no cluster wired the facade resolves to an unbound backend that refuses on first use
    // (R-27): the gear's leader jobs retry instead of running unled.
    let hub = ClientHub::new();
    let election = resolve_election(&hub).await.unwrap();
    let provider = ClusterLeaseProvider::new(
        election,
        ElectionTiming::new(
            ElectionConfig::new(Duration::from_secs(30), 2).unwrap(),
            Duration::from_secs(5),
        ),
    );
    let idle: LeaderWork =
        Arc::new(|term: CancellationToken| Box::pin(async move { term.cancelled().await }));
    assert!(matches!(
        provider
            .run_while_leader(LeadershipName::Purge, CancellationToken::new(), idle)
            .await,
        Err(DomainError::Coordination(..))
    ));
}

#[tokio::test]
async fn losing_leadership_stops_the_work_after_its_current_transaction() {
    let leaders = FakeClusterLeaders::new();
    let (provider, wiring) = provider(&leaders).await;
    let (events, mut received) = mpsc::unbounded_channel();
    let work: LeaderWork = Arc::new(move |term: CancellationToken| {
        let events = events.clone();
        Box::pin(async move {
            let _sent = events.send("started");
            term.cancelled().await;
            let _sent = events.send("stopped");
        })
    });
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            provider
                .run_while_leader(LeadershipName::ExceptionSweep, shutdown, work)
                .await
        }
    });
    leaders.wait_for_candidates(SWEEP, 1).await;
    leaders.grant(SWEEP, 0).await;
    assert_eq!(next(&mut received).await, Some("started"));
    leaders.revoke(SWEEP, 0).await;
    assert_eq!(next(&mut received).await, Some("stopped"));
    shutdown.cancel();
    task.await.unwrap().unwrap();
    wiring.stop().await;
}

#[tokio::test]
async fn with_two_concurrent_leaders_only_one_conditional_write_wins() {
    // ADR-0008: leadership is advisory. Two instances that both believe they lead (a partition, a
    // paused holder) race for the same conditional write; the database lets exactly one through.
    let f = fixture().await;
    f.inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "1"})))
        .await
        .unwrap();
    let AcceptOutcome::Quarantined(entry) = f
        .inbox
        .accept_one(fact(TENANT, "fact-1", 1, json!({"q": "2"})))
        .await
        .unwrap()
    else {
        panic!("expected a quarantine");
    };

    let leaders = FakeClusterLeaders::new();
    let wins = Arc::new(AtomicUsize::new(0));
    let losses = Arc::new(AtomicUsize::new(0));
    let (done, mut finished) = mpsc::unbounded_channel::<()>();
    let shutdown = CancellationToken::new();
    let mut tasks = Vec::new();
    let mut wirings = Vec::new();
    for _ in 0..2 {
        let (provider, wiring) = provider(&leaders).await;
        wirings.push(wiring);
        let db = f.db().clone();
        let (wins, losses, done) = (Arc::clone(&wins), Arc::clone(&losses), done.clone());
        let entry_id = entry.entry_id();
        let work: LeaderWork = Arc::new(move |term: CancellationToken| {
            let (db, wins, losses, done) = (
                db.clone(),
                Arc::clone(&wins),
                Arc::clone(&losses),
                done.clone(),
            );
            Box::pin(async move {
                let conn = db.conn().unwrap();
                let moved = inbox_repo::transition(
                    &conn,
                    &AccessScope::for_tenant(TENANT),
                    entry_id,
                    InboxTransition::DiscardQuarantined,
                )
                .await
                .unwrap();
                if moved {
                    wins.fetch_add(1, Ordering::SeqCst);
                } else {
                    losses.fetch_add(1, Ordering::SeqCst);
                }
                let _sent = done.send(());
                term.cancelled().await;
            })
        });
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            provider
                .run_while_leader(LeadershipName::ExceptionSweep, shutdown, work)
                .await
        }));
    }
    leaders.wait_for_candidates(SWEEP, 2).await;
    leaders.grant(SWEEP, 0).await;
    leaders.grant(SWEEP, 1).await;
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(5), finished.recv())
            .await
            .unwrap();
    }
    assert_eq!(wins.load(Ordering::SeqCst), 1);
    assert_eq!(losses.load(Ordering::SeqCst), 1);
    shutdown.cancel();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    for wiring in wirings {
        wiring.stop().await;
    }
}

/// A retrier of `child` exceptions with reason `awaiting_attribution` that records its calls.
#[derive(Default)]
struct RecordingRetrier {
    retried: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl ExceptionRetrier for RecordingRetrier {
    fn subject_kind(&self) -> SubjectKind {
        SubjectKind::Child
    }

    fn retries(&self, reason_code: &str) -> bool {
        reason_code == "awaiting_attribution"
    }

    async fn retry(
        &self,
        _db: &Db,
        scope: &AccessScope,
        exception: &ExceptionRecord,
    ) -> Result<(), DomainError> {
        assert!(
            scope.contains_uuid(
                toolkit_security::pep_properties::OWNER_TENANT_ID,
                exception.key.tenant_id()
            ),
            "a retry runs under its tenant's scope"
        );
        self.retried.lock().unwrap().push(exception.exception_id);
        Ok(())
    }
}

#[tokio::test]
async fn the_sweeper_re_enqueues_retryable_subjects_tenant_by_tenant() {
    let test_db = migrated_db().await;
    let db = &test_db.db;
    let pdp = FakePdp::new();
    let outside = Uuid::from_u128(0xC);
    pdp.put_outside_grant(outside);
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let key = |tenant: Uuid, kind: SubjectKind, reason: &str| {
        exception_key(tenant, kind, format!("subject-{tenant}"), reason)
    };
    let retryable = record_exception(
        db,
        &key(TENANT, SubjectKind::Child, "awaiting_attribution"),
        "d",
    )
    .await;
    // Not retryable: another reason, another kind, and a tenant outside the grant.
    record_exception(db, &key(TENANT, SubjectKind::Child, "price_uncovered"), "d").await;
    record_exception(
        db,
        &key(TENANT, SubjectKind::Fact, "awaiting_attribution"),
        "d",
    )
    .await;
    record_exception(
        db,
        &key(outside, SubjectKind::Child, "awaiting_attribution"),
        "d",
    )
    .await;

    let retrier = Arc::new(RecordingRetrier::default());
    let sweeper = ExceptionSweeper::new(
        vec![Arc::clone(&retrier) as Arc<dyn ExceptionRetrier>],
        grant,
        Arc::new(OrmExceptionRepo),
    );
    let report = sweeper
        .sweep_once(db, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.retried, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(report.tenants, 1, "the tenant outside the grant is skipped");
    let handed = { retrier.retried.lock().unwrap().clone() };
    assert_eq!(handed, vec![retryable.exception_id]);
}

#[tokio::test]
async fn with_no_retrier_the_sweeper_reads_nothing() {
    let test_db = migrated_db().await;
    // An open exception a retrier would handle: only the missing retrier keeps it from a sweep.
    record_exception(
        &test_db.db,
        &exception_key(TENANT, SubjectKind::Child, "child", "awaiting_attribution"),
        "d",
    )
    .await;
    let pdp = FakePdp::new();
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let report = ExceptionSweeper::new(Vec::new(), grant, Arc::new(OrmExceptionRepo))
        .sweep_once(&test_db.db, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.tenants, 0, "no tenant was discovered");
    assert_eq!(pdp.evaluations(), 0, "no tenant's grant was asked for");
}

/// A retrier of `child` exceptions whose re-enqueue always fails, as when the subject's cause
/// persists; it records each exception it was handed.
#[derive(Default)]
struct FailingRetrier {
    attempted: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl ExceptionRetrier for FailingRetrier {
    fn subject_kind(&self) -> SubjectKind {
        SubjectKind::Child
    }

    fn retries(&self, _reason_code: &str) -> bool {
        true
    }

    async fn retry(
        &self,
        _db: &Db,
        _scope: &AccessScope,
        exception: &ExceptionRecord,
    ) -> Result<(), DomainError> {
        self.attempted.lock().unwrap().push(exception.exception_id);
        Err(DomainError::Database(
            "the subject's cause persists".to_owned(),
            None,
        ))
    }
}

impl FailingRetrier {
    fn take(&self) -> Vec<Uuid> {
        let mut attempted = std::mem::take(&mut *self.attempted.lock().unwrap());
        attempted.sort_unstable();
        attempted
    }
}

/// Opens one `child` exception per key in a single transaction.
async fn record_many(db: &Db, keys: Vec<ExceptionKey>) -> Vec<Uuid> {
    let register = exception_register();
    let mut ids: Vec<Uuid> = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let mut ids = Vec::with_capacity(keys.len());
                for key in &keys {
                    let scope = AccessScope::for_tenant(key.tenant_id());
                    ids.push(register.record(tx, &scope, key, "d").await?.exception_id);
                }
                Ok::<_, DomainError>(ids)
            })
        })
        .await
        .unwrap();
    ids.sort_unstable();
    ids
}

fn page_size() -> usize {
    usize::try_from(SWEEP_BATCH).unwrap()
}

#[tokio::test]
async fn every_sweep_reaches_exceptions_beyond_one_page_of_failing_ones() {
    let test_db = migrated_db().await;
    let db = &test_db.db;
    let pdp = FakePdp::new();
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let open = page_size() + 100;
    let keys = (0..open)
        .map(|n| {
            exception_key(
                TENANT,
                SubjectKind::Child,
                format!("child-{n}"),
                "awaiting_attribution",
            )
        })
        .collect();
    let all = record_many(db, keys).await;
    let retrier = Arc::new(FailingRetrier::default());
    let sweeper = ExceptionSweeper::new(
        vec![Arc::clone(&retrier) as Arc<dyn ExceptionRetrier>],
        grant,
        Arc::new(OrmExceptionRepo),
    );
    // Every retry fails and every exception stays open; each sweep still visits all of them,
    // once each.
    for _ in 0..2 {
        let report = sweeper
            .sweep_once(db, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report.tenants, 1);
        assert_eq!(report.failed, open);
        assert_eq!(retrier.take(), all);
    }
}

#[tokio::test]
async fn every_sweep_reaches_tenants_beyond_one_discovery_page() {
    let test_db = migrated_db().await;
    let db = &test_db.db;
    let pdp = FakePdp::new();
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let tenants = page_size() + 1;
    let keys = (1..=tenants)
        .map(|n| {
            let tenant = Uuid::from_u128(0x1000 + u128::try_from(n).unwrap());
            exception_key(tenant, SubjectKind::Child, "child", "awaiting_attribution")
        })
        .collect();
    let all = record_many(db, keys).await;
    let retrier = Arc::new(FailingRetrier::default());
    let report = ExceptionSweeper::new(
        vec![Arc::clone(&retrier) as Arc<dyn ExceptionRetrier>],
        grant,
        Arc::new(OrmExceptionRepo),
    )
    .sweep_once(db, &CancellationToken::new())
    .await
    .unwrap();
    assert_eq!(report.tenants, tenants);
    assert_eq!(report.failed, tenants);
    assert_eq!(retrier.take(), all);
}

/// A retrier that cancels the sweep's token on its first call, as a lost term does.
struct CancellingRetrier {
    stop: CancellationToken,
    attempted: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl ExceptionRetrier for CancellingRetrier {
    fn subject_kind(&self) -> SubjectKind {
        SubjectKind::Child
    }

    fn retries(&self, _reason_code: &str) -> bool {
        true
    }

    async fn retry(
        &self,
        _db: &Db,
        _scope: &AccessScope,
        exception: &ExceptionRecord,
    ) -> Result<(), DomainError> {
        self.attempted.lock().unwrap().push(exception.exception_id);
        self.stop.cancel();
        Ok(())
    }
}

#[tokio::test]
async fn a_cancelled_sweep_hands_out_no_further_exception() {
    let test_db = migrated_db().await;
    let db = &test_db.db;
    let pdp = FakePdp::new();
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let keys = (0..3)
        .map(|n| {
            exception_key(
                TENANT,
                SubjectKind::Child,
                format!("child-{n}"),
                "awaiting_attribution",
            )
        })
        .collect();
    record_many(db, keys).await;
    let stop = CancellationToken::new();
    let retrier = Arc::new(CancellingRetrier {
        stop: stop.clone(),
        attempted: Mutex::new(Vec::new()),
    });
    let report = ExceptionSweeper::new(
        vec![Arc::clone(&retrier) as Arc<dyn ExceptionRetrier>],
        grant,
        Arc::new(OrmExceptionRepo),
    )
    .sweep_once(db, &stop)
    .await
    .unwrap();
    // The term ended during the first retry: an old leader stops beside the new one.
    let attempted = { retrier.attempted.lock().unwrap().len() };
    assert_eq!(attempted, 1);
    assert_eq!(report.retried, 1);
}

#[tokio::test]
async fn a_pdp_outage_ends_the_sweep_instead_of_skipping_every_tenant() {
    let test_db = migrated_db().await;
    let db = &test_db.db;
    let pdp = FakePdp::new();
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let key = exception_key(TENANT, SubjectKind::Child, "child", "awaiting_attribution");
    record_many(db, vec![key]).await;
    pdp.set_unavailable(true);
    let retrier = Arc::new(FailingRetrier::default());
    let swept = ExceptionSweeper::new(
        vec![Arc::clone(&retrier) as Arc<dyn ExceptionRetrier>],
        grant,
        Arc::new(OrmExceptionRepo),
    )
    .sweep_once(db, &CancellationToken::new())
    .await;
    assert!(
        matches!(swept, Err(DomainError::AuthzUnavailable(..))),
        "{swept:?}"
    );
    assert!(retrier.take().is_empty());
}
