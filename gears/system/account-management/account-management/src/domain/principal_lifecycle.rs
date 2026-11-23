//! Delivery of principal-lifecycle events to the hooks other gears register.
//!
//! The contract hooks implement lives in the SDK
//! ([`account_management_sdk::principal_lifecycle`]); this module is AM's
//! side of it: the [`PrincipalLifecycleNotifier`] port the domain calls, the
//! mapping of hook failures onto AM's own error shapes, and the bridge that
//! delivers [`PrincipalLifecycleEvent::TenantHardDelete`] from the
//! hard-delete pipeline. The production notifier, which discovers hooks
//! through the types registry and `ClientHub`, is
//! [`crate::infra::principal_lifecycle::HookDispatcher`].

use std::sync::Arc;

use account_management_sdk::{PrincipalLifecycleEvent, PrincipalLifecycleHookError};
use async_trait::async_trait;
use futures::FutureExt;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::system_actor::for_principal_lifecycle_tenant_hook;
use crate::domain::tenant::hooks::{HookError, TenantHardDeleteHook};

/// Delivers one event to every registered principal-lifecycle hook.
#[async_trait]
pub trait PrincipalLifecycleNotifier: Send + Sync {
    /// Deliver `event` to every registered hook, continuing past a failing
    /// hook so the others still run, and return the most severe failure.
    /// `Ok(())` when there is no hook to deliver to.
    ///
    /// # Errors
    ///
    /// [`PrincipalLifecycleHookError::Terminal`] if any hook failed
    /// terminally, otherwise [`PrincipalLifecycleHookError::Retryable`] if any
    /// hook — or the discovery of the hooks themselves — failed.
    async fn notify(
        &self,
        ctx: &SecurityContext,
        event: &PrincipalLifecycleEvent,
    ) -> Result<(), PrincipalLifecycleHookError>;
}

/// Map a delivery failure onto the error a request-path operation returns:
/// retryable becomes 503 so the client retries the whole operation (which is
/// idempotent end to end), terminal becomes 500.
pub(crate) fn into_domain_error(
    event: &PrincipalLifecycleEvent,
    err: PrincipalLifecycleHookError,
) -> DomainError {
    match err {
        PrincipalLifecycleHookError::Terminal { detail } => DomainError::internal(format!(
            "principal-lifecycle hook failed terminally on {}: {detail}",
            event.kind()
        )),
        // `Retryable` plus any future (`#[non_exhaustive]`) variant: a
        // retry is the safe default, it re-delivers to every hook.
        other => DomainError::service_unavailable(format!(
            "principal-lifecycle hook unavailable on {}: {other}",
            event.kind()
        )),
    }
}

/// Build the hard-delete hook that delivers
/// [`PrincipalLifecycleEvent::TenantHardDelete`] before AM tears a tenant
/// down. Retryable failures defer the tenant to the next retention tick and
/// terminal ones park it, exactly as for any other hard-delete hook.
pub(crate) fn build_tenant_hard_delete_hook(
    notifier: Arc<dyn PrincipalLifecycleNotifier>,
) -> TenantHardDeleteHook {
    Arc::new(move |tenant_id: Uuid| {
        let notifier = Arc::clone(&notifier);
        async move {
            let ctx = for_principal_lifecycle_tenant_hook(tenant_id);
            let event = PrincipalLifecycleEvent::TenantHardDelete { tenant_id };
            notifier
                .notify(&ctx, &event)
                .await
                .map_err(|err| match err {
                    PrincipalLifecycleHookError::Terminal { detail } => HookError::Terminal {
                        detail: format!("principal-lifecycle hook: {detail}"),
                    },
                    other => HookError::Retryable {
                        detail: format!("principal-lifecycle hook: {other}"),
                    },
                })
        }
        .boxed()
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "principal_lifecycle_tests.rs"]
mod principal_lifecycle_tests;
