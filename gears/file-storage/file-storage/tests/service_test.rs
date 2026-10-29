//! `FileService` end to end: upload/bind/download lifecycle, versions, listing, deletes, ownership
//! transfer and the audit trail / outbox rows of every write. Temp-file `SQLite`, in-memory backend,
//! tenant-only authorizer.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation};
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::etag;
use file_storage::domain::service::FileService;
use file_storage::infra::signed_url::Claims;
use file_storage_sdk::{CustomMetadataEntry, CustomMetadataPatch, NewFile, OwnerKind};

mod common;
use common::{Stack, StackBuilder, ctx, token_of};

fn new_file() -> NewFile {
    NewFile {
        custom_metadata: vec![CustomMetadataEntry {
            key: "tag".to_owned(),
            value: "a".to_owned(),
        }],
        ..common::new_file_of("doc.txt", "text/plain")
    }
}

/// A file with `v1` bound and `v2` uploaded but not bound: `(file_id, v1, v2)`.
async fn file_with_two_versions(
    svc: &FileService,
    dp: &DataPlaneService,
    ctx: &SecurityContext,
) -> (Uuid, Uuid, Uuid) {
    let t1 = svc.create_file(ctx, new_file(), None, false).await.unwrap();
    dp.put_content(
        ctx,
        t1.file_id,
        t1.version_id,
        "text/plain",
        Bytes::from_static(b"v1"),
    )
    .await
    .unwrap();
    svc.bind(ctx, t1.file_id, t1.version_id, None)
        .await
        .unwrap();

    let t2 = svc.presign_version(ctx, t1.file_id).await.unwrap();
    dp.put_content(
        ctx,
        t1.file_id,
        t2.version_id,
        "text/plain",
        Bytes::from_static(b"v2"),
    )
    .await
    .unwrap();
    (t1.file_id, t1.version_id, t2.version_id)
}

/// Binds `version_id` with the file's current `ETag` as `If-Match`.
async fn bind_with_current_etag(
    svc: &FileService,
    ctx: &SecurityContext,
    file_id: Uuid,
    version_id: Uuid,
) {
    let cur = svc.get_file(ctx, file_id).await.unwrap();
    svc.bind(ctx, file_id, version_id, etag::etag_for(&cur).as_deref())
        .await
        .unwrap();
}

#[tokio::test]
async fn full_upload_bind_download_lifecycle() {
    let Stack { svc, dp, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    assert!(ticket.upload_url.contains("fs-token="), "signed upload URL");
    assert!(ticket.upload_url.starts_with("http://sidecar.test"));
    let pre = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    assert!(pre.content_id.is_none());
    assert_eq!(etag::etag_for(&pre), None);

    let content = Bytes::from_static(b"hello world");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    let bound = svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();
    assert_eq!(bound.content_id, Some(ticket.version_id));
    let etag = etag::etag_for(&bound).expect("etag after bind");
    assert!(etag.starts_with('"') && etag.ends_with('"'));

    let dl = svc.download_url(&ctx, ticket.file_id, None).await.unwrap();
    assert_eq!(dl.version_id, ticket.version_id);
    assert_eq!(dl.etag, etag);
    // The `get` token carries the version's stored MIME and the same `ETag`.
    let payload = token_of(&dl.download_url)
        .split('.')
        .next()
        .expect("payload segment");
    let claims: Claims =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).expect("base64url")).unwrap();
    assert_eq!(claims.content_type, "text/plain");
    assert_eq!(claims.etag, dl.etag);

    let read = dp
        .read_content(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();
    assert_eq!(read, content);
    let versions = svc
        .list_versions(&ctx, ticket.file_id, None, None)
        .await
        .unwrap()
        .items;
    assert_eq!(versions.len(), 1);
    assert!(versions[0].is_current);
    assert_eq!(versions[0].size, 11);
}

#[tokio::test]
async fn bind_with_wrong_if_match_returns_precondition_failed() {
    let Stack { svc, dp, store, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let (file_id, _v1, v2) = file_with_two_versions(&svc, &dp, &ctx).await;
    let audit_before = store.list_audit(file_id).await.unwrap().len();

    let err = svc
        .bind(&ctx, file_id, v2, Some("\"deadbeef\""))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "got {err:?}"
    );
    let audit_after = store.list_audit(file_id).await.unwrap().len();
    assert_eq!(
        audit_after, audit_before,
        "a rejected bind must not write an audit row"
    );

    bind_with_current_etag(&svc, &ctx, file_id, v2).await;
    assert_eq!(
        svc.get_file(&ctx, file_id).await.unwrap().content_id,
        Some(v2)
    );
}

