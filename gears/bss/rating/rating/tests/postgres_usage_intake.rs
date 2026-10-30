//! PostgreSQL tier of usage capture (feature 04; DESIGN §3.7 "Database profiles for tests"): the
//! migration's PostgreSQL-only checks (the quantity pattern), its partial and expression indexes,
//! and the vectors that need row locks or two concurrent writers — V01, V04, V68 and V76 — plus
//! the natural-key race across two sources, driven so the race does happen.
//! Ignored by default; run with `make test-integration LANE=rating-pg` (Docker required).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use bss_rating::BssRatingGear;
use bss_rating::domain::error::DomainError;
use bss_rating::domain::usage::declarations::PageDeclarations;
use bss_rating::domain::usage::entry::EntryIdentity;
use bss_rating::domain::usage::feed::FeedCursor;
use bss_rating::domain::usage::intake::{
    IntakeParts, PageOutcome, PreparedPage, RestartOutcome, UsageIntake,
};
use bss_rating::domain::usage::loss::{LossKind, NewSourceLoss, TaintedDays};
use bss_rating::domain::usage::normalize::{NormalizedUsage, RejectReason};
use bss_rating::domain::usage::{ContributionSink, ContributionState, SourceLossRepo};
use bss_rating::infra::storage::repo::usage_repo::OrmUsageRepo;
use bss_rating::test_providers::usage::{meter, sum_declaration};
use common::TestDb;
use common::usage::{
    Fixture, T1, checkpoint, committed, configure, fixture_over, hourly, losses, page_entries,
    records, scope, source, step, stored, try_step,
};
use serde_json::{Value, json};
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::ConnectOpts;
use toolkit_db::secure::DbTx;
use toolkit_security::AccessScope;
use uuid::Uuid;

struct Pg {
    fixture: Fixture,
    url: String,
    _container: ContainerAsync<testcontainers_modules::postgres::Postgres>,
}

impl Pg {
    /// `sql` on a connection of its own, outside the gear's secure layer: the schema's own
    /// refusals, and what a concurrent session sees.
    async fn raw(&self, sql: &str) -> Result<sea_orm::ExecResult, sea_orm::DbErr> {
        use sea_orm::ConnectionTrait;
        let conn = sea_orm::Database::connect(&self.url).await.unwrap();
        conn.execute_unprepared(sql).await
    }

    /// The number `sql` (a `SELECT count(*)`) counts.
    async fn count(&self, sql: &str) -> i64 {
        use sea_orm::ConnectionTrait;
        let conn = sea_orm::Database::connect(&self.url).await.unwrap();
        conn.query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            sql.to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
    }
}

async fn pg() -> Pg {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = toolkit_db::connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(8),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        BssRatingGear::default().migrations(),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    Pg {
        fixture: fixture_over(TestDb::from_parts(db, dir)),
        url,
        _container: container,
    }
}

