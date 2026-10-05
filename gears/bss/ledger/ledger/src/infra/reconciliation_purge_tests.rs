//! Unit tests for [`RetiredRunPurger`] — the rotation, the three budgets, and
//! the failure path — over an in-memory store double, plus one `SQLite`
//! round-trip through the real [`ReconciliationRunRepo`] pinning the
//! eligibility predicate and the per-statement `LIMIT`.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test assertions unwrap"
)]

use std::collections::{HashMap, HashSet};

use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};

use super::*;
use crate::infra::storage::migrations::Migrator;

/// A store double: `tenant -> purgeable rows left`, a set of tenants whose
/// purge fails, and a log of every `(tenant, limit)` call.
#[derive(Default)]
struct FakeStore {
    rows: Mutex<HashMap<Uuid, u64>>,
    failing: HashSet<Uuid>,
    calls: Mutex<Vec<(Uuid, u64)>>,
}

impl FakeStore {
    fn with_rows(rows: &[(Uuid, u64)]) -> Self {
        Self {
            rows: Mutex::new(rows.iter().copied().collect()),
            ..Self::default()
        }
    }
}

/// Shares one [`FakeStore`] between the purger (which owns a `Box`) and the test.
struct Shared(std::sync::Arc<FakeStore>);

#[async_trait]
impl UneventfulRunStore for Shared {
    async fn purge_uneventful_runs(&self, tenant: Uuid, limit: u64) -> Result<u64, RepoError> {
        self.0.calls.lock().unwrap().push((tenant, limit));
        if self.0.failing.contains(&tenant) {
            return Err(RepoError::Db("boom".to_owned()));
        }
        let mut rows = self.0.rows.lock().unwrap();
        let left = rows.entry(tenant).or_insert(0);
        let taken = (*left).min(limit);
        *left -= taken;
        Ok(taken)
    }
}

