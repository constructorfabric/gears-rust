//! Fast SQLite round-trip for `ChainStateRepo`. Opens an in-memory database,
//! runs the migrator, then inside a transaction: reads the tip of a fresh
//! tenant (genesis → `None`), `advance`s it (insert), reads it back, `advance`s
//! again with new values (update), and reads the updated tip. SQLite has no
//! triggers, so the upsert exercises the `ON CONFLICT (tenant_id) DO UPDATE`
//! path directly.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::similar_names
)]

use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::{ChainStateRepo, TipRow};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

#[tokio::test]
async fn chain_state_tip_round_trips_on_sqlite() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrator");
    let provider = DBProvider::<DbError>::new(db);
    let repo = ChainStateRepo::new();

    let tenant_id = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant_id);

    let first = TipRow {
        last_row_hash: vec![0xAA, 0xBB, 0xCC],
        last_entry_id: Uuid::now_v7(),
        last_period_id: "202606".to_owned(),
        last_seq: 1,
    };
    let second = TipRow {
        last_row_hash: vec![0x11, 0x22, 0x33, 0x44],
        last_entry_id: Uuid::now_v7(),
        last_period_id: "202607".to_owned(),
        last_seq: 2,
    };

    provider
        .transaction(move |tx| {
            Box::pin(async move {
                // Genesis: no tip row yet.
                let absent = repo
                    .read_tip(tx, &scope, tenant_id)
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))?;
                assert!(absent.is_none(), "fresh tenant must have no chain tip");

                // Advance with an absent tip → INSERT.
                repo.advance(tx, &scope, tenant_id, &first)
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))?;

                let after_insert = repo
                    .read_tip(tx, &scope, tenant_id)
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))?
                    .expect("tip must be present after first advance");
                assert_eq!(after_insert, first, "inserted tip must round-trip");

                // Advance again → ON CONFLICT (tenant_id) DO UPDATE.
                repo.advance(tx, &scope, tenant_id, &second)
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))?;

                let after_update = repo
                    .read_tip(tx, &scope, tenant_id)
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))?
                    .expect("tip must be present after second advance");
                assert_eq!(after_update, second, "updated tip must round-trip");

                Ok(())
            })
        })
        .await
        .expect("chain_state round-trip inside transaction");
}
