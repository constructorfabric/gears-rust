//! Audit-trail integration tests: one audit row per write, none on a rolled-back mutation.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use bytes::Bytes;
use uuid::Uuid;

use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::service::FileService;
use file_storage::infra::storage::Store;
use file_storage_sdk::{CustomMetadataPatch, NewFile};

mod common;
use common::{Stack, StackBuilder, ctx};

async fn build_service() -> (
    Arc<FileService>,
    Arc<MultipartService>,
    DataPlaneService,
    Store,
) {
    let s = StackBuilder::new("audit-test").build().await;
    (s.svc, s.msvc, s.dp, s.store)
}

fn new_file() -> NewFile {
    common::new_file_of("audit.txt", "text/plain")
}

/// Every write leaves exactly one `success` audit row for its file, and the outbox gets the
/// file's lifecycle events (unpublished): create, upload, bind, metadata patch, a second version
/// bound over the first, `delete_version`, `delete_file`. Audit and event rows survive the delete
/// (no FK cascade).
#[tokio::test]
async fn writes_leave_audit_rows_and_outbox_events() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();

    let ticket = svc
        .create_file(
            &ctx,
            common::new_file_for(owner, "audit.txt", "text/plain"),
            None,
            false,
        )
        .await
        .unwrap();
    let file_id = ticket.file_id;
    dp.put_content(
        &ctx,
        file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"v1"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, file_id, ticket.version_id, None)
        .await
        .unwrap();
    svc.update_metadata(
        &ctx,
        file_id,
        CustomMetadataPatch {
            entries: vec![("k".to_owned(), Some("v".to_owned()))],
        },
        None,
    )
    .await
    .unwrap();

    let v2 = svc.presign_version(&ctx, file_id).await.unwrap();
    dp.put_content(
        &ctx,
        file_id,
        v2.version_id,
        "text/plain",
        Bytes::from_static(b"v2"),
    )
    .await
    .unwrap();
    let cur = svc.get_file(&ctx, file_id).await.unwrap();
    svc.bind(
        &ctx,
        file_id,
        v2.version_id,
        file_storage::domain::etag::etag_for(&cur).as_deref(),
    )
    .await
    .unwrap();
    svc.delete_version(&ctx, file_id, ticket.version_id)
        .await
        .unwrap();
    let cur = svc.get_file(&ctx, file_id).await.unwrap();
    svc.delete_file(
        &ctx,
        file_id,
        file_storage::domain::etag::etag_for(&cur).as_deref(),
    )
    .await
    .unwrap();

    let rows = store.list_audit(file_id).await.unwrap();
    assert!(
        rows.iter()
            .all(|r| r.outcome == "success" && r.file_id == Some(file_id)),
        "every audit row is a success for this file: {rows:?}"
    );
    let mut operations: Vec<&str> = rows.iter().map(|r| r.operation.as_str()).collect();
    operations.sort_unstable();
    assert_eq!(
        operations,
        [
            "create",
            "delete_file",
            "delete_version",
            "finalize_version",
            "finalize_version",
            "patch_content",
            "patch_content",
            "patch_metadata",
        ]
    );

    let events = store.list_file_events(file_id).await.unwrap();
    assert!(
        events
            .iter()
            .all(|e| e.file_id == file_id && e.owner_id == owner && e.published_at.is_none()),
        "every event is for this file and owner, still unpublished: {events:?}"
    );
    let mut types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    types.sort_unstable();
    assert_eq!(
        types
            .iter()
            .filter(|t| matches!(
                **t,
                "file.created" | "file.content_updated" | "file.deleted"
            ))
            .copied()
            .collect::<Vec<_>>(),
        [
            "file.content_updated",
            "file.content_updated",
            "file.created",
            "file.deleted"
        ],
        "all events: {types:?}"
    );
}

/// A random `version_id` on a single-version file must 404, not delete the file.
#[tokio::test]
async fn delete_version_single_version_file_wrong_id_returns_not_found() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let t1 = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        t1.file_id,
        t1.version_id,
        "text/plain",
        Bytes::from_static(b"v1"),
    )
    .await
    .unwrap();

    let wrong_version_id = Uuid::now_v7();
    let err = svc
        .delete_version(&ctx, t1.file_id, wrong_version_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::VersionNotFound { .. }),
        "expected VersionNotFound, got {err:?}"
    );

    let file = store
        .get_file(&toolkit_security::AccessScope::allow_all(), t1.file_id)
        .await
        .unwrap();
    assert!(file.is_some(), "files row must still exist");
    let version = store.get_version(t1.file_id, t1.version_id).await.unwrap();
    assert!(version.is_some(), "file_versions row must still exist");
}

/// Deleting the only version by its real id still deletes the whole file.
#[tokio::test]
async fn delete_version_single_version_file_matching_id_deletes_whole_file() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let t1 = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        t1.file_id,
        t1.version_id,
        "text/plain",
        Bytes::from_static(b"v1"),
    )
    .await
    .unwrap();

    svc.delete_version(&ctx, t1.file_id, t1.version_id)
        .await
        .unwrap();

    let file = store
        .get_file(&toolkit_security::AccessScope::allow_all(), t1.file_id)
        .await
        .unwrap();
    assert!(file.is_none(), "files row must be gone");
}

#[tokio::test]
async fn multipart_complete_leaves_audit_rows() {
    let s = StackBuilder::new("audit-test").build().await;
    let Stack {
        backend,
        msvc,
        multipart_store,
        store,
        svc,
        ..
    } = s;

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let part_data = Bytes::from_static(b"part1");
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            part_data.len() as u64,
            None,
            false,
        )
        .await
        .unwrap();
    common::put_part(
        &multipart_store,
        &backend,
        &plan,
        ticket.file_id,
        1,
        part_data,
    )
    .await;

    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap();

    let rows = store.list_audit(ticket.file_id).await.unwrap();
    let complete_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.operation == "multipart_complete")
        .collect();
    assert_eq!(
        complete_rows.len(),
        1,
        "expected exactly 1 multipart_complete audit row"
    );
    assert_eq!(complete_rows[0].outcome, "success");

    assert_eq!(
        rows.iter()
            .filter(|r| r.operation == "finalize_version")
            .count(),
        1,
        "expected exactly 1 finalize_version audit row from multipart complete"
    );

    svc.bind(&ctx, ticket.file_id, plan.version_id, None)
        .await
        .unwrap();
    let rows2 = store.list_audit(ticket.file_id).await.unwrap();
    assert_eq!(
        rows2
            .iter()
            .filter(|r| r.operation == "patch_content")
            .count(),
        1,
        "expected exactly 1 patch_content row"
    );
}

/// A stale `expected_meta_version` rolls back the `meta_version` bump and the audit row together.
#[tokio::test]
async fn failed_metadata_cas_leaves_no_audit_row() {
    let (svc, _msvc, _dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let rows_before = store.list_audit(ticket.file_id).await.unwrap();
    assert_eq!(rows_before.len(), 1);

    let patch = CustomMetadataPatch {
        entries: vec![("x".to_owned(), Some("y".to_owned()))],
    };
    let err = svc
        .update_metadata(&ctx, ticket.file_id, patch, Some(99))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "expected PreconditionFailed, got {err:?}"
    );

    let rows_after = store.list_audit(ticket.file_id).await.unwrap();
    assert_eq!(
        rows_after.len(),
        rows_before.len(),
        "no new audit row should appear when the CAS rolls back"
    );
}
