//! First registration of a currency scale against posted history, on SQLite.
#![allow(clippy::unwrap_used)]

use sea_orm::{EntityTrait, IntoActiveModel};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{AccessScope, Db, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

use super::ReferenceRepo;
use crate::domain::model::{CurrencyScaleRow, RepoError};
use crate::infra::storage::entity::{journal_entry, journal_line};

async fn setup() -> (ReferenceRepo, Db) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    (ReferenceRepo::new(DBProvider::new(db.clone())), db)
}

/// Seed one posted EUR@2 line whose functional money is JPY@0.
async fn seed_line(db: &Db, tenant: Uuid) {
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let entry_id = Uuid::now_v7();
    let entry = journal_entry::Model {
        entry_id,
        tenant_id: tenant,
        legal_entity_id: tenant,
        period_id: "202606".to_owned(),
        entry_currency: "EUR".to_owned(),
        source_doc_type: "MANUAL_ADJUSTMENT".to_owned(),
        source_business_id: "seed".to_owned(),
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
    .into_active_model();
    journal_entry::Entity::insert(entry.clone())
        .secure()
        .scope_with_model(&scope, &entry)
        .unwrap()
        .exec(&conn)
        .await
        .unwrap();
    let line = journal_line::Model {
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
        side: "DR".to_owned(),
        amount: "10".to_owned(),
        currency: "EUR".to_owned(),
        currency_scale: 2,
        invoice_id: None,
        due_date: None,
        revenue_stream: None,
        mapping_status: "RESOLVED".to_owned(),
        functional_amount: Some("1500".to_owned()),
        functional_currency: Some("JPY".to_owned()),
        functional_currency_scale: Some(0),
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
    .into_active_model();
    journal_line::Entity::insert(line.clone())
        .secure()
        .scope_with_model(&scope, &line)
        .unwrap()
        .exec(&conn)
        .await
        .unwrap();
}

/// Run one first registration in its own transaction.
async fn register(
    repo: &ReferenceRepo,
    db: &Db,
    tenant: Uuid,
    currency: &str,
    scale: u8,
) -> Result<bool, RepoError> {
    let repo = repo.clone();
    let row = CurrencyScaleRow {
        tenant_id: tenant,
        currency: currency.to_owned(),
        currency_scale: scale,
        source: "test".to_owned(),
    };
    let outcome = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = outcome.clone();
    let _ = db
        .transaction_ref(move |tx| {
            let repo = repo.clone();
            let row = row.clone();
            let slot = slot.clone();
            Box::pin(async move {
                let result = repo.insert_currency_scale_if_absent_txn(tx, row).await;
                let rollback = result.is_err();
                *slot.lock().unwrap() = Some(result);
                if rollback {
                    return Err(toolkit_db::DbError::Other(anyhow::anyhow!("rolled back")));
                }
                Ok::<_, toolkit_db::DbError>(())
            })
        })
        .await;
    outcome.lock().unwrap().take().unwrap()
}

#[tokio::test]
async fn a_first_registration_that_disagrees_with_posted_lines_is_locked() {
    let (repo, db) = setup().await;
    let tenant = Uuid::now_v7();
    seed_line(&db, tenant).await;
    let scope = AccessScope::for_tenant(tenant);
    for (currency, scale) in [("EUR", 3), ("JPY", 2)] {
        assert!(
            matches!(
                register(&repo, &db, tenant, currency, scale).await,
                Err(RepoError::CurrencyScaleLocked(code)) if code == currency
            ),
            "{currency}@{scale}"
        );
        assert!(
            repo.find_currency_scale(&scope, tenant, currency)
                .await
                .unwrap()
                .is_none(),
            "{currency}: no registry row written"
        );
    }
    // The scale the history already uses registers, once.
    assert!(register(&repo, &db, tenant, "EUR", 2).await.unwrap());
    assert!(!register(&repo, &db, tenant, "EUR", 2).await.unwrap());
    assert!(register(&repo, &db, tenant, "JPY", 0).await.unwrap());
    // Another tenant's history does not lock this tenant.
    assert!(
        register(&repo, &db, Uuid::now_v7(), "EUR", 3)
            .await
            .unwrap()
    );
}
