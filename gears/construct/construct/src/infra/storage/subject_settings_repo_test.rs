//! Repository tests for subject settings on an in-memory `SQLite` database:
//! the stored row round-trips, and the tenant scope hides other tenants' rows.

use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, ScopeValue, pep_properties};
use uuid::Uuid;

use super::subject_settings_repo::{SeaOrmSubjectSettingsRepository, coverage};
use crate::domain::subject_settings::{
    ScopeRefusal, SettingsLookup, SettingsWrite, SubjectKey, SubjectSettings,
    SubjectSettingsRepository, UncheckablePredicate,
};
use crate::test_support::{
    CoveredLookup, inmem_db, insert_subject_settings, seed_subject_settings,
};

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
        .expect("find")
        .stored();

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
        .expect("find")
        .stored();

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
        .expect("find")
        .stored();
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
        .expect("find")
        .stored();
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
        .expect("find")
        .stored();
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
        .expect("find")
        .stored();

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
        .expect("find")
        .stored();

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
        .expect("find")
        .stored();
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
        .expect("find")
        .stored();
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
        .expect("find")
        .stored();
    assert_eq!(
        found,
        Some(SubjectSettings {
            personalization_enabled: true,
            erasure_in_progress: true,
        })
    );
}

#[tokio::test]
async fn setting_personalization_outside_the_scope_is_refused_before_the_write() {
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

    let written = repo
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
        .expect("set");

    assert_eq!(
        written,
        SettingsWrite::OutOfScope(ScopeRefusal::NotCovered),
        "a row of another tenant is outside the scope"
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
        .expect("find")
        .stored();
    assert_eq!(untouched, Some(ERASING));
}

#[tokio::test]
async fn setting_personalization_under_another_owner_is_refused_before_the_write() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSettingsRepository::new();
    let (tenant, subject, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let key = SubjectKey {
        tenant_id: tenant,
        subject_id: subject,
    };
    seed_subject_settings(&db, key, ERASING).await;
    let others_scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant]),
        ScopeFilter::in_uuids(pep_properties::OWNER_ID, vec![other]),
    ]));

    let written = repo
        .set_personalization(&conn, &others_scope, key, true)
        .await
        .expect("set");

    assert_eq!(
        written,
        SettingsWrite::OutOfScope(ScopeRefusal::NotCovered),
        "the subject's row is outside another owner's scope"
    );
    let untouched = repo
        .find(&conn, &AccessScope::for_tenant(tenant), key)
        .await
        .expect("find")
        .stored();
    assert_eq!(untouched, Some(ERASING));
}

#[tokio::test]
async fn under_the_owners_own_scope_the_row_is_written_and_read() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSettingsRepository::new();
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let key = SubjectKey {
        tenant_id: tenant,
        subject_id: subject,
    };
    let owners_scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant]),
        ScopeFilter::in_uuids(pep_properties::OWNER_ID, vec![subject]),
    ]));

    repo.set_personalization(&conn, &owners_scope, key, false)
        .await
        .expect("the owner's scope covers the owner's row");
    let found = repo
        .find(&conn, &owners_scope, key)
        .await
        .expect("find")
        .stored();

    assert_eq!(found, Some(SubjectSettings::new_subject(false)));
}

fn filter(property: &str, values: Vec<Uuid>) -> ScopeFilter {
    ScopeFilter::in_uuids(property, values)
}

fn scope(constraints: Vec<Vec<ScopeFilter>>) -> AccessScope {
    AccessScope::from_constraints(constraints.into_iter().map(ScopeConstraint::new).collect())
}

