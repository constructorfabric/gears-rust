#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "migration test: raw SQL against the schema under migration"
)]
//! `m20261111_000001_gts_type_dictionary` on PostgreSQL (testcontainers;
//! `--features integration`, Docker required; `RG_PG_REQUIRE_DOCKER=1` makes a
//! missing Docker fail instead of skip), through the production runner.
//!
//! Every group, closure row, membership and dictionary row (sparse keys and
//! timestamps included) survives the group schema ID rename, the `SMALLINT` keys and their foreign
//! keys stay, the type definitions go, and the 2600-byte `resource_id` bound
//! holds -- with pre-upgrade refusal of legacy oversized identifiers and a
//! whole-migration rollback when a step after the first DDL fails.

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, Db, connect_db};

use resource_group::infra::storage::migrations::Migrator;

const TENANT_TYPE: &str = "gts.cf.core.rg.type.v1~cf.core._.tenant.v1~";
const DEPT_TYPE: &str = "gts.cf.core.rg.type.v1~cf.test.rg.dept.v1~";
const USER_TYPE: &str = "gts.cf.test.rg.user.v1~";

const RETIRED_TYPE: &str = "gts.cf.core.rg.type.v1~cf.core._.tenant.v1~cf.test.rg.retired.v1~";

// Sparse dictionary keys: the migration must keep them, not renumber.
const TENANT_KEY: i64 = 3;
const DEPT_KEY: i64 = 37;
const USER_KEY: i64 = 91;
const RETIRED_KEY: i64 = 92;

const ROOT: &str = "019a0000-0000-7000-8000-000000000001";
const CHILD: &str = "019a0000-0000-7000-8000-000000000002";
const GRANDCHILD: &str = "019a0000-0000-7000-8000-000000000003";
const OTHER_ROOT: &str = "019a0000-0000-7000-8000-000000000004";

struct Fixture {
    _container: testcontainers::ContainerAsync<Postgres>,
    db: Db,
    raw: DatabaseConnection,
}

fn require_docker() -> bool {
    std::env::var_os("RG_PG_REQUIRE_DOCKER").is_some_and(|v| v != "0" && !v.is_empty())
}

/// PostgreSQL with only the initial migration applied, and one raw connection.
async fn at_initial_schema() -> Option<Fixture> {
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");
    let container = match request.start().await {
        Ok(c) => c,
        Err(e) => {
            assert!(!require_docker(), "PostgreSQL container: {e}");
            eprintln!("skipping -- no Docker: {e}");
            return None;
        }
    };
    let port = container.get_host_port_ipv4(5432).await.expect("port");
    let url = format!("postgres://user:pass@127.0.0.1:{port}/app");
    let db = connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(2),
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
    let mut opts = ConnectOptions::new(url);
    opts.max_connections(1).min_connections(1);
    let raw = Database::connect(opts).await.expect("raw connection");
    Some(Fixture {
        _container: container,
        db,
        raw,
    })
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

async fn seed_hierarchy(raw: &DatabaseConnection) {
    run(
        raw,
        &format!(
            "INSERT INTO gts_type (id, schema_id, metadata_schema, created_at, updated_at) \
             OVERRIDING SYSTEM VALUE VALUES \
             ({TENANT_KEY}, '{TENANT_TYPE}', NULL, '2026-01-01T00:00:01Z', NULL), \
             ({DEPT_KEY}, '{DEPT_TYPE}', '{{\"type\":\"object\"}}', '2026-01-01T00:00:02Z', '2026-02-01T00:00:00Z'), \
             ({USER_KEY}, '{USER_TYPE}', NULL, '2026-01-01T00:00:03Z', NULL), \
             ({RETIRED_KEY}, '{RETIRED_TYPE}', NULL, '2026-01-01T00:00:04Z', NULL)"
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
             ('{ROOT}', NULL, {TENANT_KEY}, 'root', NULL, '{ROOT}', '2026-03-01T00:00:01Z', NULL), \
             ('{CHILD}', '{ROOT}', {DEPT_KEY}, 'child', '{{\"k\":1}}', '{ROOT}', '2026-03-01T00:00:02Z', '2026-03-02T00:00:00Z'), \
             ('{GRANDCHILD}', '{CHILD}', {DEPT_KEY}, 'grandchild', NULL, '{ROOT}', '2026-03-01T00:00:03Z', NULL), \
             ('{OTHER_ROOT}', NULL, {TENANT_KEY}, 'other', NULL, '{OTHER_ROOT}', '2026-03-01T00:00:04Z', NULL)"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group_closure (ancestor_id, descendant_id, depth) VALUES \
             ('{ROOT}', '{ROOT}', 0), ('{CHILD}', '{CHILD}', 0), ('{GRANDCHILD}', '{GRANDCHILD}', 0), \
             ('{OTHER_ROOT}', '{OTHER_ROOT}', 0), \
             ('{ROOT}', '{CHILD}', 1), ('{ROOT}', '{GRANDCHILD}', 2), ('{CHILD}', '{GRANDCHILD}', 1)"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id, created_at) VALUES \
             ('{CHILD}', {USER_KEY}, 'u1', '2026-04-01T00:00:01Z'), \
             ('{GRANDCHILD}', {USER_KEY}, 'u1', '2026-04-01T00:00:02Z'), \
             ('{ROOT}', {USER_KEY}, '{}', '2026-04-01T00:00:03Z')",
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
            "SELECT id::text, parent_id::text, gts_type_id::text, pg_typeof(gts_type_id)::text, \
             name, metadata::text, tenant_id::text, created_at::text, updated_at::text \
             FROM resource_group ORDER BY id",
        )
        .await,
        rows(
            raw,
            "SELECT ancestor_id::text, descendant_id::text, depth::text \
             FROM resource_group_closure ORDER BY 1, 2",
        )
        .await,
        rows(
            raw,
            "SELECT group_id::text, gts_type_id::text, pg_typeof(gts_type_id)::text, \
             md5(resource_id), octet_length(resource_id)::text, created_at::text \
             FROM resource_group_membership ORDER BY 1, 2, 4",
        )
        .await,
        rows(
            raw,
            "SELECT id::text, schema_id::text, created_at::text FROM gts_type ORDER BY id",
        )
        .await,
    ]
}

