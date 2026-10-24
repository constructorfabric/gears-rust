//! Content repository tests against in-memory `SQLite` with every migration
//! applied: tenant isolation (absent, not forbidden), lifecycle transitions,
//! cascades, the one-active and one-draft indexes and the service-scope active
//! load.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::json;
use time::OffsetDateTime;
use toolkit_db::DbError;
use toolkit_db::odata::LimitCfg;
use toolkit_db::secure::{AccessScope, DbConn, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, SortDir};
use uuid::Uuid;

use super::{
    OrmActiveContentLoader, OrmAssignmentRepository, OrmBundleRepository, OrmVersionRepository,
};
use crate::domain::model::{
    Assignment, AssignmentId, Bundle, BundleId, BundleVersion, Document, DocumentId, VersionId,
    VersionState,
};
use crate::domain::repos::{
    ActiveContentLoader, AssignmentRepository, AssignmentVisibility, BundleRepository, RepoError,
    VersionRepository, conflict,
};
use crate::infra::storage::entity::{bundle_version, document};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

use crate::test_support::migrated_db;

/// The subject the fixtures write as.
const ACTOR: Uuid = Uuid::from_u128(0xAC7);

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

fn draft(bundle: &Bundle, ordinal: u32, documents: Vec<Document>) -> BundleVersion {
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
        created_by: ACTOR,
        updated_at: at,
        updated_by: ACTOR,
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
    let editor = Uuid::new_v4();
    let shadow = repo
        .set_enforce(&conn, &scope(a), created.id, false, editor, at)
        .await
        .unwrap();
    assert!(!shadow.enforce);
    assert_eq!(shadow.updated_at, at);
    assert_eq!((shadow.created_by, shadow.updated_by), (ACTOR, editor));
    assert_eq!((shadow.bundle_id, shadow.tenant_id), (bundle.id, child));
    let err = repo
        .set_enforce(&conn, &scope(b), created.id, true, editor, at)
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
// Assignment visibility and listing
// ---------------------------------------------------------------------------

/// A visibility that admits nothing until fields are set.
fn nothing_visible() -> AssignmentVisibility {
    AssignmentVisibility {
        read: None,
        subtree: Vec::new(),
        reaching: Vec::new(),
        withdrawable: Vec::new(),
    }
}

/// The owner-side alternative: `owner` administers `target`'s assignments
/// of bundles it can read.
fn administers(owner: Uuid, target: Uuid) -> AssignmentVisibility {
    AssignmentVisibility {
        read: Some(scope(owner)),
        subtree: vec![owner, target],
        reaching: vec![owner, target],
        withdrawable: Vec::new(),
    }
}

async fn seed_assignment(conn: &DbConn<'_>, bundle: &Bundle, tenant: Uuid) -> Assignment {
    OrmAssignmentRepository
        .insert(
            conn,
            &scope(bundle.owner_tenant_id),
            &assignment_of(bundle, tenant),
        )
        .await
        .expect("insert assignment")
}

async fn visible_to(
    conn: &DbConn<'_>,
    visibility: &AssignmentVisibility,
    assignment: &Assignment,
) -> bool {
    listed_ids(conn, visibility).await.contains(&assignment.id)
}

async fn listed_ids(conn: &DbConn<'_>, visibility: &AssignmentVisibility) -> Vec<AssignmentId> {
    OrmAssignmentRepository
        .list_page(conn, visibility, &ODataQuery::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|a| a.id)
        .collect()
}

fn filtered(text: &str) -> ODataQuery {
    ODataQuery::default().with_filter(
        toolkit_odata::parse_filter_string(text)
            .expect("the fixture parses")
            .into_expr(),
    )
}

#[tokio::test]
async fn an_owner_side_row_is_visible_when_bundle_target_and_owner_all_fit() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (owner, target) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let row = seed_assignment(&conn, &bundle, target).await;

    let visibility = administers(owner, target);
    assert!(visible_to(&conn, &visibility, &row).await);
    assert_eq!(listed_ids(&conn, &visibility).await, vec![row.id]);
}

