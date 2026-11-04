#![allow(clippy::expect_used)]

use std::sync::Arc;

use quota_enforcement_sdk::{
    LeaseToken, PartialIdempotencyWrite, PayloadHash, StorageError, TenantId,
};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::StoragePlugin;
use crate::infra::storage::SqlFoundationStore;
use crate::test_support::{
    FakeConsumptionStore, FakeLeaseStore, FakePolicyStore, FakeQuotaStore, tenant, test_db,
};

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_type("service")
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("security context")
}

async fn plugin() -> StoragePlugin {
    StoragePlugin::new(
        Arc::new(SqlFoundationStore::new(test_db().await)),
        Arc::new(FakeQuotaStore::default()),
        Arc::new(FakePolicyStore::default()),
        Arc::new(FakeConsumptionStore),
        Arc::new(FakeLeaseStore),
    )
}

#[tokio::test]
async fn settlements_reach_the_lease_store_with_the_token_they_name() {
    let plugin = plugin().await;
    let token = LeaseToken::new(Uuid::now_v7());
    let partial = PartialIdempotencyWrite {
        tenant_id: TenantId::new(Uuid::now_v7()),
        key: "k".to_owned(),
        payload_hash: PayloadHash::from_bytes([1; 32]),
    };
    let ctx = ctx();

    let committed = plugin
        .commit_lease(
            &ctx,
            &AccessScope::allow_all(),
            token,
            Some(1),
            &partial,
            &[],
        )
        .await;
    assert!(matches!(committed, Err(StorageError::LeaseNotFound { token: t }) if t == token));
    let released = plugin
        .release_lease(&ctx, &AccessScope::allow_all(), token, &partial, &[])
        .await;
    assert!(matches!(released, Err(StorageError::LeaseNotFound { token: t }) if t == token));
}
