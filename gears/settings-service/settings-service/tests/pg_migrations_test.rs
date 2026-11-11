#![cfg(feature = "integration")]
// The fixture, the seed and the reader are plain helpers, which
// `allow-expect-in-tests` does not reach; a failed expectation there is the
// test failing, as in the sibling PostgreSQL suites.
#![allow(clippy::expect_used)]
//! The migration chain on a real `PostgreSQL`: the backend every stand runs
//! on, and the one the `sqlite::memory:` suites beside the migrations cannot
//! stand in for. A statement is rendered once per backend, and what `SQLite`
//! accepts `PostgreSQL` may refuse — `m20260917_000001_value_type_namespace`
//! did, over a placeholder spelt `SQLite`'s way, and until this suite nothing
//! had run the chain on `PostgreSQL` short of a stand.
//!
//! The chain goes through `run_migrations_for_testing`, the toolkit runner a
//! stand's startup goes through, so what applies here is what boots. Docker
//! required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_migrations_test
//! ```

use std::collections::BTreeMap;

use cf_gears_settings_service::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use toolkit_db::migration_runner::run_migrations_for_testing;
use uuid::Uuid;

mod common;
use common::pg_fixture;

/// The migration that repoints value types from the toolkit namespace at the
/// gears one.
const MOVE: &str = "m20260917_000001_value_type_namespace";

/// A run of the chain, as the toolkit runner takes it.
type Chain = Vec<Box<dyn MigrationTrait>>;
const OLD_BOOL: &str = "gts.cf.toolkit.settings.type_bool_flag.v1~";
const NEW_BOOL: &str = "gts.cf.core.settings.type_bool_flag.v1~";

/// The chain split where the move sits: everything before it, and the move
/// with everything after it.
fn split_at_the_move() -> (Chain, Chain) {
    let mut from_the_move = Migrator::migrations();
    let at = from_the_move
        .iter()
        .position(|m| m.name() == MOVE)
        .expect("the move is in the chain");
    let before_the_move = from_the_move.drain(..at).collect();
    (before_the_move, from_the_move)
}

fn names(migrations: &Chain) -> Vec<String> {
    migrations.iter().map(|m| m.name().to_owned()).collect()
}

async fn exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_unprepared(sql).await.expect("statement runs");
}

/// Rows a stand had written under the old namespace, and the ones the move
/// must leave alone. Ids are UUIDs, as the `PostgreSQL` columns demand.
async fn seed_rows_written_before_the_move(conn: &DatabaseConnection) {
    let category = Uuid::new_v4();
    exec(
        conn,
        &format!(
            "INSERT INTO categories (id, key, name)
             VALUES ('{category}', 'network', 'Network')"
        ),
    )
    .await;

    for (slug, value_type) in [
        ("old_one", OLD_BOOL),
        ("old_two", "gts.cf.toolkit.settings.type_port.v1~"),
        ("already_moved", NEW_BOOL),
        // A third-party value type that merely resembles ours: the move is
        // anchored at the start of the id, so this must be left alone.
        ("foreign", "gts.acme.settings.type_bool_flag.v1~"),
        // Our namespace, but not our catalogue: `_` is a LIKE wildcard, so a
        // pattern that does not escape it would take `typeX` for `type_`.
        ("look_alike", "gts.cf.toolkit.settings.typeXflag.v1~"),
    ] {
        let id = Uuid::new_v4();
        exec(
            conn,
            &format!(
                "INSERT INTO setting_declarations
                   (id, key, leaf_slug, value_type_id, category_id, default_value,
                    scope_class, created_by)
                 VALUES ('{id}',
                         'gts.cf.core.settings.setting_type.v1~acme.settings.network.{slug}.v1~',
                         '{slug}', '{value_type}', '{category}', 'true', 'local', 'fixture')"
            ),
        )
        .await;
    }
}

/// Every declaration's value type, keyed by its slug.
async fn value_types(conn: &DatabaseConnection) -> BTreeMap<String, String> {
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT leaf_slug, value_type_id FROM setting_declarations",
    ))
    .await
    .expect("readable")
    .iter()
    .map(|row| {
        (
            row.try_get::<String>("", "leaf_slug").expect("a slug"),
            row.try_get::<String>("", "value_type_id").expect("an id"),
        )
    })
    .collect()
}