#[tokio::test]
async fn an_owner_side_row_needs_its_bundle_within_the_read_scope() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (owner, target) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let row = seed_assignment(&conn, &bundle, target).await;

    let mut visibility = administers(owner, target);
    visibility.read = Some(scope(Uuid::new_v4()));
    assert!(!visible_to(&conn, &visibility, &row).await);
    assert!(listed_ids(&conn, &visibility).await.is_empty());
}

#[tokio::test]
async fn an_owner_side_row_needs_its_target_in_the_subtree() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (owner, target) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let row = seed_assignment(&conn, &bundle, target).await;

    let mut visibility = administers(owner, target);
    visibility.subtree = vec![owner];
    assert!(!visible_to(&conn, &visibility, &row).await);
    assert!(listed_ids(&conn, &visibility).await.is_empty());
}

#[tokio::test]
async fn an_owner_side_row_needs_its_owner_among_the_reaching_tenants() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (owner, target) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let row = seed_assignment(&conn, &bundle, target).await;

    let mut visibility = administers(owner, target);
    visibility.reaching = vec![target];
    assert!(!visible_to(&conn, &visibility, &row).await);
    assert!(listed_ids(&conn, &visibility).await.is_empty());
}

#[tokio::test]
async fn a_dormant_row_is_visible_where_it_is_withdrawable_whatever_its_bundle() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (owner, target) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let row = seed_assignment(&conn, &bundle, target).await;

    // No read grant at all, and the owner is on the far side of a barrier.
    let visibility = AssignmentVisibility {
        reaching: vec![target],
        withdrawable: vec![target],
        ..nothing_visible()
    };
    assert!(visible_to(&conn, &visibility, &row).await);
    assert_eq!(listed_ids(&conn, &visibility).await, vec![row.id]);
}

#[tokio::test]
async fn a_row_whose_owner_reaches_the_target_is_not_withdrawable() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (owner, target) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let row = seed_assignment(&conn, &bundle, target).await;

    let visibility = AssignmentVisibility {
        reaching: vec![owner, target],
        withdrawable: vec![target],
        ..nothing_visible()
    };
    assert!(!visible_to(&conn, &visibility, &row).await);
    assert!(listed_ids(&conn, &visibility).await.is_empty());
}

#[tokio::test]
async fn a_row_at_a_tenant_that_is_not_withdrawable_is_not_visible_through_it() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (owner, target) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let row = seed_assignment(&conn, &bundle, target).await;

    let visibility = AssignmentVisibility {
        withdrawable: vec![Uuid::new_v4()],
        ..nothing_visible()
    };
    assert!(!visible_to(&conn, &visibility, &row).await);
}

#[tokio::test]
async fn without_read_or_withdrawable_tenants_nothing_is_visible() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (owner, target) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let row = seed_assignment(&conn, &bundle, target).await;

    // Even with every tenant listed, absent read and an empty withdrawable
    // set admit nothing.
    let visibility = AssignmentVisibility {
        subtree: vec![owner, target],
        reaching: vec![owner, target],
        ..nothing_visible()
    };
    assert!(!visible_to(&conn, &visibility, &row).await);
    assert!(listed_ids(&conn, &visibility).await.is_empty());
}

#[tokio::test]
async fn assignments_are_listed_a_page_at_a_time_newest_id_first() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = OrmAssignmentRepository;
    let owner = Uuid::new_v4();
    let targets = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let mut seeded = Vec::new();
    for target in targets {
        seeded.push(seed_assignment(&conn, &bundle, target).await.id);
    }
    seeded.sort_by_key(|id| std::cmp::Reverse(id.0));
    let mut visibility = administers(owner, targets[0]);
    visibility.subtree.extend(targets);
    visibility.reaching.extend(targets);

    let first = repo
        .list_page(&conn, &visibility, &ODataQuery::default().with_limit(1))
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].id, seeded[0]);
    assert!(first.page_info.next_cursor.is_some());

    let mut ids = Vec::new();
    let mut query = ODataQuery::default().with_limit(1);
    loop {
        let page = repo.list_page(&conn, &visibility, &query).await.unwrap();
        assert!(page.items.len() <= 1);
        ids.extend(page.items.iter().map(|a| a.id));
        match page.page_info.next_cursor {
            Some(cursor) => {
                query = ODataQuery::default()
                    .with_limit(1)
                    .with_cursor(toolkit_odata::CursorV1::decode(&cursor).unwrap());
            }
            None => break,
        }
    }
    assert_eq!(ids, seeded);
}

