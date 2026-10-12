//! Fresh SQLite application-enforced decimal refund caps.
//! Original scale-2 fixtures retain their value: 1000 cents is 10 EUR.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_ledger::domain::model::RepoError;
use bss_ledger::infra::storage::{migrations::Migrator, repo::PaymentRepo};
use bss_ledger_sdk::{CurrencySpec, PostedMoney, parse_decimal};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{AccessScope, TxConfig};
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Repo(#[from] RepoError),
}
fn money(text: &str) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new("EUR".into(), 2).unwrap(),
    )
    .unwrap()
}
async fn setup(allocated: &str) -> (DBProvider<DbError>, PaymentRepo, Uuid) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .unwrap();
    let provider = DBProvider::new(db.clone());
    let repo = PaymentRepo::new(provider.clone());
    let tenant = Uuid::now_v7();
    let r = repo.clone();
    let allocated = money(allocated);
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
        Box::pin(async move {
            let scope = AccessScope::for_tenant(tenant);
            r.seed_settlement(txn, &scope, tenant, "pay-1", &money("10"), &money("0"))
                .await?;
            r.add_allocated(txn, &scope, tenant, "pay-1", &allocated)
                .await?;
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();
    (provider, repo, tenant)
}
#[derive(Clone, Copy)]
enum Counter {
    Refunded,
    Unallocated,
    Invoice,
    AllocateInvoice,
}
async fn apply(
    provider: &DBProvider<DbError>,
    repo: &PaymentRepo,
    tenant: Uuid,
    counter: Counter,
    delta: &str,
) -> Result<(), TestError> {
    let repo = repo.clone();
    let delta = money(delta);
    provider
        .db()
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
            Box::pin(async move {
                let s = AccessScope::for_tenant(tenant);
                match counter {
                    Counter::Refunded => {
                        repo.add_refunded(txn, &s, tenant, "pay-1", &delta).await?;
                    }
                    Counter::Unallocated => {
                        repo.add_refunded_unallocated(txn, &s, tenant, "pay-1", &delta)
                            .await?;
                    }
                    Counter::Invoice => {
                        repo.add_allocation_refund_refunded(
                            txn, &s, tenant, "pay-1", "inv-9", &delta,
                        )
                        .await?;
                    }
                    Counter::AllocateInvoice => {
                        repo.bump_allocation_refund(txn, &s, tenant, "pay-1", "inv-9", &delta)
                            .await?;
                    }
                }
                Ok(())
            })
        })
        .await
}
#[tokio::test]
async fn add_refunded_total_moneyout_cap_blocks_over_settled() {
    let (p, r, t) = setup("0").await;
    apply(&p, &r, t, Counter::Refunded, "6").await.unwrap();
    assert!(matches!(
        apply(&p, &r, t, Counter::Refunded, "5").await,
        Err(TestError::Repo(RepoError::MoneyOutCapExceeded(_)))
    ));
    apply(&p, &r, t, Counter::Refunded, "4").await.unwrap();
    assert!(matches!(
        apply(&p, &r, t, Counter::Refunded, "0.01").await,
        Err(TestError::Repo(RepoError::MoneyOutCapExceeded(_)))
    ));
    let state = r
        .read_settlement(&AccessScope::for_tenant(t), t, "pay-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.refunded, money("10"));
    assert_eq!(state.version, 3);
}
#[tokio::test]
async fn add_refunded_decrement_reopens_cap() {
    let (p, r, t) = setup("0").await;
    apply(&p, &r, t, Counter::Refunded, "10").await.unwrap();
    assert!(matches!(
        apply(&p, &r, t, Counter::Refunded, "0.01").await,
        Err(TestError::Repo(RepoError::MoneyOutCapExceeded(_)))
    ));
    apply(&p, &r, t, Counter::Refunded, "-10").await.unwrap();
    apply(&p, &r, t, Counter::Refunded, "10").await.unwrap();
}
#[tokio::test]
async fn add_refunded_unallocated_headroom_cap_blocks_when_allocated() {
    let (p, r, t) = setup("7").await;
    apply(&p, &r, t, Counter::Unallocated, "3").await.unwrap();
    assert!(matches!(
        apply(&p, &r, t, Counter::Unallocated, "0.01").await,
        Err(TestError::Repo(RepoError::MoneyOutCapExceeded(_)))
    ));
    assert_eq!(
        r.read_settlement(&AccessScope::for_tenant(t), t, "pay-1")
            .await
            .unwrap()
            .unwrap()
            .refunded_unallocated,
        money("3")
    );
}
#[tokio::test]
async fn add_allocation_refund_per_invoice_cap_blocks_over_allocated() {
    let (p, r, t) = setup("8").await;
    apply(&p, &r, t, Counter::AllocateInvoice, "8")
        .await
        .unwrap();
    apply(&p, &r, t, Counter::Invoice, "8").await.unwrap();
    assert!(matches!(
        apply(&p, &r, t, Counter::Invoice, "0.01").await,
        Err(TestError::Repo(RepoError::MoneyOutCapExceeded(_)))
    ));
    apply(&p, &r, t, Counter::Invoice, "-8").await.unwrap();
    apply(&p, &r, t, Counter::Invoice, "8").await.unwrap();
}
#[tokio::test]
async fn add_allocation_refund_absent_row_is_db_error() {
    let (p, r, t) = setup("0").await;
    assert!(matches!(
        apply(&p, &r, t, Counter::Invoice, "1").await,
        Err(TestError::Repo(RepoError::Db(_)))
    ));
}