/// A stand provisioned from scratch: the chain applies on an empty database,
/// a declaration written before the move reads back under the gears namespace
/// after it, and a restart finds nothing outstanding.
#[tokio::test]
async fn the_chain_applies_on_an_empty_database_and_moves_the_rows_written_before_it() {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    let (before_the_move, from_the_move) = split_at_the_move();
    let expected_before = names(&before_the_move);
    let expected_from = names(&from_the_move);

    let applied = run_migrations_for_testing(&fixture.db, before_the_move)
        .await
        .expect("the migrations before the move apply");
    assert_eq!(applied.applied_names, expected_before);
    seed_rows_written_before_the_move(&fixture.conn).await;

    let applied = run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the move and the rest of the chain apply");
    assert_eq!(applied.applied_names, expected_from);

    let after = value_types(&fixture.conn).await;
    assert_eq!(after["old_one"], NEW_BOOL);
    assert_eq!(after["old_two"], "gts.cf.core.settings.type_port.v1~");
    assert_eq!(after["already_moved"], NEW_BOOL);
    assert_eq!(after["foreign"], "gts.acme.settings.type_bool_flag.v1~");
    assert_eq!(after["look_alike"], "gts.cf.toolkit.settings.typeXflag.v1~");

    let again = run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("a restart finds nothing outstanding");
    assert_eq!(again.applied, 0);
    assert_eq!(value_types(&fixture.conn).await, after);
}

/// The display-name migration on the backend it is written for: the column
/// is a nullable `character varying(256)`, and the trigram index that the
/// search `LIKE` leans on exists.
#[tokio::test]
async fn the_display_name_column_and_its_trigram_index_exist_after_the_chain() {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");

    let column = fixture
        .conn
        .query_one_raw(Statement::from_string(
            fixture.conn.get_database_backend(),
            "SELECT data_type, character_maximum_length, is_nullable
             FROM information_schema.columns
             WHERE table_name = 'setting_declarations' AND column_name = 'display_name'",
        ))
        .await
        .expect("readable")
        .expect("the display_name column exists");
    assert_eq!(
        column.try_get::<String>("", "data_type").expect("a type"),
        "character varying"
    );
    assert_eq!(
        column
            .try_get::<i32>("", "character_maximum_length")
            .expect("a length"),
        256
    );
    assert_eq!(
        column.try_get::<String>("", "is_nullable").expect("a flag"),
        "YES",
        "display_name must be nullable"
    );

    let index = fixture
        .conn
        .query_all_raw(Statement::from_string(
            fixture.conn.get_database_backend(),
            "SELECT indexname FROM pg_indexes
             WHERE tablename = 'setting_declarations'
               AND indexname = 'idx_declarations_display_name_trgm'",
        ))
        .await
        .expect("readable");
    assert_eq!(index.len(), 1, "the trigram index on display_name exists");
}

/// The setter-tenant migration on the backend it is written for: each of the
/// three columns is a nullable `uuid`, so a row written before it reads
/// `NULL` and the upward disclosure boundary withholds its identity.
#[tokio::test]
async fn the_setter_tenant_columns_are_nullable_uuids_after_the_chain() {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");

    for (table, column) in [
        ("setting_values", "set_by_tenant"),
        ("tenant_permissions", "set_by_tenant"),
        ("audit_records", "actor_tenant_id"),
    ] {
        let found = fixture
            .conn
            .query_one_raw(Statement::from_string(
                fixture.conn.get_database_backend(),
                format!(
                    "SELECT data_type, is_nullable FROM information_schema.columns
                     WHERE table_name = '{table}' AND column_name = '{column}'"
                ),
            ))
            .await
            .expect("readable")
            .unwrap_or_else(|| panic!("`{table}.{column}` exists after the chain"));
        assert_eq!(
            found.try_get::<String>("", "data_type").expect("a type"),
            "uuid",
            "`{table}.{column}`"
        );
        assert_eq!(
            found.try_get::<String>("", "is_nullable").expect("a flag"),
            "YES",
            "`{table}.{column}` must be nullable"
        );
    }
}