#[tokio::test]
async fn tenant_isolation_hides_other_tenants_files() {
    let Stack { svc, .. } = StackBuilder::new("service-test").build().await;
    let (ctx_a, ctx_b) = (ctx(Uuid::now_v7()), ctx(Uuid::now_v7()));
    let t = svc
        .create_file(&ctx_a, new_file(), None, false)
        .await
        .unwrap();
    let err = svc.get_file(&ctx_b, t.file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { .. }),
        "got {err:?}"
    );
    assert!(svc.get_file(&ctx_a, t.file_id).await.is_ok());
}

#[tokio::test]
async fn update_metadata_merges_and_bumps_meta_version() {
    let Stack { svc, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let set = |k: &str, v: Option<&str>| CustomMetadataPatch {
        entries: vec![(k.to_owned(), v.map(str::to_owned))],
    };
    let patch = CustomMetadataPatch {
        entries: vec![
            ("tag".to_owned(), Some("b".to_owned())),     // overwrite
            ("color".to_owned(), Some("red".to_owned())), // add
        ],
    };
    let updated = svc
        .update_metadata(&ctx, t.file_id, patch, None)
        .await
        .unwrap();
    assert_eq!(updated.meta_version, 1);
    let (_f, meta) = svc.get_file_with_metadata(&ctx, t.file_id).await.unwrap();
    let map: std::collections::BTreeMap<_, _> =
        meta.into_iter().map(|e| (e.key, e.value)).collect();
    assert_eq!(map.get("tag").map(String::as_str), Some("b"));
    assert_eq!(map.get("color").map(String::as_str), Some("red"));

    svc.update_metadata(&ctx, t.file_id, set("color", None), None)
        .await
        .unwrap();
    let (_f, meta) = svc.get_file_with_metadata(&ctx, t.file_id).await.unwrap();
    assert!(meta.iter().all(|e| e.key != "color"), "color removed");
}

#[tokio::test]
async fn restore_prior_version_rebinds_pointer() {
    let Stack { svc, dp, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let (file_id, v1, v2) = file_with_two_versions(&svc, &dp, &ctx).await;
    bind_with_current_etag(&svc, &ctx, file_id, v2).await;
    let restored = svc.restore_version(&ctx, file_id, v1).await.unwrap();
    assert_eq!(restored.content_id, Some(v1));
}

#[tokio::test]
async fn download_url_pending_version_is_rejected() {
    let Stack { svc, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let err = svc
        .download_url(&ctx, t.file_id, Some(t.version_id))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Conflict { .. }), "got {err:?}");
}

#[tokio::test]
async fn delete_file_if_match_required_and_enforced() {
    let Stack { svc, dp, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = common::bound_file(&svc, &dp, &ctx, b"hello").await;
    let file = svc.get_file(&ctx, t.file_id).await.unwrap();
    let current = etag::etag_for(&file).expect("etag after bind");

    for if_match in [None, Some("\"wrong-etag\"")] {
        let err = svc
            .delete_file(&ctx, t.file_id, if_match)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::PreconditionFailed { .. }),
            "{if_match:?}: {err:?}"
        );
    }
    svc.delete_file(&ctx, t.file_id, Some(&current))
        .await
        .unwrap();
    let err = svc.get_file(&ctx, t.file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { .. }),
        "got {err:?}"
    );
}

/// `list_versions` caps at `max_page_size`, with or without a larger explicit `limit`, and
/// returns the newest page.
#[tokio::test]
async fn list_versions_caps_at_max_page_size() {
    // `default_page_size == max_page_size`, so the no-limit case exercises the cap.
    let max_page_size = 3u64;
    let Stack { svc, dp, .. } = StackBuilder::new("service-test")
        .page_sizes(max_page_size, max_page_size)
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let t0 = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let mut created = vec![t0.version_id];
    for i in 1..=max_page_size + 1 {
        let version_id = svc
            .presign_version(&ctx, t0.file_id)
            .await
            .unwrap()
            .version_id;
        created.push(version_id);
        let data = Bytes::from(format!("v{i}"));
        dp.put_content(&ctx, t0.file_id, version_id, "text/plain", data)
            .await
            .unwrap();
    }

    let newest_first: Vec<Uuid> = created
        .iter()
        .rev()
        .take(usize::try_from(max_page_size).unwrap())
        .copied()
        .collect();
    for limit in [None, Some(max_page_size + 100)] {
        let page = svc
            .list_versions(&ctx, t0.file_id, limit, None)
            .await
            .unwrap();
        let actual: Vec<Uuid> = page.items.iter().map(|v| v.version_id).collect();
        assert_eq!(
            actual, newest_first,
            "limit {limit:?}: the newest-first page, capped"
        );
    }
}

