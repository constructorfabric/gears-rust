// Created: 2026-08-25 by Constructor Tech
//! Tests for the `setting_declarations` schema.
//!
//! These exercise the constraints against a real database rather than asserting
//! that the DDL text contains them. The definition of done asks for the cross-field rules to
//! hold "in the database, not only in application code", and a string match on
//! the migration would prove neither that the SQL parses nor that it refuses
//! what it is supposed to refuse.
//!
//! `SQLite` stands in for Postgres. It does not enforce foreign keys unless the
//! connection asks, so the fixture turns them on -- which is also what makes the
//! `ON DELETE RESTRICT` test meaningful here.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DatabaseConnection};

use crate::infra::storage::migrations::Migrator;

const CATEGORY_ID: &str = "11111111-1111-1111-1111-111111111111";
const OTHER_CATEGORY_ID: &str = "22222222-2222-2222-2222-222222222222";

async fn run(db: &DatabaseConnection, sql: &str) -> Result<(), sea_orm_migration::sea_orm::DbErr> {
    db.execute_unprepared(sql).await.map(|_| ())
}

/// A migrated database with foreign keys enforced and two categories present.
async fn migrated() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    run(&db, "PRAGMA foreign_keys = ON;")
        .await
        .expect("foreign keys can be enabled");
    Migrator::up(&db, None).await.expect("migrations apply");

    for (id, key) in [(CATEGORY_ID, "network"), (OTHER_CATEGORY_ID, "storage")] {
        run(
            &db,
            &format!(
                "INSERT INTO categories (id, key, name, sort_order, created_at, updated_at)
                 VALUES ('{id}', '{key}', '{key}', 0, 'now', 'now');"
            ),
        )
        .await
        .expect("category fixture inserts");
    }
    db
}

/// An insert whose unlisted columns hold valid values.
///
/// `overrides` is `"col=val"` pairs. A pair naming a column the baseline already
/// sets *replaces* it -- appending instead would emit the column twice, and a
/// statement that fails for that reason would pass a test expecting a
/// constraint violation while proving nothing about the constraint.
fn insert(id: &str, category: &str, leaf: &str, overrides: &str) -> String {
    let key = format!("gts.cf.core.settings.setting_type.v1~acme.settings.network.{leaf}.v1~");
    let mut fields: Vec<(&str, String)> = vec![
        ("id", format!("'{id}'")),
        ("key", format!("'{key}'")),
        ("leaf_slug", format!("'{leaf}'")),
        (
            "value_type_id",
            "'gts.cf.toolkit.settings.type_bool_flag.v1~'".to_owned(),
        ),
        ("category_id", format!("'{category}'")),
        ("default_value", "'true'".to_owned()),
        ("scope_class", "'local'".to_owned()),
        ("last_change_at", "'now'".to_owned()),
        ("created_at", "'now'".to_owned()),
        ("updated_at", "'now'".to_owned()),
        ("created_by", "'tester'".to_owned()),
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
        "INSERT INTO setting_declarations ({}) VALUES ({});",
        cols.join(", "),
        vals.join(", ")
    )
}

#[tokio::test]
async fn a_well_formed_declaration_inserts() {
    let db = migrated().await;
    run(&db, &insert("d1", CATEGORY_ID, "timeout", ""))
        .await
        .expect("the baseline fixture must be valid, or every other test is vacuous");
}

#[tokio::test]
async fn an_exposable_declaration_may_not_be_pii() {
    // `anonymous_exposable` serves the unauthenticated read surface; the two
    // classifications that must never leave are excluded in the schema, not
    // only by the handler that sets the flag.
    let db = migrated().await;
    let sql = insert(
        "d1",
        CATEGORY_ID,
        "banner",
        "anonymous_exposable=1,data_classification='pii'",
    );
    assert!(run(&db, &sql).await.is_err());
}

#[tokio::test]
async fn an_exposable_declaration_may_not_be_secret() {
    let db = migrated().await;
    let sql = insert(
        "d1",
        CATEGORY_ID,
        "token",
        "anonymous_exposable=1,data_classification='secret',has_secret_trait=1",
    );
    assert!(run(&db, &sql).await.is_err());
}

#[tokio::test]
async fn a_public_declaration_may_be_exposable() {
    let db = migrated().await;
    let sql = insert("d1", CATEGORY_ID, "banner", "anonymous_exposable=1");
    run(&db, &sql)
        .await
        .expect("public and exposable is the intended pair");
}

#[tokio::test]
async fn step_up_may_be_cleared_explicitly() {
    // The column defaults to required; a row may still say otherwise, because
    // machine-managed values exist. Clearing it is a deliberate act the schema
    // permits and the write path gates.
    let db = migrated().await;
    run(
        &db,
        &insert("d1", CATEGORY_ID, "timeout", "requires_step_up=0"),
    )
    .await
    .expect("an explicit false is a valid value");
}