/// The activation migration on the backend it is written for: both
/// per-instance tables, their unique constraints, the partial indexes, and the
/// `audit_records` additions — the widened `operation` vocabulary, the four
/// nullable columns and the `outcome` check widened by `silent` on an
/// `activation` record only.
// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-schema:p1
#[tokio::test]
async fn the_activation_tables_indexes_and_audit_additions_exist_after_the_chain() {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");
    let backend = fixture.conn.get_database_backend();

    // The unique constraints.
    for constraint in [
        "uq_sub_subscriber_instance_key_tenant",
        "uq_activation_state",
    ] {
        let found = fixture
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT conname FROM pg_constraint WHERE conname = '{constraint}' AND contype = 'u'"
                ),
            ))
            .await
            .expect("readable");
        assert_eq!(found.len(), 1, "`{constraint}` exists");
        let columns = fixture
            .conn
            .query_one_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT pg_get_constraintdef(oid) AS def FROM pg_constraint
                     WHERE conname = '{constraint}'"
                ),
            ))
            .await
            .expect("readable")
            .expect("the constraint");
        assert!(
            columns
                .try_get::<String>("", "def")
                .expect("a definition")
                .contains("(subscriber, instance_id, key, tenant)"),
            "`{constraint}` is per instance"
        );
    }

    // The indexes, and the two that are partial.
    let indexes = fixture
        .conn
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT indexname, indexdef FROM pg_indexes
             WHERE indexname IN ('idx_sub_key', 'idx_sub_silent', 'idx_activation_key_tenant',
                                 'idx_activation_awaiting', 'idx_audit_change_ref')",
        ))
        .await
        .expect("readable");
    let defs: BTreeMap<String, String> = indexes
        .iter()
        .map(|row| {
            (
                row.try_get::<String>("", "indexname").expect("a name"),
                row.try_get::<String>("", "indexdef").expect("a definition"),
            )
        })
        .collect();
    assert_eq!(defs.len(), 5, "{defs:?}");
    assert!(
        defs["idx_sub_silent"].contains("(key, tenant, silent_since)")
            && defs["idx_sub_silent"].contains("WHERE (silent_since IS NOT NULL)"),
        "partial over the silent subscriptions: {}",
        defs["idx_sub_silent"]
    );
    assert!(
        defs["idx_activation_awaiting"].contains("WHERE (status = 'awaiting'"),
        "partial over the awaiting rows: {}",
        defs["idx_activation_awaiting"]
    );
    assert!(
        defs["idx_audit_change_ref"].contains("WHERE (change_ref IS NOT NULL)"),
        "partial over the rows that carry a change_ref: {}",
        defs["idx_audit_change_ref"]
    );

    per_instance_columns(&fixture.conn).await;

    // The audit additions: nullable columns, and `activation` accepted.
    for (column, kind) in [
        ("change_ref", "uuid"),
        ("detail", "text"),
        ("instance_id", "uuid"),
        ("instance_label", "text"),
    ] {
        let found = fixture
            .conn
            .query_one_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT data_type, is_nullable FROM information_schema.columns
                     WHERE table_name = 'audit_records' AND column_name = '{column}'"
                ),
            ))
            .await
            .expect("readable")
            .unwrap_or_else(|| panic!("`audit_records.{column}` exists"));
        assert_eq!(
            found.try_get::<String>("", "data_type").expect("type"),
            kind
        );
        assert_eq!(
            found.try_get::<String>("", "is_nullable").expect("flag"),
            "YES"
        );
    }
    // One `CHECK` on `operation` and one on `outcome` — which names
    // `operation` too, to admit `silent` on an `activation` record only —
    // and no other `CHECK` naming either column. Both are left `NOT VALID`:
    // widened, they cannot reject a row the narrower ones admitted, so the
    // migration skips the scan under its `ACCESS EXCLUSIVE` lock; they are
    // still enforced on every new row, as the inserts below show.
    let checks = fixture
        .conn
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT conname, convalidated FROM pg_constraint
             WHERE conrelid = 'audit_records'::regclass AND contype = 'c'
               AND pg_get_constraintdef(oid) ~ '\\y(operation|outcome)\\y'
             ORDER BY conname",
        ))
        .await
        .expect("readable");
    let names: Vec<String> = checks
        .iter()
        .map(|row| row.try_get::<String>("", "conname").expect("a name"))
        .collect();
    assert_eq!(
        names,
        [
            "audit_records_operation_check",
            "audit_records_outcome_check"
        ]
    );
    assert!(
        checks
            .iter()
            .all(|row| !row.try_get::<bool>("", "convalidated").expect("flag")),
        "the widened checks are added `NOT VALID` and not validated in the migration"
    );
    let id = Uuid::new_v4();
    exec(
        &fixture.conn,
        &format!(
            "INSERT INTO audit_records
               (id, resource, declaration_key, operation, actor, actor_classification,
                outcome, request_id, change_ref, detail)
             VALUES ('{id}', 'cf.settings:k@t', 'k', 'activation', 'gear', 'public',
                     'failure', 'req', '{}', 'could not apply')",
            Uuid::new_v4()
        ),
    )
    .await;
    assert!(
        fixture
            .conn
            .execute_unprepared(&format!(
                "INSERT INTO audit_records
                   (id, resource, declaration_key, operation, actor, actor_classification,
                    outcome, request_id)
                 VALUES ('{}', 'cf.settings:k@t', 'k', 'delete', 'gear', 'public', 'success', 'req')",
                Uuid::new_v4()
            ))
            .await
            .is_err(),
        "an operation outside the vocabulary is still refused"
    );
    let silent = |operation: &str| {
        format!(
            "INSERT INTO audit_records
               (id, resource, declaration_key, operation, actor, actor_classification,
                outcome, request_id)
             VALUES ('{}', 'cf.settings:k@t', 'k', '{operation}', 'gear', 'public', 'silent', 'req')",
            Uuid::new_v4()
        )
    };
    exec(&fixture.conn, &silent("activation")).await;
    assert!(
        fixture
            .conn
            .execute_unprepared(&silent("change"))
            .await
            .is_err(),
        "`silent` is an outcome of an `activation` record only"
    );

    state_defaults(&fixture.conn).await;
}

