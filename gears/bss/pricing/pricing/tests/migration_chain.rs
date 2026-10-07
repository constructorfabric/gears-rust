//! The new chain is replayable and every migration reverses independently, with two named
//! exceptions: the forward migration `m20260926_000013` (D-427) moves data and is irreversible, so it
//! applies once through the runner and its down refuses; `m20260929_000017` (D-446) widens a CHECK a
//! stored row may then need, so its down refuses too. The schema guard (D-423) sorts first and
//! creates nothing.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_pricing::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::{MigratorTrait, SchemaManager};
use toolkit::contracts::DatabaseCapability;

async fn migration(index: usize, tables: &[&str]) {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = Migrator::migrations();
    for prior in &chain[..index] {
        prior.up(&manager).await.unwrap();
    }
    let step = &chain[index];
    step.up(&manager).await.unwrap();
    step.up(&manager).await.unwrap();
    for table in tables {
        assert!(manager.has_table(*table).await.unwrap());
    }
    step.down(&manager).await.unwrap();
    step.down(&manager).await.unwrap();
    for table in tables {
        assert!(!manager.has_table(*table).await.unwrap());
    }
}

#[tokio::test]
async fn m20260926_000001_settings() {
    migration(2, &["pricing_settings"]).await;
}
#[tokio::test]
async fn m20260926_000002_approvals() {
    migration(
        3,
        &[
            "pricing_approval_policy",
            "pricing_approval_unit",
            "pricing_approval_unit_item",
            "pricing_approval_decision",
        ],
    )
    .await;
}
#[tokio::test]
async fn m20260926_000003_dimension_key() {
    migration(4, &["pricing_dimension_key"]).await;
}
#[tokio::test]
async fn m20260926_000004_price_book() {
    migration(5, &["pricing_price_book"]).await;
}
#[tokio::test]
async fn m20260926_000005_price_book_entry() {
    migration(6, &["pricing_price_book_entry"]).await;
}
#[tokio::test]
async fn m20260926_000006_reference_op() {
    migration(7, &["pricing_reference_op"]).await;
}
#[tokio::test]
async fn m20260926_000007_price() {
    migration(8, &["pricing_price"]).await;
}
#[tokio::test]
async fn m20260926_000008_audit() {
    migration(9, &["pricing_audit"]).await;
}
#[tokio::test]
async fn m20260926_000009_idempotency() {
    migration(10, &["pricing_idempotency"]).await;
}

#[tokio::test]
async fn m20260926_000010_plan() {
    migration(11, &["pricing_plan"]).await;
}
#[tokio::test]
async fn m20260926_000011_plan_revision() {
    migration(12, &["pricing_plan_revision"]).await;
}
#[tokio::test]
async fn m20260926_000012_plan_item() {
    migration(13, &["pricing_plan_item"]).await;
}

/// The guard (D-423) creates nothing and reverses to nothing.
#[tokio::test]
async fn m0000_the_guard_creates_nothing() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let guard = &Migrator::migrations()[0];
    guard.up(&manager).await.unwrap();
    guard.up(&manager).await.unwrap();
    let tables = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master".to_owned(),
        ))
        .await
        .unwrap();
    assert!(tables.is_empty(), "the guard created a schema object");
    guard.down(&manager).await.unwrap();
    guard.down(&manager).await.unwrap();
}

/// The named exception (D-427): 000013 applies once through the toolkit runner, a second run
/// skips it, and its down refuses by name instead of reversing a data move.
#[tokio::test]
async fn m20260926_000013_applies_once_and_refuses_to_revert() {
    let db = toolkit_db::connect_db(
        "sqlite::memory:",
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    let first = toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        bss_pricing::module::BssPricingGear::default().migrations(),
    )
    .await
    .unwrap();
    assert!(
        first
            .applied_names
            .iter()
            .any(|n| n == "m20260926_000013_model_on_the_entry"),
        "{:?}",
        first.applied_names
    );
    let again = toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        bss_pricing::module::BssPricingGear::default().migrations(),
    )
    .await
    .unwrap();
    assert!(again.applied_names.is_empty(), "{:?}", again.applied_names);
    let bare = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&bare);
    let step = Migrator::migrations()
        .into_iter()
        .find(|m| m.name() == "m20260926_000013_model_on_the_entry")
        .unwrap();
    let refused = step.down(&manager).await.unwrap_err().to_string();
    assert!(
        refused.contains("m20260926_000013_model_on_the_entry") && refused.contains("irreversible"),
        "{refused}"
    );
}