#[tokio::test]
async fn a_filter_narrows_the_listing_by_bundle_or_target_tenant() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = OrmAssignmentRepository;
    let (owner, t1, t2) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let alpha = seed_bundle(&conn, owner, "alpha").await;
    let beta = seed_bundle(&conn, owner, "beta").await;
    let a1 = seed_assignment(&conn, &alpha, t1).await;
    let a2 = seed_assignment(&conn, &alpha, t2).await;
    let b1 = seed_assignment(&conn, &beta, t1).await;
    let visibility = AssignmentVisibility {
        read: Some(scope(owner)),
        subtree: vec![owner, t1, t2],
        reaching: vec![owner, t1, t2],
        withdrawable: Vec::new(),
    };
    let ids = |page: toolkit_odata::Page<Assignment>| {
        let mut ids: Vec<_> = page.items.into_iter().map(|a| a.id).collect();
        ids.sort_by_key(|id| id.0);
        ids
    };
    let sorted = |mut v: Vec<AssignmentId>| {
        v.sort_by_key(|id| id.0);
        v
    };

    let by_bundle = repo
        .list_page(
            &conn,
            &visibility,
            &filtered(&format!("bundle_id eq {}", alpha.id.0)),
        )
        .await
        .unwrap();
    assert_eq!(ids(by_bundle), sorted(vec![a1.id, a2.id]));
    let by_tenant = repo
        .list_page(&conn, &visibility, &filtered(&format!("tenant_id eq {t1}")))
        .await
        .unwrap();
    assert_eq!(ids(by_tenant), sorted(vec![a1.id, b1.id]));
}

#[tokio::test]
async fn a_filter_never_widens_what_the_visibility_admits() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (owner, target) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    seed_assignment(&conn, &bundle, target).await;

    let page = OrmAssignmentRepository
        .list_page(
            &conn,
            &nothing_visible(),
            &filtered(&format!("tenant_id eq {target}")),
        )
        .await
        .unwrap();
    assert!(page.items.is_empty());
}

#[tokio::test]
async fn an_unknown_filter_field_is_a_query_error() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();

    let err = OrmAssignmentRepository
        .list_page(
            &conn,
            &administers(Uuid::new_v4(), Uuid::new_v4()),
            &filtered(&format!("owner_tenant_id eq {}", Uuid::new_v4())),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::Query(_)), "{err:?}");
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

    // The assigning tenant `a` is part of the chain, as an ancestor of the
    // targets would be.
    let loaded = OrmActiveContentLoader
        .load_for_tenants(&conn, &[t1, t2, other, a])
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
        assert_eq!(entry.version_id, current.id);
    }

    // Content is a separate read, of the versions asked for only.
    let content = OrmActiveContentLoader
        .load_documents(&conn, &[current.id])
        .await
        .unwrap();
    assert_eq!(content.len(), 1);
    let names: Vec<&str> = content[&current.id]
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names, ["cur1", "cur2"]);
}

