//! Real migrated SQLite baseline replacement, corruption and rollback checks.
use super::*;
use crate::infra::posting::retry::AttemptError;
use bss_ledger_sdk::{CurrencySpec, parse_decimal};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, TxConfig};
use toolkit_db::{ConnectOpts, connect_db};

async fn setup() -> (VerifiedBalanceRepo, Db, Uuid) {
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
        VerifiedBalanceRepo::new(DBProvider::new(db.clone())),
        db,
        Uuid::now_v7(),
    )
}
fn row(code: &str, scale: u8, amount: &str) -> BaselineRow {
    BaselineRow {
        grain: "account".into(),
        grain_key: format!("account|{code}"),
        balance: PostedMoney::try_new(
            parse_decimal(amount).unwrap(),
            CurrencySpec::try_new(code.into(), scale).unwrap(),
        )
        .unwrap(),
    }
}
async fn snapshot(
    repo: &VerifiedBalanceRepo,
    db: &Db,
    tenant: Uuid,
    rows: Vec<BaselineRow>,
) -> Result<(), AttemptError> {
    let repo = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            repo.snapshot(
                tx,
                &AccessScope::for_tenant(tenant),
                tenant,
                "2026-10",
                12,
                &rows,
            )
            .await?;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn signed_absolute_replacement_canonical_extremes_and_metadata() {
    let (repo, db, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    snapshot(
        &repo,
        &db,
        tenant,
        vec![
            row("EUR", 2, "-12.34"),
            row("JPY", 0, "9999999999999999999999999999"),
            row("BTC", 28, "0.0000000000000000000000000001"),
        ],
    )
    .await
    .unwrap();
    snapshot(&repo, &db, tenant, vec![row("EUR", 2, "0.09")])
        .await
        .unwrap();
    let conn = db.conn().unwrap();
    let found = repo.load_baseline(&conn, &scope, tenant).await.unwrap();
    let eur = found
        .iter()
        .find(|r| r.balance.currency().code() == "EUR")
        .unwrap();
    assert_eq!(encode_amount(&eur.balance), "0.09");
    assert_eq!(eur.version, 1);
    assert_eq!(eur.watermark_seq, 12);
    assert_eq!(found.len(), 3);
    for (code, text, scale) in [
        ("JPY", "9999999999999999999999999999", 0),
        ("BTC", "0.0000000000000000000000000001", 28),
    ] {
        let extreme = found
            .iter()
            .find(|r| r.balance.currency().code() == code)
            .unwrap();
        assert_eq!(encode_amount(&extreme.balance), text, "{code}");
        assert_eq!(extreme.balance.currency().scale(), scale, "{code}");
        assert_eq!(extreme.version, 0, "{code}");
    }
    assert!(matches!(
        snapshot(&repo, &db, tenant, vec![row("EUR", 3, "0.001")]).await,
        Err(AttemptError::Business(
            crate::domain::error::DomainError::InconsistentScale(_)
        ))
    ));
    assert_eq!(
        repo.load_baseline(&conn, &scope, tenant)
            .await
            .unwrap()
            .len(),
        3
    );
    assert!(
        repo.load_baseline(&conn, &AccessScope::for_tenant(Uuid::now_v7()), tenant)
            .await
            .unwrap()
            .is_empty()
    );
    let raw = verified_balance::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(key(tenant, "account", "account|EUR"))
        .one(&conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raw.verified_balance, "0.09");
}

#[tokio::test]
async fn stale_cas_and_missing_grain_race_roll_back_prior_rows() {
    let (repo, db, tenant) = setup().await;
    snapshot(&repo, &db, tenant, vec![row("EUR", 2, "-0.01")])
        .await
        .unwrap();
    let rp = repo.clone();
    let outcome: Result<(), AttemptError> = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant);
                let stale = rp.load_baseline(tx, &scope, tenant).await?.remove(0);
                rp.snapshot(
                    tx,
                    &scope,
                    tenant,
                    "next",
                    20,
                    &[row("EUR", 2, "0.09"), row("JPY", 0, "7")],
                )
                .await?;
                rp.replace_observed(tx, &scope, &stale, &row("EUR", 2, "0.1"), "next", 21)
                    .await?;
                Ok(())
            })
        })
        .await;
    assert!(matches!(outcome, Err(AttemptError::Conflict)));
    let found = repo
        .load_baseline(
            &db.conn().unwrap(),
            &AccessScope::for_tenant(tenant),
            tenant,
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].version, 0);
    assert_eq!(encode_amount(&found[0].balance), "-0.01");
    let rp = repo.clone();
    let outcome: Result<(), AttemptError> = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant);
                rp.snapshot(tx, &scope, tenant, "next", 20, &[row("JPY", 0, "7")])
                    .await?;
                rp.seed(tx, &scope, tenant, &row("EUR", 2, "5"), "next", 20)
                    .await?;
                Ok(())
            })
        })
        .await;
    assert!(matches!(outcome, Err(AttemptError::Conflict)));
    assert_eq!(
        repo.load_baseline(
            &db.conn().unwrap(),
            &AccessScope::for_tenant(tenant),
            tenant
        )
        .await
        .unwrap()
        .len(),
        1
    );
}

