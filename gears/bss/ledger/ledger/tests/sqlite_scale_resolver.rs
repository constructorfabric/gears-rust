//! Fast SQLite tests for currency-scale resolution and the registration
//! headroom guard. ISO default vs registry override vs unknown, plus an
//! out-of-headroom scale rejected at upsert.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use bss_ledger::domain::model::{CurrencyScaleRow, RepoError};
use bss_ledger::domain::money::{DEFAULT_PLAUSIBLE_MAX_MAJOR, ScaleError};
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

    // Non-ISO currency, scale within the default headroom, via the registry.
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "USDC".to_owned(),
            minor_units: 6,
            plausible_max_major: DEFAULT_PLAUSIBLE_MAX_MAJOR,
            source: "tenant".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(resolver.resolve(&scope, tenant, "USDC").await.unwrap(), 6);

    // High-precision crypto (BTC@8) fits under a smaller per-currency max
    // (21_000_000 major units): 2.1e7 * 10^8 = 2.1e15 <= i64::MAX (VHP-1834).
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "BTC".to_owned(),
            minor_units: 8,
            plausible_max_major: 21_000_000,
            source: "tenant".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(resolver.resolve(&scope, tenant, "BTC").await.unwrap(), 8);

    // Non-ISO, no row -> unknown.
    let unknown = resolver.resolve(&scope, tenant, "ZZZ").await.unwrap_err();
    assert!(matches!(unknown, ScaleError::UnknownCurrencyScale(_)));

    // Out-of-headroom scale rejected at registration: scale 8 under the
    // default 10^12 max overflows (10^12 * 10^8 = 10^20 > i64::MAX).
    let overflow = reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "ETH".to_owned(),
            minor_units: 8,
            plausible_max_major: DEFAULT_PLAUSIBLE_MAX_MAJOR,
            source: "tenant".to_owned(),
        })
        .await
        .unwrap_err();
    assert!(matches!(overflow, RepoError::ScaleOutOfRange(_)));
}
