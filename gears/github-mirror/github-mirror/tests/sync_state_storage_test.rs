#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use github_mirror::domain::repo::{
    EntityFingerprintRecord, EntityFingerprintRepository, SessionStatus, SyncSessionRecord,
    SyncSessionRepository, SyncWatermarkRecord, SyncWatermarkRepository,
};
use github_mirror::infra::storage::sea_orm_repo::{
    SeaOrmEntityFingerprintRepository, SeaOrmSyncSessionRepository, SeaOrmSyncWatermarkRepository,
};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

/// The sync engine is not wired into the service yet (slices 2–3 of the
/// #4632 plan), so these tests exercise the storage layer directly with a
/// scope built the same way the service builds one.
const SYNC_STATE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.sync_state",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

async fn scope_for(ctx: &SecurityContext) -> AccessScope {
    common::enforcer()
        .access_scope_with(
            ctx,
            &SYNC_STATE_RESOURCE,
            "upsert",
            None,
            &AccessRequest::new()
                .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
        )
        .await
        .expect("scope must resolve")
}

fn session_record(id: Uuid, status: SessionStatus, created_at: &str) -> SyncSessionRecord {
    SyncSessionRecord {
        id,
        repo_full_name: "acme/widget".to_owned(),
        repo_id: None,
        status,
        progress_percent: 0,
        error: None,
        summary_json: None,
        created_at: created_at.to_owned(),
        started_at: None,
        ended_at: None,
        updated_at: None,
    }
}

#[tokio::test]
async fn a_session_is_created_updated_and_listed_newest_first() {
    let db = common::inmem_db().await;
    let provider = Arc::new(DBProvider::<DbError>::new(db));
    let ctx = common::caller_in(Uuid::new_v4());
    let tenant = ctx.subject_tenant_id();
    let scope = scope_for(&ctx).await;
    let repo = SeaOrmSyncSessionRepository::new(Arc::clone(&provider));

    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    repo.upsert(
        &scope,
        tenant,
        session_record(first, SessionStatus::Queued, "2026-08-25T10:00:00Z"),
    )
    .await
    .expect("first insert");
    repo.upsert(
        &scope,
        tenant,
        session_record(second, SessionStatus::Queued, "2026-08-25T11:00:00Z"),
    )
    .await
    .expect("second insert");

    let mut finished = session_record(first, SessionStatus::Complete, "2026-08-25T10:00:00Z");
    finished.started_at = Some("2026-08-25T10:00:01Z".to_owned());
    finished.ended_at = Some("2026-08-25T10:00:30Z".to_owned());
    finished.progress_percent = 100;
    finished.summary_json = Some(r#"{"issues_synced":2}"#.to_owned());
    repo.upsert(&scope, tenant, finished)
        .await
        .expect("state transition upsert");

    let loaded = repo
        .find_by_id(&scope, first)
        .await
        .expect("find must succeed")
        .expect("session must exist");
    assert_eq!(loaded.status, SessionStatus::Complete);
    assert_eq!(loaded.progress_percent, 100);
    assert_eq!(
        loaded.summary_json.as_deref(),
        Some(r#"{"issues_synced":2}"#)
    );
    assert_eq!(loaded.ended_at.as_deref(), Some("2026-08-25T10:00:30Z"));

    let recent = repo
        .list_recent(&scope, None, 10)
        .await
        .expect("list must succeed");
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].id, second, "newest created_at must come first");
    assert_eq!(recent[1].id, first);
}

#[tokio::test]
async fn sessions_are_tenant_scoped() {
    let db = common::inmem_db().await;
    let provider = Arc::new(DBProvider::<DbError>::new(db));
    let repo = SeaOrmSyncSessionRepository::new(Arc::clone(&provider));

    let owner = common::caller_in(Uuid::new_v4());
    let owner_scope = scope_for(&owner).await;
    let id = Uuid::new_v4();
    repo.upsert(
        &owner_scope,
        owner.subject_tenant_id(),
        session_record(id, SessionStatus::InProgress, "2026-08-25T10:00:00Z"),
    )
    .await
    .expect("owner insert");

    let stranger = common::caller_in(Uuid::new_v4());
    let stranger_scope = scope_for(&stranger).await;
    let seen = repo
        .find_by_id(&stranger_scope, id)
        .await
        .expect("find must succeed");
    assert!(
        seen.is_none(),
        "another tenant must not see the session at all"
    );
    assert!(
        repo.list_recent(&stranger_scope, None, 10)
            .await
            .expect("list must succeed")
            .is_empty()
    );
}