#[test]
fn gear_chain_is_the_guard_coord_then_twenty_two_ordered_unique_migrations() {
    let names: Vec<_> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    assert_eq!(names.len(), 25);
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(names, sorted);
    assert_eq!(names[0], "m0000_pricing_refuse_a_legacy_or_stale_schema");
    assert_eq!(names[1], "m0001_create_coord_leases");
    assert_eq!(names[2], "m20260926_000001_create_pricing_settings");
    assert_eq!(names[10], "m20260926_000009_create_pricing_idempotency");
    assert_eq!(names[11], "m20260926_000010_create_pricing_plan");
    assert_eq!(names[13], "m20260926_000012_create_pricing_plan_item");
    assert_eq!(names[14], "m20260926_000013_model_on_the_entry");
    assert_eq!(names[15], "m20260927_000014_settings_currencies_and_author");
    assert_eq!(names[16], "m20260928_000015_book_description");
    assert_eq!(names[17], "m20260928_000016_unit_submit_note");
    assert_eq!(names[18], "m20260929_000017_revision_scheduled");
    assert_eq!(names[19], "m20260930_000018_usage_rating_policy");
    assert_eq!(names[20], "m20260930_000019_commercial_receipts");
    assert_eq!(names[21], "m20261002_000020_plan_summary");
    assert_eq!(names[22], "m20261002_000021_policy_references_sku");
    assert_eq!(names[23], "m20261003_000022_price_cancel_and_end");
    assert_eq!(names[24], "m20261003_000023_book_archive");
}

/// D-446: 000017 widens the revision state CHECK and adds the scheduled index. It replays without
/// effect (the `SQLite` family rebuild runs again on the rebuilt family) and its down refuses by
/// name: a revision stored `scheduled` has no state under the old CHECK.
#[tokio::test]
async fn m20260929_000017_widens_the_state_check_replays_and_refuses_to_revert() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = Migrator::migrations();
    let at = chain
        .iter()
        .position(|m| m.name() == "m20260929_000017_revision_scheduled")
        .unwrap();
    for prior in &chain[..at] {
        prior.up(&manager).await.unwrap();
    }
    let indexes = || async {
        db.query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'index' \
             AND tbl_name = 'pricing_plan_revision' AND sql IS NOT NULL ORDER BY name"
                .to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| r.try_get::<String>("", "name").unwrap())
        .collect::<Vec<String>>()
    };
    assert_eq!(
        indexes().await,
        [
            "pricing_plan_revision_open",
            "pricing_plan_revision_published"
        ]
    );
    let step = &chain[at];
    step.up(&manager).await.unwrap();
    step.up(&manager).await.unwrap();
    assert_eq!(
        indexes().await,
        [
            "pricing_plan_revision_open",
            "pricing_plan_revision_published",
            "pricing_plan_revision_scheduled"
        ]
    );
    let tables = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'pricing_plan_%' \
             ORDER BY name"
                .to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| r.try_get::<String>("", "name").unwrap())
        .collect::<Vec<String>>();
    assert_eq!(
        tables,
        ["pricing_plan_item", "pricing_plan_revision"],
        "no rebuild table is left behind"
    );
    for _ in 0..2 {
        let refused = step.down(&manager).await.unwrap_err().to_string();
        assert!(
            refused.contains("m20260929_000017_revision_scheduled: irreversible"),
            "{refused}"
        );
    }
    assert_eq!(indexes().await.len(), 3, "a refused down changes nothing");
}