/// The per-instance columns of `subscriptions` and `activation_state`, with
/// their types and nullability.
async fn per_instance_columns(conn: &DatabaseConnection) {
    let backend = conn.get_database_backend();
    for (table, column, kind, nullable) in [
        ("subscriptions", "instance_id", "uuid", "NO"),
        ("subscriptions", "instance_label", "text", "YES"),
        (
            "subscriptions",
            "silent_since",
            "timestamp with time zone",
            "YES",
        ),
        ("activation_state", "instance_id", "uuid", "NO"),
        (
            "activation_state",
            "changed_at",
            "timestamp with time zone",
            "NO",
        ),
        ("activation_state", "deliveries", "integer", "NO"),
        (
            "activation_state",
            "silent_since",
            "timestamp with time zone",
            "YES",
        ),
    ] {
        let found = conn
            .query_one_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT data_type, is_nullable FROM information_schema.columns
                     WHERE table_name = '{table}' AND column_name = '{column}'"
                ),
            ))
            .await
            .expect("readable")
            .unwrap_or_else(|| panic!("`{table}.{column}` exists"));
        assert_eq!(
            found.try_get::<String>("", "data_type").expect("type"),
            kind,
            "`{table}.{column}`"
        );
        assert_eq!(
            found.try_get::<String>("", "is_nullable").expect("flag"),
            nullable,
            "`{table}.{column}`"
        );
    }
}

