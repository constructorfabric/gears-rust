//! Repository tests for subject settings on an in-memory `SQLite` database:
//! the stored row round-trips, and the tenant scope hides other tenants' rows.

use toolkit_security::AccessScope;
use uuid::Uuid;

use super::subject_settings_repo::SeaOrmSubjectSettingsRepository;
use crate::domain::subject_settings::{SubjectSettings, SubjectSettingsRepository};
use crate::test_support::{inmem_db, insert_subject_settings, seed_subject_settings};

const ERASING: SubjectSettings = SubjectSettings {
    personalization_enabled: false,
    erasure_in_progress: true,
};

#[tokio::test]
async fn stored_settings_round_trip() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(&db, tenant, subject, ERASING).await;

    let found = SeaOrmSubjectSettingsRepository::new()
        .find(&conn, &AccessScope::for_tenant(tenant), subject)
        .await
        .expect("find");

    assert_eq!(found, Some(ERASING));
}

#[tokio::test]
async fn subject_without_a_row_is_not_found() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let tenant = Uuid::new_v4();
    seed_subject_settings(&db, tenant, Uuid::new_v4(), ERASING).await;

    let found = SeaOrmSubjectSettingsRepository::new()
        .find(&conn, &AccessScope::for_tenant(tenant), Uuid::new_v4())
        .await
        .expect("find");

    assert_eq!(found, None);
}

#[tokio::test]
async fn row_of_another_tenant_is_not_found() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSettingsRepository::new();
    let (own, other, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(&db, other, subject, ERASING).await;

    let from_own = repo
        .find(&conn, &AccessScope::for_tenant(own), subject)
        .await
        .expect("find");
    assert_eq!(from_own, None, "a cross-tenant read returns nothing");

    let from_other = repo
        .find(&conn, &AccessScope::for_tenant(other), subject)
        .await
        .expect("find");
    assert_eq!(
        from_other,
        Some(ERASING),
        "the row exists in its own tenant"
    );
}

#[tokio::test]
async fn same_subject_keeps_one_row_per_tenant() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSettingsRepository::new();
    let (first, second, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let on = SubjectSettings::new_subject(true);
    seed_subject_settings(&db, first, subject, on).await;
    seed_subject_settings(&db, second, subject, ERASING).await;

    let in_first = repo
        .find(&conn, &AccessScope::for_tenant(first), subject)
        .await
        .expect("find");
    let in_second = repo
        .find(&conn, &AccessScope::for_tenant(second), subject)
        .await
        .expect("find");

    assert_eq!(in_first, Some(on));
    assert_eq!(in_second, Some(ERASING));
}

#[tokio::test]
async fn erasure_flag_defaults_to_cleared_when_the_insert_leaves_it_out() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    insert_subject_settings(&db, tenant, subject, true, None)
        .await
        .expect("insert without the erasure flag");

    let found = SeaOrmSubjectSettingsRepository::new()
        .find(&conn, &AccessScope::for_tenant(tenant), subject)
        .await
        .expect("find");

    assert_eq!(found, Some(SubjectSettings::new_subject(true)));
}

#[tokio::test]
async fn second_row_for_the_same_tenant_and_subject_is_refused() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(&db, tenant, subject, ERASING).await;

    let second = insert_subject_settings(&db, tenant, subject, true, Some(false)).await;

    assert!(second.is_err(), "the primary key refuses a second row");
    let found = SeaOrmSubjectSettingsRepository::new()
        .find(&conn, &AccessScope::for_tenant(tenant), subject)
        .await
        .expect("find");
    assert_eq!(found, Some(ERASING), "the first row is unchanged");
}
