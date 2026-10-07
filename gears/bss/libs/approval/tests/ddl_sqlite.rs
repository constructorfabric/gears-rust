#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_approval::ddl::{
    add_submit_note, apply_add_submit_note, apply_down, apply_drop_submit_note, apply_up,
    drop_submit_note, up,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::SchemaManager;

#[tokio::test]
async fn the_template_creates_four_tables_and_its_keys_hold_on_sqlite() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    apply_up(&manager, "products_", Some("ignored_on_sqlite"))
        .await
        .unwrap();
    apply_up(&manager, "products_", Some("ignored_on_sqlite"))
        .await
        .unwrap(); // idempotent: IF NOT EXISTS everywhere
    let names: Vec<String> = db.query_all_raw(Statement::from_string(DbBackend::Sqlite,
        "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'products_approval_%' ORDER BY name".to_owned())).await.unwrap()
        .iter().map(|r| r.try_get::<String>("", "name").unwrap()).collect();
    assert_eq!(
        names,
        [
            "products_approval_decision",
            "products_approval_policy",
            "products_approval_unit",
            "products_approval_unit_item"
        ]
    );
    let unit = "INSERT INTO products_approval_unit (id, tenant_id, kind, ref_type, ref_id, state, quorum_required, generation, submitted_by, submitted_at, snapshot, snapshot_hash, version) VALUES ('u1','t1','sku_publish','sku','s1','pending',1,1,'a0','2026-09-24T10:00:00Z','{}','h',1)";
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, unit.to_owned()))
        .await
        .unwrap();
    let vote = "INSERT INTO products_approval_decision (unit_id, tenant_id, actor, generation, decision, note, at, stale) VALUES ('u1','t1','a1',1,'approve',NULL,'2026-09-24T10:05:00Z',0)";
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, vote.to_owned()))
        .await
        .unwrap();
    assert!(
        db.execute_raw(Statement::from_string(DbBackend::Sqlite, vote.to_owned()))
            .await
            .is_err(),
        "one vote per actor per generation"
    );
    let orphan = vote.replace("'u1'", "'missing'");
    assert!(
        db.execute_raw(Statement::from_string(DbBackend::Sqlite, orphan))
            .await
            .is_err(),
        "decisions require a unit"
    );
    let item = "INSERT INTO products_approval_unit_item (unit_id, tenant_id, item_type, item_id, created_by, after_json) VALUES ('u1','t1','sku','s1','a0','{}')";
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, item.to_owned()))
        .await
        .unwrap();
    assert!(
        db.execute_raw(Statement::from_string(DbBackend::Sqlite, item.to_owned()))
            .await
            .is_err(),
        "unique item within the unit"
    );
    assert!(
        db.execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            item.replace("'u1'", "'missing'")
        ))
        .await
        .is_err(),
        "items require a unit"
    );
    let next_gen = vote.replace("'a1',1,", "'a1',2,"); // same actor, generation 2
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, next_gen))
        .await
        .unwrap(); // the same actor may vote again in generation 2
    apply_down(&manager, "products_", Some("ignored_on_sqlite"))
        .await
        .unwrap();
    assert!(!manager.has_table("products_approval_unit").await.unwrap());
}

/// Each CHECK of the template refuses its bad value on `SQLite`: a negative quorum, a state and a
/// decision outside their sets.
#[tokio::test]
async fn the_templates_checks_refuse_a_bad_quorum_state_and_decision_on_sqlite() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    apply_up(&manager, "pricing_", None).await.unwrap();
    let run = |sql: String| {
        let db = db.clone();
        async move {
            db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql))
                .await
        }
    };
    let policy =
        "INSERT INTO pricing_approval_policy (tenant_id, kind, quorum) VALUES ('t1','*',0)";
    run(policy.to_owned()).await.unwrap();
    assert!(
        run(policy.replace("'*',0", "'prices',-1")).await.is_err(),
        "quorum >= 0"
    );
    let unit = "INSERT INTO pricing_approval_unit (id, tenant_id, kind, ref_type, ref_id, state, quorum_required, generation, submitted_by, submitted_at, snapshot, snapshot_hash, version) VALUES ('u1','t1','prices','book','b1','pending',1,1,'a0','2026-09-24T10:00:00Z','{}','h',1)";
    for state in ["pending", "approved", "rejected", "withdrawn"] {
        let id = format!("'u-{state}'");
        run(unit
            .replace("'u1'", &id)
            .replace("'pending'", &format!("'{state}'")))
        .await
        .unwrap();
    }
    assert!(
        run(unit.replace("'pending'", "'bogus'")).await.is_err(),
        "the state set"
    );
    assert!(
        run(unit.replace("'pending'", "'Pending'")).await.is_err(),
        "the state set is lower case"
    );
    let vote = "INSERT INTO pricing_approval_decision (unit_id, tenant_id, actor, generation, decision, note, at, stale) VALUES ('u-pending','t1','a1',1,'approve',NULL,'2026-09-24T10:05:00Z',0)";
    run(vote.to_owned()).await.unwrap();
    run(vote
        .replace("'a1'", "'a2'")
        .replace("'approve'", "'reject'"))
    .await
    .unwrap();
    assert!(
        run(vote.replace("'a1'", "'a3'").replace("'approve'", "'maybe'"))
            .await
            .is_err(),
        "the decision set"
    );
}