fn tenant(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn purger(
    store: &std::sync::Arc<FakeStore>,
    max_rows: u64,
    max_tenants: usize,
    batch: u64,
) -> RetiredRunPurger {
    RetiredRunPurger::new(
        Box::new(Shared(std::sync::Arc::clone(store))),
        max_rows,
        max_tenants,
        batch,
    )
}

fn visited(store: &FakeStore) -> Vec<Uuid> {
    let mut seen = Vec::new();
    for (t, _) in store.calls.lock().unwrap().iter() {
        if seen.last() != Some(t) {
            seen.push(*t);
        }
    }
    seen
}

fn rows_left(store: &FakeStore, t: Uuid) -> u64 {
    store.rows.lock().unwrap().get(&t).copied().unwrap_or(0)
}

#[tokio::test]
async fn disabled_purge_touches_nothing() {
    let store = std::sync::Arc::new(FakeStore::with_rows(&[(tenant(1), 3)]));
    let p = purger(&store, 0, 10, 5);

    assert!(!p.enabled());
    assert_eq!(p.purge(&[tenant(1)]).await, PurgeOutcome::default());
    assert!(store.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tenant_budget_rotates_across_ticks() {
    // Three deleted tenants, two visits per tick: tick 1 drains 1 and 2, tick
    // 2 must resume at 3 and then wrap to 1 — not re-grind the head.
    let (a, b, c) = (tenant(1), tenant(2), tenant(3));
    let store = std::sync::Arc::new(FakeStore::with_rows(&[(a, 1), (b, 1), (c, 1)]));
    let p = purger(&store, 100, 2, 10);

    let first = p.purge(&[c, a, b]).await;
    assert_eq!(
        first,
        PurgeOutcome {
            deleted: 2,
            visited: 2,
            failed: 0
        }
    );
    assert_eq!(visited(&store), vec![a, b], "sorted order, two visits");

    store.calls.lock().unwrap().clear();
    let second = p.purge(&[a, b, c]).await;
    assert_eq!(second.deleted, 1);
    assert_eq!(second.visited, 2);
    assert_eq!(visited(&store), vec![c, a], "resumes after b, then wraps");
    assert_eq!(rows_left(&store, c), 0);
}

#[tokio::test]
async fn row_budget_stop_still_rotates_to_the_next_tenant() {
    // `a` holds more than one tick's row budget. The tick that runs out of
    // budget inside `a` must still move the cursor past it, so `b` gets its
    // visit next tick instead of waiting for `a` to drain; `a`'s remainder is
    // picked up on the following lap.
    let (a, b) = (tenant(1), tenant(2));
    let store = std::sync::Arc::new(FakeStore::with_rows(&[(a, 3), (b, 1)]));
    let p = purger(&store, 1, 10, 10);

    let first = p.purge(&[a, b]).await;
    assert_eq!(first.deleted, 1);
    assert_eq!(first.visited, 1, "budget exhausted inside the first tenant");
    assert_eq!(
        store.calls.lock().unwrap().as_slice(),
        &[(a, 1)],
        "the per-statement limit is capped by the remaining row budget"
    );

    store.calls.lock().unwrap().clear();
    let second = p.purge(&[a, b]).await;
    assert_eq!(second.deleted, 1);
    assert_eq!(
        visited(&store),
        vec![b],
        "the next tick visits b, not a again"
    );
    assert_eq!(rows_left(&store, b), 0);

    store.calls.lock().unwrap().clear();
    p.purge(&[a, b]).await;
    assert_eq!(visited(&store), vec![a], "then wraps back to a");
    assert_eq!(rows_left(&store, a), 1);
}

#[tokio::test]
async fn full_batches_loop_until_a_short_one() {
    // 7 rows at 3 per statement: 3 + 3 + 1 — the `deleted == batch` path keeps
    // going on the same tenant until a short batch proves it drained.
    let a = tenant(1);
    let store = std::sync::Arc::new(FakeStore::with_rows(&[(a, 7)]));
    let p = purger(&store, 100, 10, 3);

    let outcome = p.purge(&[a]).await;

    assert_eq!(outcome.deleted, 7);
    assert_eq!(
        store.calls.lock().unwrap().as_slice(),
        &[(a, 3), (a, 3), (a, 3)]
    );
}

#[tokio::test]
async fn a_failing_tenant_is_counted_skipped_and_passed() {
    let (a, b) = (tenant(1), tenant(2));
    let store = std::sync::Arc::new(FakeStore {
        rows: Mutex::new(HashMap::from([(b, 2)])),
        failing: HashSet::from([a]),
        calls: Mutex::new(Vec::new()),
    });
    let p = purger(&store, 100, 1, 10);

    let first = p.purge(&[a, b]).await;
    assert_eq!(
        first,
        PurgeOutcome {
            deleted: 0,
            visited: 1,
            failed: 1
        }
    );

    // The cursor moved past the failing tenant: the rotation does not stall.
    let second = p.purge(&[a, b]).await;
    assert_eq!(second.deleted, 2);
    assert_eq!(second.failed, 0);
}

#[tokio::test]
async fn zero_tenant_budget_is_inert() {
    let store = std::sync::Arc::new(FakeStore::with_rows(&[(tenant(1), 1)]));
    let p = purger(&store, 100, 0, 10);

    assert_eq!(p.purge(&[tenant(1)]).await, PurgeOutcome::default());
}

// --- the real repo on SQLite ----------------------------------------------

async fn sqlite() -> DBProvider<DbError> {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrator");
    DBProvider::<DbError>::new(db)
}

/// Seed one run through the repo's own `start` / `finalize` path.
async fn seed(
    provider: &DBProvider<DbError>,
    tenant: Uuid,
    status: Option<(&'static str, i64, bool)>,
) -> Uuid {
    let run_id = Uuid::now_v7();
    provider
        .transaction(move |txn| {
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant);
                ReconciliationRunRepo::start(txn, &scope, tenant, run_id, "202609", "AR_DERIVED")
                    .await
                    .map_err(|e| DbError::Other(anyhow::anyhow!("{e}")))?;
                if let Some((status, variance, within)) = status {
                    ReconciliationRunRepo::finalize(
                        txn, &scope, tenant, run_id, status, variance, within, None, None,
                    )
                    .await
                    .map_err(|e| DbError::Other(anyhow::anyhow!("{e}")))?;
                }
                Ok(())
            })
        })
        .await
        .expect("seed run");
    run_id
}

async fn exists(repo: &ReconciliationRunRepo, tenant: Uuid, run_id: Uuid) -> bool {
    repo.read(&AccessScope::for_tenant(tenant), tenant, run_id)
        .await
        .expect("read run")
        .is_some()
}

#[tokio::test]
async fn repo_purges_only_uneventful_runs_of_the_given_tenant() {
    let provider = sqlite().await;
    let repo = ReconciliationRunRepo::new(provider.clone());
    let (dead, other) = (Uuid::now_v7(), Uuid::now_v7());

    let uneventful = seed(&provider, dead, Some(("DONE", 0, true))).await;
    let breached = seed(&provider, dead, Some(("DONE", 4_200, false))).await;
    // Within the rounding budget, but a variance was recorded: evidence.
    let rounded = seed(&provider, dead, Some(("DONE", 1, true))).await;
    let running = seed(&provider, dead, None).await;
    let failed = seed(&provider, dead, Some(("FAILED", 0, true))).await;
    let foreign = seed(&provider, other, Some(("DONE", 0, true))).await;

    let deleted = repo.purge_uneventful_runs(dead, 100).await.expect("purge");

    assert_eq!(deleted, 1);
    assert!(!exists(&repo, dead, uneventful).await);
    for (run, why) in [
        (breached, "an out-of-tolerance run"),
        (rounded, "a within-tolerance run with a recorded variance"),
        (running, "an unfinalized run"),
        (failed, "a FAILED run"),
    ] {
        assert!(exists(&repo, dead, run).await, "{why} is evidence");
    }
    assert!(
        exists(&repo, other, foreign).await,
        "the delete is scoped to the one tenant"
    );
}

#[tokio::test]
async fn repo_purge_honours_the_limit() {
    let provider = sqlite().await;
    let repo = ReconciliationRunRepo::new(provider.clone());
    let dead = Uuid::now_v7();
    for _ in 0..3 {
        seed(&provider, dead, Some(("DONE", 0, true))).await;
    }

    assert_eq!(repo.purge_uneventful_runs(dead, 0).await.expect("noop"), 0);
    assert_eq!(repo.purge_uneventful_runs(dead, 2).await.expect("purge"), 2);
    assert_eq!(repo.purge_uneventful_runs(dead, 2).await.expect("purge"), 1);
    assert_eq!(repo.purge_uneventful_runs(dead, 2).await.expect("purge"), 0);
}