#[tokio::test]
async fn loader_skips_assignments_made_from_outside_the_chain() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    // `a` assigned to `t`, then a barrier cut `a` off `t`'s chain.
    let (a, t) = (Uuid::new_v4(), Uuid::new_v4());
    let bundle = seed_bundle(&conn, a, "guard").await;
    activate(&conn, &seed_draft(&conn, &bundle, &["g"]).await).await;
    OrmAssignmentRepository
        .insert(&conn, &scope(a), &assignment_of(&bundle, t))
        .await
        .unwrap();
    assert_eq!(
        OrmActiveContentLoader
            .load_for_tenants(&conn, &[t, a])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        OrmActiveContentLoader
            .load_for_tenants(&conn, &[t])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_scope_naming_the_version_sees_and_writes_its_whole_content() {
    // A PDP grant constrained to one version's identity: the version is
    // visible, and so is every document of it, though no document id is in
    // the grant.
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let a = Uuid::new_v4();
    let repo = OrmVersionRepository;
    let bundle = seed_bundle(&conn, a, "b").await;
    let version = seed_draft(&conn, &bundle, &["x", "y"]).await;
    let only_version = AccessScope::for_resource(version.id.0);

    let read = repo
        .get_with_content(&conn, &only_version, version.id)
        .await
        .unwrap()
        .unwrap();
    let names: Vec<&str> = read.documents.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["x", "y"]);

    let mut seeded: Vec<_> = version.documents.iter().map(|d| d.id).collect();
    seeded.sort_unstable();
    assert_eq!(
        repo.lock_draft(&conn, &only_version, version.id)
            .await
            .unwrap(),
        seeded
    );

    let replaced = repo
        .replace_draft_content(&conn, &only_version, version.id, &[document_named("z")])
        .await
        .unwrap();
    let stored = repo
        .get_with_content(&conn, &scope(a), version.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.documents, replaced.documents);
    assert_eq!(stored.documents.len(), 1);

    // Another tenant's version stays out of reach whatever it names.
    let other = seed_draft(
        &conn,
        &seed_bundle(&conn, Uuid::new_v4(), "o").await,
        &["o"],
    )
    .await;
    assert_eq!(
        repo.get_with_content(&conn, &only_version, other.id)
            .await
            .unwrap(),
        None
    );
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

#[tokio::test]
async fn a_negative_stored_ordinal_fails_the_read() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let a = Uuid::new_v4();
    let bundle = seed_bundle(&conn, a, "b").await;
    let version = seed_draft(&conn, &bundle, &[]).await;
    // The entity cannot express a negative ordinal; a raw column write can.
    bundle_version::Entity::update_many()
        .col_expr(bundle_version::Column::Ordinal, Expr::value(-1))
        .filter(bundle_version::Column::Id.eq(version.id.0))
        .secure()
        .scope_with(&all())
        .exec(&conn)
        .await
        .unwrap();
    let read = OrmVersionRepository
        .get_with_content(&conn, &scope(a), version.id)
        .await;
    assert!(matches!(read, Err(RepoError::Database(_))), "{read:?}");
}

#[tokio::test]
async fn locking_a_draft_reports_its_current_documents_and_refuses_a_non_draft() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let a = Uuid::new_v4();
    let repo = OrmVersionRepository;
    let bundle = seed_bundle(&conn, a, "b").await;
    let version = seed_draft(&conn, &bundle, &["x", "y"]).await;

    let mut seeded: Vec<_> = version.documents.iter().map(|d| d.id).collect();
    seeded.sort_unstable();
    assert_eq!(
        repo.lock_draft(&conn, &scope(a), version.id).await.unwrap(),
        seeded
    );

    // A replace gives the documents fresh ids: activation can tell the
    // content it validated from the content it would activate.
    repo.replace_draft_content(&conn, &scope(a), version.id, &[document_named("x")])
        .await
        .unwrap();
    assert_ne!(
        repo.lock_draft(&conn, &scope(a), version.id).await.unwrap(),
        seeded
    );

    activate(&conn, &version).await;
    for result in [
        repo.lock_draft(&conn, &scope(a), version.id)
            .await
            .map(drop),
        repo.replace_draft_content(&conn, &scope(a), version.id, &[])
            .await
            .map(drop),
    ] {
        assert!(
            matches!(result, Err(RepoError::Conflict { reason }) if reason == conflict::VERSION_NOT_DRAFT),
            "{result:?}"
        );
    }
    assert!(matches!(
        repo.lock_draft(&conn, &scope(a), VersionId(Uuid::new_v4()))
            .await,
        Err(RepoError::NotFound)
    ));
}

