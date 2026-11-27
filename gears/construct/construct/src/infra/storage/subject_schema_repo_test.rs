//! Repository tests for the stored profile schemas on an in-memory `SQLite` database.

use construct_sdk::linked_schema::default_schema;
use construct_sdk::schema_doc::SchemaDoc;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::subject_schema_repo::SeaOrmSubjectSchemaRepository;
use crate::domain::profile_schema::{
    SchemaLookup, SchemaStore, StoredSchema, SubjectSchemaRepository,
};
use crate::domain::subject_settings::{ScopeRefusal, SubjectKey};
use crate::test_support::inmem_db;

fn key() -> SubjectKey {
    SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    }
}

#[tokio::test]
async fn a_schema_round_trips_and_a_subject_without_one_has_none() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSchemaRepository::new();
    let (key, scope) = (key(), AccessScope::allow_all());
    let doc = SchemaDoc::of(&default_schema());

    assert_eq!(
        repo.find(&conn, &scope, key).await.unwrap(),
        SchemaLookup::Absent
    );
    assert_eq!(
        repo.store(&conn, &scope, key, None, &doc, 7).await.unwrap(),
        SchemaStore::Stored(1)
    );
    assert_eq!(
        repo.find(&conn, &scope, key).await.unwrap(),
        SchemaLookup::Stored(StoredSchema {
            doc,
            version: 1,
            names_charged: 7,
        })
    );
}

#[tokio::test]
async fn a_store_on_an_older_version_is_a_conflict_and_changes_nothing() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSchemaRepository::new();
    let (key, scope) = (key(), AccessScope::allow_all());
    let mut doc = SchemaDoc::of(&default_schema());
    repo.store(&conn, &scope, key, None, &doc, 7).await.unwrap();
    assert_eq!(
        repo.store(&conn, &scope, key, None, &doc, 7).await.unwrap(),
        SchemaStore::Conflict,
        "a second first set"
    );

    doc.links.clear();
    assert_eq!(
        repo.store(&conn, &scope, key, Some(1), &doc, 7)
            .await
            .unwrap(),
        SchemaStore::Stored(2)
    );
    assert_eq!(
        repo.store(
            &conn,
            &scope,
            key,
            Some(1),
            &SchemaDoc::of(&default_schema()),
            0
        )
        .await
        .unwrap(),
        SchemaStore::Conflict
    );
    let SchemaLookup::Stored(stored) = repo.find(&conn, &scope, key).await.unwrap() else {
        panic!("a stored schema");
    };
    assert_eq!(
        (stored.version, stored.doc, stored.names_charged),
        (2, doc, 7),
        "the update leaves the count of names as the charge set it"
    );
}

#[tokio::test]
async fn a_scope_that_leaves_the_subject_out_reaches_no_schema() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSchemaRepository::new();
    let (key, other) = (key(), key());
    let doc = SchemaDoc::of(&default_schema());
    repo.store(&conn, &AccessScope::allow_all(), key, None, &doc, 0)
        .await
        .unwrap();

    let elsewhere = AccessScope::for_tenant(other.tenant_id);
    assert_eq!(
        repo.find(&conn, &elsewhere, key).await.unwrap(),
        SchemaLookup::OutOfScope(ScopeRefusal::NotCovered)
    );
    assert_eq!(
        repo.store(&conn, &elsewhere, key, Some(1), &doc, 0)
            .await
            .unwrap(),
        SchemaStore::OutOfScope(ScopeRefusal::NotCovered)
    );
}

#[tokio::test]
async fn names_are_charged_over_the_version_they_were_read_on_and_only_one_change_is_charged() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSchemaRepository::new();
    let (key, scope) = (key(), AccessScope::allow_all());
    let doc = SchemaDoc::of(&default_schema());

    // A person without a row: the charge stores one, holding the default schema, as version 0.
    assert_eq!(
        repo.reserve_names(&conn, &scope, key, None, &doc, 0, 5)
            .await
            .unwrap(),
        SchemaStore::Stored(0)
    );
    assert_eq!(
        repo.reserve_names(&conn, &scope, key, None, &doc, 0, 5)
            .await
            .unwrap(),
        SchemaStore::Conflict,
        "a second first charge"
    );
    let SchemaLookup::Stored(row) = repo.find(&conn, &scope, key).await.unwrap() else {
        panic!("the charge stored a row");
    };
    assert_eq!(
        (row.version, row.names_charged, row.doc),
        (0, 5, doc.clone())
    );

    // Two changes read version 0 with 5 names: the first is charged, the second finds the count moved.
    assert_eq!(
        repo.reserve_names(&conn, &scope, key, Some(0), &doc, 5, 9)
            .await
            .unwrap(),
        SchemaStore::Stored(0)
    );
    assert_eq!(
        repo.reserve_names(&conn, &scope, key, Some(0), &doc, 5, 12)
            .await
            .unwrap(),
        SchemaStore::Conflict
    );
    // The change that was charged stores over the version it reserved on, keeping the count.
    assert_eq!(
        repo.store(&conn, &scope, key, Some(0), &doc, 9)
            .await
            .unwrap(),
        SchemaStore::Stored(1)
    );
    let SchemaLookup::Stored(row) = repo.find(&conn, &scope, key).await.unwrap() else {
        panic!("a stored row");
    };
    assert_eq!((row.version, row.names_charged), (1, 9));
}

#[tokio::test]
async fn a_store_that_read_the_count_before_a_charge_conflicts_and_leaves_the_charge() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSchemaRepository::new();
    let (key, scope) = (key(), AccessScope::allow_all());
    let doc = SchemaDoc::of(&default_schema());
    repo.reserve_names(&conn, &scope, key, None, &doc, 0, 5)
        .await
        .unwrap();
    // Another change reads 5 and brings in no names; a second is charged 5 to 9 on the same version.
    repo.reserve_names(&conn, &scope, key, Some(0), &doc, 5, 9)
        .await
        .unwrap();

    let stale = repo
        .store(&conn, &scope, key, Some(0), &doc, 5)
        .await
        .unwrap();

    assert_eq!(
        stale,
        SchemaStore::Conflict,
        "the writer of the stale count does not go on"
    );
    let SchemaLookup::Stored(row) = repo.find(&conn, &scope, key).await.unwrap() else {
        panic!("a stored row");
    };
    assert_eq!(
        (row.version, row.names_charged),
        (0, 9),
        "the charge stands"
    );
}
