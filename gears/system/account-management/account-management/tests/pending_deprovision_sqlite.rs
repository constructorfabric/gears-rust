//! Integration tests for `PendingDeprovisionRepoImpl` — the record of
//! unfinished user deletions — against a migrated in-memory `SQLite`.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use account_management::domain::user::pending_deprovision::PendingDeprovisionRepo;
use account_management::infra::storage::repo_impl::PendingDeprovisionRepoImpl;
use uuid::Uuid;

use common::setup_sqlite;

#[tokio::test]
async fn a_recorded_deletion_is_seen_until_cleared() {
    let harness = setup_sqlite().await.expect("setup_sqlite");
    let repo = PendingDeprovisionRepoImpl::new(Arc::clone(&harness.provider));
    let (tenant, user) = (Uuid::new_v4(), Uuid::new_v4());

    assert!(!repo.is_recorded(tenant, user).await.expect("read"));

    repo.record(tenant, user).await.expect("record");
    assert!(repo.is_recorded(tenant, user).await.expect("read"));

    repo.clear(tenant, user).await.expect("clear");
    assert!(!repo.is_recorded(tenant, user).await.expect("read"));
}

#[tokio::test]
async fn recording_and_clearing_are_idempotent() {
    // A retried DELETE records the same deletion again, and clears one
    // that a concurrent attempt already cleared.
    let harness = setup_sqlite().await.expect("setup_sqlite");
    let repo = PendingDeprovisionRepoImpl::new(Arc::clone(&harness.provider));
    let (tenant, user) = (Uuid::new_v4(), Uuid::new_v4());

    repo.record(tenant, user).await.expect("first record");
    repo.record(tenant, user)
        .await
        .expect("second record is a no-op");
    assert!(repo.is_recorded(tenant, user).await.expect("read"));

    repo.clear(tenant, user).await.expect("first clear");
    repo.clear(tenant, user)
        .await
        .expect("second clear is a no-op");
    assert!(!repo.is_recorded(tenant, user).await.expect("read"));
}

#[tokio::test]
async fn records_are_keyed_by_tenant_and_user() {
    // A deletion recorded for a user in one tenant says nothing about the
    // same user id in another tenant, nor about another user.
    let harness = setup_sqlite().await.expect("setup_sqlite");
    let repo = PendingDeprovisionRepoImpl::new(Arc::clone(&harness.provider));
    let (tenant, other_tenant) = (Uuid::new_v4(), Uuid::new_v4());
    let (user, other_user) = (Uuid::new_v4(), Uuid::new_v4());

    repo.record(tenant, user).await.expect("record");

    assert!(!repo.is_recorded(other_tenant, user).await.expect("read"));
    assert!(!repo.is_recorded(tenant, other_user).await.expect("read"));

    repo.clear(other_tenant, user)
        .await
        .expect("clear other tenant");
    assert!(
        repo.is_recorded(tenant, user).await.expect("read"),
        "clearing another tenant's record leaves this one"
    );
}
