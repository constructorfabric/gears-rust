//! Ownership transfer: row update, audit row and file-events outbox, in one transaction.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::domain::service::FileService;
use file_storage::infra::storage::Store;
use file_storage_sdk::{NewFile, OwnerKind};

mod common;
use common::{StackBuilder, ctx};

async fn build_service() -> (Arc<FileService>, Store) {
    let s = StackBuilder::new("ownership-test").build().await;
    (s.svc, s.store)
}

fn new_file_for(owner_id: Uuid) -> NewFile {
    common::new_file_for(owner_id, "transfer-test.txt", "text/plain")
}

/// The owner fields change, and the same transaction leaves one `transfer_ownership` audit row and
/// one unpublished `file.owner_transferred` event for the new owner.
#[tokio::test]
async fn transfer_ownership_updates_owner_fields_and_leaves_audit_row_and_event() {
    let (svc, store) = build_service().await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let original_owner = Uuid::now_v7();
    let new_owner = Uuid::now_v7();

    let ticket = svc
        .create_file(&ctx, new_file_for(original_owner), None, false)
        .await
        .unwrap();
    let file_id = ticket.file_id;

    let updated = svc
        .transfer_ownership(&ctx, file_id, OwnerKind::App, new_owner)
        .await
        .unwrap();

    assert_eq!(
        updated.owner_kind.as_str(),
        "app",
        "owner_kind must be updated to 'app'"
    );
    assert_eq!(updated.owner_id, new_owner, "owner_id must be updated");
    assert_eq!(updated.file_id, file_id, "file_id must remain unchanged");
    assert_eq!(updated.tenant_id, tenant, "tenant_id must remain unchanged");

    let audit_rows = store.list_audit(file_id).await.unwrap();
    let transfer_rows: Vec<_> = audit_rows
        .iter()
        .filter(|r| r.operation == "transfer_ownership")
        .collect();
    assert_eq!(
        transfer_rows.len(),
        1,
        "expected exactly 1 transfer_ownership audit row"
    );
    assert_eq!(transfer_rows[0].outcome, "success");
    assert_eq!(transfer_rows[0].file_id, Some(file_id));

    let events = store.list_file_events(file_id).await.unwrap();
    let transfer_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "file.owner_transferred")
        .collect();
    assert_eq!(
        transfer_events.len(),
        1,
        "expected exactly 1 file.owner_transferred event"
    );
    let ev = transfer_events[0];
    assert_eq!(ev.file_id, file_id);
    assert_eq!(
        ev.owner_id, new_owner,
        "event owner_id must be the new owner"
    );
    assert_eq!(ev.tenant_id, tenant);
    assert!(ev.published_at.is_none(), "event must not be published yet");
}

/// A phantom file is `FileNotFound`, and the failed update writes neither an audit row nor an event.
#[tokio::test]
async fn transfer_ownership_non_existent_file_returns_not_found_and_writes_nothing() {
    let (svc, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let phantom_id = Uuid::now_v7();
    let new_owner = Uuid::now_v7();

    let err = svc
        .transfer_ownership(&ctx, phantom_id, OwnerKind::User, new_owner)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { id } if id == phantom_id),
        "expected FileNotFound, got: {err:?}"
    );

    assert!(
        store.list_audit(phantom_id).await.unwrap().is_empty(),
        "no audit rows must be written for a phantom file"
    );
    assert!(
        store.list_file_events(phantom_id).await.unwrap().is_empty(),
        "no event rows must be written for a phantom file"
    );
}

/// No principal directory is wired in, so only the nil UUID is rejected as a malformed owner.
#[tokio::test]
async fn transfer_to_malformed_owner_is_rejected() {
    let (svc, _store) = build_service().await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let original_owner = Uuid::now_v7();

    let ticket = svc
        .create_file(&ctx, new_file_for(original_owner), None, false)
        .await
        .unwrap();
    let file_id = ticket.file_id;

    let err = svc
        .transfer_ownership(&ctx, file_id, OwnerKind::App, Uuid::nil())
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "new_owner_id"),
        "expected a new_owner_id validation error, got: {err:?}"
    );

    let file = svc.get_file(&ctx, file_id).await.unwrap();
    assert_eq!(file.owner_id, original_owner);
    assert_eq!(file.owner_kind.as_str(), "user");
}
