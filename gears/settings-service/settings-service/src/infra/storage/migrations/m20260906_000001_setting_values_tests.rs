// Created: 2026-09-06 by Constructor Tech
//! Tests for the `setting_values` schema invariants, on `SQLite`.
//!
//! What is pinned: the exactly-one rule and its tie to the classification, the
//! subject pair's both-or-neither rule, the two partial unique indexes, and the
//! cascade from the declaration. Nothing here goes through a repository; the
//! constraints are the database's and are exercised as such.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DatabaseConnection};

use super::super::Migrator;

const CATEGORY_ID: &str = "11111111-1111-1111-1111-111111111111";
const DECLARATION_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const OTHER_DECLARATION_ID: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const ROOT: &str = "00000000-0000-0000-0000-000000000001";
const TENANT_A: &str = "aaaaaaaa-0000-0000-0000-000000000002";

async fn run(db: &DatabaseConnection, sql: &str) -> Result<(), sea_orm_migration::sea_orm::DbErr> {
    db.execute_unprepared(sql).await.map(|_| ())
}

/// A migrated database with foreign keys enforced, one category and two
/// declarations present.
async fn migrated() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    run(&db, "PRAGMA foreign_keys = ON;")
        .await
        .expect("foreign keys can be enabled");
    Migrator::up(&db, None).await.expect("migrations apply");
    run(
        &db,
        &format!(
            "INSERT INTO categories (id, key, name, sort_order, created_at, updated_at)
             VALUES ('{CATEGORY_ID}', 'network', 'network', 0, 'now', 'now');"
        ),
    )
    .await
    .expect("category fixture inserts");
    for (id, leaf) in [(DECLARATION_ID, "proxy"), (OTHER_DECLARATION_ID, "timeout")] {
        run(
            &db,
            &format!(
                "INSERT INTO setting_declarations
                   (id, key, leaf_slug, value_type_id, category_id, default_value, scope_class,
                    last_change_at, created_at, updated_at, created_by)
                 VALUES ('{id}',
                         'gts.cf.core.settings.setting_type.v1~acme.settings.network.{leaf}.v1~',
                         '{leaf}', 'gts.cf.toolkit.settings.type_bool_flag.v1~', '{CATEGORY_ID}',
                         'true', 'cascading', 'now', 'now', 'now', 'tester');"
            ),
        )
        .await
        .expect("declaration fixture inserts");
    }
    db
}

/// An insert whose unlisted columns hold valid values; `overrides` are
/// `col=val` pairs that replace the baseline rather than duplicating a column.
fn insert(id: &str, declaration: &str, tenant: &str, overrides: &str) -> String {
    let mut fields: Vec<(&str, String)> = vec![
        ("id", format!("'{id}'")),
        ("declaration_id", format!("'{declaration}'")),
        ("tenant_id", format!("'{tenant}'")),
        ("value", "'true'".to_owned()),
        ("last_change_at", "'now'".to_owned()),
        ("created_at", "'now'".to_owned()),
        ("updated_at", "'now'".to_owned()),
        ("set_by", "'tester'".to_owned()),
    ];
    for pair in overrides.split(',').filter(|p| !p.is_empty()) {
        let (col, val) = pair.split_once('=').expect("col=val");
        match fields.iter_mut().find(|(name, _)| *name == col) {
            Some(existing) => existing.1 = val.to_owned(),
            None => fields.push((col, val.to_owned())),
        }
    }
    let cols: Vec<&str> = fields.iter().map(|(c, _)| *c).collect();
    let vals: Vec<&str> = fields.iter().map(|(_, v)| v.as_str()).collect();
    format!(
        "INSERT INTO setting_values ({}) VALUES ({});",
        cols.join(", "),
        vals.join(", ")
    )
}

#[tokio::test]
async fn an_inline_value_at_the_root_tenant_is_stored() {
    // Platform scope is the root tenant's id: an ordinary row, no sentinel.
    let db = migrated().await;
    run(&db, &insert("v1", DECLARATION_ID, ROOT, ""))
        .await
        .expect("a platform-scoped value is an ordinary row");
}

#[tokio::test]
async fn a_row_with_both_value_and_secret_ref_is_rejected() {
    let db = migrated().await;
    let sql = insert(
        "v1",
        DECLARATION_ID,
        ROOT,
        "secret_ref='cred:1',data_classification='secret'",
    );
    assert!(run(&db, &sql).await.is_err());
}