#[tokio::test]
async fn bundles_are_found_by_name_within_the_scope() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let repo = bundles();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let alpha = seed_bundle(&conn, a, "alpha").await;
    seed_bundle(&conn, a, "alphabet").await;
    seed_bundle(&conn, b, "alpha").await;

    let exact = repo
        .list_page(&conn, &scope(a), &filtered("name eq 'alpha'"))
        .await
        .unwrap();
    assert_eq!(
        exact.items,
        vec![alpha],
        "another tenant's namesake is not seen"
    );

    let mut prefixed: Vec<String> = repo
        .list_page(&conn, &scope(a), &filtered("startswith(name, 'alpha')"))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|x| x.name)
        .collect();
    prefixed.sort();
    assert_eq!(prefixed, ["alpha", "alphabet"]);

    let none = repo
        .list_page(&conn, &scope(a), &filtered("name eq 'gamma'"))
        .await
        .unwrap();
    assert!(none.items.is_empty());
}

/// The error a listing under `visibility` refuses with.
async fn refused(conn: &DbConn<'_>, visibility: AssignmentVisibility) -> RepoError {
    OrmAssignmentRepository
        .list_page(conn, &visibility, &ODataQuery::default())
        .await
        .unwrap_err()
}

#[tokio::test]
async fn a_listing_whose_visibility_exceeds_the_bind_budget_is_refused() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let owner = Uuid::new_v4();
    let bundle = seed_bundle(&conn, owner, "alpha").await;
    let assignment = seed_assignment(&conn, &bundle, owner).await;
    let tenants = |n: usize| -> Vec<Uuid> {
        let mut tenants: Vec<Uuid> = (0..n).map(|_| Uuid::new_v4()).collect();
        tenants.push(owner);
        tenants
    };
    let large = |n: usize| AssignmentVisibility {
        read: Some(scope(owner)),
        subtree: tenants(n),
        reaching: tenants(n),
        withdrawable: Vec::new(),
    };

    // Within the budget, however large, the listing runs.
    assert_eq!(listed_ids(&conn, &large(10_000)).await, [assignment.id]);
    // Beyond it, it is refused before reaching the database.
    let err = refused(&conn, large(20_000)).await;
    assert!(matches!(err, RepoError::TooLarge), "{err:?}");

    // The withdrawable alternative counts too, alone ...
    let withdrawable_only = AssignmentVisibility {
        read: None,
        subtree: Vec::new(),
        reaching: tenants(20_000),
        withdrawable: tenants(20_000),
    };
    let err = refused(&conn, withdrawable_only).await;
    assert!(matches!(err, RepoError::TooLarge), "{err:?}");
    // ... and together with the administered one, where neither alone would
    // exceed the budget.
    let both = AssignmentVisibility {
        read: Some(scope(owner)),
        subtree: tenants(8_000),
        reaching: tenants(8_000),
        withdrawable: tenants(8_000),
    };
    let err = refused(&conn, both).await;
    assert!(matches!(err, RepoError::TooLarge), "{err:?}");
}

/// A first page of one row, ordered by `field` (`id` breaks ties, newest
/// first).
fn ordered_by(field: &str, dir: SortDir) -> ODataQuery {
    ODataQuery::default()
        .with_order(ODataOrderBy(vec![OrderKey {
            field: field.to_owned(),
            dir,
        }]))
        .with_limit(1)
}

/// The page after one whose next cursor is `cursor`.
fn after(cursor: &str) -> ODataQuery {
    ODataQuery::default()
        .with_limit(1)
        .with_cursor(CursorV1::decode(cursor).unwrap())
}

/// `rows` ordered by `key`, ties broken by identity, newest first - the
/// order the listings promise.
fn sorted_by<T, K: Ord>(
    mut rows: Vec<T>,
    key: impl Fn(&T) -> K,
    id: impl Fn(&T) -> Uuid,
) -> Vec<T> {
    rows.sort_by(|a, b| key(a).cmp(&key(b)).then_with(|| id(b).cmp(&id(a))));
    rows
}

