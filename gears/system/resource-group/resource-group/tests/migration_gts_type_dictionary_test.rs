#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "migration test: raw SQL against the schema under migration"
)]
//! `m20261111_000001_gts_type_dictionary` on SQLite with foreign keys enforced,
//! through the production migration runner: an existing hierarchy keeps every
//! group, closure row, membership and dictionary row (sparse keys and
//! timestamps included) byte for byte; the type definitions are dropped; the
//! numeric foreign keys, indexes and the resource identifier bound hold; and a
//! refused preflight leaves the old schema untouched.

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, Db, connect_db};

use resource_group::infra::storage::migrations::Migrator;

const TENANT_TYPE: &str = "gts.cf.core.rg.type.v1~cf.test.rg.tenant.v1~";
const DEPT_TYPE: &str = "gts.cf.core.rg.type.v1~cf.test.rg.dept.v1~";
const USER_TYPE: &str = "gts.cf.test.rg.user.v1~";

// UUIDs as SeaORM stores them on SQLite (16-byte BLOB), in hex.
const ROOT: &str = "019A0000000070008000000000000001";
const CHILD: &str = "019A0000000070008000000000000002";
const GRANDCHILD: &str = "019A0000000070008000000000000003";
const OTHER_ROOT: &str = "019A0000000070008000000000000004";

