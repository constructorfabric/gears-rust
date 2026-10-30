//! Fast SQLite tests for currency-scale resolution and the registration
//! scale guard. An ISO 4217 code always has its ISO scale (the registry never
//! overrides it); a non-ISO code takes its registry row; an unknown code is an
//! error. A scale beyond the ledger's 28-digit decimal limit is rejected at upsert.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic
)]

use bss_ledger::domain::model::{CurrencyScaleRow, RepoError};
use bss_ledger::domain::money::ScaleError;
use bss_ledger::infra::currency_scale::CurrencyScaleResolver;
use bss_ledger::infra::storage::entity::currency_scale_registry;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::ReferenceRepo;
use sea_orm::ActiveValue::Set;
use sea_orm::EntityTrait;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::{AccessScope, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

#[tokio::test]
async fn resolves_iso_and_registry_codes_and_rejects_unknown_and_overflow() {
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

#[tokio::test]
async fn iso_scale_comes_from_the_shared_table_and_the_registry_never_overrides_it() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrator");
    let reference = ReferenceRepo::new(DBProvider::<DbError>::new(db.clone()));
    let resolver = CurrencyScaleResolver::new(reference.clone());
    let conn = db.conn().expect("connection");

    let tenant = Uuid::now_v7();
    let other_tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let other_scope = AccessScope::for_tenant(other_tenant);

    // HUF has two minor digits in ISO 4217; BIF and CLF were not in the old ledger table.
    for (code, scale) in [("HUF", 2), ("BIF", 0), ("CLF", 4), ("UYW", 4), ("XOF", 0)] {
        assert_eq!(
            resolver.resolve(&scope, tenant, code).await.unwrap(),
            scale,
            "an ISO default is wrong"
        );
        assert_eq!(
            resolver
                .resolve_in(&conn, &scope, tenant, code)
                .await
                .unwrap(),
            scale,
            "an ISO default is wrong inside a transaction snapshot"
        );
    }

    // A code with no minor unit has no implicit scale.
    for code in ["XAU", "XXX", "eur"] {
        let error = resolver.resolve(&scope, tenant, code).await.unwrap_err();
        assert!(matches!(error, ScaleError::UnknownCurrencyScale(_)));
    }

    // The registry refuses an ISO code at another scale, and names the ISO scale.
    let refused = reference
        .upsert_currency_scale(row(tenant, "HUF", 0))
        .await
        .unwrap_err();
    match refused {
        RepoError::InvalidRequest(message) => {
            assert_eq!(
                message,
                "currency HUF is an ISO 4217 code with scale 2; \
                 the registry cannot set scale 0 for it",
                "the refusal names the code, its ISO scale and the refused scale"
            );
        }
        _ => panic!("an ISO code at another scale must be refused"),
    }
    assert!(
        reference
            .find_currency_scale(&scope, tenant, "HUF")
            .await
            .unwrap()
            .is_none(),
        "the refused row was never written"
    );
    // The ISO scale itself is accepted, again and again.
    reference
        .upsert_currency_scale(row(tenant, "HUF", 2))
        .await
        .unwrap();
    reference
        .upsert_currency_scale(row(tenant, "HUF", 2))
        .await
        .unwrap();
    assert_eq!(resolver.resolve(&scope, tenant, "HUF").await.unwrap(), 2);

    // A non-ISO code takes its registry row, for its tenant only.
    reference
        .upsert_currency_scale(row(tenant, "XTS", 6))
        .await
        .unwrap();
    assert_eq!(resolver.resolve(&scope, tenant, "XTS").await.unwrap(), 6);
    assert_eq!(
        resolver
            .resolve_in(&conn, &scope, tenant, "XTS")
            .await
            .unwrap(),
        6
    );
    assert!(matches!(
        resolver.resolve(&other_scope, other_tenant, "XTS").await,
        Err(ScaleError::UnknownCurrencyScale(_))
    ));

    // A row that contradicts ISO and got into the table another way (an old
    // database, a raw insert) is ignored: the ISO scale holds for every tenant.
    let stale = currency_scale_registry::ActiveModel {
        tenant_id: Set(other_tenant),
        currency: Set("HUF".to_owned()),
        currency_scale: Set(0),
        source: Set("test".to_owned()),
    };
    currency_scale_registry::Entity::insert(stale.clone())
        .secure()
        .scope_with_model(&other_scope, &stale)
        .unwrap()
        .exec(&conn)
        .await
        .unwrap();
    assert_eq!(
        reference
            .find_currency_scale(&other_scope, other_tenant, "HUF")
            .await
            .unwrap()
            .map(|stored| stored.currency_scale),
        Some(0),
        "the contradicting row is stored"
    );
    assert_eq!(
        resolver
            .resolve(&other_scope, other_tenant, "HUF")
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        resolver
            .resolve_in(&conn, &other_scope, other_tenant, "HUF")
            .await
            .unwrap(),
        2
    );
}

fn row(tenant_id: Uuid, currency: &str, currency_scale: u8) -> CurrencyScaleRow {
    CurrencyScaleRow {
        tenant_id,
        currency: currency.to_owned(),
        currency_scale,
        source: "tenant".to_owned(),
    }
}