/// Every bundle a listing pages through from `first`, one row per page; a
/// cursor that does not advance stops after `rows` pages instead of
/// looping.
async fn bundle_pages(conn: &DbConn<'_>, first: ODataQuery, rows: usize) -> Vec<BundleId> {
    let repo = bundles();
    let mut seen = Vec::new();
    let mut page = repo.list_page(conn, &all(), &first).await.unwrap();
    for _ in 0..=rows {
        seen.extend(page.items.iter().map(|x| x.id));
        let Some(cursor) = page.page_info.next_cursor else {
            break;
        };
        page = repo.list_page(conn, &all(), &after(&cursor)).await.unwrap();
    }
    seen
}

/// [`bundle_pages`] for the assignments `visibility` admits.
async fn assignment_pages(
    conn: &DbConn<'_>,
    visibility: &AssignmentVisibility,
    first: ODataQuery,
    rows: usize,
) -> Vec<AssignmentId> {
    let repo = OrmAssignmentRepository;
    let mut seen = Vec::new();
    let mut page = repo.list_page(conn, visibility, &first).await.unwrap();
    for _ in 0..=rows {
        seen.extend(page.items.iter().map(|x| x.id));
        let Some(cursor) = page.page_info.next_cursor else {
            break;
        };
        page = repo
            .list_page(conn, visibility, &after(&cursor))
            .await
            .unwrap();
    }
    seen
}

#[tokio::test]
async fn bundles_page_in_name_and_owner_order_across_equal_values() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let mut seeded = Vec::new();
    for (tenant, name) in [
        (a, "alpha"),
        (b, "alpha"),
        (a, "beta"),
        (b, "beta"),
        (a, "gamma"),
    ] {
        seeded.push(seed_bundle(&conn, tenant, name).await);
    }
    let n = seeded.len();
    let ids = |rows: Vec<Bundle>| rows.into_iter().map(|x| x.id).collect::<Vec<_>>();

    assert_eq!(
        bundle_pages(&conn, ordered_by("name", SortDir::Asc), n).await,
        ids(sorted_by(seeded.clone(), |x| x.name.clone(), |x| x.id.0)),
        "by name, ascending"
    );
    assert_eq!(
        bundle_pages(&conn, ordered_by("name", SortDir::Desc), n).await,
        ids(sorted_by(
            seeded.clone(),
            |x| std::cmp::Reverse(x.name.clone()),
            |x| x.id.0
        )),
        "by name, descending; the tiebreaker stays newest first"
    );
    assert_eq!(
        bundle_pages(&conn, ordered_by("owner_tenant_id", SortDir::Asc), n).await,
        ids(sorted_by(seeded, |x| x.owner_tenant_id, |x| x.id.0)),
        "by owning tenant"
    );
}

#[tokio::test]
async fn assignments_page_in_bundle_and_target_order_across_equal_values() {
    let db = migrated_db().await;
    let conn = db.conn().unwrap();
    let owner = Uuid::new_v4();
    let (first, second) = (
        seed_bundle(&conn, owner, "first").await,
        seed_bundle(&conn, owner, "second").await,
    );
    let (t1, t2) = (Uuid::new_v4(), Uuid::new_v4());
    let mut seeded = Vec::new();
    for (bundle, tenant) in [(&first, t1), (&first, t2), (&second, t1), (&second, t2)] {
        seeded.push(seed_assignment(&conn, bundle, tenant).await);
    }
    let everything = AssignmentVisibility {
        read: Some(all()),
        subtree: vec![t1, t2],
        reaching: vec![owner],
        withdrawable: Vec::new(),
    };
    let n = seeded.len();
    let ids = |rows: Vec<Assignment>| rows.into_iter().map(|x| x.id).collect::<Vec<_>>();

    assert_eq!(
        assignment_pages(&conn, &everything, ordered_by("bundle_id", SortDir::Asc), n).await,
        ids(sorted_by(seeded.clone(), |x| x.bundle_id.0, |x| x.id.0)),
        "by bundle"
    );
    assert_eq!(
        assignment_pages(&conn, &everything, ordered_by("tenant_id", SortDir::Asc), n).await,
        ids(sorted_by(seeded, |x| x.tenant_id, |x| x.id.0)),
        "by target tenant"
    );
}
