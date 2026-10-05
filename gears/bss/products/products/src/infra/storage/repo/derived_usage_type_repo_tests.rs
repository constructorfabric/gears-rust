//! The derived usage type store (P-D-231): scoped by tenant, the code unique per tenant, versions
//! append-only with their digest as written.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::test_support::test_db;
use toolkit_db::secure::AccessScope;

fn new_type(code: &str) -> NewDerivedType {
    NewDerivedType {
        code: code.to_owned(),
        name: format!("{code} name"),
    }
}
fn new_version(type_id: Uuid, version: u32, digest: &str) -> NewDerivedVersion {
    NewDerivedVersion {
        type_id,
        version,
        declaration_json: serde_json::json!({ "v": version }),
        digest: digest.to_owned(),
        created_by: Uuid::from_u128(7),
        created_at: OffsetDateTime::now_utc(),
    }
}

/// A type and its versions read back as written, the digest included; the latest version and the
/// version list follow the inserts.
#[tokio::test]
async fn a_type_and_its_versions_read_back_as_written() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let now = OffsetDateTime::now_utc();
    let t = create_type(
        &conn,
        &scope,
        tenant,
        new_type("cloudlets"),
        Uuid::from_u128(7),
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        find_type(&conn, &scope, tenant, "cloudlets").await.unwrap(),
        Some(t.clone())
    );
    let one = insert_version(&conn, &scope, tenant, new_version(t.id, 1, &"a".repeat(64)))
        .await
        .unwrap();
    let two = insert_version(&conn, &scope, tenant, new_version(t.id, 2, &"b".repeat(64)))
        .await
        .unwrap();
    assert_eq!(one.digest, "a".repeat(64), "the digest is stored as given");
    assert_eq!(
        find_version(&conn, &scope, tenant, t.id, 1).await.unwrap(),
        Some(one.clone())
    );
    assert_eq!(
        list_versions(&conn, &scope, tenant, t.id).await.unwrap(),
        [one, two]
    );
    assert_eq!(
        find_version(&conn, &scope, tenant, t.id, 3).await.unwrap(),
        None
    );
    let latest = latest_versions(&conn, &scope, tenant, &[t.id, Uuid::new_v4()])
        .await
        .unwrap();
    assert_eq!(latest.len(), 1, "an unknown type adds no row");
    let row = &latest[&t.id];
    assert_eq!(row.version, 2);
    assert_eq!(row.digest, "b".repeat(64));
    assert_eq!(row.declaration_json, serde_json::json!({ "v": 2 }));
    assert!(matches!(
        insert_version(&conn, &scope, tenant, new_version(t.id, 2, &"c".repeat(64))).await,
        Err(RepoError::Refused(RepoRefusal::DerivedVersionTaken))
    ));
}

/// The code is unique per tenant, not across tenants.
#[tokio::test]
async fn the_code_is_unique_per_tenant() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let now = OffsetDateTime::now_utc();
    let actor = Uuid::from_u128(7);
    create_type(&conn, &scope, tenant, new_type("cloudlets"), actor, now)
        .await
        .unwrap();
    assert!(matches!(
        create_type(&conn, &scope, tenant, new_type("cloudlets"), actor, now).await,
        Err(RepoError::Refused(RepoRefusal::DerivedCodeTaken))
    ));
    let other = Uuid::new_v4();
    create_type(
        &conn,
        &AccessScope::for_tenant(other),
        other,
        new_type("cloudlets"),
        actor,
        now,
    )
    .await
    .unwrap();
}

/// Another tenant reads nothing of this tenant's: neither the type by its code, nor a version by
/// the type's id, nor a line of the list.
#[tokio::test]
async fn another_tenant_reads_nothing() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let t = create_type(
        &conn,
        &scope,
        tenant,
        new_type("cloudlets"),
        Uuid::from_u128(7),
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    insert_version(&conn, &scope, tenant, new_version(t.id, 1, &"a".repeat(64)))
        .await
        .unwrap();
    let other = Uuid::new_v4();
    let theirs = AccessScope::for_tenant(other);
    assert_eq!(
        find_type(&conn, &theirs, other, "cloudlets").await.unwrap(),
        None
    );
    assert_eq!(
        find_version(&conn, &theirs, other, t.id, 1).await.unwrap(),
        None
    );
    assert_eq!(
        list_versions(&conn, &theirs, other, t.id).await.unwrap(),
        []
    );
    assert!(
        latest_versions(&conn, &theirs, other, &[t.id])
            .await
            .unwrap()
            .is_empty()
    );
    let page = list(&conn, &theirs, other, &ODataQuery::default())
        .await
        .unwrap();
    assert!(page.items.is_empty());
    // The tenant's own scope names the tenant: asking it for another tenant's rows finds none
    // either.
    assert_eq!(
        find_type(&conn, &scope, other, "cloudlets").await.unwrap(),
        None
    );
}
