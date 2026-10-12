//! Fast SQLite tests for currency-scale resolution and the registration
//! scale guard. ISO default vs registry override vs unknown, plus a scale
//! beyond the ledger's 28-digit decimal limit rejected at upsert.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use bss_ledger::domain::model::{CurrencyScaleRow, RepoError};
use bss_ledger::domain::money::ScaleError;
use bss_ledger::infra::currency_scale::CurrencyScaleResolver;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::ReferenceRepo;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

#[tokio::test]
async fn resolves_iso_override_and_rejects_unknown_and_overflow() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrator");
    let reference = ReferenceRepo::new(DBProvider::<DbError>::new(db));
    let resolver = CurrencyScaleResolver::new(reference.clone());

    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);

    // ISO default, no registry row.
    assert_eq!(resolver.resolve(&scope, tenant, "USD").await.unwrap(), 2);

    // Non-ISO currency, scale within the supported range, via the registry.
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "USDC".to_owned(),
            currency_scale: 6,
            source: "tenant".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(resolver.resolve(&scope, tenant, "USDC").await.unwrap(), 6);

    // High-precision crypto (BTC@8) registers at its own scale; the only limit is
    // the 28-digit coefficient bound shared by every currency.
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "BTC".to_owned(),
            currency_scale: 8,
            source: "tenant".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(resolver.resolve(&scope, tenant, "BTC").await.unwrap(), 8);

    // Non-ISO, no row -> unknown.
    let unknown = resolver.resolve(&scope, tenant, "ZZZ").await.unwrap_err();
    assert!(matches!(unknown, ScaleError::UnknownCurrencyScale(_)));

    // A scale above the supported 0..=28 range is rejected at registration.
    let overflow = reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "ETH".to_owned(),
            currency_scale: 29,
            source: "tenant".to_owned(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(overflow, RepoError::ScaleOutOfRange(_)),
        "scale 29 must be rejected: {overflow:?}"
    );
    // The rejected row was never written: the currency stays unknown.
    let unknown = resolver.resolve(&scope, tenant, "ETH").await.unwrap_err();
    assert!(matches!(unknown, ScaleError::UnknownCurrencyScale(_)));
}
