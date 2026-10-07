#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The tenant's one default category on `PostgreSQL` (P-D-218): the partial unique index
//! `uq_products_category_default` refusing a second default is `CATEGORY_DEFAULT_TAKEN` from both
//! writes, never a driver failure the doors would answer as a 500.
mod pg_support;

use bss_products::{
    domain::category::{CategoryPatch, NewCategory},
    infra::storage::{
        RepoError, RepoRefusal,
        repo::{self, HeadWrite},
    },
};
use pg_support::Pg;
use sea_orm::DbErr;
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit_db::{
    Db, DbError,
    secure::{AccessScope, DBRunner, TxConfig},
};
use uuid::Uuid;

#[derive(Debug)]
enum TxError {
    Db(DbError),
    Repo(RepoError),
}
impl From<DbError> for TxError {
    fn from(e: DbError) -> Self {
        Self::Db(e)
    }
}
impl From<RepoError> for TxError {
    fn from(e: RepoError) -> Self {
        Self::Repo(e)
    }
}
/// The same typed driver extraction the doors pass to `transaction_with_retry`.
fn db_error(error: &TxError) -> Option<&DbErr> {
    match error {
        TxError::Db(DbError::Sea(e)) | TxError::Repo(RepoError::Driver { source: e, .. }) => {
            Some(e)
        }
        _ => None,
    }
}
const MAKE_DEFAULT: CategoryPatch = CategoryPatch {
    name: None,
    is_default: Some(true),
    sort_order: None,
};
/// The door's move: clear the other default, then set this one, at version 1.
async fn move_default(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<HeadWrite<bss_products_sdk::models::Category>, RepoError> {
    let now = OffsetDateTime::now_utc();
    repo::clear_default_category(tx, scope, tenant, Some(id), now).await?;
    repo::update_category(tx, scope, tenant, id, 1, MAKE_DEFAULT, now).await
}

struct Fixture {
    pg: Pg,
    db: Db,
    scope: AccessScope,
    tenant: Uuid,
}
impl Fixture {
    async fn new() -> Self {
        let pg = Pg::applied().await;
        let db = pg.db().await;
        let tenant = Uuid::new_v4();
        Self {
            pg,
            db,
            scope: AccessScope::for_tenant(tenant),
            tenant,
        }
    }
    async fn category(&self, code: &str, is_default: bool) -> Result<Uuid, RepoError> {
        repo::insert_category(
            &self.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            NewCategory {
                code: code.into(),
                name: code.into(),
                is_default,
                sort_order: 0,
            },
            OffsetDateTime::now_utc(),
        )
        .await
        .map(|c| c.id)
    }
}

fn is_default_taken(result: Result<impl std::fmt::Debug, RepoError>) {
    match result {
        Err(RepoError::Refused(RepoRefusal::CategoryDefaultTaken)) => {}
        other => panic!("expected CATEGORY_DEFAULT_TAKEN, got {other:?}"),
    }
}

/// A second default, written straight past the move, is the index's refusal by name on both the
/// insert and the update.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_second_default_is_the_default_taken_not_a_driver_failure() {
    let f = Fixture::new().await;
    f.category("first", true).await.unwrap();
    is_default_taken(f.category("second", true).await);
    let third = f.category("third", false).await.unwrap();
    is_default_taken(
        repo::update_category(
            &f.db.conn().unwrap(),
            &f.scope,
            f.tenant,
            third,
            1,
            CategoryPatch {
                name: None,
                is_default: Some(true),
                sort_order: None,
            },
            OffsetDateTime::now_utc(),
        )
        .await,
    );
    // Another tenant has its own default.
    let other = Uuid::new_v4();
    repo::insert_category(
        &f.db.conn().unwrap(),
        &AccessScope::for_tenant(other),
        other,
        NewCategory {
            code: "first".into(),
            name: "first".into(),
            is_default: true,
            sort_order: 0,
        },
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
}

/// **Two moves at once on two connections: one wins, the other is `CATEGORY_DEFAULT_TAKEN`**,
/// never a driver failure, and the tenant keeps one default. Read committed, as the doors run:
/// the second clear waits on the first's lock on the old default, finds it cleared once the first
/// commits, and its set meets the winner's default in the index.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_moves_on_two_connections_leave_one_default() {
    let f = Fixture::new().await;
    f.category("first", true).await.unwrap();
    let b = f.category("b", false).await.unwrap();
    let c = f.category("c", false).await.unwrap();
    let first = f.pg.db().await;
    let second = f.pg.db().await;
    let observer = f.pg.raw().await;
    let moved = Arc::new(tokio::sync::Notify::new());
    let ready = moved.clone();
    let (scope_b, scope_c, tenant) = (f.scope.clone(), f.scope.clone(), f.tenant);
    let winner = first.transaction_with_retry::<_, TxError, _, _>(
        TxConfig::default(),
        db_error,
        move |tx| {
            let scope = scope_b.clone();
            let ready = ready.clone();
            let observer = observer.clone();
            Box::pin(async move {
                let written = move_default(tx, &scope, tenant, b).await?;
                ready.notify_one();
                pg_support::wait_until_a_backend_blocks(&observer).await;
                Ok(written)
            })
        },
    );
    let loser = async {
        moved.notified().await;
        second
            .transaction_with_retry::<_, TxError, _, _>(TxConfig::default(), db_error, move |tx| {
                let scope = scope_c.clone();
                Box::pin(async move { Ok(move_default(tx, &scope, tenant, c).await?) })
            })
            .await
    };
    let (won, lost) = tokio::join!(winner, loser);
    assert!(
        matches!(won, Ok(HeadWrite::Written(ref cat)) if cat.id == b && cat.is_default),
        "{won:?}"
    );
    match lost {
        Err(TxError::Repo(RepoError::Refused(RepoRefusal::CategoryDefaultTaken))) => {}
        other => panic!("expected CATEGORY_DEFAULT_TAKEN, got {other:?}"),
    }
    let defaults: Vec<Uuid> = repo::list_categories(&f.db.conn().unwrap(), &f.scope, f.tenant)
        .await
        .unwrap()
        .into_iter()
        .filter(|cat| cat.is_default)
        .map(|cat| cat.id)
        .collect();
    assert_eq!(defaults, vec![b]);
}