#[tokio::test]
async fn corruption_and_version_overflow_are_not_repaired() {
    let (repo, db, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    snapshot(&repo, &db, tenant, vec![row("EUR", 2, "1")])
        .await
        .unwrap();
    for (column, value) in [
        (C::VerifiedBalance, Expr::value("1.00")),
        (C::VerifiedBalance, Expr::value("-0")),
        (C::VerifiedBalance, Expr::value("bogus")),
    ] {
        verified_balance::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(column, value)
            .exec(&db.conn().unwrap())
            .await
            .unwrap();
        assert!(matches!(
            repo.load_baseline(&db.conn().unwrap(), &scope, tenant)
                .await,
            Err(RepoError::InvalidStoredMoney(_))
        ));
        assert!(matches!(
            snapshot(&repo, &db, tenant, vec![row("EUR", 2, "2")]).await,
            Err(AttemptError::Business(
                crate::domain::error::DomainError::Internal(_)
            ))
        ));
    }
    verified_balance::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(C::VerifiedBalance, Expr::value("1"))
        .col_expr(C::Version, Expr::value(i64::MAX))
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        snapshot(&repo, &db, tenant, vec![row("EUR", 2, "2")]).await,
        Err(AttemptError::Business(
            crate::domain::error::DomainError::Internal(_)
        ))
    ));
    assert_eq!(
        repo.load_baseline(&db.conn().unwrap(), &scope, tenant)
            .await
            .unwrap()[0]
            .version,
        i64::MAX
    );
}