struct Fixture {
    db: Db,
    raw: DatabaseConnection,
    _dir: tempdir::Dir,
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            if let Err(e) = std::fs::remove_dir_all(&self.0) {
                eprintln!("leaving {}: {e}", self.0.display());
            }
        }
    }
    pub fn new() -> Dir {
        let path = std::env::temp_dir().join(format!("rg-migration-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).expect("temp dir");
        Dir(path)
    }
}

/// A file database with only the initial migration applied, and a second raw
/// connection to it with foreign keys enforced.
async fn at_initial_schema() -> Fixture {
    let dir = tempdir::new();
    let dsn = format!("sqlite://{}?mode=rwc", dir.0.join("rg.db").display());
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect");
    let initial: Vec<Box<dyn MigrationTrait>> =
        Migrator::migrations().into_iter().take(1).collect();
    run_migrations_for_testing(&db, initial)
        .await
        .expect("initial migration");
    let raw = Database::connect(&dsn).await.expect("raw connection");
    run(&raw, "PRAGMA foreign_keys = ON").await;
    Fixture { db, raw, _dir: dir }
}

async fn exec(db: &DatabaseConnection, sql: &str) -> Result<(), DbErr> {
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .map(drop)
}

async fn run(db: &DatabaseConnection, sql: &str) {
    exec(db, sql)
        .await
        .unwrap_or_else(|e| panic!("`{sql}` failed: {e}"));
}

async fn rows(db: &DatabaseConnection, sql: &str) -> Vec<Vec<String>> {
    db.query_all_raw(Statement::from_string(
        db.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .expect(sql)
    .into_iter()
    .map(|row| {
        (0..)
            .map_while(|i| row.try_get_by_index::<Option<String>>(i).ok())
            .map(|v| v.unwrap_or_else(|| "NULL".to_owned()))
            .collect()
    })
    .collect()
}

// Sparse dictionary keys: the migration must keep them, not renumber.
const TENANT_KEY: i64 = 3;
const DEPT_KEY: i64 = 37;
const USER_KEY: i64 = 91;
const RETIRED_KEY: i64 = 92;
const RETIRED_TYPE: &str = "gts.cf.core.rg.type.v1~cf.test.rg.retired.v1~";

/// Two roots, a three-level chain, its closure rows and memberships, a
/// dictionary row nothing references, and placement rules.
async fn seed_hierarchy(raw: &DatabaseConnection) {
    run(
        raw,
        &format!(
            "INSERT INTO gts_type (id, schema_id, metadata_schema, created_at, updated_at) VALUES \
             ({TENANT_KEY}, '{TENANT_TYPE}', NULL, '2026-01-01 00:00:01', NULL), \
             ({DEPT_KEY}, '{DEPT_TYPE}', '{{\"type\":\"object\"}}', '2026-01-01 00:00:02', '2026-02-01 00:00:00'), \
             ({USER_KEY}, '{USER_TYPE}', NULL, '2026-01-01 00:00:03', NULL), \
             ({RETIRED_KEY}, '{RETIRED_TYPE}', NULL, '2026-01-01 00:00:04', NULL)"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO gts_type_allowed_parent (type_id, parent_type_id) VALUES \
             ({DEPT_KEY}, {TENANT_KEY}), ({DEPT_KEY}, {DEPT_KEY})"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO gts_type_allowed_membership (type_id, membership_type_id) VALUES \
             ({DEPT_KEY}, {USER_KEY})"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group (id, parent_id, gts_type_id, name, metadata, tenant_id, created_at, updated_at) VALUES \
             (X'{ROOT}', NULL, {TENANT_KEY}, 'root', NULL, X'{ROOT}', '2026-03-01 00:00:01', NULL), \
             (X'{CHILD}', X'{ROOT}', {DEPT_KEY}, 'child', '{{\"k\":1}}', X'{ROOT}', '2026-03-01 00:00:02', '2026-03-02 00:00:00'), \
             (X'{GRANDCHILD}', X'{CHILD}', {DEPT_KEY}, 'grandchild', NULL, X'{ROOT}', '2026-03-01 00:00:03', NULL), \
             (X'{OTHER_ROOT}', NULL, {TENANT_KEY}, 'other', NULL, X'{OTHER_ROOT}', '2026-03-01 00:00:04', NULL)"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group_closure (ancestor_id, descendant_id, depth) VALUES \
             (X'{ROOT}', X'{ROOT}', 0), (X'{CHILD}', X'{CHILD}', 0), \
             (X'{GRANDCHILD}', X'{GRANDCHILD}', 0), (X'{OTHER_ROOT}', X'{OTHER_ROOT}', 0), \
             (X'{ROOT}', X'{CHILD}', 1), (X'{ROOT}', X'{GRANDCHILD}', 2), \
             (X'{CHILD}', X'{GRANDCHILD}', 1)"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id, created_at) VALUES \
             (X'{CHILD}', {USER_KEY}, 'u1', '2026-04-01 00:00:01'), \
             (X'{GRANDCHILD}', {USER_KEY}, 'u1', '2026-04-01 00:00:02'), \
             (X'{ROOT}', {USER_KEY}, '{}', '2026-04-01 00:00:03')",
            "r".repeat(2600)
        ),
    )
    .await;
}

/// Every preserved row, as text, in a stable order.
async fn snapshot(raw: &DatabaseConnection) -> Vec<Vec<Vec<String>>> {
    vec![
        rows(
            raw,
            "SELECT hex(id), CASE WHEN parent_id IS NULL THEN NULL ELSE hex(parent_id) END, \
             CAST(gts_type_id AS TEXT), typeof(gts_type_id), name, metadata, hex(tenant_id), \
             created_at, updated_at FROM resource_group ORDER BY 1",
        )
        .await,
        rows(
            raw,
            "SELECT hex(ancestor_id), hex(descendant_id), CAST(depth AS TEXT) \
             FROM resource_group_closure ORDER BY 1, 2",
        )
        .await,
        rows(
            raw,
            "SELECT hex(group_id), CAST(gts_type_id AS TEXT), typeof(gts_type_id), \
             hex(resource_id), created_at FROM resource_group_membership ORDER BY 1, 2, 4",
        )
        .await,
        rows(
            raw,
            "SELECT CAST(id AS TEXT), schema_id, created_at FROM gts_type ORDER BY id",
        )
        .await,
    ]
}

async fn table_names(raw: &DatabaseConnection, pattern: &str) -> Vec<String> {
    rows(
        raw,
        &format!(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE '{pattern}' ORDER BY name"
        ),
    )
    .await
    .into_iter()
    .map(|r| r[0].clone())
    .collect()
}

async fn columns(raw: &DatabaseConnection, table: &str) -> Vec<String> {
    rows(
        raw,
        &format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid"),
    )
    .await
    .into_iter()
    .map(|r| r[0].clone())
    .collect()
}

async fn upgrade(db: &Db) -> Result<(), toolkit_db::migration_runner::MigrationError> {
    run_migrations_for_testing(db, Migrator::migrations())
        .await
        .map(drop)
}

#[tokio::test]
async fn oversized_resource_id_refuses_upgrade_before_any_ddl() {
    let f = at_initial_schema().await;
    seed_hierarchy(&f.raw).await;
    // 1301 characters, but 2602 UTF-8 bytes: the portable contract counts
    // bytes rather than SQLite's character length.
    let resource = "\u{436}".repeat(1301);
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES (X'{ROOT}', {USER_KEY}, '{resource}')"
        ),
    )
    .await;
    let before = snapshot(&f.raw).await;

    let err = upgrade(&f.db)
        .await
        .expect_err("oversized legacy resource id");
    let diagnostic = err.to_string();
    assert!(diagnostic.contains("2600"), "{diagnostic}");
    assert!(diagnostic.contains("UTF-8"), "{diagnostic}");
    assert!(diagnostic.contains("before upgrading"), "{diagnostic}");
    assert_eq!(
        table_names(&f.raw, "gts_type%").await,
        [
            "gts_type",
            "gts_type_allowed_membership",
            "gts_type_allowed_parent"
        ],
        "the preflight runs before any DDL"
    );
    assert!(
        columns(&f.raw, "gts_type")
            .await
            .contains(&"metadata_schema".to_owned())
    );
    assert!(table_names(&f.raw, "%_new").await.is_empty());
    assert_eq!(snapshot(&f.raw).await, before);

    run(
        &f.raw,
        "UPDATE resource_group_membership SET resource_id = 'remapped-resource' \
         WHERE length(CAST(resource_id AS BLOB)) > 2600",
    )
    .await;
    upgrade(&f.db).await.expect("upgrade after remediation");
    assert_eq!(table_names(&f.raw, "gts_type%").await, ["gts_type"]);
    assert_eq!(
        rows(
            &f.raw,
            "SELECT CAST(count(*) AS TEXT) FROM resource_group_membership"
        )
        .await,
        vec![vec!["4"]]
    );
}

