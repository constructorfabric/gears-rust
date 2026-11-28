//! Unit tests for [`super::cascade::build_cascade_cleanup_hook`].
//!
//! The hook is wired against a [`FakeTenantCleanup`] that records every
//! `delete_tenant_groups` call and returns a canned outcome. Pins:
//!
//! * The call names the tenant being deleted and AM's user-group type
//!   (never another gear's groups).
//! * Success (including a tenant with no groups left) → `Ok`.
//! * RG failure → `Retryable`.
//! * RG slower than the overall budget → `Retryable`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use resource_group_sdk::ResourceGroupTenantCleanup;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::USER_GROUP_TYPE_CODE;
use super::cascade::build_cascade_cleanup_hook;
use crate::domain::tenant::hooks::HookError;

/// Canned outcome of the fake's `delete_tenant_groups`.
#[derive(Clone)]
enum Outcome {
    Deleted(u64),
    Unavailable,
    Hang,
}

struct FakeTenantCleanup {
    outcome: Outcome,
    calls: Mutex<Vec<(Uuid, String)>>,
}

impl FakeTenantCleanup {
    fn new(outcome: Outcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            calls: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl ResourceGroupTenantCleanup for FakeTenantCleanup {
    async fn delete_tenant_groups(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
        type_code: &str,
    ) -> Result<u64, CanonicalError> {
        self.calls
            .lock()
            .unwrap()
            .push((tenant_id, type_code.to_owned()));
        match self.outcome {
            Outcome::Deleted(n) => Ok(n),
            Outcome::Unavailable => Err(CanonicalError::internal("rg backend down").create()),
            Outcome::Hang => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(0)
            }
        }
    }
}

#[tokio::test]
async fn cascade_deletes_only_user_groups_of_the_tenant() {
    let fake = FakeTenantCleanup::new(Outcome::Deleted(3));
    let hook = build_cascade_cleanup_hook(fake.clone());
    let tenant = Uuid::from_u128(0x77);

    hook(tenant).await.expect("success");

    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls, vec![(tenant, USER_GROUP_TYPE_CODE.to_owned())]);
}

#[tokio::test]
async fn cascade_with_no_groups_left_is_success() {
    let fake = FakeTenantCleanup::new(Outcome::Deleted(0));
    let hook = build_cascade_cleanup_hook(fake);
    hook(Uuid::from_u128(0x78)).await.expect("no-op success");
}

#[tokio::test]
async fn cascade_maps_rg_failure_to_retryable() {
    let fake = FakeTenantCleanup::new(Outcome::Unavailable);
    let hook = build_cascade_cleanup_hook(fake);
    let err = hook(Uuid::from_u128(0x79)).await.expect_err("rg down");
    assert!(matches!(err, HookError::Retryable { .. }), "{err:?}");
}

#[tokio::test(start_paused = true)]
async fn cascade_exceeding_budget_is_retryable() {
    let fake = FakeTenantCleanup::new(Outcome::Hang);
    let hook = build_cascade_cleanup_hook(fake);
    let err = hook(Uuid::from_u128(0x7A)).await.expect_err("budget");
    match err {
        HookError::Retryable { detail } => assert!(detail.contains("budget"), "{detail}"),
        other @ HookError::Terminal { .. } => panic!("expected Retryable, got {other:?}"),
    }
}
