//! The quota catalog store on `SQLite` (P-D-274): the evidence tables are append-only, a candidate
//! moves only its review columns, every CHECK and key holds, and no child row names another
//! tenant's parent. The Postgres twins are in `tests/postgres_quota_catalog.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr, Statement};

const QUOTA_TABLES: &[&str] = &[
    "products_quota_adapter_target",
    "products_quota_snapshot",
    "products_quota_candidate",
    "products_quota_mapping_version",
    "products_quota_definition_version",
    "products_quota_definition_resource_type",
    "products_quota_availability",
];
const T1: &str = "00000000-0000-0000-0000-0000000000a1";
const TARGET: &str = "00000000-0000-0000-0000-0000000000b1";
const SNAPSHOT: &str = "00000000-0000-0000-0000-0000000000b2";
const CANDIDATE: &str = "00000000-0000-0000-0000-0000000000b3";
const MAPPING: &str = "00000000-0000-0000-0000-0000000000b4";
const PROFILE: &str = "00000000-0000-0000-0000-0000000000b5";
const DEFINITION: &str = "00000000-0000-0000-0000-0000000000b6";
const RESOURCE: &str = "00000000-0000-0000-0000-0000000000b7";
const ACTOR: &str = "00000000-0000-0000-0000-0000000000c1";
const AT: &str = "2026-11-19T12:00:00Z";

fn hex(c: char) -> String {
    c.to_string().repeat(64)
}

struct Fixture {
    db: DatabaseConnection,
    target: &'static str,
    snapshot: &'static str,
}

impl Fixture {
    /// The chain's migration applied and one row in each table, through raw SQL.
    async fn seeded() -> Self {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        let f = Self {
            db,
            target: TARGET,
            snapshot: SNAPSHOT,
        };
        f.exec(&target_row(T1, TARGET, "pool-a")).await.unwrap();
        f.exec(&snapshot_row(T1, SNAPSHOT, TARGET, "7", &hex('a')))
            .await
            .unwrap();
        f.exec(&candidate_row(
            T1,
            CANDIDATE,
            "discovery",
            Some(SNAPSHOT),
            "staged",
            1,
        ))
        .await
        .unwrap();
        f.exec(&mapping_row(T1, MAPPING, 1)).await.unwrap();
        f.exec(&definition_row(T1, DEFINITION, 1, CANDIDATE))
            .await
            .unwrap();
        f.exec(&format!(
            "INSERT INTO products_quota_definition_resource_type VALUES ('{T1}', '{DEFINITION}', \
             1, '{RESOURCE}')"
        ))
        .await
        .unwrap();
        f.insert_availability("disabled", Some("qualification_pending"))
            .await
            .unwrap();
        f
    }

    async fn exec(&self, sql: &str) -> Result<(), DbErr> {
        self.db
            .execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .map(|_| ())
    }

    async fn insert_snapshot(
        &self,
        target: &str,
        revision: &str,
        digest: &str,
    ) -> Result<(), DbErr> {
        self.exec(&snapshot_row(
            T1,
            &uuid::Uuid::new_v4().to_string(),
            target,
            revision,
            digest,
        ))
        .await
    }

    async fn insert_snapshot_for_tenant(
        &self,
        tenant: uuid::Uuid,
        target: &str,
        revision: &str,
    ) -> Result<(), DbErr> {
        self.exec(&snapshot_row(
            &tenant.to_string(),
            &uuid::Uuid::new_v4().to_string(),
            target,
            revision,
            &hex('c'),
        ))
        .await
    }

    /// A candidate of `origin` and `snapshot` under a capability key no other row holds, so only
    /// the CHECKs can refuse it, never a key's trigger.
    async fn insert_candidate_with_origin(
        &self,
        origin: &str,
        snapshot: Option<&str>,
    ) -> Result<(), DbErr> {
        let row = candidate_row(
            T1,
            &uuid::Uuid::new_v4().to_string(),
            origin,
            snapshot,
            "staged",
            1,
        )
        .replacen(
            "'tenant-vcpu'",
            &format!("'key-{}'", uuid::Uuid::new_v4().simple()),
            1,
        );
        self.exec(&row).await
    }

