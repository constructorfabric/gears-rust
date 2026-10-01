//! Content repository tests against in-memory `SQLite` with every migration
//! applied: tenant isolation (absent, not forbidden), lifecycle transitions,
//! cascades, the one-active and one-draft indexes and the service-scope active
//! load.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use sea_orm::ActiveValue::Set;
use sea_orm::EntityTrait;
use sea_orm_migration::MigratorTrait;
use serde_json::json;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::odata::LimitCfg;
use toolkit_db::secure::{AccessScope, DbConn, SecureEntityExt, secure_insert};
use toolkit_db::{ConnectOpts, Db, DbError, connect_db};
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use super::{
    OrmActiveContentLoader, OrmAssignmentRepository, OrmBundleRepository, OrmVersionRepository,
};
use crate::domain::model::{
    Assignment, AssignmentId, Bundle, BundleId, BundleVersion, Document, DocumentId, VersionId,
    VersionState,
};
use crate::domain::repos::{
    ActiveContentLoader, AssignmentRepository, BundleRepository, RepoError, VersionRepository,
    conflict,
};
use crate::infra::storage::Migrator;
use crate::infra::storage::entity::{bundle_version, document};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

async fn migrated_db() -> Db {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .map_err(|e| e.to_string())
        .expect("migrations apply");
    db
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc().replace_nanosecond(0).unwrap()
}

fn scope(tenant: Uuid) -> AccessScope {
    AccessScope::for_tenant(tenant)
}

fn all() -> AccessScope {
    AccessScope::allow_all()
}

fn bundles() -> OrmBundleRepository {
    OrmBundleRepository::new(LimitCfg {
        default: 25,
        max: 100,
    })
}

fn new_bundle(tenant: Uuid, name: &str) -> Bundle {
    let at = now();
    Bundle {
        id: BundleId(Uuid::new_v4()),
        owner_tenant_id: tenant,
        name: name.to_owned(),
        description: format!("{name} guardrails"),
        created_at: at,
        created_by: Uuid::new_v4(),
        updated_at: at,
    }
}

fn document_named(name: &str) -> Document {
    Document {
        id: DocumentId(Uuid::new_v4()),
        name: name.to_owned(),
        content: format!("package {name}\ndeny if {{ true }}"),
        resource_types: vec![
            "gts.cf.compute.vm.v1~".to_owned(),
            "gts.cf.compute.*".to_owned(),
        ],
        actions: vec!["create".to_owned(), "delete".to_owned()],
    }
}

fn draft(bundle: &Bundle, ordinal: i32, documents: Vec<Document>) -> BundleVersion {
    BundleVersion {
        id: VersionId(Uuid::new_v4()),
        bundle_id: bundle.id,
        owner_tenant_id: bundle.owner_tenant_id,
        ordinal,
        state: VersionState::Draft,
        created_at: now(),
        activated_at: None,
        activated_by: None,
        documents,
    }
}

fn assignment_of(bundle: &Bundle, tenant: Uuid) -> Assignment {
    let at = now();
    Assignment {
        id: AssignmentId(Uuid::new_v4()),
        bundle_id: bundle.id,
        tenant_id: tenant,
        owner_tenant_id: bundle.owner_tenant_id,
        enforce: true,
        created_at: at,
        updated_at: at,
    }
}

async fn seed_bundle(conn: &DbConn<'_>, tenant: Uuid, name: &str) -> Bundle {
    bundles()
        .insert(conn, &scope(tenant), &new_bundle(tenant, name))
        .await
        .expect("insert bundle")
}

async fn seed_draft(conn: &DbConn<'_>, bundle: &Bundle, names: &[&str]) -> BundleVersion {
    let repo = OrmVersionRepository;
    let s = scope(bundle.owner_tenant_id);
    let ordinal = repo.next_ordinal(conn, &s, bundle.id).await.unwrap();
    let documents = names.iter().map(|n| document_named(n)).collect();
    repo.insert_draft(conn, &s, &draft(bundle, ordinal, documents))
        .await
        .expect("insert draft")
}