/// **Retiring the default clears it first** (P-D-220) on Postgres: `clear_default_of` clears only
/// the named category, and only while it holds the default, answering it with its new version;
/// the retirement then finds it unused and not default, and the tenant has no default until
/// another category is made one.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn retiring_the_default_clears_it_first_and_leaves_no_default() {
    let f = Fixture::new().await;
    let main = f.category("main", true).await.unwrap();
    let side = f.category("side", false).await.unwrap();
    let conn = f.db.conn().unwrap();
    let now = OffsetDateTime::now_utc();
    assert!(
        repo::clear_default_of(&conn, &f.scope, f.tenant, side, now)
            .await
            .unwrap()
            .is_none(),
        "a category that is not the default clears nothing"
    );
    let cleared = repo::clear_default_of(&conn, &f.scope, f.tenant, main, now)
        .await
        .unwrap()
        .expect("the default is cleared");
    assert_eq!(
        (cleared.id, cleared.is_default, cleared.version),
        (main, false, 2)
    );
    assert!(
        repo::clear_default_of(&conn, &f.scope, f.tenant, main, now)
            .await
            .unwrap()
            .is_none(),
        "a second clear finds no default"
    );
    let retired = repo::retire_category_if_unused(&conn, &f.scope, f.tenant, main, now)
        .await
        .unwrap();
    assert!(
        matches!(retired, Some(HeadWrite::Written(ref c)) if c.status == "retired" && !c.is_default && c.version == 3),
        "{retired:?}"
    );
    let defaults = || async {
        repo::list_categories(&f.db.conn().unwrap(), &f.scope, f.tenant)
            .await
            .unwrap()
            .into_iter()
            .filter(|cat| cat.is_default)
            .map(|cat| cat.id)
            .collect::<Vec<Uuid>>()
    };
    assert!(defaults().await.is_empty());
    assert!(matches!(
        move_default(&conn, &f.scope, f.tenant, side).await.unwrap(),
        HeadWrite::Written(_)
    ));
    assert_eq!(defaults().await, vec![side]);
}