    async fn insert_availability(&self, state: &str, reason: Option<&str>) -> Result<(), DbErr> {
        let revision: i64 = self
            .db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT count(*) + 1 AS n FROM products_quota_availability".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap();
        self.exec(&availability_row(T1, revision, state, reason, None))
            .await
    }

    async fn table_exists(&self, table: &str) -> bool {
        SchemaManager::new(&self.db).has_table(table).await.unwrap()
    }

    async fn down(&self) -> Result<(), DbErr> {
        Migration.down(&SchemaManager::new(&self.db)).await
    }

    async fn plan(&self, sql: &str) -> String {
        self.db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("EXPLAIN QUERY PLAN {sql}"),
            ))
            .await
            .unwrap()
            .iter()
            .map(|row| row.try_get::<String>("", "detail").unwrap())
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The text column `v` of the one row `sql` reads.
    async fn text(&self, sql: &str) -> String {
        self.db
            .query_one_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "v")
            .unwrap()
    }
}

fn target_row(tenant: &str, id: &str, key: &str) -> String {
    format!(
        "INSERT INTO products_quota_adapter_target VALUES ('{tenant}', '{id}', \
         'plugin-instance', \
         '{key}', 'Pool A', '{ACTOR}', '{AT}')"
    )
}

fn snapshot_row(tenant: &str, id: &str, target: &str, revision: &str, digest: &str) -> String {
    format!(
        "INSERT INTO products_quota_snapshot VALUES ('{tenant}', '{id}', '{target}', \
         '9144d0b7-3497-4db9-89e7-69c2c5d8f035', '{revision}', '{AT}', 1, '{digest}', '{{}}', \
         '{{}}', '{{}}', '{ACTOR}', '{AT}')"
    )
}

fn candidate_row(
    tenant: &str,
    id: &str,
    origin: &str,
    snapshot: Option<&str>,
    state: &str,
    version: i64,
) -> String {
    let snapshot = snapshot.map_or_else(|| "NULL".to_owned(), |s| format!("'{s}'"));
    format!(
        "INSERT INTO products_quota_candidate (tenant_id, id, target_id, origin, snapshot_row_id, \
         record_id, capability_key, record_json, record_digest, evidence_json, state, version, \
         created_by, created_at) VALUES ('{tenant}', '{id}', '{TARGET}', '{origin}', {snapshot}, \
         '{}', 'tenant-vcpu', '{{}}', '{}', '{{}}', '{state}', {version}, '{ACTOR}', '{AT}')",
        uuid::Uuid::new_v4(),
        hex('d'),
    )
}

fn mapping_row(tenant: &str, id: &str, version: i64) -> String {
    format!(
        "INSERT INTO products_quota_mapping_version VALUES ('{tenant}', '{id}', {version}, \
         '{TARGET}', 'tenant-vcpu', 'plugin', 'pool-a', 'tenant-vcpu-mapping-3', '{{}}', '{}', \
         '{PROFILE}', 1, '{{}}', '{}', '{ACTOR}', '{AT}')",
        hex('e'),
        hex('f'),
    )
}

fn definition_row(tenant: &str, id: &str, version: i64, candidate: &str) -> String {
    definition_row_at(tenant, id, (version, 2), candidate)
}

/// [`definition_row`] at `(version, sku_version)`.
fn definition_row_at(
    tenant: &str,
    id: &str,
    (version, sku_version): (i64, i64),
    candidate: &str,
) -> String {
    format!(
        "INSERT INTO products_quota_definition_version VALUES ('{tenant}', '{id}', {version}, \
         '{candidate}', '{}', {sku_version}, '{}', '{}', '{PROFILE}', '{MAPPING}', 1, '{{}}', \
         '{}', '{ACTOR}', '{AT}')",
        uuid::Uuid::from_u128(0x5c),
        uuid::Uuid::from_u128(0x3e),
        uuid::Uuid::from_u128(0x5b),
        hex('0'),
    )
}