async fn activate(conn: &DbConn<'_>, version: &BundleVersion) -> BundleVersion {
    OrmVersionRepository
        .activate(
            conn,
            &scope(version.owner_tenant_id),
            version.id,
            Uuid::new_v4(),
            now(),
        )
        .await
        .expect("activate")
        .activated
}

async fn state_of(conn: &DbConn<'_>, id: VersionId) -> VersionState {
    OrmVersionRepository
        .get_with_content(conn, &all(), id)
        .await
        .unwrap()
        .expect("version exists")
        .state
}

fn is_conflict(err: &RepoError, expected: &str) -> bool {
    matches!(err, RepoError::Conflict { reason } if *reason == expected)
}

// ---------------------------------------------------------------------------
// Bundles
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bundle_insert_get_list_are_tenant_scoped() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = bundles();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());

    let first = seed_bundle(&conn, a, "alpha").await;
    seed_bundle(&conn, a, "beta").await;
    let other = seed_bundle(&conn, b, "alpha").await;

    assert_eq!(
        repo.get(&conn, &scope(a), first.id).await.unwrap(),
        Some(first.clone())
    );
    // Another tenant's bundle is absent, not refused.
    assert_eq!(repo.get(&conn, &scope(b), first.id).await.unwrap(), None);
    assert_eq!(repo.get(&conn, &scope(a), other.id).await.unwrap(), None);

    let page_a = repo
        .list_page(&conn, &scope(a), &ODataQuery::new())
        .await
        .unwrap();
    assert_eq!(page_a.items.len(), 2);
    assert!(page_a.items.iter().all(|x| x.owner_tenant_id == a));
    let page_b = repo
        .list_page(&conn, &scope(b), &ODataQuery::new())
        .await
        .unwrap();
    assert_eq!(page_b.items, vec![other]);

    let limited = repo
        .list_page(&conn, &scope(a), &ODataQuery::new().with_limit(1))
        .await
        .unwrap();
    assert_eq!(limited.items.len(), 1);
    assert!(limited.page_info.next_cursor.is_some());
}

#[tokio::test]
async fn bundle_insert_outside_scope_and_duplicate_name_are_refused() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = bundles();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());

    let err = repo
        .insert(&conn, &scope(b), &new_bundle(a, "alpha"))
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");

    seed_bundle(&conn, a, "alpha").await;
    let err = repo
        .insert(&conn, &scope(a), &new_bundle(a, "alpha"))
        .await
        .unwrap_err();
    assert!(is_conflict(&err, conflict::BUNDLE_NAME_TAKEN), "{err:?}");
}