async fn table_names(raw: &DatabaseConnection, pattern: &str) -> Vec<String> {
    rows(
        raw,
        &format!(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name LIKE '{pattern}' ORDER BY 1"
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
        &format!(
            "SELECT column_name::text FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = '{table}' ORDER BY ordinal_position"
        ),
    )
    .await
    .into_iter()
    .map(|r| r[0].clone())
    .collect()
}

async fn history(raw: &DatabaseConnection) -> Vec<Vec<String>> {
    let tables = table_names(raw, "toolkit_migrations__%").await;
    assert_eq!(tables.len(), 1, "{tables:?}");
    rows(
        raw,
        &format!(
            "SELECT version::text FROM \"{}\" ORDER BY version",
            tables[0]
        ),
    )
    .await
}

async fn upgrade(db: &Db) -> Result<(), toolkit_db::migration_runner::MigrationError> {
    run_migrations_for_testing(db, Migrator::migrations())
        .await
        .map(drop)
}

#[tokio::test]
async fn pg_oversized_resource_id_refuses_upgrade_before_ddl() {
    let Some(f) = at_initial_schema().await else {
        return;
    };
    seed_hierarchy(&f.raw).await;
    // 1301 characters, 2602 UTF-8 bytes.
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES ('{OTHER_ROOT}', {USER_KEY}, '{}')",
            "\u{436}".repeat(1301)
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
    assert_eq!(snapshot(&f.raw).await, before);
    assert_eq!(
        table_names(&f.raw, "gts_type%").await,
        [
            "gts_type",
            "gts_type_allowed_membership",
            "gts_type_allowed_parent"
        ]
    );
    assert_eq!(history(&f.raw).await.len(), 1);

    run(
        &f.raw,
        "UPDATE resource_group_membership SET resource_id = 'remapped-resource' \
         WHERE octet_length(resource_id) > 2600",
    )
    .await;
    upgrade(&f.db).await.expect("upgrade after remediation");
    assert_eq!(table_names(&f.raw, "gts_type%").await, ["gts_type"]);
    assert_eq!(
        rows(
            &f.raw,
            "SELECT count(*)::text FROM resource_group_membership"
        )
        .await,
        vec![vec!["4"]]
    );
}

#[tokio::test]
async fn pg_tenant_type_collision_rolls_back_the_group_type_rename() {
    let Some(f) = at_initial_schema().await else {
        return;
    };
    seed_hierarchy(&f.raw).await;
    run(
        &f.raw,
        &format!(
            "INSERT INTO gts_type (schema_id) VALUES ('{}')",
            resource_group_sdk::TENANT_GROUP_TYPE_ID
        ),
    )
    .await;
    let before = snapshot(&f.raw).await;
    let err = upgrade(&f.db).await.expect_err("tenant type collision");
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(snapshot(&f.raw).await, before);
}

#[tokio::test]
async fn pg_an_existing_hierarchy_keeps_every_row_and_dictionary_key() {
    let Some(f) = at_initial_schema().await else {
        return;
    };
    seed_hierarchy(&f.raw).await;
    let mut expected = snapshot(&f.raw).await;
    // Dictionary rows ordered by sparse key. Only the group schema IDs change.
    let dictionary = &mut expected[3];
    dictionary[0][1] = resource_group_sdk::TENANT_GROUP_TYPE_ID.into();
    dictionary[1][1] = "gts.cf.core.rg.group.v1~cf.test.rg.dept.v1~".into();
    dictionary[3][1] = "gts.cf.core.rg.group.v1~cf.core.rg.tenant.v1~cf.test.rg.retired.v1~".into();
    upgrade(&f.db).await.expect("upgrade");
    assert!(
        !columns(&f.raw, "resource_group")
            .await
            .iter()
            .any(|name| name == "membership_type_id")
    );

    assert_eq!(
        snapshot(&f.raw).await,
        expected,
        "rows, keys and timestamps are kept"
    );
    upgrade(&f.db).await.expect("a repeated upgrade is a no-op");
    assert_eq!(snapshot(&f.raw).await, expected);
    assert_eq!(history(&f.raw).await.len(), Migrator::migrations().len());

    assert_eq!(table_names(&f.raw, "gts_type%").await, ["gts_type"]);
    assert_eq!(
        columns(&f.raw, "gts_type").await,
        ["id", "schema_id", "created_at"]
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT table_name::text, data_type::text FROM information_schema.columns \
             WHERE table_schema = 'public' AND column_name = 'gts_type_id' ORDER BY 1"
        )
        .await,
        vec![
            vec!["resource_group", "smallint"],
            vec!["resource_group_membership", "smallint"],
        ]
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT conname::text FROM pg_constraint \
             WHERE conrelid IN ('resource_group'::regclass, 'resource_group_membership'::regclass) \
               AND contype IN ('f', 'c') ORDER BY 1"
        )
        .await,
        vec![
            vec!["ck_rgm_resource_id_bytes"],
            vec!["fk_resource_group_parent"],
            vec!["fk_rg_gts_type"],
            vec!["fk_rgm_group_id"],
            vec!["fk_rgm_gts_type"],
            vec!["resource_group_name_check"],
        ]
    );
}