#[tokio::test]
async fn a_watermark_is_keyed_by_repo_and_family_and_promotes_its_candidate() {
    let db = common::inmem_db().await;
    let provider = Arc::new(DBProvider::<DbError>::new(db));
    let ctx = common::caller_in(Uuid::new_v4());
    let tenant = ctx.subject_tenant_id();
    let scope = scope_for(&ctx).await;
    let repo = SeaOrmSyncWatermarkRepository::new(Arc::clone(&provider));

    let sweeping = SyncWatermarkRecord {
        repo_id: 42,
        family: "issues".to_owned(),
        last_seen_updated_at: None,
        page1_etag: None,
        sweep_in_progress: true,
        candidate_high_water: Some("2026-08-25T09:00:00Z".to_owned()),
    };
    repo.upsert(&scope, tenant, sweeping)
        .await
        .expect("first upsert");

    let promoted = SyncWatermarkRecord {
        repo_id: 42,
        family: "issues".to_owned(),
        last_seen_updated_at: Some("2026-08-25T09:00:00Z".to_owned()),
        page1_etag: Some("W/\"abc\"".to_owned()),
        sweep_in_progress: false,
        candidate_high_water: None,
    };
    repo.upsert(&scope, tenant, promoted)
        .await
        .expect("promotion upsert");

    let loaded = repo
        .find(&scope, 42, "issues")
        .await
        .expect("find must succeed")
        .expect("watermark must exist");
    assert!(!loaded.sweep_in_progress);
    assert_eq!(
        loaded.last_seen_updated_at.as_deref(),
        Some("2026-08-25T09:00:00Z")
    );
    assert!(loaded.candidate_high_water.is_none());

    assert!(
        repo.find(&scope, 42, "pull_requests")
            .await
            .expect("find must succeed")
            .is_none(),
        "a different family must have its own row"
    );
}

#[tokio::test]
async fn a_fingerprint_upsert_is_idempotent_and_tracks_refinement() {
    let db = common::inmem_db().await;
    let provider = Arc::new(DBProvider::<DbError>::new(db));
    let ctx = common::caller_in(Uuid::new_v4());
    let tenant = ctx.subject_tenant_id();
    let scope = scope_for(&ctx).await;
    let repo = SeaOrmEntityFingerprintRepository::new(Arc::clone(&provider));

    let pending = EntityFingerprintRecord {
        repo_id: 42,
        family: "issues".to_owned(),
        entity_id: "11".to_owned(),
        fingerprint: "00aa".to_owned(),
        updated_at: Some("2026-08-25T09:00:00Z".to_owned()),
        node_id: None,
        child_counts_hash: None,
        last_refined_at: None,
        refinement_status: "pending".to_owned(),
    };
    repo.upsert(&scope, tenant, pending.clone())
        .await
        .expect("first upsert");

    let mut complete = pending;
    complete.fingerprint = "00ab".to_owned();
    complete.refinement_status = "complete".to_owned();
    complete.last_refined_at = Some("2026-08-25T09:05:00Z".to_owned());
    repo.upsert(&scope, tenant, complete)
        .await
        .expect("second upsert");

    let loaded = repo
        .find(&scope, 42, "issues", "11")
        .await
        .expect("find must succeed")
        .expect("fingerprint must exist");
    assert_eq!(loaded.fingerprint, "00ab");
    assert_eq!(loaded.refinement_status, "complete");
    assert_eq!(
        loaded.last_refined_at.as_deref(),
        Some("2026-08-25T09:05:00Z")
    );
}