#[tokio::test]
async fn bundle_update_changes_name_and_description_within_scope() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = bundles();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, a, "alpha").await;
    seed_bundle(&conn, a, "taken").await;
    let at = now() + time::Duration::seconds(5);

    let renamed = repo
        .update(&conn, &scope(a), bundle.id, Some("renamed"), None, at)
        .await
        .unwrap();
    assert_eq!(renamed.name, "renamed");
    assert_eq!(renamed.description, bundle.description, "left unchanged");
    assert_eq!(renamed.updated_at, at);
    let cleared = repo
        .update(&conn, &scope(a), bundle.id, None, Some(""), at)
        .await
        .unwrap();
    assert_eq!(cleared.description, "");

    let err = repo
        .update(&conn, &scope(a), bundle.id, Some("taken"), None, at)
        .await
        .unwrap_err();
    assert!(is_conflict(&err, conflict::BUNDLE_NAME_TAKEN), "{err:?}");
    let err = repo
        .update(&conn, &scope(b), bundle.id, Some("x"), None, at)
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn version_content_round_trips_by_name_with_patterns_and_actions() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = OrmVersionRepository;
    let a = Uuid::new_v4();
    let bundle = seed_bundle(&conn, a, "alpha").await;
    let inserted = seed_draft(&conn, &bundle, &["b_doc", "a_doc"]).await;

    let read = repo
        .get_with_content(&conn, &scope(a), inserted.id)
        .await
        .unwrap()
        .unwrap();
    let names: Vec<&str> = read.documents.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["a_doc", "b_doc"], "documents ordered by name");
    let mut expected = inserted.documents.clone();
    expected.sort_by(|x, y| x.name.cmp(&y.name));
    assert_eq!(read.documents, expected);
    assert_eq!(read.state, VersionState::Draft);
    assert_eq!(read.ordinal, 1);

    let headers = repo
        .list_for_bundle(&conn, &scope(a), bundle.id)
        .await
        .unwrap();
    assert_eq!(headers.len(), 1);
    assert!(headers[0].documents.is_empty());
    assert_eq!(
        repo.next_ordinal(&conn, &scope(a), bundle.id)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn version_other_tenant_sees_nothing_and_cannot_attach() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = OrmVersionRepository;
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, a, "alpha").await;
    let version = seed_draft(&conn, &bundle, &["one"]).await;

    assert_eq!(
        repo.get_with_content(&conn, &scope(b), version.id)
            .await
            .unwrap(),
        None
    );
    assert!(
        repo.list_for_bundle(&conn, &scope(b), bundle.id)
            .await
            .unwrap()
            .is_empty()
    );
    let err = repo
        .insert_draft(&conn, &scope(b), &draft(&bundle, 5, Vec::new()))
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");
    let err = repo
        .replace_draft_content(&conn, &scope(b), version.id, &[])
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");
    let err = repo
        .delete_draft(&conn, &scope(b), version.id)
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");
}

#[tokio::test]
async fn version_a_bundle_has_one_draft_and_duplicate_document_names_are_refused() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = OrmVersionRepository;
    let a = Uuid::new_v4();
    let bundle = seed_bundle(&conn, a, "alpha").await;
    seed_draft(&conn, &bundle, &["one"]).await;

    let err = repo
        .insert_draft(&conn, &scope(a), &draft(&bundle, 2, Vec::new()))
        .await
        .unwrap_err();
    assert!(is_conflict(&err, conflict::DRAFT_EXISTS), "{err:?}");

    let other = seed_bundle(&conn, a, "beta").await;
    let twice = vec![document_named("same"), document_named("same")];
    let err = repo
        .insert_draft(&conn, &scope(a), &draft(&other, 1, twice))
        .await
        .unwrap_err();
    assert!(
        is_conflict(&err, conflict::DUPLICATE_DOCUMENT_NAME),
        "{err:?}"
    );

    let mut active = draft(&other, 1, Vec::new());
    active.state = VersionState::Active;
    let err = repo
        .insert_draft(&conn, &scope(a), &active)
        .await
        .unwrap_err();
    assert!(is_conflict(&err, conflict::VERSION_NOT_DRAFT), "{err:?}");
}

#[tokio::test]
async fn version_replace_content_only_while_draft() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = OrmVersionRepository;
    let a = Uuid::new_v4();
    let bundle = seed_bundle(&conn, a, "alpha").await;
    let version = seed_draft(&conn, &bundle, &["old1", "old2"]).await;

    let replacement = vec![document_named("new")];
    let replaced = repo
        .replace_draft_content(&conn, &scope(a), version.id, &replacement)
        .await
        .unwrap();
    assert_eq!(replaced.documents, replacement);
    let read = repo
        .get_with_content(&conn, &scope(a), version.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.documents, replacement);

    let twice = vec![document_named("same"), document_named("same")];
    let err = repo
        .replace_draft_content(&conn, &scope(a), version.id, &twice)
        .await
        .unwrap_err();
    assert!(
        is_conflict(&err, conflict::DUPLICATE_DOCUMENT_NAME),
        "{err:?}"
    );

    activate(&conn, &read).await;
    let err = repo
        .replace_draft_content(&conn, &scope(a), version.id, &[])
        .await
        .unwrap_err();
    assert!(is_conflict(&err, conflict::VERSION_NOT_DRAFT), "{err:?}");
}