/// A page of `entries` read from `Oldest`, prepared as the reader prepares it.
async fn prepared(f: &Fixture, entries: Vec<Value>) -> PreparedPage {
    let mut declarations = PageDeclarations::new();
    declarations.insert(meter("hours"), Some(sum_declaration("h", &[])));
    let mut scopes = BTreeMap::new();
    scopes.insert(T1, scope(f, T1).await);
    PreparedPage {
        entries: page_entries(entries),
        next: FeedCursor::parse("page-1").unwrap(),
        declarations,
        scopes,
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn v01_on_postgres_a_page_applied_twice_has_no_second_effect() {
    let pg = pg().await;
    let f = &pg.fixture;
    let s = source(&["hours"], &[T1]);
    configure(f, std::slice::from_ref(&s)).await;
    let entries = [hourly(T1, "k1", "8"), hourly(T1, "k2", "3")];
    f.feed.push(entries.clone());
    assert_eq!(committed(step(f, &s).await).captured, 2);
    f.feed.push(entries.clone());
    let again = committed(step(f, &s).await);
    assert_eq!((again.captured, again.duplicates), (0, 2));
    assert_eq!(f.contributions.interpreted().len(), 2);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn v04_on_postgres_a_page_that_fails_before_commit_leaves_nothing() {
    let pg = pg().await;
    let f = &pg.fixture;
    let s = source(&["hours"], &[T1]);
    configure(f, std::slice::from_ref(&s)).await;
    f.feed.push([hourly(T1, "k1", "8"), hourly(T1, "k2", "3")]);
    f.contributions.fail.store(true, Ordering::SeqCst);
    assert!(try_step(f, &s).await.is_err());
    assert!(records(f, T1).await.is_empty());
    assert_eq!(checkpoint(f, &s).await.cursor, None);
    f.contributions.fail.store(false, Ordering::SeqCst);
    assert_eq!(committed(step(f, &s).await).captured, 2);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn v68_two_readers_of_one_source_commit_one_page_once() {
    let pg = pg().await;
    let f = &pg.fixture;
    let s = source(&["hours"], &[T1]);
    configure(f, std::slice::from_ref(&s)).await;
    let entries = vec![hourly(T1, "k1", "8"), hourly(T1, "k2", "3")];
    let page = prepared(f, entries.clone()).await;
    let (first, second) = tokio::join!(
        f.intake.apply_page(f.db(), &s, None, page.clone()),
        f.intake.apply_page(f.db(), &s, None, page),
    );
    let outcomes = [first.unwrap(), second.unwrap()];
    let committed = outcomes
        .iter()
        .filter(|o| matches!(o, PageOutcome::Committed(_)))
        .count();
    let moved = outcomes
        .iter()
        .filter(|o| matches!(o, PageOutcome::CursorMoved))
        .count();
    assert_eq!(
        (committed, moved),
        (1, 1),
        "the second fails the cursor CAS"
    );
    assert_eq!(records(f, T1).await.len(), 2);
    assert_eq!(f.contributions.interpreted().len(), 2, "no second effect");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn v76_a_stale_restart_racing_a_page_changes_nothing() {
    let pg = pg().await;
    let f = &pg.fixture;
    let s = source(&["hours"], &[T1]);
    configure(f, std::slice::from_ref(&s)).await;
    // Both race from a committed cursor: a restart from `Oldest` leaves `Oldest` as it was, so a
    // race that starts there has nothing for the CAS to tell apart.
    let first = prepared(f, vec![hourly(T1, "k1", "8")]).await;
    let last_good = first.next.clone();
    assert!(matches!(
        f.intake.apply_page(f.db(), &s, None, first).await.unwrap(),
        PageOutcome::Committed(_)
    ));
    let mut page = prepared(f, vec![hourly(T1, "k2", "3")]).await;
    page.next = FeedCursor::parse("page-2").unwrap();
    let owner = scope(f, T1).await;
    // The new leader commits the next page; a stale leader, refused for the same position, tries
    // to restart.
    let (paged, restarted) = tokio::join!(
        f.intake
            .apply_page(f.db(), &s, Some(last_good.clone()), page),
        f.intake
            .restart_after_retention_loss(f.db(), &s, Some(last_good), owner),
    );
    match (paged.unwrap(), restarted.unwrap()) {
        (PageOutcome::Committed(_), RestartOutcome::CursorMoved) => {
            assert!(losses(f, &s).await.is_empty(), "no loss row");
            assert_eq!(
                checkpoint(f, &s).await.cursor,
                Some(FeedCursor::parse("page-2").unwrap()),
                "no cursor reset"
            );
        }
        (PageOutcome::CursorMoved, RestartOutcome::Restarted(loss)) => {
            // The restart came first; then the page read after the last good cursor is stale.
            assert_eq!(loss.kind, LossKind::CursorBeyondRetention);
            assert_eq!(
                checkpoint(f, &s).await.cursor,
                None,
                "read again from `Oldest`"
            );
            assert_eq!(records(f, T1).await.len(), 1, "only the first page");
        }
        other => panic!("exactly one transaction wins the CAS, got {other:?}"),
    }
}

/// The contribution step held open: a page's transaction stops in it, after its capture insert,
/// until the test releases it.
#[derive(Default)]
struct Held {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl ContributionSink for Held {
    async fn interpret(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _record: &NormalizedUsage,
    ) -> Result<(), DomainError> {
        self.reached.notify_one();
        self.release.notified().await;
        Ok(())
    }

    async fn withdraw(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _target: EntryIdentity,
        _from: ContributionState,
        _invalidation: Uuid,
    ) -> Result<(), DomainError> {
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_natural_key_race_between_two_sources_aborts_once_then_rejects() {
    let pg = pg().await;
    let f = &pg.fixture;
    let a = source(&["hours"], &[T1]);
    let b = source(&["hours", "gb"], &[T1]);
    configure(f, &[a.clone(), b.clone()]).await;
    let original = hourly(T1, "k1", "8");
    let mut impostor = original.clone();
    impostor["id"] = json!(Uuid::from_u128(0xbeef));
    // Source a stores the key and holds its transaction open before the commit.
    let held = Arc::new(Held::default());
    let holding = UsageIntake::new(IntakeParts {
        contributions: held.clone(),
        ..f.intake_parts()
    });
    let first = tokio::spawn({
        let (db, a, page) = (
            f.db().clone(),
            a.clone(),
            prepared(f, vec![original.clone()]).await,
        );
        async move { holding.apply_page(&db, &a, None, page).await }
    });
    held.reached.notified().await;
    // Source b does not see the uncommitted key, so its insert waits on the unique index.
    let second = tokio::spawn({
        let (intake, db, b) = (f.intake.clone(), f.db().clone(), b.clone());
        let page = prepared(f, vec![impostor.clone()]).await;
        async move { intake.apply_page(&db, &b, None, page).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while pg
            .count("SELECT count(*) FROM pg_locks WHERE NOT granted")
            .await
            == 0
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the second insert waits on the first transaction");
    held.release.notify_one();
    assert!(matches!(
        first.await.unwrap(),
        Ok(PageOutcome::Committed(_))
    ));
    let raced = second.await.unwrap();
    assert!(
        matches!(raced, Err(DomainError::Concurrent(_))),
        "the losing page aborts as concurrent: {raced:?}"
    );
    // The aborted page is read again: its re-read finds the stored key and rejects the impostor.
    assert!(
        records(f, T1).await.len() == 1,
        "the aborted page wrote nothing"
    );
    f.intake
        .apply_page(f.db(), &b, None, prepared(f, vec![impostor.clone()]).await)
        .await
        .unwrap();
    assert_eq!(
        stored(f, &original).await.unwrap().contribution_state,
        "captured"
    );
    let other = stored(f, &impostor).await.unwrap();
    assert_eq!(other.contribution_state, "rejected");
    assert_eq!(
        other.reject_reason.as_deref(),
        Some(RejectReason::NaturalKeyConflict.as_str())
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_schema_refuses_what_the_code_never_writes() {
    let pg = pg().await;
    let f = &pg.fixture;
    let s = source(&["hours"], &[T1]);
    configure(f, std::slice::from_ref(&s)).await;
    let entry = hourly(T1, "k1", "8");
    f.feed.push([entry.clone()]);
    committed(step(f, &s).await);
    let id = common::usage::id_of(&entry);
    let row = format!("tenant_id = '{T1}' AND usage_record_id = '{id}'");
    for (what, sql) in [
        (
            "a quantity outside the collector's pattern",
            format!("UPDATE bss_rating__usage_record SET quantity = '08' WHERE {row}"),
        ),
        (
            "an interval that ends before it starts",
            format!(
                "UPDATE bss_rating__usage_record SET interval_end = interval_start - interval '1 hour' WHERE {row}"
            ),
        ),
        (
            "a set-aside entry outside the subscription naming a loss",
            format!(
                "INSERT INTO bss_rating__usage_capture_reject VALUES ('{}', '{}', '{{}}', now(), 'outside_subscription', '{}')",
                s.source_id(),
                "0".repeat(64),
                Uuid::from_u128(1)
            ),
        ),
        (
            "an unidentifiable entry naming no loss",
            format!(
                "INSERT INTO bss_rating__usage_capture_reject VALUES ('{}', '{}', '{{}}', now(), 'unidentifiable', NULL)",
                s.source_id(),
                "1".repeat(64)
            ),
        ),
    ] {
        assert!(pg.raw(&sql).await.is_err(), "{what} is refused");
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn one_invalidation_names_a_target_and_one_loss_is_open_per_cursor() {
    let pg = pg().await;
    let f = &pg.fixture;
    let s = source(&["hours"], &[T1]);
    configure(f, std::slice::from_ref(&s)).await;
    let target = hourly(T1, "k1", "8");
    let other = hourly(T1, "k2", "3");
    let (first, second) = (
        bss_rating::test_providers::usage::invalidation_of(&target, "DUPLICATE_EMISSION"),
        bss_rating::test_providers::usage::invalidation_of(&other, "DUPLICATE_EMISSION"),
    );
    f.feed.push([target.clone(), other, first, second.clone()]);
    // The fixture reads three entries a page: two pages hold the four.
    committed(step(f, &s).await);
    committed(step(f, &s).await);
    assert_eq!(
        stored(f, &second).await.unwrap().contribution_state,
        "applied",
        "both invalidations are stored"
    );
    // `uq_bss_rating__usage_record__invalidates`: a second invalidation of the same target.
    let second_id = common::usage::id_of(&second);
    let target_id = common::usage::id_of(&target);
    assert!(
        pg.raw(&format!(
            "UPDATE bss_rating__usage_record SET invalidates_id = '{target_id}' \
             WHERE tenant_id = '{T1}' AND usage_record_id = '{second_id}'"
        ))
        .await
        .is_err()
    );
    // `uq_bss_rating__source_loss__open` hashes the cursor: a 4 KiB one is indexed, and a second
    // open loss of the same source, kind and cursor is refused.
    let long_cursor = FeedCursor::parse("c".repeat(4096)).unwrap();
    let loss = NewSourceLoss {
        loss_id: Uuid::now_v7(),
        tenant_id: T1,
        source_id: s.source_id(),
        tenant_scope: s.tenant_scope().clone(),
        kind: LossKind::CursorBeyondRetention,
        meters: s.subscription().meters().to_vec(),
        detected_at: time::OffsetDateTime::now_utc(),
        last_good_cursor: Some(long_cursor),
        last_good_at: None,
        restart_cursor: None,
        tainted: TaintedDays::none(),
    };
    let owner = scope(f, T1).await;
    let opened = {
        let (owner, loss) = (owner.clone(), loss.clone());
        f.db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move { OrmUsageRepo.open(tx, &owner, &loss).await })
            })
            .await
            .unwrap()
    };
    assert!(
        pg.raw(&format!(
            "INSERT INTO bss_rating__source_loss (tenant_id, loss_id, source_id, tenant_scope, kind, \
             gts_type_ids, detected_at, last_good_cursor, state, tainted_days) \
             SELECT tenant_id, '{}', source_id, tenant_scope, kind, gts_type_ids, detected_at, \
             last_good_cursor, 'open', tainted_days FROM bss_rating__source_loss WHERE loss_id = '{}'",
            Uuid::now_v7(),
            opened.loss_id
        ))
        .await
        .is_err(),
        "a second open loss of one cursor"
    );
    // Through the repository, the same loss is reused.
    let again = f
        .db()
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move { OrmUsageRepo.open(tx, &owner, &loss).await })
        })
        .await
        .unwrap();
    assert_eq!(again.loss_id, opened.loss_id);
}
