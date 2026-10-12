//! SQLite read-side guards SQLite has no constraint for: an AR invoice row with a
//! disputed amount above its balance, and a line locked to a snapshot that does
//! not exist.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sea_orm::{EntityTrait, IntoActiveModel};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{AccessScope, Db, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

use super::JournalRepo;
use crate::domain::model::RepoError;
use crate::infra::storage::entity::{ar_invoice_balance, journal_entry, journal_line};

async fn setup() -> (JournalRepo, Db) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    (JournalRepo::new(DBProvider::new(db.clone())), db)
}

/// Insert one row through the secure scope, as every repository does.
macro_rules! insert {
    ($db:expr, $entity:path, $model:expr) => {{
        let am = $model.into_active_model();
        <$entity>::insert(am.clone())
            .secure()
            .scope_with_model(&AccessScope::allow_all(), &am)
            .unwrap()
            .exec(&$db.conn().unwrap())
            .await
            .unwrap();
    }};
}

fn invoice_row(tenant: Uuid, balance: &str, disputed: &str) -> ar_invoice_balance::Model {
    ar_invoice_balance::Model {
        tenant_id: tenant,
        payer_tenant_id: tenant,
        account_id: Uuid::now_v7(),
        invoice_id: "inv-1".to_owned(),
        currency: "EUR".to_owned(),
        currency_scale: 2,
        balance: balance.to_owned(),
        disputed: disputed.to_owned(),
        functional_balance: None,
        functional_currency: None,
        functional_currency_scale: None,
        original_posted_at: None,
        due_date: None,
        last_entry_seq: None,
        version: 0,
    }
}

#[tokio::test]
async fn disputed_above_balance_is_invalid_stored_money_on_the_list_read() {
    let (repo, db) = setup().await;
    let tenant = Uuid::now_v7();
    insert!(
        db,
        ar_invoice_balance::Entity,
        invoice_row(tenant, "10", "10")
    );
    let scope = AccessScope::for_tenant(tenant);
    assert_eq!(
        repo.list_ar_invoice_balances(&scope, tenant, None)
            .await
            .unwrap()
            .len(),
        1,
        "disputed equal to the balance is valid"
    );
    let other = Uuid::now_v7();
    insert!(
        db,
        ar_invoice_balance::Entity,
        invoice_row(other, "10", "10.01")
    );
    assert!(matches!(
        repo.list_ar_invoice_balances(&AccessScope::for_tenant(other), other, None)
            .await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
}

#[tokio::test]
async fn a_dangling_rate_snapshot_lock_fails_closed() {
    let (repo, db) = setup().await;
    let tenant = Uuid::now_v7();
    let entry_id = Uuid::now_v7();
    insert!(
        db,
        journal_entry::Entity,
        journal_entry::Model {
            entry_id,
            tenant_id: tenant,
            legal_entity_id: tenant,
            period_id: "202606".to_owned(),
            entry_currency: "EUR".to_owned(),
            source_doc_type: "REFUND".to_owned(),
            source_business_id: "refund-1".to_owned(),
            reverses_entry_id: None,
            reverses_period_id: None,
            posted_at_utc: time::OffsetDateTime::now_utc(),
            effective_at: chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
            origin: "SYSTEM".to_owned(),
            posted_by_actor_id: tenant,
            correlation_id: tenant,
            rounding_evidence: serde_json::Value::Null,
            created_seq: 1,
            row_hash: None,
            prev_hash: None,
            prev_entry_id: None,
            prev_period_id: None,
        }
    );
    let mut line = locked_line(tenant, entry_id);
    // SQLite keeps no FK on the lock: point it at a snapshot that never existed.
    line.rate_snapshot_ref = Some(Uuid::now_v7());
    insert!(db, journal_line::Entity, line);
    let scope = AccessScope::for_tenant(tenant);
    assert!(matches!(
        repo.locked_rate_evidence_for(&scope, tenant, "refund-1", "REFUND")
            .await,
        Err(RepoError::InvalidStoredMoney(detail)) if detail.contains("snapshot missing")
    ));
    // An unknown operation and a line without a lock stay `None`.
    assert!(
        repo.locked_rate_evidence_for(&scope, tenant, "absent", "REFUND")
            .await
            .unwrap()
            .is_none()
    );
}

/// A minimal valid cross-currency journal line of `entry_id`.
fn locked_line(tenant: Uuid, entry_id: Uuid) -> journal_line::Model {
    journal_line::Model {
        line_id: Uuid::now_v7(),
        entry_id,
        tenant_id: tenant,
        period_id: "202606".to_owned(),
        payer_tenant_id: tenant,
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::now_v7(),
        account_class: "AR".to_owned(),
        gl_code: None,
        side: "CR".to_owned(),
        amount: "10".to_owned(),
        currency: "EUR".to_owned(),
        currency_scale: 2,
        invoice_id: None,
        due_date: None,
        revenue_stream: None,
        mapping_status: "RESOLVED".to_owned(),
        functional_amount: Some("11".to_owned()),
        functional_currency: Some("USD".to_owned()),
        functional_currency_scale: Some(2),
        tax_jurisdiction: None,
        tax_filing_period: None,
        tax_rate_ref: None,
        legal_entity_id: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
        po_allocation_group: None,
        credit_grant_event_type: None,
        ar_status: None,
        rate_snapshot_ref: None,
    }
}
