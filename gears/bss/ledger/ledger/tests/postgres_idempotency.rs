//! Postgres-only: the idempotency gate. First `claim` of a key wins
//! (`Claimed`); after a `finalize` stamps the result entry, a second `claim`
//! of the same key returns `Replay` with a populated `result_entry_id`; a
//! `claim` with a different payload hash still returns the stored row (the
//! caller maps the hash mismatch to `IDEMPOTENCY_PAYLOAD_CONFLICT`). Ignored
//! by default; run with `cargo test -p bss-ledger -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic,
    clippy::needless_pass_by_value
)]

use bss_ledger::domain::model::RepoError;
use bss_ledger::infra::posting::idempotency::{ClaimOutcome, IdempotencyGate};
use bss_ledger::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, Database, DbErr, Statement, TransactionTrait};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

/// Map a component `RepoError` into a `DbError` so the gate result can be the
/// transaction's typed success value (`T`), surviving COMMIT.
fn lift(e: RepoError) -> DbError {
    DbError::Sea(DbErr::Custom(e.to_string()))
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn claim_then_finalize_then_replay() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let raw = Database::connect(&url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();

    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    let provider = DBProvider::<DbError>::new(tdb);

    let tenant = Uuid::now_v7();
    let entry_id = Uuid::now_v7();
    let flow = "MANUAL_ADJUSTMENT";
    let business_id = "biz-1";
    let hash_a = "a".repeat(64);
    let hash_b = "b".repeat(64);
    let gate = IdempotencyGate::new();

    // First claim wins.
    let claimed = provider
        .transaction(|txn| {
            let gate = gate.clone();
            let hash = hash_a.clone();
            Box::pin(async move {
                gate.claim(txn, tenant, flow, business_id, &hash)
                    .await
                    .map_err(lift)
            })
        })
        .await
        .unwrap();
    assert!(matches!(claimed, ClaimOutcome::Claimed));

    // Seed a journal entry so finalize references a real id (one txn so the
    // deferred balance trigger sees both lines at COMMIT).
    let seed = raw.begin().await.unwrap();
    seed.execute(pg(format!(
        "INSERT INTO bss.ledger_journal_entry
            (entry_id, tenant_id, legal_entity_id, period_id, entry_currency,
             source_doc_type, source_business_id, posted_at_utc, effective_at,
             origin, posted_by_actor_id, correlation_id)
         VALUES ('{entry_id}','{tenant}','{tenant}','202606','USD',
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
             VALUES ('{line}','{entry_id}','{tenant}','202606','{tenant}','{tenant}',
                     '{class}','{side}', 1000, 'USD', 2, 'RESOLVED')"
        )))
        .await
        .unwrap();
    }
    seed.commit().await.unwrap();

    // Finalize the claim.
    provider
        .transaction(|txn| {
            let gate = gate.clone();
            Box::pin(async move {
                gate.finalize(txn, tenant, flow, business_id, entry_id, 1)
                    .await
                    .map_err(lift)
            })
        })
        .await
        .unwrap();

    // Replay with the same hash → stored row with the populated entry id.
    let replay_same = provider
        .transaction(|txn| {
            let gate = gate.clone();
            let hash = hash_a.clone();
            Box::pin(async move {
                gate.claim(txn, tenant, flow, business_id, &hash)
                    .await
                    .map_err(lift)
            })
        })
        .await
        .unwrap();
    match replay_same {
        ClaimOutcome::Replay(row) => {
            assert_eq!(row.result_entry_id, Some(entry_id));
            assert_eq!(row.payload_hash, hash_a);
            assert_eq!(row.status, "POSTED");
        }
        ClaimOutcome::Claimed => panic!("expected a replay"),
    }

    // Replay with a different hash → still the stored row (caller compares
    // payload_hash to detect IDEMPOTENCY_PAYLOAD_CONFLICT).
    let replay_diff = provider
        .transaction(|txn| {
            let gate = gate.clone();
            let hash = hash_b.clone();
            Box::pin(async move {
                gate.claim(txn, tenant, flow, business_id, &hash)
                    .await
                    .map_err(lift)
            })
        })
        .await
        .unwrap();
    match replay_diff {
        ClaimOutcome::Replay(row) => {
            assert_eq!(row.payload_hash, hash_a, "stored hash is the original");
        }
        ClaimOutcome::Claimed => panic!("expected a replay"),
    }
}