#[tokio::test]
async fn an_existing_hierarchy_keeps_every_row_and_dictionary_key() {
    let f = at_initial_schema().await;
    seed_hierarchy(&f.raw).await;
    let before = snapshot(&f.raw).await;
    upgrade(&f.db).await.expect("upgrade");
    assert!(
        !columns(&f.raw, "resource_group")
            .await
            .iter()
            .any(|name| name == "membership_type_id")
    );

    assert_eq!(
        snapshot(&f.raw).await,
        before,
        "rows, keys and timestamps are kept"
    );
    upgrade(&f.db).await.expect("a repeated upgrade is a no-op");
    assert_eq!(
        snapshot(&f.raw).await,
        before,
        "the repeated upgrade changes nothing"
    );
    assert_eq!(
        rows(
            &f.raw,
            &format!("SELECT schema_id FROM gts_type WHERE id = {RETIRED_KEY}")
        )
        .await,
        vec![vec![RETIRED_TYPE]],
        "a dictionary row nothing references is kept"
    );
    assert_eq!(
        table_names(&f.raw, "gts_type%").await,
        ["gts_type"],
        "the placement rules are dropped"
    );
    assert_eq!(
        columns(&f.raw, "gts_type").await,
        ["id", "schema_id", "created_at"]
    );
    assert!(
        table_names(&f.raw, "%_new").await.is_empty(),
        "no rebuild leftovers"
    );
    let mut indexes: Vec<String> = rows(
        &f.raw,
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_%'",
    )
    .await
    .into_iter()
    .map(|r| r[0].clone())
    .collect();
    indexes.sort();
    assert_eq!(
        indexes,
        [
            "idx_rg_gts_type_id",
            "idx_rg_name",
            "idx_rg_parent_id",
            "idx_rg_tenant_id",
            "idx_rgc_ancestor_depth",
            "idx_rgc_descendant_id",
            "idx_rgm_gts_type_resource",
        ]
    );
    // New dictionary keys continue after the highest one ever issued.
    run(
        &f.raw,
        "INSERT INTO gts_type (schema_id) VALUES ('gts.cf.test.rg.new.v1~')",
    )
    .await;
    assert_eq!(
        rows(
            &f.raw,
            "SELECT CAST(id AS TEXT) FROM gts_type WHERE schema_id = 'gts.cf.test.rg.new.v1~'"
        )
        .await,
        vec![vec![(RETIRED_KEY + 1).to_string()]]
    );
}