/// Run 7.3 (D-445): 000016 adds `pricing_approval_unit.submit_note`, through the approval library's
/// separate step, and nothing else. It re-applies without effect and reverses on its own: its down
/// drops the column, twice without effect.
#[tokio::test]
async fn m20260928_000016_adds_and_drops_the_units_submit_note() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = Migrator::migrations();
    let at = chain
        .iter()
        .position(|m| m.name() == "m20260928_000016_unit_submit_note")
        .unwrap();
    for prior in &chain[..at] {
        prior.up(&manager).await.unwrap();
    }
    let columns = || async {
        db.query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM pragma_table_info('pricing_approval_unit') ORDER BY cid".to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| r.try_get::<String>("", "name").unwrap())
        .collect::<Vec<String>>()
    };
    let before = columns().await;
    let step = &chain[at];
    step.up(&manager).await.unwrap();
    step.up(&manager).await.unwrap();
    let mut expected = before.clone();
    expected.push("submit_note".to_owned());
    assert_eq!(columns().await, expected, "appended, the others in place");
    step.down(&manager).await.unwrap();
    step.down(&manager).await.unwrap();
    assert_eq!(columns().await, before);
}

/// Run 7.2: 000015 adds `pricing_price_book.description` and nothing else. It re-applies without
/// effect and reverses on its own: its down drops the column, twice without effect.
#[tokio::test]
async fn m20260928_000015_adds_and_drops_the_book_description() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = Migrator::migrations();
    let at = chain
        .iter()
        .position(|m| m.name() == "m20260928_000015_book_description")
        .unwrap();
    for prior in &chain[..at] {
        prior.up(&manager).await.unwrap();
    }
    let columns = || async {
        let mut names: Vec<String> = db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT name FROM pragma_table_info('pricing_price_book')".to_owned(),
            ))
            .await
            .unwrap()
            .iter()
            .map(|r| r.try_get::<String>("", "name").unwrap())
            .collect();
        names.sort();
        names
    };
    let before = columns().await;
    let step = &chain[at];
    step.up(&manager).await.unwrap();
    step.up(&manager).await.unwrap();
    let mut expected = before.clone();
    expected.push("description".to_owned());
    expected.sort();
    assert_eq!(columns().await, expected);
    step.down(&manager).await.unwrap();
    step.down(&manager).await.unwrap();
    assert_eq!(columns().await, before);
}

/// D-438: 000014 adds two columns to `pricing_settings` and nothing else. It re-applies without
/// effect (a column the table holds is not added twice) and reverses on its own: its down drops
/// the two columns, twice without effect.
#[tokio::test]
async fn m20260927_000014_adds_and_drops_its_two_columns() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = Migrator::migrations();
    let at = chain
        .iter()
        .position(|m| m.name() == "m20260927_000014_settings_currencies_and_author")
        .unwrap();
    for prior in &chain[..at] {
        prior.up(&manager).await.unwrap();
    }
    let columns = || async {
        let mut names: Vec<String> = db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT name FROM pragma_table_info('pricing_settings')".to_owned(),
            ))
            .await
            .unwrap()
            .iter()
            .map(|r| r.try_get::<String>("", "name").unwrap())
            .collect();
        names.sort();
        names
    };
    let before = columns().await;
    let step = &chain[at];
    step.up(&manager).await.unwrap();
    step.up(&manager).await.unwrap();
    let mut expected = before.clone();
    expected.extend(["currencies".to_owned(), "updated_by".to_owned()]);
    expected.sort();
    assert_eq!(columns().await, expected);
    step.down(&manager).await.unwrap();
    step.down(&manager).await.unwrap();
    assert_eq!(columns().await, before);
}

/// The runner sorts the gear's WHOLE list by name, outbox and broker included: the guard must
/// still come first there, or those migrations commit before it refuses (plan review M1).
#[test]
fn the_guard_sorts_first_in_the_gears_whole_list() {
    let mut names: Vec<_> = bss_pricing::module::BssPricingGear::default()
        .migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    names.sort();
    assert_eq!(names[0], "m0000_pricing_refuse_a_legacy_or_stale_schema");
}

#[tokio::test]
async fn m20260930_000019_commercial_receipts() {
    migration(
        20,
        &[
            "pricing_acceptance",
            "pricing_hold",
            "pricing_commercial_command",
        ],
    )
    .await;
}