#[tokio::test]
async fn a_row_with_neither_value_nor_secret_ref_is_rejected() {
    // Neither doubly-valued nor valueless: a row the resolver could not
    // interpret is refused by the schema, not by whoever reads it.
    let db = migrated().await;
    assert!(
        run(&db, &insert("v1", DECLARATION_ID, ROOT, "value=NULL"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_json_null_is_a_value_not_an_absence() {
    // A setting whose type admits `null` stores the JSON value `null`, which
    // is a non-NULL column and satisfies the exactly-one check.
    let db = migrated().await;
    run(&db, &insert("v1", DECLARATION_ID, ROOT, "value='null'"))
        .await
        .expect("JSON null is a value");
}

#[tokio::test]
async fn a_secret_ref_requires_the_secret_classification_and_vice_versa() {
    let db = migrated().await;
    run(
        &db,
        &insert(
            "v1",
            DECLARATION_ID,
            ROOT,
            "value=NULL,secret_ref='cred:1',data_classification='secret'",
        ),
    )
    .await
    .expect("secret by reference");
    assert!(
        run(
            &db,
            &insert(
                "v2",
                OTHER_DECLARATION_ID,
                ROOT,
                "value=NULL,secret_ref='cred:2'"
            )
        )
        .await
        .is_err(),
        "a reference under a public classification is a contradiction"
    );
    assert!(
        run(
            &db,
            &insert(
                "v3",
                OTHER_DECLARATION_ID,
                TENANT_A,
                "data_classification='secret'"
            )
        )
        .await
        .is_err(),
        "an inline value under a secret classification would be plaintext"
    );
}

#[tokio::test]
async fn a_subject_is_named_by_both_halves_or_neither() {
    let db = migrated().await;
    assert!(
        run(
            &db,
            &insert(
                "v1",
                DECLARATION_ID,
                ROOT,
                "subject_type='gts.cf.core.hosts.host.v1~'"
            )
        )
        .await
        .is_err()
    );
    assert!(
        run(&db, &insert("v2", DECLARATION_ID, ROOT, "subject_id='h-1'"))
            .await
            .is_err()
    );
    run(
        &db,
        &insert(
            "v3",
            DECLARATION_ID,
            ROOT,
            "subject_type='gts.cf.core.hosts.host.v1~',subject_id='h-1'",
        ),
    )
    .await
    .expect("both halves name a subject");
}

#[tokio::test]
async fn one_value_per_declaration_and_tenant_without_a_subject() {
    // The case the subject NULLs make easy to miss: two subject-less rows for
    // one pair collide only because the index is partial on `subject_type IS NULL`.
    let db = migrated().await;
    run(&db, &insert("v1", DECLARATION_ID, ROOT, ""))
        .await
        .expect("first");
    assert!(
        run(&db, &insert("v2", DECLARATION_ID, ROOT, ""))
            .await
            .is_err(),
        "a second platform row for one declaration violates uq_value_scope"
    );
    run(&db, &insert("v3", DECLARATION_ID, TENANT_A, ""))
        .await
        .expect("another tenant is another scope");
}

#[tokio::test]
async fn subject_rows_are_a_separate_shape_with_their_own_uniqueness() {
    let db = migrated().await;
    run(&db, &insert("v1", DECLARATION_ID, TENANT_A, ""))
        .await
        .expect("tenant row");
    let subject = "subject_type='gts.cf.core.hosts.host.v1~',subject_id='h-1'";
    run(&db, &insert("v2", DECLARATION_ID, TENANT_A, subject))
        .await
        .expect("a subject row coexists with the tenant row");
    assert!(
        run(&db, &insert("v3", DECLARATION_ID, TENANT_A, subject))
            .await
            .is_err(),
        "the same subject pair twice violates uq_value_scope_subject"
    );
    run(
        &db,
        &insert(
            "v4",
            DECLARATION_ID,
            TENANT_A,
            "subject_type='gts.cf.core.hosts.host.v1~',subject_id='h-2'",
        ),
    )
    .await
    .expect("another subject of the same type is another scope");
}

#[tokio::test]
async fn needs_review_defaults_to_clear() {
    let db = migrated().await;
    run(&db, &insert("v1", DECLARATION_ID, ROOT, ""))
        .await
        .expect("insert");
    let flagged = db
        .query_one_raw(sea_orm_migration::sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT needs_review FROM setting_values WHERE id = 'v1'".to_owned(),
        ))
        .await
        .expect("query")
        .expect("row");
    let needs_review: i32 = flagged.try_get("", "needs_review").expect("column");
    assert_eq!(needs_review, 0);
}

#[tokio::test]
async fn deleting_a_declaration_cascades_to_its_values() {
    let db = migrated().await;
    run(&db, &insert("v1", DECLARATION_ID, ROOT, ""))
        .await
        .expect("insert");
    run(
        &db,
        &format!("DELETE FROM setting_declarations WHERE id = '{DECLARATION_ID}';"),
    )
    .await
    .expect("hard delete of the declaration");
    let left = db
        .query_one_raw(sea_orm_migration::sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT count(*) AS n FROM setting_values".to_owned(),
        ))
        .await
        .expect("query")
        .expect("row");
    let n: i32 = left.try_get("", "n").expect("column");
    assert_eq!(n, 0, "ON DELETE CASCADE removed the value");
}