#[tokio::test]
async fn invalid_keys_duplicates_and_foreign_scope_fail_without_writes() {
    let (repo, db, tenant) = setup().await;
    let good = row("EUR", 2, "1");
    assert!(
        snapshot(&repo, &db, tenant, vec![good.clone(), good.clone()])
            .await
            .is_err()
    );
    let mut bad = good.clone();
    bad.grain = "unknown".into();
    // Rejected by the repository before any write, not by the grain CHECK.
    assert!(matches!(
        snapshot(&repo, &db, tenant, vec![bad]).await,
        Err(AttemptError::Business(
            crate::domain::error::DomainError::InvalidRequest(_)
        ))
    ));
    let mut bad = good.clone();
    bad.grain_key = "account|JPY".into();
    assert!(snapshot(&repo, &db, tenant, vec![bad]).await.is_err());
    let rp = repo.clone();
    let result: Result<(), AttemptError> = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                rp.snapshot(
                    tx,
                    &AccessScope::for_tenant(Uuid::now_v7()),
                    tenant,
                    "p",
                    1,
                    &[good],
                )
                .await?;
                Ok(())
            })
        })
        .await;
    assert!(result.is_err());
    assert!(
        repo.load_baseline(
            &db.conn().unwrap(),
            &AccessScope::for_tenant(tenant),
            tenant
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn non_currency_key_metadata_mismatch_and_corruption_abort_whole_snapshot() {
    let (repo, db, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    let mut invoice = row("EUR", 2, "-4.25");
    invoice.grain = "ar_invoice".into();
    invoice.grain_key = "payer|account|invoice".into();
    snapshot(&repo, &db, tenant, vec![invoice.clone()])
        .await
        .unwrap();
    let mut changed = invoice.clone();
    changed.balance = row("JPY", 0, "9").balance;
    assert!(matches!(
        snapshot(&repo, &db, tenant, vec![row("GBP", 2, "1"), changed]).await,
        Err(AttemptError::Business(
            crate::domain::error::DomainError::CurrencyMismatch(_)
        ))
    ));
    assert_eq!(
        repo.load_baseline(&db.conn().unwrap(), &scope, tenant)
            .await
            .unwrap()
            .len(),
        1
    );
    for (column, value) in [
        (C::Currency, Expr::value("eur")),
        (C::VerifiedBalance, Expr::value("0.001")),
        (C::Version, Expr::value(-1_i64)),
    ] {
        // Restore the isolated fixture's canonical metadata before the next independent corruption.
        verified_balance::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(C::Currency, Expr::value("EUR"))
            .col_expr(C::CurrencyScale, Expr::value(2_i16))
            .col_expr(C::VerifiedBalance, Expr::value("-4.25"))
            .col_expr(C::Version, Expr::value(0_i64))
            .col_expr(C::WatermarkSeq, Expr::value(12_i64))
            .col_expr(column, value)
            .exec(&db.conn().unwrap())
            .await
            .unwrap();
        assert!(matches!(
            repo.load_baseline(&db.conn().unwrap(), &scope, tenant)
                .await,
            Err(RepoError::InvalidStoredMoney(_))
        ));
        assert!(matches!(
            snapshot(
                &repo,
                &db,
                tenant,
                vec![row("GBP", 2, "1"), invoice.clone()]
            )
            .await,
            Err(AttemptError::Business(
                crate::domain::error::DomainError::Internal(_)
            ))
        ));
        assert!(
            verified_balance::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(key(tenant, "account", "account|GBP"))
                .one(&db.conn().unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn the_key_builders_put_the_currency_where_the_decoder_reads_it() {
    use verified_balance::{
        GRAIN_ACCOUNT, GRAIN_AR_INVOICE, GRAIN_AR_INVOICE_DISPUTED, GRAIN_AR_PAYER,
        GRAIN_REUSABLE_CREDIT, GRAIN_TAX, GRAIN_UNALLOCATED,
    };
    let (payer, account) = (Uuid::now_v7(), Uuid::now_v7());
    let eur = row("EUR", 2, "1").balance;
    let usd = row("USD", 2, "1").balance;
    for (grain, grain_key) in [
        (GRAIN_ACCOUNT, key_account(account, "EUR")),
        (GRAIN_AR_PAYER, key_payer_account_ccy(payer, account, "EUR")),
        (
            GRAIN_UNALLOCATED,
            key_payer_account_ccy(payer, account, "EUR"),
        ),
        (
            GRAIN_REUSABLE_CREDIT,
            key_reusable(payer, account, "EUR", "PROMO"),
        ),
    ] {
        assert_eq!(validate_key(grain, &grain_key, &eur), Ok(()), "{grain}");
        assert!(validate_key(grain, &grain_key, &usd).is_err(), "{grain}");
    }
    // Keys without a currency accept any currency metadata.
    for (grain, grain_key) in [
        (
            GRAIN_AR_INVOICE,
            key_payer_account_invoice(payer, account, "INV-1"),
        ),
        (
            GRAIN_AR_INVOICE_DISPUTED,
            key_payer_account_invoice(payer, account, "INV-1"),
        ),
        (GRAIN_TAX, key_tax(account, "DE", "2026Q2")),
    ] {
        assert_eq!(validate_key(grain, &grain_key, &usd), Ok(()), "{grain}");
    }
}