/// A state's status is checked and defaults to `awaiting`, its delivery count
/// to `0`; `silent` is in the vocabulary.
async fn state_defaults(conn: &DatabaseConnection) {
    let backend = conn.get_database_backend();
    exec(
        conn,
        &format!(
            "INSERT INTO activation_state
               (subscriber, instance_id, key, tenant, change_ref, changed_at, expected_value)
             VALUES ('gear', '{}', 'k', 't', '{}', now(), 'true')",
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
    )
    .await;
    let status = conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT status, deliveries FROM activation_state WHERE subscriber = 'gear'",
        ))
        .await
        .expect("readable")
        .expect("the row");
    assert_eq!(
        status.try_get::<String>("", "status").expect("status"),
        "awaiting"
    );
    assert_eq!(status.try_get::<i32>("", "deliveries").expect("count"), 0);
    exec(
        conn,
        &format!(
            "INSERT INTO activation_state
               (subscriber, instance_id, key, tenant, change_ref, changed_at, expected_value,
                status)
             VALUES ('gear', '{}', 'k', 't', '{}', now(), 'true', 'silent')",
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
    )
    .await;
}

/// What the constraints do on `PostgreSQL`, row by row: a second subscription
/// or activation state for one subscriber, instance, key and tenant is refused
/// while two instances of one subscriber hold two of each; a `status` outside
/// `awaiting`, `succeeded`, `failed` and `silent` is refused; and a `silent`
/// outcome is refused on any record but an `activation` one.
// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-instance-tests:p1
#[tokio::test]
async fn the_per_instance_constraints_refuse_a_second_row_and_an_unknown_status() {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");
    let conn = &fixture.conn;
    let (one, two) = (Uuid::new_v4(), Uuid::new_v4());
    let subscription = |instance: Uuid, tenant: &str| {
        format!(
            "INSERT INTO subscriptions (id, subscriber, instance_id, key, tenant)
             VALUES ('{}', 'gear', '{instance}', 'k', '{tenant}')",
            Uuid::new_v4()
        )
    };
    let state = |instance: Uuid, status: &str| {
        format!(
            "INSERT INTO activation_state
               (subscriber, instance_id, key, tenant, change_ref, changed_at, expected_value, status)
             VALUES ('gear', '{instance}', 'k', 't', '{}', now(), 'true', '{status}')",
            Uuid::new_v4()
        )
    };

    exec(conn, &subscription(one, "t")).await;
    assert!(
        conn.execute_unprepared(&subscription(one, "t"))
            .await
            .is_err(),
        "`uq_sub_subscriber_instance_key_tenant` refuses the same instance twice"
    );
    exec(conn, &subscription(two, "t")).await;
    exec(conn, &subscription(one, "other")).await;

    exec(conn, &state(one, "awaiting")).await;
    assert!(
        conn.execute_unprepared(&state(one, "failed"))
            .await
            .is_err(),
        "`uq_activation_state` keeps one row per instance"
    );
    exec(conn, &state(two, "silent")).await;
    let unknown = conn
        .execute_unprepared(&state(Uuid::new_v4(), "expired"))
        .await;
    assert!(
        unknown.is_err(),
        "a status outside the four values is refused"
    );
    for status in ["succeeded", "failed", "silent"] {
        exec(conn, &state(Uuid::new_v4(), status)).await;
    }

    // `silent` on a subscription and its partial index: a row with it set is
    // what `idx_sub_silent` holds.
    exec(
        conn,
        &format!("UPDATE subscriptions SET silent_since = now() WHERE instance_id = '{two}'"),
    )
    .await;
    let silent = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT 1 FROM subscriptions WHERE silent_since IS NOT NULL",
        ))
        .await
        .expect("readable");
    assert_eq!(silent.len(), 1);
}

/// The activation migration, the first of the two that make the feature's
/// schema.
const ACTIVATION: &str = "m20261031_000001_activation_state";

/// The operations and outcomes an earlier build's `CHECK`s admit.
const OPERATIONS_BEFORE: [&str; 7] = [
    "create",
    "change",
    "revert",
    "remove",
    "clone",
    "secret_use",
    "stage",
];
const OUTCOMES_BEFORE: [&str; 2] = ["success", "failure"];

/// How many migrations of the chain precede the activation one.
fn before_activation() -> u32 {
    let at = Migrator::migrations()
        .iter()
        .position(|m| m.name() == ACTIVATION)
        .expect("the activation migration is in the chain");
    u32::try_from(at).expect("a small list")
}

