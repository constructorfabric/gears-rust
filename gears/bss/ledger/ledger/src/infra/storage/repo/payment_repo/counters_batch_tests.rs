//! The batched counter writes: several settlement deltas in one read and one
//! CAS, and several invoices' allocation grains with one locked read.
#![allow(clippy::unwrap_used)]

use super::*;
use bss_ledger_sdk::{CurrencySpec, MoneyError, parse_decimal};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, TxConfig};
use toolkit_db::{ConnectOpts, connect_db};

fn money(text: &str) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new("EUR".into(), 2).unwrap(),
    )
    .unwrap()
}

async fn setup() -> (PaymentRepo, Db, Uuid) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    (
        PaymentRepo::new(DBProvider::new(db.clone())),
        db,
        Uuid::now_v7(),
    )
}

/// A transaction body error: the repository's, or the transaction's own.
#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// The repository error a failed body rolled back with.
fn repo_result<T>(result: Result<T, TestError>) -> Result<T, RepoError> {
    result.map_err(|error| match error {
        TestError::Repo(error) => error,
        TestError::Db(error) => panic!("transaction failure: {error}"),
    })
}

/// Run one body in a serializable transaction and keep its repository result.
macro_rules! attempt {
    ($db:expr, |$txn:ident| $body:expr) => {
        repo_result(
            $db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |$txn| {
                Box::pin(async move { Ok::<_, TestError>($body.await?) })
            })
            .await,
        )
    };
}

#[tokio::test]
async fn several_settlement_deltas_land_in_one_cas_in_any_order() {
    let (repo, db, tenant) = setup().await;
    let scope = AccessScope::allow_all();
    let (rp, sc) = (repo.clone(), scope.clone());
    attempt!(db, |txn| rp.seed_settlement(
        txn,
        &sc,
        tenant,
        "p",
        &money("1"),
        &money("0.03")
    ))
    .unwrap();
    // A full return with the settled decrement FIRST: checked step by step this
    // would read `fee 0.03 > settled 0`, but the combined post-state is valid.
    let (rp, sc) = (repo.clone(), scope.clone());
    attempt!(db, |txn| rp.add_settlement_deltas(
        txn,
        &sc,
        tenant,
        "p",
        &[
            (SettlementCounter::Settled, &money("-1")),
            (SettlementCounter::Fee, &money("-0.03")),
        ],
    ))
    .unwrap();
    let state = repo
        .read_settlement(&scope, tenant, "p")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((state.settled, state.fee), (money("0"), money("0")));
    assert_eq!(state.version, 1, "both counters moved in one version step");
}

#[tokio::test]
async fn deltas_on_one_counter_sum_and_a_violating_combination_writes_nothing() {
    let (repo, db, tenant) = setup().await;
    let scope = AccessScope::allow_all();
    let (rp, sc) = (repo.clone(), scope.clone());
    attempt!(db, |txn| rp.seed_settlement(
        txn,
        &sc,
        tenant,
        "p",
        &money("1"),
        &money("0")
    ))
    .unwrap();
    let (rp, sc) = (repo.clone(), scope.clone());
    attempt!(db, |txn| rp.add_settlement_deltas(
        txn,
        &sc,
        tenant,
        "p",
        &[
            (SettlementCounter::Allocated, &money("0.4")),
            (SettlementCounter::Allocated, &money("0.2")),
        ],
    ))
    .unwrap();
    let state = repo
        .read_settlement(&scope, tenant, "p")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.allocated, money("0.6"));
    // Returning more than the unallocated remainder breaks allocated <= settled.
    let (rp, sc) = (repo.clone(), scope.clone());
    let over = attempt!(db, |txn| rp.add_settlement_deltas(
        txn,
        &sc,
        tenant,
        "p",
        &[(SettlementCounter::Settled, &money("-0.5"))],
    ));
    assert!(
        matches!(over, Err(RepoError::MoneyOutCapExceeded(_))),
        "{over:?}"
    );
    let (rp, sc) = (repo.clone(), scope.clone());
    let foreign = attempt!(db, |txn| rp.add_settlement_deltas(
        txn,
        &sc,
        tenant,
        "p",
        &[(
            SettlementCounter::Fee,
            &PostedMoney::try_new(
                parse_decimal("0.01").unwrap(),
                CurrencySpec::try_new("EUR".into(), 3).unwrap(),
            )
            .unwrap(),
        )],
    ));
    assert!(
        matches!(foreign, Err(RepoError::Money(MoneyError::ScaleMismatch))),
        "{foreign:?}"
    );
    let (rp, sc) = (repo.clone(), scope.clone());
    attempt!(db, |txn| rp.add_settlement_deltas(
        txn,
        &sc,
        tenant,
        "p",
        &[]
    ))
    .unwrap();
    let after = repo
        .read_settlement(&scope, tenant, "p")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after, state, "refused and empty batches write nothing");
}

#[tokio::test]
async fn several_invoices_are_bumped_with_one_read_and_duplicates_fall_back() {
    let (repo, db, tenant) = setup().await;
    let scope = AccessScope::allow_all();
    let (rp, sc) = (repo.clone(), scope.clone());
    attempt!(db, |txn| rp.bump_allocation_refund(
        txn,
        &sc,
        tenant,
        "p",
        "inv-a",
        &money("1")
    ))
    .unwrap();
    let (rp, sc) = (repo.clone(), scope.clone());
    let (to_a, to_b, to_c) = (money("2"), money("0.5"), money("0.25"));
    attempt!(db, |txn| rp.bump_allocation_refunds(
        txn,
        &sc,
        tenant,
        "p",
        &[("inv-a", &to_a), ("inv-b", &to_b), ("inv-c", &to_c)],
    ))
    .unwrap();
    let read = |invoice: &'static str| {
        let (repo, scope) = (repo.clone(), scope.clone());
        async move {
            repo.read_allocation_refund(&scope, tenant, "p", invoice)
                .await
                .unwrap()
                .unwrap()
        }
    };
    let grain_a = read("inv-a").await;
    assert_eq!(
        (grain_a.allocated, grain_a.version),
        (money("3"), 1),
        "existing grain CAS"
    );
    assert_eq!(
        read("inv-b").await.allocated,
        money("0.5"),
        "missing grain inserted"
    );
    assert_eq!(read("inv-c").await.allocated, money("0.25"));
    // The same invoice twice: each step sees the previous one.
    let (rp, sc) = (repo.clone(), scope.clone());
    let (first, second) = (money("0.1"), money("0.2"));
    attempt!(db, |txn| rp.bump_allocation_refunds(
        txn,
        &sc,
        tenant,
        "p",
        &[("inv-b", &first), ("inv-b", &second)],
    ))
    .unwrap();
    assert_eq!(read("inv-b").await.allocated, money("0.8"));
    // A split below zero is refused as a cap, and the batch rolls back.
    let (rp, sc) = (repo.clone(), scope.clone());
    let (ok, bad) = (money("1"), money("-1"));
    let refused = attempt!(db, |txn| rp.bump_allocation_refunds(
        txn,
        &sc,
        tenant,
        "p",
        &[("inv-a", &ok), ("inv-c", &bad)],
    ));
    assert!(
        matches!(refused, Err(RepoError::MoneyOutCapExceeded(_))),
        "{refused:?}"
    );
    assert_eq!(read("inv-a").await.allocated, money("3"), "rolled back");
}
