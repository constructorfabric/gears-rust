//! The payer's unallocated balance read: the stored pool at its own scale, or a
//! zero at the registry scale for a payer with no pool row, by the same
//! absent-pool rule as the grant cap. SQLite-backed.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use bss_ledger_sdk::{CurrencySpec, PostedMoney};
use sea_orm::{ActiveValue::Set, EntityTrait};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{AccessScope, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

use super::{CreditApplicationService, unallocated_or_zero};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::NoopLedgerMetrics;
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::storage::entity::unallocated_balance;

async fn service() -> (CreditApplicationService, toolkit_db::secure::Db) {
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
        CreditApplicationService::new(
            DBProvider::new(db.clone()),
            Arc::new(LedgerEventPublisher::noop()),
            Arc::new(NoopLedgerMetrics),
        ),
        db,
    )
}

#[tokio::test]
async fn the_unallocated_balance_is_the_stored_pool_or_a_zero_at_the_registry_scale() {
    let (service, db) = service().await;
    let tenant = Uuid::now_v7();
    let payer = Uuid::now_v7();
    let s = AccessScope::for_tenant(tenant);
    // No pool row: a zero at the currency's registry scale (ISO defaults here).
    for (code, scale) in [("USD", 2), ("JPY", 0)] {
        let zero = service
            .read_unallocated_balance(&s, tenant, payer, code)
            .await
            .unwrap();
        assert!(zero.amount().is_zero(), "{code}");
        assert_eq!(zero.currency().code(), code);
        assert_eq!(zero.currency().scale(), scale, "{code}");
    }
    // An unprovisioned currency is refused, never a fabricated zero.
    assert!(matches!(
        service.read_unallocated_balance(&s, tenant, payer, "ZZZZ").await,
        Err(DomainError::InvalidRequest(detail)) if detail.contains("ZZZZ")
    ));
    // A stored pool comes back with its own stored scale, not the registry's.
    let row = unallocated_balance::ActiveModel {
        tenant_id: Set(tenant),
        payer_tenant_id: Set(payer),
        account_id: Set(Uuid::now_v7()),
        currency: Set("EUR".into()),
        currency_scale: Set(3),
        balance: Set("1.125".into()),
        functional_balance: Set(None),
        functional_currency: Set(None),
        functional_currency_scale: Set(None),
        last_entry_seq: Set(None),
        version: Set(1),
    };
    unallocated_balance::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&s, &row)
        .unwrap()
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    let pool = service
        .read_unallocated_balance(&s, tenant, payer, "EUR")
        .await
        .unwrap();
    assert_eq!(bss_ledger_sdk::canonical_decimal(pool.amount()), "1.125");
    assert_eq!(pool.currency().scale(), 3);
}

/// The one absent-pool rule: a missing pool is a zero in the supplied spec, a
/// stored pool is returned as stored for the caller to compare.
#[test]
fn an_absent_pool_is_a_zero_in_the_supplied_spec() {
    let spec = CurrencySpec::try_new("EUR".to_owned(), 3).unwrap();
    let zero = unallocated_or_zero(None, &spec).unwrap();
    assert!(zero.amount().is_zero());
    assert_eq!(zero.currency(), &spec);
    let stored = PostedMoney::try_new(
        rust_decimal::Decimal::new(5, 0),
        CurrencySpec::try_new("EUR".to_owned(), 2).unwrap(),
    )
    .unwrap();
    assert_eq!(
        unallocated_or_zero(Some(stored.clone()), &spec).unwrap(),
        stored
    );
}