/// How many migrations the activation one and everything after it come to.
fn from_activation() -> u32 {
    u32::try_from(Migrator::migrations().len()).expect("a small list") - before_activation()
}

/// The chain up to just before the activation migration, applied with the
/// migrator itself so that its `down` finds the history it records.
async fn migrate_to_before_activation(conn: &DatabaseConnection) {
    Migrator::up(conn, Some(before_activation()))
        .await
        .expect("everything before the activation migration applies");
}

/// One audit row of every operation an earlier build admits, each with both
/// outcomes it admits. The row's `declaration_key` names the pair.
async fn seed_every_earlier_audit_value(conn: &DatabaseConnection) {
    for operation in OPERATIONS_BEFORE {
        for outcome in OUTCOMES_BEFORE {
            exec(
                conn,
                &format!(
                    "INSERT INTO audit_records
                       (id, resource, declaration_key, operation, actor, actor_classification,
                        outcome, request_id)
                     VALUES ('{}', 'cf.settings:k@t', '{operation}/{outcome}', '{operation}',
                             'who', 'public', '{outcome}', 'req')",
                    Uuid::new_v4()
                ),
            )
            .await;
        }
    }
}

/// The seeded `(operation, outcome)` pairs now in `audit_records`, as the
/// `declaration_key`s the seed gave them.
async fn audit_keys(conn: &DatabaseConnection) -> Vec<String> {
    let mut keys: Vec<String> = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT declaration_key FROM audit_records ORDER BY declaration_key",
        ))
        .await
        .expect("readable")
        .iter()
        .map(|row| row.try_get::<String>("", "declaration_key").expect("a key"))
        .collect();
    keys.sort();
    keys
}

fn seeded_keys() -> Vec<String> {
    let mut keys: Vec<String> = OPERATIONS_BEFORE
        .iter()
        .flat_map(|op| OUTCOMES_BEFORE.iter().map(move |out| format!("{op}/{out}")))
        .collect();
    keys.sort();
    keys
}

async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
    conn.query_one_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .expect("readable")
        .expect("a row")
        .try_get::<i64>("", "n")
        .expect("a count")
}

async fn table_exists(conn: &DatabaseConnection, table: &str) -> bool {
    count(
        conn,
        &format!(
            "SELECT count(*) AS n FROM information_schema.tables
             WHERE table_schema = current_schema() AND table_name = '{table}'"
        ),
    )
    .await
        == 1
}

async fn audit_column_exists(conn: &DatabaseConnection, column: &str) -> bool {
    count(
        conn,
        &format!(
            "SELECT count(*) AS n FROM information_schema.columns
             WHERE table_name = 'audit_records' AND column_name = '{column}'"
        ),
    )
    .await
        == 1
}

async fn index_exists(conn: &DatabaseConnection, index: &str) -> bool {
    count(
        conn,
        &format!("SELECT count(*) AS n FROM pg_indexes WHERE indexname = '{index}'"),
    )
    .await
        == 1
}

/// The `operation` and `outcome` `CHECK`s of `audit_records`:
/// `(name, definition, validated)`, by name.
async fn audit_checks(conn: &DatabaseConnection) -> Vec<(String, String, bool)> {
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT conname, pg_get_constraintdef(oid) AS def, convalidated FROM pg_constraint
         WHERE conrelid = 'audit_records'::regclass AND contype = 'c'
           AND pg_get_constraintdef(oid) ~ '\\y(operation|outcome)\\y'
         ORDER BY conname",
    ))
    .await
    .expect("readable")
    .iter()
    .map(|row| {
        (
            row.try_get::<String>("", "conname").expect("a name"),
            row.try_get::<String>("", "def").expect("a definition"),
            row.try_get::<bool>("", "convalidated").expect("a flag"),
        )
    })
    .collect()
}

fn audit_insert(operation: &str, outcome: &str, extra_columns: &str, extra_values: &str) -> String {
    format!(
        "INSERT INTO audit_records
           (id, resource, declaration_key, operation, actor, actor_classification,
            outcome, request_id{extra_columns})
         VALUES ('{}', 'cf.settings:k@t', 'new/{operation}/{outcome}', '{operation}', 'gear',
                 'public', '{outcome}', 'req'{extra_values})",
        Uuid::new_v4()
    )
}