#[tokio::test]
async fn a_secret_classification_requires_the_secret_trait() {
    let db = migrated().await;
    let sql = insert("d1", CATEGORY_ID, "token", "data_classification='secret'");
    assert!(run(&db, &sql).await.is_err());
}

#[tokio::test]
async fn the_secret_trait_requires_the_secret_classification() {
    // The check is an equivalence, not an implication: a trait-bearing value
    // type classified `public` would let a credential through the masking path.
    let db = migrated().await;
    let sql = insert("d1", CATEGORY_ID, "token", "has_secret_trait=1");
    assert!(run(&db, &sql).await.is_err());
}

#[tokio::test]
async fn a_secret_declaration_with_both_set_inserts() {
    let db = migrated().await;
    let sql = insert(
        "d1",
        CATEGORY_ID,
        "token",
        "data_classification='secret',has_secret_trait=1",
    );
    run(&db, &sql)
        .await
        .expect("the consistent pair is allowed");
}

#[tokio::test]
async fn a_contributed_declaration_must_name_its_module() {
    let db = migrated().await;
    let sql = insert("d1", CATEGORY_ID, "timeout", "source='module_contributed'");
    assert!(run(&db, &sql).await.is_err());
}

#[tokio::test]
async fn an_admin_authored_declaration_must_not_name_a_module() {
    let db = migrated().await;
    let sql = insert("d1", CATEGORY_ID, "timeout", "owner_module='chat-engine'");
    assert!(run(&db, &sql).await.is_err());
}

#[tokio::test]
async fn the_key_is_globally_unique() {
    let db = migrated().await;
    run(&db, &insert("d1", CATEGORY_ID, "timeout", ""))
        .await
        .expect("first insert");
    assert!(
        run(&db, &insert("d2", CATEGORY_ID, "timeout", ""))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn one_leaf_name_may_appear_in_two_categories() {
    // Uniqueness is per category, because the keys differ by the category
    // segment they embed.
    let db = migrated().await;
    run(&db, &insert("d1", CATEGORY_ID, "timeout", ""))
        .await
        .expect("first category");
    let mut second = insert("d2", OTHER_CATEGORY_ID, "timeout", "");
    second = second.replace(".network.timeout.", ".storage.timeout.");
    run(&db, &second).await.expect("second category");
}

#[tokio::test]
async fn a_leaf_name_may_not_repeat_within_one_category() {
    let db = migrated().await;
    run(&db, &insert("d1", CATEGORY_ID, "timeout", ""))
        .await
        .expect("first insert");
    let mut clash = insert("d2", CATEGORY_ID, "timeout", "");
    clash = clash.replace(".network.timeout.v1~'", ".network.timeout.v2~'");
    assert!(run(&db, &clash).await.is_err());
}

#[tokio::test]
async fn a_retired_predecessor_does_not_hold_its_leaf_name() {
    // The slug index is partial on `status = 'active'`: the name is what a live
    // declaration occupies, and a retired one has left it. Without this, the
    // evolve path (`…v1~` retired, `…v2~` created) could never take the name
    // back for the successor.
    let db = migrated().await;
    run(
        &db,
        &insert("d1", CATEGORY_ID, "timeout", "status='retired'"),
    )
    .await
    .expect("retired predecessor");
    let mut successor = insert("d2", CATEGORY_ID, "timeout", "");
    successor = successor.replace(".network.timeout.v1~'", ".network.timeout.v2~'");
    run(&db, &successor)
        .await
        .expect("the successor takes the name the retired row no longer holds");
}

#[tokio::test]
async fn a_category_holding_a_declaration_cannot_be_deleted() {
    // The no-orphan rule as the database enforces it. The service checks first
    // so the common case reports a conflict rather than a constraint error, but
    // a declaration inserted between that check and the delete lands here.
    let db = migrated().await;
    run(&db, &insert("d1", CATEGORY_ID, "timeout", ""))
        .await
        .expect("declaration inserts");
    assert!(
        run(
            &db,
            &format!("DELETE FROM categories WHERE id = '{CATEGORY_ID}';")
        )
        .await
        .is_err(),
        "ON DELETE RESTRICT must refuse while a declaration references the category"
    );
}

#[tokio::test]
async fn an_empty_category_can_still_be_deleted() {
    let db = migrated().await;
    run(
        &db,
        &format!("DELETE FROM categories WHERE id = '{OTHER_CATEGORY_ID}';"),
    )
    .await
    .expect("nothing references it");
}