#[tokio::test]
async fn version_activation_supersedes_the_previous_active_version() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = OrmVersionRepository;
    let a = Uuid::new_v4();
    let bundle = seed_bundle(&conn, a, "alpha").await;
    let v1 = seed_draft(&conn, &bundle, &["one"]).await;
    let actor = Uuid::new_v4();
    let at = now();

    let first = repo
        .activate(&conn, &scope(a), v1.id, actor, at)
        .await
        .unwrap();
    assert_eq!(first.superseded, None);
    assert_eq!(first.activated.state, VersionState::Active);
    assert_eq!(first.activated.activated_by, Some(actor));
    assert_eq!(first.activated.activated_at, Some(at));

    let v2 = seed_draft(&conn, &bundle, &["two"]).await;
    let second = repo
        .activate(&conn, &scope(a), v2.id, actor, at)
        .await
        .unwrap();
    assert_eq!(second.superseded, Some(v1.id));
    assert_eq!(state_of(&conn, v1.id).await, VersionState::Superseded);
    assert_eq!(state_of(&conn, v2.id).await, VersionState::Active);

    let again = repo
        .activate(&conn, &scope(a), v1.id, actor, at)
        .await
        .unwrap_err();
    assert!(
        is_conflict(&again, conflict::VERSION_NOT_DRAFT),
        "{again:?}"
    );
    let missing = repo
        .activate(&conn, &scope(a), VersionId(Uuid::new_v4()), actor, at)
        .await
        .unwrap_err();
    assert!(matches!(missing, RepoError::NotFound), "{missing:?}");
}

#[tokio::test]
async fn version_activation_rolls_back_with_its_transaction() {
    let db = migrated_db().await;
    let a = Uuid::new_v4();
    let (v1, v2) = {
        let conn = db.conn().unwrap();
        let bundle = seed_bundle(&conn, a, "alpha").await;
        let v1 = activate(&conn, &seed_draft(&conn, &bundle, &["one"]).await).await;
        let v2 = seed_draft(&conn, &bundle, &["two"]).await;
        (v1, v2)
    };

    let id = v2.id;
    let result = db
        .transaction_ref(move |tx| {
            Box::pin(async move {
                OrmVersionRepository
                    .activate(tx, &scope(a), id, Uuid::new_v4(), now())
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::new(e)))?;
                Err::<(), _>(DbError::Other(anyhow::anyhow!("forced after activation")))
            })
        })
        .await;
    assert!(result.is_err());

    let conn = db.conn().unwrap();
    assert_eq!(state_of(&conn, v1.id).await, VersionState::Active);
    assert_eq!(state_of(&conn, v2.id).await, VersionState::Draft);
}

#[tokio::test]
async fn version_second_activation_cannot_leave_two_active() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let a = Uuid::new_v4();
    let bundle = seed_bundle(&conn, a, "alpha").await;
    let candidate = seed_draft(&conn, &bundle, &["one"]).await;

    // A concurrent activation that committed after this caller's read: an
    // active row this caller's scoped supersession step cannot see, so only
    // the partial unique index stands between the bundle and two actives.
    secure_insert::<bundle_version::Entity>(
        bundle_version::ActiveModel {
            id: Set(Uuid::new_v4()),
            bundle_id: Set(bundle.id.0),
            owner_tenant_id: Set(Uuid::new_v4()),
            ordinal: Set(99),
            state: Set(bundle_version::STATE_ACTIVE),
            created_at: Set(now()),
            activated_at: Set(Some(now())),
            activated_by: Set(Some(Uuid::new_v4())),
        },
        &all(),
        &conn,
    )
    .await
    .unwrap();

    let err = OrmVersionRepository
        .activate(&conn, &scope(a), candidate.id, Uuid::new_v4(), now())
        .await
        .unwrap_err();
    assert!(
        is_conflict(&err, conflict::CONCURRENT_ACTIVATION),
        "{err:?}"
    );
    assert_eq!(state_of(&conn, candidate.id).await, VersionState::Draft);
    let actives = bundle_version::Entity::find()
        .secure()
        .scope_with(&all())
        .all(&conn)
        .await
        .unwrap()
        .into_iter()
        .filter(|v| v.bundle_id == bundle.id.0 && v.state == bundle_version::STATE_ACTIVE)
        .count();
    assert_eq!(actives, 1);
}