/// Applying the activation migrations over an `audit_records` that already
/// holds a row of every operation and outcome an earlier build admits: no
/// row is lost or converted, the widened checks — left `NOT VALID`, so the
/// existing rows were never scanned — admit what they add and still refuse
/// what they do not.
#[tokio::test]
async fn the_activation_migrations_keep_every_earlier_audit_row_and_widen_the_checks() {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    let conn = &fixture.conn;
    migrate_to_before_activation(conn).await;
    seed_every_earlier_audit_value(conn).await;
    assert_eq!(audit_keys(conn).await, seeded_keys());

    Migrator::up(conn, None)
        .await
        .expect("the activation migrations apply over a populated audit_records");

    assert_eq!(
        audit_keys(conn).await,
        seeded_keys(),
        "every seeded row survives, none converted"
    );
    assert_eq!(
        count(
            conn,
            "SELECT count(*) AS n FROM audit_records
             WHERE change_ref IS NOT NULL OR detail IS NOT NULL
                OR instance_id IS NOT NULL OR instance_label IS NOT NULL"
        )
        .await,
        0,
        "the new columns read NULL on the rows from before"
    );
    let checks = audit_checks(conn).await;
    assert_eq!(checks.len(), 2, "{checks:?}");
    assert!(
        checks.iter().all(|(_, _, validated)| !validated),
        "added NOT VALID, and the migration does not scan the existing rows: {checks:?}"
    );

    exec(
        conn,
        &audit_insert(
            "activation",
            "failure",
            ", change_ref, detail",
            &format!(", '{}', 'could not apply'", Uuid::new_v4()),
        ),
    )
    .await;
    exec(conn, &audit_insert("activation", "silent", "", "")).await;
    for (operation, outcome, why) in [
        ("delete", "success", "an operation outside the vocabulary"),
        (
            "change",
            "silent",
            "`silent` on a record that is not an activation",
        ),
        ("activation", "unknown", "an outcome outside the vocabulary"),
    ] {
        assert!(
            conn.execute_unprepared(&audit_insert(operation, outcome, "", ""))
                .await
                .is_err(),
            "{why} is still refused: the NOT VALID checks enforce every new row"
        );
    }
    assert_eq!(
        count(conn, "SELECT count(*) AS n FROM audit_records").await,
        i64::try_from(seeded_keys().len()).expect("small") + 2,
        "only the two admitted rows were added"
    );
}