#[tokio::test]
async fn pg_constraints_hold_after_the_upgrade() {
    let Some(f) = at_initial_schema().await else {
        return;
    };
    seed_hierarchy(&f.raw).await;
    upgrade(&f.db).await.expect("upgrade");

    let duplicate = exec(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES ('{CHILD}', {USER_KEY}, 'u1')"
        ),
    )
    .await
    .expect_err("membership primary key");
    assert!(
        duplicate
            .to_string()
            .contains("resource_group_membership_pkey"),
        "{duplicate}"
    );

    for (constraint, sql) in [
        (
            "fk_rgm_group_id",
            format!(
                "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
                 VALUES ('00000000-0000-0000-0000-000000000099', {USER_KEY}, 'u3')"
            ),
        ),
        (
            "fk_rgm_gts_type",
            format!(
                "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
                 VALUES ('{ROOT}', 999, 'u3')"
            ),
        ),
        (
            "fk_rg_gts_type",
            format!(
                "INSERT INTO resource_group (id, parent_id, gts_type_id, name, tenant_id) \
                 VALUES ('00000000-0000-0000-0000-000000000098', NULL, 999, 'x', '{ROOT}')"
            ),
        ),
        (
            "fk_rgm_gts_type",
            format!("DELETE FROM gts_type WHERE id = {USER_KEY}"),
        ),
        (
            "fk_resource_group_parent",
            format!("DELETE FROM resource_group WHERE id = '{CHILD}'"),
        ),
    ] {
        let err = exec(&f.raw, &sql).await.expect_err(constraint);
        assert!(err.to_string().contains(constraint), "{constraint}: {err}");
    }

    let same_identifier = exec(
        &f.raw,
        &format!("INSERT INTO gts_type (schema_id) VALUES ('{USER_TYPE}')"),
    )
    .await
    .expect_err("one key per identifier");
    assert!(
        same_identifier
            .to_string()
            .contains("gts_type_schema_id_key"),
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
                 VALUES ('{OTHER_ROOT}', {USER_KEY}, '{resource}')"
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

/// A step failing after DDL has started -- `DROP COLUMN metadata_schema`
/// refused because a view depends on it, after the rule tables were already
/// dropped -- rolls the whole migration back.
#[tokio::test]
async fn pg_a_failure_after_the_first_ddl_rolls_back_the_whole_migration() {
    let Some(f) = at_initial_schema().await else {
        return;
    };
    seed_hierarchy(&f.raw).await;
    run(
        &f.raw,
        "CREATE VIEW rg_type_schemas AS SELECT id, metadata_schema FROM gts_type",
    )
    .await;
    let before = snapshot(&f.raw).await;

    upgrade(&f.db)
        .await
        .expect_err("a dependent view blocks dropping metadata_schema");

    assert_eq!(snapshot(&f.raw).await, before);
    assert_eq!(history(&f.raw).await.len(), 1, "not recorded as applied");
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
        rows(&f.raw, "SELECT count(*)::text FROM gts_type_allowed_parent").await,
        vec![vec!["2"]]
    );
    assert!(
        rows(
            &f.raw,
            "SELECT 1 FROM pg_constraint WHERE conname = 'ck_rgm_resource_id_bytes'"
        )
        .await
        .is_empty()
    );
}
