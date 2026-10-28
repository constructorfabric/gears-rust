//! Cross-user file-enumeration authorization tests.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use uuid::Uuid;

use file_storage::domain::authz::Authorizer;
use file_storage::domain::error::DomainError;
use file_storage::domain::service::FileService;
use file_storage_sdk::{NewFile, OwnerFilter, OwnerKind};

mod common;
use common::{ScopedTestAuthorizer, StackBuilder, ctx_for as ctx};

struct Harness {
    file_svc: Arc<FileService>,
    authz: Arc<ScopedTestAuthorizer>,
}

async fn build_harness() -> Harness {
    let authz = Arc::new(ScopedTestAuthorizer::admin_gated());
    let s = StackBuilder::new("list-authz-test")
        .authorizer(Arc::clone(&authz) as Arc<dyn Authorizer>)
        .build()
        .await;
    Harness {
        file_svc: s.svc,
        authz,
    }
}

fn new_file(owner_id: Uuid) -> NewFile {
    common::new_file_for(owner_id, "victim.bin", "application/octet-stream")
}

fn owner_filter(owner_id: Uuid) -> OwnerFilter {
    OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id,
    }
}

/// A non-owner, non-admin listing another user's files must be denied without leaking them.
#[tokio::test]
async fn list_files_foreign_owner_without_admin_is_denied() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let user_b = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);
    let ctx_b = ctx(tenant, user_b);

    h.file_svc
        .create_file(&ctx_a, new_file(user_a), None, false)
        .await
        .expect("user A creates own file");

    let result = h
        .file_svc
        .list_files(&ctx_b, owner_filter(user_a), Some(10), None)
        .await;
    assert!(
        matches!(result, Err(DomainError::Forbidden)),
        "expected Forbidden, got {result:?}"
    );
}

#[tokio::test]
async fn list_files_self_owner_is_allowed() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);

    let ticket = h
        .file_svc
        .create_file(&ctx_a, new_file(user_a), None, false)
        .await
        .expect("user A creates own file");

    let found = h
        .file_svc
        .list_files(&ctx_a, owner_filter(user_a), Some(10), None)
        .await
        .expect("self-owner list should succeed");
    assert!(found.items.iter().any(|f| f.file_id == ticket.file_id));
}

#[tokio::test]
async fn list_files_foreign_owner_with_admin_scope_is_allowed() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let admin = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);
    let ctx_admin = ctx(tenant, admin);

    let ticket = h
        .file_svc
        .create_file(&ctx_a, new_file(user_a), None, false)
        .await
        .expect("user A creates own file");
    h.authz.set_admin(true);

    let found = h
        .file_svc
        .list_files(&ctx_admin, owner_filter(user_a), Some(10), None)
        .await
        .expect("admin should be able to list foreign owner's files");
    assert!(found.items.iter().any(|f| f.file_id == ticket.file_id));
}
