//! Cascade cleanup hook for tenant hard-delete.
//!
//! Implements `cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`.
//!
//! AM only triggers the cleanup; Resource Group owns its mechanics. The
//! hook makes a single [`ResourceGroupTenantCleanup::delete_tenant_groups`]
//! call naming the tenant and AM's user-group type, and RG removes every
//! such group with its subtree, memberships and closure rows. Only AM's
//! own group type is addressed, so groups other gears created for the
//! tenant are left to their owners.
//!
//! The call goes through RG's un-gated tenant-cleanup surface rather than
//! the PEP-gated `ResourceGroupClient`: the tenant is `Deleted` by the time
//! the hook runs, and no PDP grant can address a deleted tenant's rows
//! (see the trait's docs for why both a tenant-bound and a platform-scoped
//! grant fail closed).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use resource_group_sdk::ResourceGroupTenantCleanup;
use tracing::info;
use uuid::Uuid;

use super::USER_GROUP_TYPE_CODE;
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::system_actor::for_user_groups_cascade;
use crate::domain::tenant::hooks::{HookError, TenantHardDeleteHook};

/// Overall budget per cascade invocation. Caps the time the retention
/// pipeline can spend on a single tenant before returning
/// [`HookError::Retryable`] so the next tick resumes, keeping one slow RG
/// call from monopolising the pipeline.
#[allow(
    clippy::duration_suboptimal_units,
    reason = "from_mins is unstable on workspace MSRV; keep from_secs"
)]
const CASCADE_BUDGET: Duration = Duration::from_secs(120);

// @cpt-begin:cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger:p1:inst-flow-cascade-hook
/// Build the cascade cleanup hook that deletes a tenant's user groups via
/// RG during hard-delete.
///
/// A tenant with no user groups is a successful no-op. Any RG failure, and
/// exceeding [`CASCADE_BUDGET`], returns [`HookError::Retryable`] so the
/// pipeline defers the tenant to the next retention tick; the call is
/// idempotent, so a retry after a partial run finishes the job.
pub fn build_cascade_cleanup_hook(
    client: Arc<dyn ResourceGroupTenantCleanup + Send + Sync>,
) -> TenantHardDeleteHook {
    Arc::new(move |tenant_id: Uuid| {
        let client = Arc::clone(&client);
        async move {
            let start = Instant::now();
            let ctx = for_user_groups_cascade(tenant_id);
            let outcome = tokio::time::timeout(
                CASCADE_BUDGET,
                client.delete_tenant_groups(&ctx, tenant_id, USER_GROUP_TYPE_CODE),
            )
            .await;
            match outcome {
                Ok(Ok(groups_deleted)) => {
                    record("success");
                    info!(
                        target: "am.user_groups",
                        tenant_id = %tenant_id,
                        groups_deleted,
                        elapsed_ms = start.elapsed().as_millis(),
                        "cascade cleanup completed for tenant user groups"
                    );
                    Ok(())
                }
                Ok(Err(e)) => {
                    record("error");
                    Err(retryable(format!("resource-group: {e}")))
                }
                Err(_elapsed) => {
                    record("budget_exceeded");
                    Err(retryable(format!(
                        "cascade cleanup for tenant {tenant_id} exceeded overall budget of \
                         {}s; retrying on next retention tick",
                        CASCADE_BUDGET.as_secs()
                    )))
                }
            }
        }
        .boxed()
    })
}
// @cpt-end:cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger:p1:inst-flow-cascade-hook

/// Emit the cascade outcome on the shared dependency-health counter.
fn record(outcome: &'static str) {
    emit_metric(
        AM_DEPENDENCY_HEALTH,
        MetricKind::Counter,
        &[
            ("target", "resource_group"),
            ("op", "cascade_cleanup"),
            ("outcome", outcome),
        ],
    );
}

fn retryable(detail: impl Into<String>) -> HookError {
    HookError::Retryable {
        detail: detail.into(),
    }
}