#[tokio::test]
async fn version_delete_draft_cascades_and_refuses_non_drafts() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = OrmVersionRepository;
    let a = Uuid::new_v4();
    let bundle = seed_bundle(&conn, a, "alpha").await;
    let doomed = seed_draft(&conn, &bundle, &["one", "two"]).await;
    repo.delete_draft(&conn, &scope(a), doomed.id)
        .await
        .unwrap();
    assert_eq!(
        repo.get_with_content(&conn, &scope(a), doomed.id)
            .await
            .unwrap(),
        None
    );

    let kept = activate(&conn, &seed_draft(&conn, &bundle, &["three"]).await).await;
    let documents = document::Entity::find()
        .secure()
        .scope_with(&all())
        .all(&conn)
        .await
        .unwrap();
    assert!(documents.iter().all(|d| d.version_id == kept.id.0));
    assert_eq!(documents.len(), 1, "the doomed documents cascaded");

    let err = repo
        .delete_draft(&conn, &scope(a), kept.id)
        .await
        .unwrap_err();
    assert!(is_conflict(&err, conflict::VERSION_NOT_DRAFT), "{err:?}");
    let err = repo
        .delete_draft(&conn, &scope(a), doomed.id)
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");
}

#[tokio::test]
async fn version_undecodable_rows_are_database_errors() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let a = Uuid::new_v4();
    let bundle = seed_bundle(&conn, a, "alpha").await;
    let version = seed_draft(&conn, &bundle, &[]).await;
    secure_insert::<document::Entity>(
        document::ActiveModel {
            id: Set(Uuid::new_v4()),
            version_id: Set(version.id.0),
            owner_tenant_id: Set(a),
            name: Set("odd".to_owned()),
            content: Set(String::new()),
            resource_types: Set(json!({"not": "an array"})),
            actions: Set(json!([])),
        },
        &scope(a),
        &conn,
    )
    .await
    .unwrap();

    let err = OrmVersionRepository
        .get_with_content(&conn, &scope(a), version.id)
        .await
        .unwrap_err();
    let RepoError::Database(message) = err else {
        panic!("expected a database error, got {err:?}");
    };
    assert!(
        message.contains("resource_types is not a JSON array of text"),
        "{message}"
    );
}

// ---------------------------------------------------------------------------
// Assignments
// ---------------------------------------------------------------------------

