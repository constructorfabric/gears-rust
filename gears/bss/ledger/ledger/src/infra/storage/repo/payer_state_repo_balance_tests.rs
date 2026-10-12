//! The outstanding-balance probe over canonical AR text: a fully paid payer's
//! grain is exactly "0" and is not outstanding.
#![allow(clippy::unwrap_used)]

use sea_orm::{EntityTrait, IntoActiveModel};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{AccessScope, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

use super::PayerStateRepo;
use crate::infra::storage::entity::ar_payer_balance;

#[tokio::test]
async fn a_zero_ar_grain_is_not_outstanding_and_any_other_balance_is() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let repo = PayerStateRepo::new(DBProvider::new(db.clone()));
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    for (balance, outstanding) in [
        (None, false),
        (Some("0"), false),
        (Some("5"), true),
        (Some("0.01"), true),
    ] {
        let payer = Uuid::now_v7();
        if let Some(balance) = balance {
            let row = ar_payer_balance::Model {
                tenant_id: tenant,
                payer_tenant_id: payer,
                account_id: Uuid::now_v7(),
                currency: "EUR".to_owned(),
                currency_scale: 2,
                balance: balance.to_owned(),
                functional_balance: None,
                functional_currency: None,
                functional_currency_scale: None,
                last_entry_seq: None,
                version: 0,
            }
            .into_active_model();
            ar_payer_balance::Entity::insert(row.clone())
                .secure()
                .scope_with_model(&AccessScope::allow_all(), &row)
                .unwrap()
                .exec(&db.conn().unwrap())
                .await
                .unwrap();
        }
        assert_eq!(
            repo.has_outstanding_balance(&scope, tenant, payer)
                .await
                .unwrap(),
            outstanding,
            "{balance:?}"
        );
    }
}