fn availability_row(
    tenant: &str,
    revision: i64,
    state: &str,
    reason: Option<&str>,
    qualification: Option<(&str, &str, &str)>,
) -> String {
    let reason = reason.map_or_else(|| "NULL".to_owned(), |r| format!("'{r}'"));
    let (id, version, digest) = qualification.unwrap_or(("NULL", "NULL", "NULL"));
    format!(
        "INSERT INTO products_quota_availability VALUES ('{tenant}', '{DEFINITION}', 1, \
         {revision}, '{state}', {reason}, {id}, {version}, {digest}, '{AT}', '{ACTOR}')"
    )
}

/// The quota tables' named indexes and triggers as `SQLite`'s catalog lists them, each as its
/// kind (`unique index`, `index` or `trigger`) and name, sorted. A primary key's automatic index
/// has no SQL and is not listed.
async fn quota_guards(db: &DatabaseConnection) -> Vec<String> {
    db.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT CASE WHEN type = 'index' AND sql LIKE 'CREATE UNIQUE INDEX %' THEN 'unique index' \
         ELSE type END || ' ' || name AS v FROM sqlite_master WHERE type IN ('index', 'trigger') \
         AND substr(tbl_name, 1, 15) = 'products_quota_' AND sql IS NOT NULL ORDER BY v"
            .to_owned(),
    ))
    .await
    .unwrap()
    .iter()
    .map(|row| row.try_get::<String>("", "v").unwrap())
    .collect()
}

/// `up` twice, `down` twice and `up` again: the tables, their indexes and their triggers are
/// gone after `down` and all back after the second `up`, the unique lineage index and its
/// no-replace trigger included.
#[tokio::test]
async fn applies_replays_and_reverts_on_sqlite() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.unwrap();
    Migration.up(&manager).await.unwrap();
    for table in QUOTA_TABLES {
        assert!(manager.has_table(*table).await.unwrap(), "{table}");
    }
    let guards = quota_guards(&db).await;
    for named in [
        "unique index uq_products_quota_mapping_version_lineage",
        "trigger products_quota_mapping_version_no_replace_lineage",
    ] {
        assert!(guards.iter().any(|g| g == named), "{named}: {guards:?}");
    }
    Migration.down(&manager).await.unwrap();
    Migration.down(&manager).await.unwrap();
    for table in QUOTA_TABLES {
        assert!(!manager.has_table(*table).await.unwrap(), "{table}");
    }
    assert_eq!(quota_guards(&db).await, Vec::<String>::new());
    Migration.up(&manager).await.unwrap();
    for table in QUOTA_TABLES {
        assert!(manager.has_table(*table).await.unwrap(), "{table} again");
    }
    assert_eq!(quota_guards(&db).await, guards, "the guards again");
}