#[test]
fn the_postgres_template_carries_the_schema_and_the_queue_index() {
    let pg = up("pricing_", Some("bss"), DbBackend::Postgres).join("\n");
    assert!(pg.contains("CREATE TABLE IF NOT EXISTS bss.pricing_approval_unit"));
    assert!(pg.contains("CREATE INDEX IF NOT EXISTS ix_pricing_approval_unit_queue ON bss.pricing_approval_unit USING btree (tenant_id, state, kind, submitted_at)"));
    assert!(pg.contains("CHECK (state IN ('pending','approved','rejected','withdrawn'))"));
    assert!(pg.contains("PRIMARY KEY (unit_id, actor, generation)"));
}

/// The unit table's columns, in order, on `SQLite`.
async fn unit_columns(db: &sea_orm::DatabaseConnection) -> Vec<String> {
    db.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT name FROM pragma_table_info('pricing_approval_unit') ORDER BY cid".to_owned(),
    ))
    .await
    .unwrap()
    .iter()
    .map(|r| r.try_get::<String>("", "name").unwrap())
    .collect()
}

/// Products P-D-219, pricing D-445: `submit_note` is a separate step a gear's forward migration
/// runs. The shipped template does not name it (it is the body of deployed migrations); the step
/// appends the column, replays as a no-op, and its reverse drops it and replays too.
#[tokio::test]
async fn the_submit_note_step_appends_the_column_replays_and_reverses_on_sqlite() {
    for backend in [DbBackend::Sqlite, DbBackend::Postgres] {
        assert!(
            !up("pricing_", Some("bss"), backend)
                .join("\n")
                .contains("submit_note"),
            "the deployed template stays as it shipped ({backend:?})"
        );
    }
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    apply_up(&manager, "pricing_", Some("bss")).await.unwrap();
    let before = unit_columns(&db).await;
    assert_eq!(before.last().map(String::as_str), Some("version"));
    apply_add_submit_note(&manager, "pricing_", Some("bss"))
        .await
        .unwrap();
    apply_add_submit_note(&manager, "pricing_", Some("bss"))
        .await
        .unwrap();
    let after = unit_columns(&db).await;
    assert_eq!(
        after[..before.len()],
        before[..],
        "the old columns keep their places"
    );
    assert_eq!(after[before.len()..], ["submit_note"]);
    let unit = "INSERT INTO pricing_approval_unit (id, tenant_id, kind, ref_type, ref_id, state, quorum_required, generation, submitted_by, submitted_at, snapshot, snapshot_hash, version) VALUES ('u1','t1','prices','book','b1','pending',1,1,'a0','2026-09-24T10:00:00Z','{}','h',1)";
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, unit.to_owned()))
        .await
        .unwrap();
    let note: Option<String> = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT submit_note AS v FROM pricing_approval_unit".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "v")
        .unwrap();
    assert_eq!(note, None, "a unit written without a note reads null");
    apply_drop_submit_note(&manager, "pricing_", Some("bss"))
        .await
        .unwrap();
    apply_drop_submit_note(&manager, "pricing_", Some("bss"))
        .await
        .unwrap();
    assert_eq!(unit_columns(&db).await, before);
}

#[test]
fn the_submit_note_step_is_qualified_on_postgres_and_guarded_there_by_if_not_exists() {
    assert_eq!(
        add_submit_note("products_", Some("bss"), DbBackend::Postgres),
        "ALTER TABLE bss.products_approval_unit ADD COLUMN IF NOT EXISTS submit_note text"
    );
    assert_eq!(
        drop_submit_note("products_", Some("bss"), DbBackend::Postgres),
        "ALTER TABLE bss.products_approval_unit DROP COLUMN IF EXISTS submit_note"
    );
    assert_eq!(
        add_submit_note("products_", Some("bss"), DbBackend::Sqlite),
        "ALTER TABLE products_approval_unit ADD COLUMN submit_note text"
    );
    assert_eq!(
        drop_submit_note("products_", Some("bss"), DbBackend::Sqlite),
        "ALTER TABLE products_approval_unit DROP COLUMN submit_note"
    );
}
