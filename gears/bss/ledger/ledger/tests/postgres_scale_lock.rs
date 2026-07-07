//! Postgres-only: the scale-immutability guard. Once a journal_line
//! exists for a (tenant, currency), a changed scale is rejected
//! (`CurrencyScaleLocked`); the same scale is idempotent; a currency with
//! no postings is freely (re)scalable. Ignored by default; run with
//! `cargo test -p bss-ledger -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use bss_ledger::domain::model::{CurrencyScaleRow, RepoError};
use bss_ledger::domain::money::DEFAULT_PLAUSIBLE_MAX_MAJOR;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::ReferenceRepo;
use sea_orm::{ConnectionTrait, Database, Statement, TransactionTrait};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn scale_locked_once_postings_exist() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    // sea_orm connection for the migrator + raw seed SQL (bss-qualified).
    let raw = Database::connect(&url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();
    // The repo's connection sets search_path=bss (as the gear config does in
    // prod) so its unqualified entity queries resolve into the bss schema.
    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();

    let tenant = Uuid::now_v7();
    let entry = Uuid::now_v7();
    let reference = ReferenceRepo::new(DBProvider::<DbError>::new(tdb));

    // Register USD@2 and post a balanced USD entry (DR AR / CR CASH_CLEARING).
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "USD".to_owned(),
            minor_units: 2,
            plausible_max_major: DEFAULT_PLAUSIBLE_MAX_MAJOR,
            source: "iso".to_owned(),
        })
        .await
        .unwrap();

    // Seed entry + lines in one transaction so the deferred balance trigger
    // sees both lines at COMMIT (an autocommitted empty header would fail).
    let seed = raw.begin().await.unwrap();
    seed.execute(pg(format!(
        "INSERT INTO bss.ledger_journal_entry
            (entry_id, tenant_id, legal_entity_id, period_id, entry_currency,
             source_doc_type, source_business_id, posted_at_utc, effective_at,
             origin, posted_by_actor_id, correlation_id)
         VALUES ('{entry}','{tenant}','{tenant}','202606','USD',
                 'MANUAL_ADJUSTMENT','biz-1', now(), CURRENT_DATE,
                 'SYSTEM','{tenant}','{tenant}')"
    )))
    .await
    .unwrap();
    for (side, class) in [("DR", "AR"), ("CR", "CASH_CLEARING")] {
        let line = Uuid::now_v7();
        seed.execute(pg(format!(
            "INSERT INTO bss.ledger_journal_line
                (line_id, entry_id, tenant_id, period_id, payer_tenant_id, account_id,
                 account_class, side, amount_minor, currency, currency_scale, mapping_status)
             VALUES ('{line}','{entry}','{tenant}','202606','{tenant}','{tenant}',
                     '{class}','{side}', 1000, 'USD', 2, 'RESOLVED')"
        )))
        .await
        .unwrap();
    }
    seed.commit().await.unwrap();

    // Same scale -> idempotent no-op.
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "USD".to_owned(),
            minor_units: 2,
            plausible_max_major: DEFAULT_PLAUSIBLE_MAX_MAJOR,
            source: "iso".to_owned(),
        })
        .await
        .expect("same scale must be a no-op");

    // Changed scale with postings present -> locked.
    let locked = reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "USD".to_owned(),
            minor_units: 3,
            plausible_max_major: DEFAULT_PLAUSIBLE_MAX_MAJOR,
            source: "iso".to_owned(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(locked, RepoError::CurrencyScaleLocked(_)),
        "got {locked:?}"
    );

    // A currency with no postings is freely scalable.
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: tenant,
            currency: "EUR".to_owned(),
            minor_units: 2,
            plausible_max_major: DEFAULT_PLAUSIBLE_MAX_MAJOR,
            source: "iso".to_owned(),
        })
        .await
        .expect("EUR has no postings; upsert must succeed");

    // Defense-in-depth: a DIRECT SQL UPDATE bypassing the app-level lock is
    // rejected by the `trg_currency_scale_immutable` trigger once a posting
    // exists — USD has a posted line, so re-denominating its scale must fail.
    let raw_update = raw
        .execute(pg(format!(
            "UPDATE bss.ledger_currency_scale_registry SET minor_units = 3 \
             WHERE tenant_id = '{tenant}' AND currency = 'USD'"
        )))
        .await;
    assert!(
        raw_update.is_err(),
        "direct UPDATE of a locked scale must be rejected by the DB trigger"
    );

    // The trigger fires only on a genuine change: a same-value UPDATE passes.
    raw.execute(pg(format!(
        "UPDATE bss.ledger_currency_scale_registry SET minor_units = 2 \
         WHERE tenant_id = '{tenant}' AND currency = 'USD'"
    )))
    .await
    .expect("same-value UPDATE must pass the trigger");

    // And a currency with no postings can be re-denominated by direct SQL.
    raw.execute(pg(format!(
        "UPDATE bss.ledger_currency_scale_registry SET minor_units = 3 \
         WHERE tenant_id = '{tenant}' AND currency = 'EUR'"
    )))
    .await
    .expect("EUR has no postings; direct UPDATE must pass the trigger");
}