/// Deleting the current version is a conflict and an unknown id is `VersionNotFound`; neither
/// touches the file.
#[tokio::test]
async fn delete_version_rejects_current_and_unknown_versions() {
    let Stack { svc, dp, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let (file_id, v1, _v2) = file_with_two_versions(&svc, &dp, &ctx).await;

    let err = svc.delete_version(&ctx, file_id, v1).await.unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "current: got {err:?}"
    );
    let err = svc
        .delete_version(&ctx, file_id, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::VersionNotFound { .. }),
        "unknown: got {err:?}"
    );

    let versions = svc
        .list_versions(&ctx, file_id, None, None)
        .await
        .unwrap()
        .items;
    assert_eq!(versions.len(), 2);
    assert_eq!(
        svc.get_file(&ctx, file_id).await.unwrap().content_id,
        Some(v1)
    );
}

/// Reproduces a stale "is current?" snapshot: a concurrent `bind` promotes the version after the
/// check. Driven via the store directly; the DB-level guard must refuse the delete.
#[tokio::test]
async fn delete_version_then_bind_cannot_dangle() {
    let Stack { svc, dp, store, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let (file_id, _v1, v2) = file_with_two_versions(&svc, &dp, &ctx).await;
    bind_with_current_etag(&svc, &ctx, file_id, v2).await;

    let audit = AuditEntry::success(
        ctx.subject_tenant_id(),
        "user",
        ctx.subject_id(),
        Some(file_id),
        AuditOperation::DeleteVersion,
        serde_json::json!({ "version_id": v2 }),
    );
    let removed = store.delete_version(file_id, v2, audit).await.unwrap();
    assert!(
        !removed,
        "guarded delete of the (now-current) version must not remove the row"
    );
    assert_eq!(
        svc.get_file(&ctx, file_id).await.unwrap().content_id,
        Some(v2)
    );
    assert!(store.get_version(file_id, v2).await.unwrap().is_some());
}

/// A random `version_id` on a single-version file is `VersionNotFound` and must not delete the
/// file; deleting the only version by its real id deletes the whole file.
#[tokio::test]
async fn delete_only_version_deletes_the_whole_file() {
    let Stack { svc, store, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let scope = toolkit_security::AccessScope::allow_all();

    let err = svc
        .delete_version(&ctx, t.file_id, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::VersionNotFound { .. }),
        "got {err:?}"
    );
    assert!(store.get_file(&scope, t.file_id).await.unwrap().is_some());
    assert!(
        store
            .get_version(t.file_id, t.version_id)
            .await
            .unwrap()
            .is_some()
    );

    svc.delete_version(&ctx, t.file_id, t.version_id)
        .await
        .unwrap();
    assert!(store.get_file(&scope, t.file_id).await.unwrap().is_none());
}

/// The owner fields change, and the same transaction leaves one `transfer_ownership` audit row and
/// one unpublished `file.owner_transferred` event for the new owner.
#[tokio::test]
async fn transfer_ownership_updates_owner_fields_and_leaves_audit_row_and_event() {
    let Stack { svc, store, .. } = StackBuilder::new("service-test").build().await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let new_owner = Uuid::now_v7();
    let file_id = common::bare_file(&svc, &ctx).await;

    let updated = svc
        .transfer_ownership(&ctx, file_id, OwnerKind::App, new_owner)
        .await
        .unwrap();
    assert_eq!(
        (updated.owner_kind, updated.owner_id),
        (OwnerKind::App, new_owner)
    );

    let audit = store.list_audit(file_id).await.unwrap();
    let transfers: Vec<_> = audit
        .iter()
        .filter(|r| r.operation == "transfer_ownership")
        .collect();
    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0].outcome, "success");
    let events = store.list_file_events(file_id).await.unwrap();
    let transfer_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "file.owner_transferred")
        .collect();
    assert_eq!(transfer_events.len(), 1);
    assert_eq!(
        transfer_events[0].owner_id, new_owner,
        "the event names the new owner"
    );
    assert_eq!(transfer_events[0].tenant_id, tenant);
    assert!(transfer_events[0].published_at.is_none());
}

