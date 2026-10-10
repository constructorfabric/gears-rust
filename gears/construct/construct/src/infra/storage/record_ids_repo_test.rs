//! Record identity repository tests on an in-memory `SQLite` database: the
//! insert is the repeat check, and the tenant scope guards it.

use toolkit_security::AccessScope;
use uuid::Uuid;

use super::record_ids_repo::{SeaOrmRecordIdRepository, record_key};
use crate::domain::error::DomainError;
use crate::domain::record_intake::{IdInsert, RecordId, RecordIdRepository};
use crate::test_support::{inmem_db, stored_record_ids};

fn id(tenant_id: Uuid, connector: &str, provenance: &str, version: &str) -> RecordId {
    RecordId {
        tenant_id,
        connector: connector.to_owned(),
        provenance: provenance.to_owned(),
        version: version.to_owned(),
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
    let tenant = Uuid::new_v4();
    let key = |c, p, v| record_key(&id(tenant, c, p, v));

    assert_eq!(key("c", "p", "v"), key("c", "p", "v"));
    assert_eq!(key("c", "p", "v").len(), 64);
    // Moving characters across a boundary gives another identity.
    assert_ne!(key("ab", "c", "v"), key("a", "bc", "v"));
    assert_ne!(key("c", "pv", ""), key("c", "p", "v"));
}
