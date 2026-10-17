//! Repository tests for subject settings on an in-memory `SQLite` database:
//! the stored row round-trips, and the tenant scope hides other tenants' rows.

use toolkit_security::AccessScope;
use uuid::Uuid;

use super::subject_settings_repo::SeaOrmSubjectSettingsRepository;
use crate::domain::subject_settings::{SubjectKey, SubjectSettings, SubjectSettingsRepository};
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
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let found = SeaOrmSubjectSettingsRepository::new()
        .find(
            &conn,
            &AccessScope::for_tenant(tenant),
            SubjectKey {
                tenant_id: tenant,
                subject_id: subject,
            },
        )
        .await
        .expect("find");

    assert_eq!(found, Some(ERASING));
}

#[tokio::test]
async fn subject_without_a_row_is_not_found() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let tenant = Uuid::new_v4();
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: Uuid::new_v4(),
        },
        ERASING,
    )
    .await;

    let found = SeaOrmSubjectSettingsRepository::new()
        .find(
            &conn,
            &AccessScope::for_tenant(tenant),
            SubjectKey {
                tenant_id: tenant,
                subject_id: Uuid::new_v4(),
            },
        )
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
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: other,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let from_own = repo
        .find(
            &conn,
            &AccessScope::for_tenant(own),
            SubjectKey {
                tenant_id: own,
                subject_id: subject,
            },
        )
        .await
        .expect("find");
    assert_eq!(from_own, None, "a cross-tenant read returns nothing");

    let from_other = repo
        .find(
            &conn,
            &AccessScope::for_tenant(other),
            SubjectKey {
                tenant_id: other,
                subject_id: subject,
            },
        )
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
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: first,
            subject_id: subject,
        },
        on,
    )
    .await;
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: second,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let in_first = repo
        .find(
            &conn,
            &AccessScope::for_tenant(first),
            SubjectKey {
                tenant_id: first,
                subject_id: subject,
            },
        )
        .await
        .expect("find");
    let in_second = repo
        .find(
            &conn,
            &AccessScope::for_tenant(second),
            SubjectKey {
                tenant_id: second,
                subject_id: subject,
            },
        )
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
    insert_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        true,
        None,
    )
    .await
    .expect("insert without the erasure flag");

    let found = SeaOrmSubjectSettingsRepository::new()
        .find(
            &conn,
            &AccessScope::for_tenant(tenant),
            SubjectKey {
                tenant_id: tenant,
                subject_id: subject,
            },
        )
        .await
        .expect("find");

    assert_eq!(found, Some(SubjectSettings::new_subject(true)));
}

#[tokio::test]
async fn second_row_for_the_same_tenant_and_subject_is_refused() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let second = insert_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        true,
        Some(false),
    )
    .await;

    assert!(second.is_err(), "the primary key refuses a second row");
    let found = SeaOrmSubjectSettingsRepository::new()
        .find(
            &conn,
            &AccessScope::for_tenant(tenant),
            SubjectKey {
                tenant_id: tenant,
                subject_id: subject,
            },
        )
        .await
        .expect("find");
    assert_eq!(found, Some(ERASING), "the first row is unchanged");
}

#[tokio::test]
async fn setting_personalization_creates_a_row_without_an_erasure() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSettingsRepository::new();
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let scope = AccessScope::for_tenant(tenant);

    repo.set_personalization(
        &conn,
        &scope,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        false,
    )
    .await
    .expect("create");

    let found = repo
        .find(
            &conn,
            &scope,
            SubjectKey {
                tenant_id: tenant,
                subject_id: subject,
            },
        )
        .await
        .expect("find");
    assert_eq!(found, Some(SubjectSettings::new_subject(false)));
}

#[tokio::test]
async fn setting_personalization_keeps_the_erasure_flag_of_an_existing_row() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSettingsRepository::new();
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let scope = AccessScope::for_tenant(tenant);
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    repo.set_personalization(
        &conn,
        &scope,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        true,
    )
    .await
    .expect("update");

    let found = repo
        .find(
            &conn,
            &scope,
            SubjectKey {
                tenant_id: tenant,
                subject_id: subject,
            },
        )
        .await
        .expect("find");
    assert_eq!(
        found,
        Some(SubjectSettings {
            personalization_enabled: true,
            erasure_in_progress: true,
        })
    );
}

#[tokio::test]
async fn setting_personalization_outside_the_scope_is_forbidden() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSettingsRepository::new();
    let (own, other, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: other,
            subject_id: subject,
        },
        ERASING,
    )
    .await;

    let err = repo
        .set_personalization(
            &conn,
            &AccessScope::for_tenant(own),
            SubjectKey {
                tenant_id: other,
                subject_id: subject,
            },
            true,
        )
        .await
        .expect_err("a row of another tenant is outside the scope");

    assert!(
        matches!(err, crate::domain::error::DomainError::Forbidden(_)),
        "got {err:?}"
    );
    let untouched = repo
        .find(
            &conn,
            &AccessScope::for_tenant(other),
            SubjectKey {
                tenant_id: other,
                subject_id: subject,
            },
        )
        .await
        .expect("find");
    assert_eq!(untouched, Some(ERASING));
}

#[tokio::test]
async fn a_scope_over_several_tenants_reads_only_the_named_tenants_row() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSettingsRepository::new();
    let (named, other, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let in_tenant = |tenant_id| SubjectKey {
        tenant_id,
        subject_id: subject,
    };
    seed_subject_settings(&db, in_tenant(other), ERASING).await;
    let both = AccessScope::for_tenants(vec![named, other]);

    let in_named = repo
        .find(&conn, &both, in_tenant(named))
        .await
        .expect("find");
    let in_other = repo
        .find(&conn, &both, in_tenant(other))
        .await
        .expect("find");

    assert_eq!(in_named, None, "the other tenant's row is not read");
    assert_eq!(in_other, Some(ERASING));
}