#[tokio::test]
async fn a_heartbeat_writes_progress_and_nothing_else() {
    let db = common::inmem_db().await;
    let provider = Arc::new(DBProvider::<DbError>::new(db));
    let ctx = common::caller_in(Uuid::new_v4());
    let tenant = ctx.subject_tenant_id();
    let scope = scope_for(&ctx).await;
    let repo = SeaOrmSyncSessionRepository::new(Arc::clone(&provider));

    let id = Uuid::new_v4();
    let mut running = session_record(id, SessionStatus::InProgress, "2026-08-25T10:00:00Z");
    running.repo_id = Some(42);
    running.started_at = Some("2026-08-25T10:00:01Z".to_owned());
    running.error = Some("a warning from the run".to_owned());
    running.summary_json = Some(r#"{"issues_synced":2}"#.to_owned());
    running.updated_at = Some("2026-08-25T10:00:01Z".to_owned());
    repo.upsert(&scope, tenant, running.clone())
        .await
        .expect("the running session must insert");

    repo.record_heartbeat(&scope, id, 40, "2026-08-25T10:00:20Z")
        .await
        .expect("the heartbeat must write");

    let loaded = repo
        .find_by_id(&scope, id)
        .await
        .expect("find must succeed")
        .expect("session must exist");

    assert_eq!(loaded.progress_percent, 40);
    assert_eq!(loaded.updated_at.as_deref(), Some("2026-08-25T10:00:20Z"));
    assert_eq!(
        (
            loaded.status,
            loaded.repo_id,
            loaded.error.as_deref(),
            loaded.summary_json.as_deref(),
            loaded.created_at.as_str(),
            loaded.started_at.as_deref(),
            loaded.ended_at.as_deref()
        ),
        (
            running.status,
            running.repo_id,
            running.error.as_deref(),
            running.summary_json.as_deref(),
            running.created_at.as_str(),
            running.started_at.as_deref(),
            running.ended_at.as_deref()
        ),
        "a tick carries a stale copy of the row, so it must write the two \
         columns it owns and leave the worker's writes alone"
    );
}

#[tokio::test]
async fn a_heartbeat_for_a_session_that_is_not_there_is_an_error() {
    let db = common::inmem_db().await;
    let provider = Arc::new(DBProvider::<DbError>::new(db));
    let ctx = common::caller_in(Uuid::new_v4());
    let tenant = ctx.subject_tenant_id();
    let scope = scope_for(&ctx).await;
    let repo = SeaOrmSyncSessionRepository::new(Arc::clone(&provider));

    let real = Uuid::new_v4();
    repo.upsert(
        &scope,
        tenant,
        session_record(real, SessionStatus::InProgress, "2026-08-25T10:00:00Z"),
    )
    .await
    .expect("the real session must insert");

    let outcome = repo
        .record_heartbeat(&scope, Uuid::new_v4(), 40, "2026-08-25T10:00:20Z")
        .await;

    assert!(
        outcome.is_err(),
        "a heartbeat that matched no row must say so: the liveness check reads \
         a row that stopped moving as a dead process"
    );

    let untouched = repo
        .find_by_id(&scope, real)
        .await
        .expect("find must succeed")
        .expect("the real session must still be there");
    assert_eq!(untouched.progress_percent, 0);
    assert_eq!(untouched.updated_at, None);
}

/// Three sessions queued in the same second. `created_at` alone cannot order
/// them, so the id is the tie-break: without it the second page's filter
/// would ask for rows strictly older than a timestamp two rows still carry,
/// and those rows would never be served.
#[tokio::test]
async fn sessions_sharing_a_created_at_page_without_repeating_or_skipping() {
    let db = common::inmem_db().await;
    let provider = Arc::new(DBProvider::<DbError>::new(db));
    let ctx = common::caller_in(Uuid::new_v4());
    let tenant = ctx.subject_tenant_id();
    let scope = scope_for(&ctx).await;
    let repo = SeaOrmSyncSessionRepository::new(Arc::clone(&provider));

    let same_instant = "2026-08-25T10:00:00Z";
    let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
    for id in &ids {
        repo.upsert(
            &scope,
            tenant,
            session_record(*id, SessionStatus::Queued, same_instant),
        )
        .await
        .expect("each session must insert");
    }

    let first = repo
        .list_recent(&scope, None, 2)
        .await
        .expect("the first page must list");
    assert_eq!(first.len(), 2);

    let last = first.last().expect("the first page has rows");
    let second = repo
        .list_recent(&scope, Some((last.created_at.as_str(), last.id)), 2)
        .await
        .expect("the second page must list");
    assert_eq!(
        second.len(),
        1,
        "the third session shares the timestamp the cursor carries, so only \
         the id can tell the reader it has not been served yet"
    );

    let mut served: Vec<Uuid> = first.iter().chain(second.iter()).map(|s| s.id).collect();
    let mut expected = ids.clone();
    served.sort_unstable();
    expected.sort_unstable();
    assert_eq!(
        served, expected,
        "every session must appear exactly once across the two pages"
    );

    assert!(
        first[0].id > first[1].id && first[1].id > second[0].id,
        "within one timestamp the order is by id, newest first"
    );
}