#[tokio::test]
async fn every_evidence_table_refuses_update_and_delete() {
    let f = Fixture::seeded().await;
    for (table, key) in [
        ("products_quota_adapter_target", "id"),
        ("products_quota_snapshot", "id"),
        ("products_quota_mapping_version", "mapping_id"),
        ("products_quota_definition_version", "definition_id"),
        ("products_quota_definition_resource_type", "definition_id"),
        ("products_quota_availability", "definition_id"),
    ] {
        let update = f
            .exec(&format!("UPDATE {table} SET {key} = {key}"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            update.contains(&format!("{table} is append-only: UPDATE")),
            "{table} update: {update}"
        );
        let delete = f
            .exec(&format!("DELETE FROM {table}"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            delete.contains(&format!("{table} is append-only: DELETE")),
            "{table} delete: {delete}"
        );
    }
}

#[tokio::test]
async fn a_candidate_moves_only_its_review_columns() {
    let f = Fixture::seeded().await;
    f.exec(&format!(
        "UPDATE products_quota_candidate SET state = 'submitted', version = 2, \
         proposal_json = '{{}}', proposal_evidence_json = '{{}}', submitted_by = '{ACTOR}', \
         submitted_at = '{AT}', submit_note = 'n'"
    ))
    .await
    .unwrap();
    for set in [
        "tenant_id = 'x'".to_owned(),
        "id = 'x'".to_owned(),
        "target_id = 'x'".to_owned(),
        "origin = 'manual'".to_owned(),
        "snapshot_row_id = NULL".to_owned(),
        "record_id = 'x'".to_owned(),
        "capability_key = 'other'".to_owned(),
        "record_json = '[]'".to_owned(),
        format!("record_digest = '{}'", hex('1')),
        "evidence_json = '[]'".to_owned(),
        "created_by = 'x'".to_owned(),
        "created_at = 'x'".to_owned(),
    ] {
        let content = f
            .exec(&format!("UPDATE products_quota_candidate SET {set}"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            content.contains("products_quota_candidate content is immutable"),
            "{set}: {content}"
        );
    }
    f.exec(&format!(
        "UPDATE products_quota_candidate SET state = 'approved', version = 3, \
         decided_by = '{ACTOR}', decided_at = '{AT}', decided_note = 'ok', \
         definition_id = '{DEFINITION}', definition_version = 1"
    ))
    .await
    .unwrap();
    let delete = f
        .exec("DELETE FROM products_quota_candidate")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        delete.contains("products_quota_candidate is append-only: DELETE"),
        "{delete}"
    );
}

/// A decided candidate is frozen: once approved or rejected, no column moves, its review
/// columns included, and a no-op `UPDATE` is refused too.
#[tokio::test]
async fn a_decided_candidate_is_frozen() {
    let f = Fixture::seeded().await;
    for state in ["approved", "rejected"] {
        let id = uuid::Uuid::new_v4();
        f.exec(&candidate_row(
            T1,
            &id.to_string(),
            "manual",
            None,
            state,
            3,
        ))
        .await
        .unwrap();
        for set in [
            "state = state",
            "decided_note = 'changed'",
            "state = 'submitted', version = 4",
            "definition_id = NULL, definition_version = NULL",
            "proposal_json = '{\"other\": 1}'",
        ] {
            let refused = f
                .exec(&format!(
                    "UPDATE products_quota_candidate SET {set} WHERE id = '{id}'"
                ))
                .await
                .unwrap_err()
                .to_string();
            assert!(
                refused.contains("products_quota_candidate is decided: UPDATE is not permitted"),
                "{state} {set}: {refused}"
            );
        }
    }
    f.exec(&format!(
        "UPDATE products_quota_candidate SET submit_note = 'still open' WHERE id = '{CANDIDATE}'"
    ))
    .await
    .unwrap();
}

/// `REPLACE` resolves a key conflict by deleting the stored row, which fires no delete trigger;
/// every quota table refuses an insert whose key is stored, so a stored row is never replaced.
#[tokio::test]
async fn a_replace_never_deletes_a_stored_row() {
    let f = Fixture::seeded().await;
    let replace = |insert: String| insert.replacen("INSERT INTO", "REPLACE INTO", 1);
    let lone_target = uuid::Uuid::new_v4().to_string();
    let lone_snapshot = uuid::Uuid::new_v4().to_string();
    let lone_candidate = uuid::Uuid::new_v4().to_string();
    let lone_mapping = uuid::Uuid::new_v4().to_string();
    let lone_definition = uuid::Uuid::new_v4().to_string();
    let record = uuid::Uuid::new_v4();
    let manual = |id: &str| {
        format!(
            "INSERT INTO products_quota_candidate (tenant_id, id, target_id, origin, record_id, \
             capability_key, record_json, record_digest, evidence_json, state, version, \
             created_by, created_at) VALUES ('{T1}', '{id}', '{TARGET}', 'manual', '{record}', \
             'k', '{{}}', '{}', '{{}}', 'staged', 1, '{ACTOR}', '{AT}')",
            hex('d')
        )
    };
    f.exec(&target_row(T1, &lone_target, "pool-z"))
        .await
        .unwrap();
    f.exec(&snapshot_row(T1, &lone_snapshot, TARGET, "8", &hex('a')))
        .await
        .unwrap();
    f.exec(&candidate_row(
        T1,
        &lone_candidate,
        "discovery",
        Some(&lone_snapshot),
        "staged",
        1,
    ))
    .await
    .unwrap();
    f.exec(&manual(&uuid::Uuid::new_v4().to_string()))
        .await
        .unwrap();
    f.exec(&mapping_row(T1, &lone_mapping, 2)).await.unwrap();
    let mut attempts = vec![
        replace(target_row(T1, &lone_target, "pool-y")),
        replace(target_row(T1, &uuid::Uuid::new_v4().to_string(), "pool-z")),
        replace(snapshot_row(T1, &lone_snapshot, TARGET, "9", &hex('b'))),
        replace(snapshot_row(
            T1,
            &uuid::Uuid::new_v4().to_string(),
            TARGET,
            "8",
            &hex('b'),
        )),
        replace(candidate_row(
            T1,
            &lone_candidate,
            "manual",
            None,
            "staged",
            1,
        )),
        replace(candidate_row(
            T1,
            &uuid::Uuid::new_v4().to_string(),
            "discovery",
            Some(&lone_snapshot),
            "staged",
            1,
        )),
        replace(manual(&uuid::Uuid::new_v4().to_string())),
        replace(mapping_row(T1, &lone_mapping, 2)),
        replace(mapping_row(T1, &uuid::Uuid::new_v4().to_string(), 2)),
    ];
    for attempt in attempts.drain(..) {
        let refused = f.exec(&attempt).await.unwrap_err().to_string();
        assert!(
            refused.contains("UNIQUE constraint failed") && refused.contains("never replaced"),
            "{attempt}: {refused}"
        );
    }
    f.exec(&definition_row(T1, &lone_definition, 1, &lone_candidate))
        .await
        .unwrap();
    attempts = vec![
        replace(definition_row(T1, &lone_definition, 1, CANDIDATE)),
        replace(definition_row(
            T1,
            &uuid::Uuid::new_v4().to_string(),
            1,
            &lone_candidate,
        )),
        replace(format!(
            "INSERT INTO products_quota_definition_resource_type VALUES ('{T1}', \
             '{DEFINITION}', 1, '{RESOURCE}')"
        )),
        replace(availability_row(T1, 1, "enabled", None, None)),
    ];
    for attempt in attempts {
        let refused = f.exec(&attempt).await.unwrap_err().to_string();
        assert!(
            refused.contains("UNIQUE constraint failed") && refused.contains("never replaced"),
            "{attempt}: {refused}"
        );
    }
    for (sql, stored) in [
        (
            format!(
                "SELECT target_key AS v FROM products_quota_adapter_target \
                 WHERE id = '{lone_target}'"
            ),
            "pool-z",
        ),
        (
            format!(
                "SELECT source_revision AS v FROM products_quota_snapshot \
                 WHERE id = '{lone_snapshot}'"
            ),
            "8",
        ),
        (
            format!(
                "SELECT origin AS v FROM products_quota_candidate WHERE id = '{lone_candidate}'"
            ),
            "discovery",
        ),
        (
            "SELECT state AS v FROM products_quota_availability WHERE revision = 1".to_owned(),
            "disabled",
        ),
    ] {
        assert_eq!(f.text(&sql).await, stored, "{sql}");
    }
}

/// A `REPLACE` that names `rowid`, or an `UPDATE OR REPLACE` that moves it, would settle a
/// conflict on the implicit row id by deleting a stored row, past every key trigger. No quota table
/// has a row id (`WITHOUT ROWID`), so neither form can name one, and the stored rows stay.
#[tokio::test]
async fn no_quota_table_keeps_a_rowid() {
    let f = Fixture::seeded().await;
    for table in QUOTA_TABLES {
        let read = f.exec(&format!("SELECT rowid FROM {table}")).await;
        assert!(read.is_err(), "{table} has a rowid");
    }
    let second = uuid::Uuid::new_v4().to_string();
    f.exec(&candidate_row(T1, &second, "manual", None, "staged", 1))
        .await
        .unwrap();
    let fresh = uuid::Uuid::new_v4().to_string();
    let replace = target_row(T1, &fresh, "pool-r").replacen(
        "INSERT INTO products_quota_adapter_target VALUES (",
        "REPLACE INTO products_quota_adapter_target (rowid, tenant_id, id, plugin_instance_id, \
         target_key, name, created_by, created_at) VALUES (1, ",
        1,
    );
    assert!(replace.contains("rowid"), "{replace}");
    let refused = f.exec(&replace).await.unwrap_err().to_string();
    assert!(refused.contains("rowid"), "{refused}");
    let refused = f
        .exec(&format!(
            "UPDATE OR REPLACE products_quota_candidate SET rowid = 1 WHERE id = '{second}'"
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("rowid"), "{refused}");
    for (sql, stored) in [
        (
            format!(
                "SELECT target_key AS v FROM products_quota_adapter_target WHERE id = '{TARGET}'"
            ),
            "pool-a",
        ),
        (
            format!("SELECT state AS v FROM products_quota_candidate WHERE id = '{CANDIDATE}'"),
            "staged",
        ),
        (
            format!("SELECT origin AS v FROM products_quota_candidate WHERE id = '{second}'"),
            "manual",
        ),
    ] {
        assert_eq!(f.text(&sql).await, stored, "{sql}");
    }
}

#[tokio::test]
async fn one_snapshot_per_target_and_revision() {
    let f = Fixture::seeded().await;
    let again = f.insert_snapshot(f.target, "7", &hex('b')).await;
    assert!(
        again.unwrap_err().to_string().contains("UNIQUE"),
        "the revision is taken"
    );
    f.insert_snapshot(f.target, "8", &hex('b')).await.unwrap();
}

#[tokio::test]
async fn the_checks_hold() {
    let f = Fixture::seeded().await;
    for revision in ["07", "-1", "1.5", "", &"9".repeat(40), "1a"] {
        assert!(
            f.insert_snapshot(f.target, revision, &hex('a'))
                .await
                .is_err(),
            "non-canonical revision {revision:?}"
        );
    }
    f.insert_snapshot(f.target, "0", &hex('a')).await.unwrap();
    f.insert_snapshot(f.target, &"9".repeat(39), &hex('a'))
        .await
        .unwrap();
    for digest in ["A", &"A".repeat(64), &"a".repeat(63), &"g".repeat(64)] {
        assert!(
            f.insert_snapshot(f.target, "8", digest).await.is_err(),
            "digest {digest:?}"
        );
    }
    for (origin, snapshot, why) in [
        ("discovery", None, "discovery needs a snapshot"),
        ("manual", Some(f.snapshot), "manual has none"),
    ] {
        let refused = f
            .insert_candidate_with_origin(origin, snapshot)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("chk_products_quota_candidate_origin_snapshot"),
            "{why}: {refused}"
        );
    }
    assert!(
        f.insert_candidate_with_origin("import", None)
            .await
            .is_err(),
        "origin"
    );
    f.insert_candidate_with_origin("manual", None)
        .await
        .unwrap();
    for (state, version) in [("lost", 1), ("staged", 0)] {
        let row = candidate_row(
            T1,
            &uuid::Uuid::new_v4().to_string(),
            "manual",
            None,
            state,
            version,
        );
        assert!(f.exec(&row).await.is_err(), "candidate {state} {version}");
    }
    assert!(
        f.insert_availability("enabled", Some("qualification_pending"))
            .await
            .is_err(),
        "reason on enabled"
    );
    assert!(
        f.insert_availability("disabled", None).await.is_err(),
        "no reason on disabled"
    );
    assert!(
        f.insert_availability("paused", Some("x")).await.is_err(),
        "availability state"
    );
    for qualification in [
        ("'00000000-0000-0000-0000-0000000000d1'", "NULL", "NULL"),
        ("NULL", "1", "NULL"),
        ("NULL", "NULL", "'d'"),
    ] {
        let row = availability_row(T1, 9, "disabled", Some("x"), Some(qualification));
        assert!(
            f.exec(&row).await.is_err(),
            "qualification {qualification:?}"
        );
    }
    let whole = availability_row(
        T1,
        9,
        "disabled",
        Some("x"),
        Some(("'00000000-0000-0000-0000-0000000000d1'", "1", "'d'")),
    );
    f.exec(&whole).await.unwrap();
    assert!(
        f.exec(&availability_row(T1, 0, "disabled", Some("x"), None))
            .await
            .is_err(),
        "revision"
    );
    for (key, name) in [
        ("", "N"),
        (&"k".repeat(256)[..], "N"),
        ("k", ""),
        ("k", &"n".repeat(201)[..]),
    ] {
        let row = format!(
            "INSERT INTO products_quota_adapter_target VALUES ('{T1}', '{}', 'p', '{key}', \
             '{name}', '{ACTOR}', '{AT}')",
            uuid::Uuid::new_v4()
        );
        assert!(f.exec(&row).await.is_err(), "target {key:?} {name:?}");
    }
    assert!(
        f.exec(&mapping_row(T1, MAPPING, 0)).await.is_err(),
        "mapping version"
    );
    // A candidate no definition names yet, so only the CHECK can refuse its definition rows.
    let fresh = uuid::Uuid::new_v4().to_string();
    f.exec(
        &candidate_row(T1, &fresh, "manual", None, "staged", 1).replacen(
            "'tenant-vcpu'",
            "'tenant-ram'",
            1,
        ),
    )
    .await
    .unwrap();
    for (versions, why) in [((0, 2), "definition version"), ((1, 0), "SKU version")] {
        let refused = f
            .exec(&definition_row_at(
                T1,
                &uuid::Uuid::new_v4().to_string(),
                versions,
                &fresh,
            ))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("chk_products_quota_definition_version_version"),
            "{why}: {refused}"
        );
    }
    f.exec(&definition_row_at(
        T1,
        &uuid::Uuid::new_v4().to_string(),
        (1, 1),
        &fresh,
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn a_child_row_cannot_name_another_tenants_parent() {
    let f = Fixture::seeded().await;
    let other = uuid::Uuid::new_v4();
    let foreign = f.insert_snapshot_for_tenant(other, f.target, "9").await;
    assert!(
        foreign.unwrap_err().to_string().contains("FOREIGN KEY"),
        "snapshot"
    );
    let other = other.to_string();
    for row in [
        candidate_row(&other, CANDIDATE, "discovery", Some(SNAPSHOT), "staged", 1),
        mapping_row(&other, MAPPING, 1),
        definition_row(&other, DEFINITION, 1, CANDIDATE),
        format!(
            "INSERT INTO products_quota_definition_resource_type VALUES ('{other}', \
             '{DEFINITION}', 1, '{RESOURCE}')"
        ),
        availability_row(&other, 1, "disabled", Some("qualification_pending"), None),
    ] {
        let e = f.exec(&row).await.unwrap_err().to_string();
        assert!(e.contains("FOREIGN KEY"), "{row}: {e}");
    }
}

/// A mapping's lineage `(target, capability_key)` holds one row per version (P-D-270): a first
/// version under another mapping id is refused as a unique failure on the lineage, so two first
/// approvals of one capability cannot fork it; the lineage's next version is stored.
#[tokio::test]
async fn one_mapping_version_per_lineage_and_version() {
    let f = Fixture::seeded().await;
    let fork = f
        .exec(&mapping_row(T1, &uuid::Uuid::new_v4().to_string(), 1))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        fork.contains(
            "UNIQUE constraint failed: products_quota_mapping_version.tenant_id, \
             products_quota_mapping_version.target_id, \
             products_quota_mapping_version.capability_key"
        ),
        "{fork}"
    );
    f.exec(&mapping_row(T1, MAPPING, 2)).await.unwrap();
}

#[tokio::test]
async fn one_candidate_per_snapshot_key_and_manual_record() {
    let f = Fixture::seeded().await;
    let again = f
        .exec(&candidate_row(
            T1,
            &uuid::Uuid::new_v4().to_string(),
            "discovery",
            Some(SNAPSHOT),
            "staged",
            1,
        ))
        .await;
    assert!(
        again.unwrap_err().to_string().contains("UNIQUE"),
        "snapshot key"
    );
    let record = uuid::Uuid::new_v4();
    let manual = |id: uuid::Uuid| {
        format!(
            "INSERT INTO products_quota_candidate (tenant_id, id, target_id, origin, record_id, \
             capability_key, record_json, record_digest, evidence_json, state, version, \
             created_by, created_at) VALUES ('{T1}', '{id}', '{TARGET}', 'manual', '{record}', \
             'k', '{{}}', '{}', '{{}}', 'staged', 1, '{ACTOR}', '{AT}')",
            hex('d')
        )
    };
    f.exec(&manual(uuid::Uuid::new_v4())).await.unwrap();
    let taken = f.exec(&manual(uuid::Uuid::new_v4())).await;
    assert!(
        taken.unwrap_err().to_string().contains("UNIQUE"),
        "manual record"
    );
    let twice = f
        .exec(&definition_row(
            T1,
            uuid::Uuid::new_v4().to_string().as_str(),
            1,
            CANDIDATE,
        ))
        .await;
    assert!(
        twice.unwrap_err().to_string().contains("UNIQUE"),
        "one definition per candidate"
    );
}

/// Every list reads the tenant's rows through a tenant-led index: no full scan, and no sort of
/// the tenant's rows where the list's order is the index's. The definition list is held to the
/// rule only: a tenant-led search and no scan. Which access path `SQLite` takes is its planner's
/// choice (the primary key without statistics, the SKU index and a sort with them), and both
/// keep to the tenant's rows.
#[tokio::test]
async fn the_lists_read_through_tenant_led_indexes() {
    let f = Fixture::seeded().await;
    for (sql, index, ordered) in [
        (
            format!(
                "SELECT * FROM products_quota_adapter_target WHERE tenant_id = '{T1}' \
                 ORDER BY name, id LIMIT 51"
            ),
            "idx_products_quota_adapter_target_name",
            true,
        ),
        (
            format!(
                "SELECT * FROM products_quota_candidate WHERE tenant_id = '{T1}' \
                 ORDER BY created_at DESC, id DESC LIMIT 51"
            ),
            "idx_products_quota_candidate_created",
            true,
        ),
        (
            format!(
                "SELECT * FROM products_quota_candidate WHERE tenant_id = '{T1}' \
                 AND target_id = '{TARGET}' ORDER BY created_at DESC, id DESC LIMIT 51"
            ),
            "idx_products_quota_candidate_target",
            true,
        ),
        (
            format!(
                "SELECT * FROM products_quota_candidate WHERE tenant_id = '{T1}' \
                 AND state = 'staged' ORDER BY created_at DESC, id DESC LIMIT 51"
            ),
            "idx_products_quota_candidate_state",
            true,
        ),
        (
            format!(
                "SELECT * FROM products_quota_definition_version WHERE tenant_id = '{T1}' \
                 AND sku_id = 'x' AND sku_version <= 4 ORDER BY definition_id, version LIMIT 51"
            ),
            "SEARCH products_quota_definition_version USING",
            false,
        ),
    ] {
        let plan = f.plan(&sql).await;
        assert!(plan.contains(index), "{sql}: {plan}");
        assert!(plan.contains("(tenant_id=?"), "{sql}: {plan}");
        assert!(!plan.contains("SCAN products_quota"), "{sql}: {plan}");
        if ordered {
            assert!(!plan.contains("TEMP B-TREE"), "{sql}: {plan}");
        }
    }
}

/// `down` refuses while any quota row exists, before it drops anything: one target row is
/// enough, and the message names its table.
#[tokio::test]
async fn down_refuses_while_one_quota_row_exists() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migration.up(&SchemaManager::new(&db)).await.unwrap();
    let f = Fixture {
        db,
        target: TARGET,
        snapshot: SNAPSHOT,
    };
    f.exec(&target_row(T1, TARGET, "pool-a")).await.unwrap();
    let error = f
        .down()
        .await
        .expect_err("a stored row refuses the revert")
        .to_string();
    assert!(
        error.contains("m20261119_000015_quota_catalog")
            && error.contains("products_quota_adapter_target holds a row"),
        "{error}"
    );
    for table in QUOTA_TABLES {
        assert!(f.table_exists(table).await, "{table} is kept");
    }
}

/// With a row in every quota table, `down` names every table and keeps every row.
#[tokio::test]
async fn down_refuses_while_any_quota_row_exists() {
    let f = Fixture::seeded().await;
    let error = f
        .down()
        .await
        .expect_err("stored rows refuse the revert")
        .to_string();
    for table in QUOTA_TABLES {
        assert!(
            error.contains(&format!("{table} holds a row")),
            "{table}: {error}"
        );
        assert!(f.table_exists(table).await, "{table} is kept");
        assert_eq!(
            f.text(&format!("SELECT CAST(count(*) AS TEXT) AS v FROM {table}"))
                .await,
            "1",
            "{table}"
        );
    }
}