/// A phantom file is `FileNotFound` and the nil UUID is a malformed owner (no principal directory
/// is wired in); neither writes an audit row or an event, and the owner stays unchanged.
#[tokio::test]
async fn transfer_ownership_rejections_write_nothing() {
    let Stack { svc, store, .. } = StackBuilder::new("service-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let phantom_id = Uuid::now_v7();
    let err = svc
        .transfer_ownership(&ctx, phantom_id, OwnerKind::User, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { id } if id == phantom_id),
        "got {err:?}"
    );
    assert!(store.list_audit(phantom_id).await.unwrap().is_empty());
    assert!(store.list_file_events(phantom_id).await.unwrap().is_empty());

    let original_owner = Uuid::now_v7();
    let file = common::new_file_for(original_owner, "doc.txt", "text/plain");
    let file_id = svc
        .create_file(&ctx, file, None, false)
        .await
        .unwrap()
        .file_id;
    let audit_before = store.list_audit(file_id).await.unwrap().len();
    let err = svc
        .transfer_ownership(&ctx, file_id, OwnerKind::App, Uuid::nil())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "new_owner_id"),
        "got {err:?}"
    );
    let file = svc.get_file(&ctx, file_id).await.unwrap();
    assert_eq!(
        (file.owner_id, file.owner_kind),
        (original_owner, OwnerKind::User)
    );
    assert_eq!(store.list_audit(file_id).await.unwrap().len(), audit_before);
}

/// Every write leaves exactly one `success` audit row for its file, in order, and the outbox gets
/// exactly the file's lifecycle events (unpublished): create, upload, bind, a second version
/// bound over the first, metadata patch, `delete_version`, `delete_file`. Audit and event rows
/// survive the delete (no FK cascade).
#[tokio::test]
async fn writes_leave_audit_rows_and_outbox_events() {
    let Stack { svc, dp, store, .. } = StackBuilder::new("audit-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let (file_id, v1, v2) = file_with_two_versions(&svc, &dp, &ctx).await;
    let owner = svc.get_file(&ctx, file_id).await.unwrap().owner_id;
    let patch = CustomMetadataPatch {
        entries: vec![("k".to_owned(), Some("v".to_owned()))],
    };
    svc.update_metadata(&ctx, file_id, patch, None)
        .await
        .unwrap();
    bind_with_current_etag(&svc, &ctx, file_id, v2).await;
    svc.delete_version(&ctx, file_id, v1).await.unwrap();
    let cur = svc.get_file(&ctx, file_id).await.unwrap();
    svc.delete_file(&ctx, file_id, etag::etag_for(&cur).as_deref())
        .await
        .unwrap();

    let rows = store.list_audit(file_id).await.unwrap();
    assert!(
        rows.iter()
            .all(|r| r.outcome == "success" && r.file_id == Some(file_id)),
        "every audit row is a success for this file: {rows:?}"
    );
    let operations: Vec<&str> = rows.iter().map(|r| r.operation.as_str()).collect();
    assert_eq!(
        operations,
        [
            "create",
            "finalize_version",
            "patch_content",
            "finalize_version",
            "patch_metadata",
            "patch_content",
            "delete_version",
            "delete_file",
        ]
    );

    let events = store.list_file_events(file_id).await.unwrap();
    assert!(
        events
            .iter()
            .all(|e| e.file_id == file_id && e.owner_id == owner && e.published_at.is_none()),
        "every event is for this file and owner, still unpublished: {events:?}"
    );
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        types,
        [
            "file.created",
            "file.content_updated",
            "file.content_updated",
            "file.deleted"
        ]
    );
}

#[tokio::test]
async fn multipart_complete_leaves_audit_rows() {
    let Stack {
        backend,
        msvc,
        multipart_store,
        store,
        svc,
        ..
    } = StackBuilder::new("audit-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = common::bare_file(&svc, &ctx).await;
    let plan = common::session_with_part(
        &msvc,
        &multipart_store,
        &backend,
        &ctx,
        file_id,
        b"part1",
        false,
    )
    .await;
    msvc.complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap();

    let rows = store.list_audit(file_id).await.unwrap();
    let complete: Vec<_> = rows
        .iter()
        .filter(|r| r.operation == "multipart_complete")
        .collect();
    assert_eq!(complete.len(), 1);
    assert_eq!(complete[0].outcome, "success");
    assert_eq!(
        rows.iter()
            .filter(|r| r.operation == "finalize_version")
            .count(),
        1
    );
}

/// A stale `expected_meta_version` rolls back the `meta_version` bump and the audit row together.
#[tokio::test]
async fn failed_metadata_cas_leaves_no_audit_row() {
    let Stack { svc, store, .. } = StackBuilder::new("audit-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let before = store.list_audit(t.file_id).await.unwrap().len();

    let patch = CustomMetadataPatch {
        entries: vec![("x".to_owned(), Some("y".to_owned()))],
    };
    let err = svc
        .update_metadata(&ctx, t.file_id, patch, Some(99))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "got {err:?}"
    );
    assert_eq!(store.list_audit(t.file_id).await.unwrap().len(), before);
}