/// The coverage rules, through `find` on a stored row and on a missing one:
/// a scope that covers the key answers `Stored` or `Absent`, one that does not
/// answers the same for both.
#[tokio::test]
async fn find_decides_coverage_from_the_key_with_the_secure_orm_rules() {
    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let repo = SeaOrmSubjectSettingsRepository::new();
    let (tenant, subject, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let stored_key = SubjectKey {
        tenant_id: tenant,
        subject_id: subject,
    };
    let missing_key = SubjectKey {
        tenant_id: tenant,
        subject_id: other,
    };
    seed_subject_settings(&db, stored_key, ERASING).await;
    let own_tenant = filter(pep_properties::OWNER_TENANT_ID, vec![tenant]);

    let covering = [
        AccessScope::allow_all(),
        AccessScope::for_tenant(tenant),
        scope(vec![vec![
            own_tenant.clone(),
            filter(pep_properties::RESOURCE_ID, vec![subject, other]),
            filter(pep_properties::OWNER_ID, vec![subject, other]),
        ]]),
        scope(vec![vec![filter(
            pep_properties::OWNER_TENANT_ID,
            vec![Uuid::new_v4(), tenant],
        )]]),
        scope(vec![vec![ScopeFilter::eq(
            pep_properties::OWNER_TENANT_ID,
            tenant,
        )]]),
        scope(vec![
            vec![filter(
                pep_properties::OWNER_TENANT_ID,
                vec![Uuid::new_v4()],
            )],
            vec![own_tenant.clone()],
        ]),
    ];
    for covering_scope in &covering {
        let label = format!("{covering_scope:?}");
        assert_eq!(
            repo.find(&conn, covering_scope, stored_key)
                .await
                .expect("find"),
            SettingsLookup::Stored(ERASING),
            "{label}"
        );
        assert_eq!(
            repo.find(&conn, covering_scope, missing_key)
                .await
                .expect("find"),
            SettingsLookup::Absent,
            "{label}"
        );
    }

    let stranger = Uuid::new_v4();
    let not_covering = [
        AccessScope::deny_all(),
        scope(vec![vec![filter(
            pep_properties::OWNER_TENANT_ID,
            vec![Uuid::new_v4(), Uuid::new_v4()],
        )]]),
        scope(vec![vec![
            own_tenant.clone(),
            filter(pep_properties::RESOURCE_ID, vec![stranger]),
        ]]),
        scope(vec![vec![
            own_tenant.clone(),
            filter(pep_properties::OWNER_ID, vec![stranger]),
        ]]),
        scope(vec![vec![
            own_tenant.clone(),
            filter("category_id", vec![subject, other]),
        ]]),
    ];
    for not_covering_scope in &not_covering {
        let label = format!("{not_covering_scope:?}");
        for key in [stored_key, missing_key] {
            assert_eq!(
                repo.find(&conn, not_covering_scope, key)
                    .await
                    .expect("find"),
                SettingsLookup::OutOfScope(ScopeRefusal::NotCovered),
                "{label}"
            );
        }
    }

    for (undecidable_scope, kind) in [
        (
            scope(vec![vec![
                own_tenant.clone(),
                ScopeFilter::in_group_typed(
                    pep_properties::OWNER_ID,
                    "gts.cf.core.rg.member.v1~",
                    vec![subject.into()],
                ),
            ]]),
            UncheckablePredicate::Group,
        ),
        (
            scope(vec![vec![ScopeFilter::in_tenant_subtree(
                pep_properties::OWNER_TENANT_ID,
                tenant,
                false,
                Vec::new(),
            )]]),
            UncheckablePredicate::TenantSubtree,
        ),
        (
            scope(vec![vec![
                own_tenant.clone(),
                ScopeFilter::in_group_subtree_typed(
                    pep_properties::OWNER_ID,
                    "gts.cf.core.rg.member.v1~",
                    vec![subject.into()],
                ),
            ]]),
            UncheckablePredicate::GroupSubtree,
        ),
    ] {
        for key in [stored_key, missing_key] {
            assert_eq!(
                repo.find(&conn, &undecidable_scope, key)
                    .await
                    .expect("find"),
                SettingsLookup::OutOfScope(ScopeRefusal::Undecidable(kind)),
            );
        }
    }
}

/// One covering constraint wins over an undecidable one; without one, an
/// undecidable constraint makes the scope undecidable.
#[test]
fn constraints_combine_as_or_with_undecidable_below_covered() {
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let key = SubjectKey {
        tenant_id: tenant,
        subject_id: subject,
    };
    let group = || {
        vec![ScopeFilter::in_group_typed(
            pep_properties::OWNER_ID,
            "gts.cf.core.rg.member.v1~",
            vec![subject.into()],
        )]
    };

    assert_eq!(
        coverage(
            &scope(vec![
                group(),
                vec![filter(pep_properties::OWNER_TENANT_ID, vec![tenant])]
            ]),
            key
        ),
        Ok(())
    );
    assert_eq!(
        coverage(
            &scope(vec![
                group(),
                vec![filter(
                    pep_properties::OWNER_TENANT_ID,
                    vec![Uuid::new_v4()]
                )],
            ]),
            key
        ),
        Err(ScopeRefusal::Undecidable(UncheckablePredicate::Group))
    );
}

/// Every small scope of `Eq`/`In` filters, checked against the secure ORM
/// itself: the coverage check says "covered" exactly when the scoped select
/// returns the row, and exactly when the scoped write is accepted. This pins
/// the check to the SQL it predicts on both paths.
#[tokio::test]
async fn coverage_agrees_with_the_secure_orm_on_every_small_scope() {
    use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
    use toolkit_db::secure::{SecureEntityExt, SecureInsertExt};

    use super::subject_settings_entity::{self as entity, Entity};

    let db = inmem_db().await;
    let conn = db.conn().expect("connection");
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let key = SubjectKey {
        tenant_id: tenant,
        subject_id: subject,
    };
    seed_subject_settings(&db, key, ERASING).await;
    let miss = Uuid::new_v4();

    let mut filters = Vec::new();
    for (property, hit) in [
        (pep_properties::OWNER_TENANT_ID, tenant),
        (pep_properties::RESOURCE_ID, subject),
        (pep_properties::OWNER_ID, subject),
        ("category_id", subject),
    ] {
        for values in [vec![hit], vec![miss], vec![miss, hit]] {
            filters.push(ScopeFilter::in_uuids(property, values));
        }
        filters.push(ScopeFilter::eq(property, hit));
        filters.push(ScopeFilter::eq(property, miss));
        // The key's UUIDs held as strings, as a caller could build by hand.
        filters.push(ScopeFilter::eq(property, hit.to_string()));
        filters.push(ScopeFilter::r#in(
            property,
            vec![ScopeValue::from(hit.to_string().to_uppercase())],
        ));
    }
    let mut scopes = Vec::new();
    for first in &filters {
        scopes.push(scope(vec![vec![first.clone()]]));
        for second in &filters {
            scopes.push(scope(vec![vec![first.clone(), second.clone()]]));
            scopes.push(scope(vec![vec![first.clone()], vec![second.clone()]]));
        }
    }

    for checked in &scopes {
        let row = Entity::find()
            .secure()
            .scope_with(checked)
            .filter(
                Condition::all()
                    .add(entity::Column::TenantId.eq(tenant))
                    .add(entity::Column::SubjectId.eq(subject)),
            )
            .one(&conn)
            .await;
        let sql_sees_the_row = matches!(row, Ok(Some(_)));
        assert_eq!(
            coverage(checked, key).is_ok(),
            sql_sees_the_row,
            "read: {checked:?} (secure ORM: {row:?})"
        );
        // The secure ORM's own check of an insert of the key's row, not run.
        let row = entity::ActiveModel {
            tenant_id: ActiveValue::Set(tenant),
            subject_id: ActiveValue::Set(subject),
            personalization_enabled: ActiveValue::Set(false),
            erasure_in_progress: ActiveValue::NotSet,
        };
        let orm_write = Entity::insert(row.clone())
            .secure()
            .scope_with_model(checked, &row)
            .map(|_| ());
        assert_eq!(
            coverage(checked, key).is_ok(),
            orm_write.is_ok(),
            "write: {checked:?} (secure ORM: {orm_write:?})"
        );
        let written = SeaOrmSubjectSettingsRepository::new()
            .set_personalization(&conn, checked, key, false)
            .await
            .expect("set");
        assert_eq!(
            written == SettingsWrite::Written,
            orm_write.is_ok(),
            "repository write: {checked:?} ({written:?})"
        );
    }
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

    assert_eq!(
        in_named,
        SettingsLookup::Absent,
        "the other tenant's row is not read"
    );
    assert_eq!(in_other, SettingsLookup::Stored(ERASING));
}
