//! Minimal database support shared by pricing boot and future REST tests.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_pricing::module::BssPricingGear;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};

/// Create a fresh database through the gear's actual runtime migration chain.
///
/// # Errors
/// Propagates connection or migration failures.
pub async fn migrated_db() -> anyhow::Result<DBProvider<DbError>> {
    let db = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..ConnectOpts::default()
        },
    )
    .await?;
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        BssPricingGear::default().migrations(),
    )
    .await?;
    Ok(DBProvider::new(db))
}