#[tokio::test]
async fn constraints_hold_after_the_upgrade() {
    let f = at_initial_schema().await;
    seed_hierarchy(&f.raw).await;
    upgrade(&f.db).await.expect("upgrade");

    let duplicate = exec(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES (X'{CHILD}', {USER_KEY}, 'u1')"
        ),
    )
    .await
    .expect_err("membership primary key");
    assert!(duplicate.to_string().contains("UNIQUE"), "{duplicate}");

    for (what, sql) in [
        (
            "membership group",
            format!(
                "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
                 VALUES (X'00000000000000000000000000000099', {USER_KEY}, 'u3')"
            ),
        ),
        (
            "membership type",
            format!(
                "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
                 VALUES (X'{ROOT}', 999, 'u3')"
            ),
        ),
        (
            "group type",
            format!(
                "INSERT INTO resource_group (id, parent_id, gts_type_id, name, tenant_id) \
                 VALUES (X'00000000000000000000000000000098', NULL, 999, 'x', X'{ROOT}')"
            ),
        ),
        (
            "dictionary row in use",
            format!("DELETE FROM gts_type WHERE id = {USER_KEY}"),
        ),
        (
            "parent with children",
            format!("DELETE FROM resource_group WHERE id = X'{ROOT}'"),
        ),
    ] {
        let err = exec(&f.raw, &sql).await.expect_err(what);
        assert!(err.to_string().contains("FOREIGN KEY"), "{what}: {err}");
    }

    let same_identifier = exec(
        &f.raw,
        &format!("INSERT INTO gts_type (schema_id) VALUES ('{USER_TYPE}')"),
    )
    .await
    .expect_err("one key per identifier");
    assert!(
        same_identifier.to_string().contains("UNIQUE"),
        "{same_identifier}"
    );

    for (resource, accepted) in [
        ("a".repeat(2600), true),
        ("a".repeat(2601), false),
        ("\u{436}".repeat(1300), true),
        ("\u{436}".repeat(1301), false),
    ] {
        let result = exec(
            &f.raw,
            &format!(
                "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
                 VALUES (X'{OTHER_ROOT}', {USER_KEY}, '{resource}')"
            ),
        )
        .await;
        if accepted {
            result.expect("resource id is exactly 2600 UTF-8 bytes");
        } else {
            let err = result.expect_err("resource id exceeds 2600 UTF-8 bytes");
            assert!(
                err.to_string().contains("ck_rgm_resource_id_bytes"),
                "{err}"
            );
        }
    }
}

/// A failure after DDL has started -- the membership copy into the rebuilt
/// table hitting an orphaned row the old database let in with foreign keys
/// off -- rolls the whole migration back: definitions, rule tables, old
/// memberships and the migration history are as before, with no leftovers.
#[tokio::test]
async fn a_failed_membership_rebuild_rolls_back_the_whole_migration() {
    let f = at_initial_schema().await;
    seed_hierarchy(&f.raw).await;
    run(&f.raw, "PRAGMA foreign_keys = OFF").await;
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES (X'{ROOT}', 999, 'orphan-type'), \
             (X'00000000000000000000000000000099', {USER_KEY}, 'orphan-group')"
        ),
    )
    .await;
    run(&f.raw, "PRAGMA foreign_keys = ON").await;
    let before = snapshot(&f.raw).await;
    let history_table = table_names(&f.raw, "toolkit_migrations__%").await;
    assert_eq!(history_table.len(), 1, "{history_table:?}");
    let history = format!(
        "SELECT version FROM \"{}\" ORDER BY version",
        history_table[0]
    );
    let history_before = rows(&f.raw, &history).await;
    assert_eq!(
        history_before.len(),
        1,
        "only the initial migration is applied"
    );

    upgrade(&f.db)
        .await
        .expect_err("the copy into the rebuilt membership table fails");

    assert_eq!(snapshot(&f.raw).await, before);
    assert_eq!(
        rows(&f.raw, &history).await,
        history_before,
        "not recorded as applied"
    );
    assert_eq!(
        table_names(&f.raw, "gts_type%").await,
        [
            "gts_type",
            "gts_type_allowed_membership",
            "gts_type_allowed_parent"
        ]
    );
    assert_eq!(
        columns(&f.raw, "gts_type").await,
        [
            "id",
            "schema_id",
            "metadata_schema",
            "created_at",
            "updated_at"
        ]
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT CAST(count(*) AS TEXT) FROM gts_type_allowed_parent"
        )
        .await,
        vec![vec!["2"]]
    );
    assert_eq!(
        rows(
            &f.raw,
            &format!("SELECT metadata_schema FROM gts_type WHERE id = {DEPT_KEY}")
        )
        .await,
        vec![vec!["{\"type\":\"object\"}"]]
    );
    assert!(
        table_names(&f.raw, "%_new").await.is_empty(),
        "no rebuild leftovers"
    );
}

#[tokio::test]
async fn down_is_refused_as_irreversible() {
    let f = at_initial_schema().await;
    upgrade(&f.db).await.expect("upgrade");
    let migration = Migrator::migrations()
        .into_iter()
        .find(|migration| migration.name() == "m20261111_000001_gts_type_dictionary")
        .expect("the dictionary migration");
    let err = migration
        .down(&SchemaManager::new(&f.raw))
        .await
        .expect_err("irreversible");
    assert!(err.to_string().contains("irreversible"), "{err}");
    assert!(err.to_string().contains("backup"), "{err}");
}