#[tokio::test]
async fn assignment_is_unique_per_bundle_and_tenant_and_updates_enforce() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = OrmAssignmentRepository;
    let (a, b, child) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, a, "alpha").await;

    let created = repo
        .insert(&conn, &scope(a), &assignment_of(&bundle, child))
        .await
        .unwrap();
    assert!(created.enforce);
    let err = repo
        .insert(&conn, &scope(a), &assignment_of(&bundle, child))
        .await
        .unwrap_err();
    assert!(is_conflict(&err, conflict::ASSIGNMENT_EXISTS), "{err:?}");

    let mut foreign = assignment_of(&bundle, Uuid::new_v4());
    foreign.owner_tenant_id = b;
    let err = repo.insert(&conn, &scope(b), &foreign).await.unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");
    assert_eq!(repo.get(&conn, &scope(b), created.id).await.unwrap(), None);

    let at = now() + time::Duration::seconds(5);
    let shadow = repo
        .set_enforce(&conn, &scope(a), created.id, false, at)
        .await
        .unwrap();
    assert!(!shadow.enforce);
    assert_eq!(shadow.updated_at, at);
    assert_eq!((shadow.bundle_id, shadow.tenant_id), (bundle.id, child));
    let err = repo
        .set_enforce(&conn, &scope(b), created.id, true, at)
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");

    assert_eq!(
        repo.list_for_bundle(&conn, &scope(a), bundle.id)
            .await
            .unwrap(),
        vec![shadow]
    );
    let err = repo.delete(&conn, &scope(b), created.id).await.unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");
    repo.delete(&conn, &scope(a), created.id).await.unwrap();
    assert_eq!(repo.get(&conn, &scope(a), created.id).await.unwrap(), None);
    let err = repo.delete(&conn, &scope(a), created.id).await.unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");
}

// ---------------------------------------------------------------------------
// The active-content load
// ---------------------------------------------------------------------------

#[tokio::test]
async fn loader_returns_only_active_content() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let assignments = OrmAssignmentRepository;
    let (a, t1, t2) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

    // Bundle with a superseded, an active and a newer draft version.
    let governed = seed_bundle(&conn, a, "governed").await;
    activate(&conn, &seed_draft(&conn, &governed, &["old"]).await).await;
    let current = activate(
        &conn,
        &seed_draft(&conn, &governed, &["cur1", "cur2"]).await,
    )
    .await;
    seed_draft(&conn, &governed, &["next"]).await;
    // Bundle with a draft only.
    let drafted = seed_bundle(&conn, a, "drafted").await;
    seed_draft(&conn, &drafted, &["d"]).await;
    // An active bundle without assignments contributes nothing either.
    let unassigned = seed_bundle(&conn, a, "unassigned").await;
    activate(&conn, &seed_draft(&conn, &unassigned, &["u"]).await).await;

    for bundle in [&governed, &drafted] {
        for tenant in [t1, t2] {
            assignments
                .insert(&conn, &scope(a), &assignment_of(bundle, tenant))
                .await
                .unwrap();
        }
    }

    // Only the tenants of the chain are loaded.
    let other = Uuid::new_v4();
    let none = OrmActiveContentLoader
        .load_for_tenants(&conn, &[other])
        .await
        .unwrap();
    assert!(none.is_empty());

    let loaded = OrmActiveContentLoader
        .load_for_tenants(&conn, &[t1, t2, other])
        .await
        .unwrap();
    assert_eq!(loaded.len(), 2);
    let mut tenants: Vec<Uuid> = loaded.iter().map(|c| c.assignment.tenant_id).collect();
    tenants.sort();
    let mut expected = vec![t1, t2];
    expected.sort();
    assert_eq!(tenants, expected);
    for entry in &loaded {
        assert_eq!(entry.assignment.bundle_id, governed.id);
        assert_eq!(entry.version.id, current.id);
        assert_eq!(entry.version.state, VersionState::Active);
        let names: Vec<&str> = entry
            .version
            .documents
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(names, ["cur1", "cur2"]);
    }
    assert!(Arc::ptr_eq(&loaded[0].version, &loaded[1].version));
}

#[tokio::test]
async fn loader_is_empty_without_active_versions() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let a = Uuid::new_v4();
    let bundle = seed_bundle(&conn, a, "alpha").await;
    seed_draft(&conn, &bundle, &["one"]).await;
    OrmAssignmentRepository
        .insert(&conn, &scope(a), &assignment_of(&bundle, a))
        .await
        .unwrap();
    assert!(
        OrmActiveContentLoader
            .load_for_tenants(&conn, &[a])
            .await
            .unwrap()
            .is_empty()
    );
}
