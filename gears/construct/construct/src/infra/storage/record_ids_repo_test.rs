//! Record identity repository tests on an in-memory `SQLite` database: the
//! insert is the repeat check, and the tenant scope guards it.

use toolkit_security::AccessScope;
use uuid::Uuid;

use super::record_ids_repo::SeaOrmRecordIdRepository;
use crate::domain::error::DomainError;
use crate::domain::record_intake::{
    IdInsert, IdentityParts, RecordId, RecordIdRepository, RecordKey,
};
use crate::test_support::{inmem_db, stored_record_ids};

fn key(connector: &str, provenance: &str, version: &str) -> RecordKey {
    RecordKey::of(IdentityParts {
        connector,
        provenance,
        version,
    })
}

fn id(tenant_id: Uuid, connector: &str, provenance: &str, version: &str) -> RecordId {
    RecordId {
        tenant_id,
        key: key(connector, provenance, version),
        connector: connector.to_owned(),
        subject_id: Some(Uuid::new_v4()),
    }
}

#[tokio::test]
async fn a_new_identity_is_inserted() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let tenant = Uuid::new_v4();

    let inserted = SeaOrmRecordIdRepository::new()
        .insert(
            &conn,
            &AccessScope::for_tenant(tenant),
            id(tenant, "c", "p", "v"),
        )
        .await
        .expect("insert");

    assert_eq!(inserted, IdInsert::Inserted);
    assert_eq!(stored_record_ids(&db).await, 1);
}

#[tokio::test]
async fn a_second_insert_of_the_same_identity_is_a_repeat() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmRecordIdRepository::new();
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    repo.insert(&conn, &scope, id(tenant, "c", "p", "v"))
        .await
        .expect("first");

    // Another subject does not make it another identity.
    let again = repo
        .insert(&conn, &scope, id(tenant, "c", "p", "v"))
        .await
        .expect("again");

    assert_eq!(again, IdInsert::AlreadyThere);
    assert_eq!(stored_record_ids(&db).await, 1);
}

#[tokio::test]
async fn an_insert_outside_the_scope_is_forbidden_and_stores_nothing() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let scope = AccessScope::for_tenant(Uuid::new_v4());

    let result = SeaOrmRecordIdRepository::new()
        .insert(&conn, &scope, id(Uuid::new_v4(), "c", "p", "v"))
        .await;

    assert!(
        matches!(result, Err(DomainError::Forbidden(_))),
        "got {result:?}"
    );
    assert_eq!(stored_record_ids(&db).await, 0);
}

#[test]
fn the_key_separates_the_parts_of_the_identity() {
    assert_eq!(key("c", "p", "v"), key("c", "p", "v"));
    assert_eq!(key("c", "p", "v").as_str().len(), 64);
    // Moving characters across a boundary gives another identity.
    assert_ne!(key("ab", "c", "v"), key("a", "bc", "v"));
    assert_ne!(key("c", "pv", ""), key("c", "p", "v"));
    // A NUL inside a part does not stand for a boundary.
    assert_ne!(key("c", "p\0", "q"), key("c", "p", "\0q"));
    assert_ne!(key("c\0", "p", "v"), key("c", "\0p", "v"));
}

/// The parts are not stored, so a stored key can never be computed again:
/// the encoding must not change, or every resend of a record received before
/// the change would be taken as a new record. The value is the quickstart's.
#[test]
fn the_key_encoding_does_not_change() {
    assert_eq!(
        key(
            "11111111-6a88-4768-9dfc-6bcd5187d9ed",
            "chat_engine/thread-42/msg-7",
            "2026-09-10T09:13:00Z"
        )
        .as_str(),
        "ec8d74b4e17e2460d53844da5611d8930e4cd83f8152997413b3c973059be4ad"
    );
}