/// Rolling the activation migrations back while no `activation` record
/// exists: the tables, the four audit columns and `idx_audit_change_ref` go,
/// the earlier `CHECK`s are back and validated, and the rows written before
/// are intact; an `activation` record is refused again.
#[tokio::test]
async fn rolling_the_activation_migrations_back_without_an_activation_record_restores_the_earlier_schema()
 {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    let conn = &fixture.conn;
    migrate_to_before_activation(conn).await;
    seed_every_earlier_audit_value(conn).await;
    Migrator::up(conn, None).await.expect("the chain applies");
    exec(
        conn,
        &format!(
            "INSERT INTO subscriptions (id, subscriber, instance_id, key, tenant)
             VALUES ('{}', 'gear', '{}', 'k', 't')",
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
    )
    .await;
    assert!(table_exists(conn, "subscriptions").await);
    assert!(index_exists(conn, "idx_audit_change_ref").await);

    Migrator::down(conn, Some(from_activation()))
        .await
        .expect("no activation record: the narrowed checks validate and the rollback applies");

    for table in ["subscriptions", "activation_state"] {
        assert!(!table_exists(conn, table).await, "`{table}` is dropped");
    }
    for column in ["change_ref", "detail", "instance_id", "instance_label"] {
        assert!(
            !audit_column_exists(conn, column).await,
            "`audit_records.{column}` is dropped"
        );
    }
    for index in [
        "idx_audit_change_ref",
        "idx_sub_key",
        "idx_sub_silent",
        "idx_activation_key_tenant",
        "idx_activation_awaiting",
    ] {
        assert!(!index_exists(conn, index).await, "`{index}` is dropped");
    }
    let checks = audit_checks(conn).await;
    let names: Vec<&str> = checks.iter().map(|(name, _, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "audit_records_operation_check",
            "audit_records_outcome_check"
        ]
    );
    assert!(
        checks.iter().all(|(_, def, validated)| *validated
            && !def.contains("activation")
            && !def.contains("silent")),
        "the pre-activation checks are back, validated: {checks:?}"
    );
    assert_eq!(
        audit_keys(conn).await,
        seeded_keys(),
        "the rows written before are intact"
    );
    for (operation, outcome) in [("activation", "success"), ("activation", "silent")] {
        assert!(
            conn.execute_unprepared(&audit_insert(operation, outcome, "", ""))
                .await
                .is_err(),
            "an earlier build's checks refuse `{operation}`/`{outcome}`"
        );
    }

    Migrator::up(conn, None)
        .await
        .expect("the migrations apply again after the rollback");
    assert!(table_exists(conn, "activation_state").await);
    assert!(index_exists(conn, "idx_audit_change_ref").await);
}

/// Rolling back while an `activation` record exists: the narrowing `CHECK`
/// is validated, finds the record and refuses the step, and nothing of the
/// activation migration is half-applied — its tables and columns, its widened
/// checks and every row are as before the attempt. The two migrations are two
/// transactions: the index migration's rollback ran first and committed, so
/// the index is the one thing the failed attempt took.
#[tokio::test]
async fn rolling_back_while_an_activation_record_exists_is_refused_and_applies_nothing_of_it() {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    let conn = &fixture.conn;
    migrate_to_before_activation(conn).await;
    seed_every_earlier_audit_value(conn).await;
    Migrator::up(conn, None).await.expect("the chain applies");
    exec(
        conn,
        &audit_insert(
            "activation",
            "silent",
            ", change_ref",
            &format!(", '{}'", Uuid::new_v4()),
        ),
    )
    .await;
    exec(
        conn,
        &format!(
            "INSERT INTO activation_state
               (subscriber, instance_id, key, tenant, change_ref, changed_at, expected_value)
             VALUES ('gear', '{}', 'k', 't', '{}', now(), 'true')",
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
    )
    .await;
    let checks_before = audit_checks(conn).await;
    let audit_rows = count(conn, "SELECT count(*) AS n FROM audit_records").await;

    let refused = Migrator::down(conn, Some(from_activation()))
        .await
        .expect_err("the narrowed checks refuse the activation record");
    let message = refused.to_string();
    assert!(
        message.contains("audit_records_operation_check") && message.contains("violated"),
        "the validating step is what refuses: {message}"
    );

    for table in ["subscriptions", "activation_state"] {
        assert!(table_exists(conn, table).await, "`{table}` is kept");
    }
    assert_eq!(
        count(conn, "SELECT count(*) AS n FROM activation_state").await,
        1,
        "the state row is kept"
    );
    for column in ["change_ref", "detail", "instance_id", "instance_label"] {
        assert!(
            audit_column_exists(conn, column).await,
            "`audit_records.{column}` is kept"
        );
    }
    assert_eq!(
        audit_checks(conn).await,
        checks_before,
        "the widened checks are still in place, not half-replaced"
    );
    assert_eq!(
        count(conn, "SELECT count(*) AS n FROM audit_records").await,
        audit_rows,
        "no audit row is lost"
    );
    assert_eq!(
        count(
            conn,
            "SELECT count(*) AS n FROM audit_records WHERE operation = 'activation'"
        )
        .await,
        1
    );
    assert!(
        !index_exists(conn, "idx_audit_change_ref").await,
        "the index migration's own rollback committed before the refused step"
    );
    let recorded = count(
        conn,
        &format!("SELECT count(*) AS n FROM seaql_migrations WHERE version = '{ACTIVATION}'"),
    )
    .await;
    assert_eq!(
        recorded, 1,
        "the activation migration is still recorded as applied"
    );
}
