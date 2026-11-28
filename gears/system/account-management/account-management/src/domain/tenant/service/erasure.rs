//! Tenant erasure on `TenantService` — `erase_tenant`.
//!
//! A soft-deleted tenant keeps its name until the hard-delete sweep
//! removes the row, and gears that report on past periods keep their
//! copy of the name for longer still. An erasure under the account's
//! deletion is the explicit act that ends both: AM replaces the name
//! with the tenant's id, then tells other gears through
//! [`PrincipalLifecycleEvent::TenantErased`] so they drop their copies.
//! A deletion, soft or hard, never does either.

use account_management_sdk::{PrincipalLifecycleEvent, Tenant};
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::principal_lifecycle;
use crate::domain::tenant::model::TenantStatus;
use crate::domain::tenant::repo::TenantRepo;

use super::{TenantService, pep};

impl<R: TenantRepo> TenantService<R> {
    /// Implements FEATURE `Erase Tenant`.
    ///
    /// Erase the name of a soft-deleted tenant: replace it with the
    /// tenant's id, then deliver
    /// [`PrincipalLifecycleEvent::TenantErased`] to every registered
    /// principal-lifecycle hook. Status, `deleted_at` and the retention
    /// timer are unchanged; the tenant is hard-deleted when its window
    /// ends, exactly as without the erasure.
    ///
    /// Idempotent end to end: erasing an erased tenant writes nothing
    /// and delivers the event again, so a client that saw a hook failure
    /// retries the same call until it succeeds.
    ///
    /// # Errors
    ///
    /// - [`DomainError::CrossTenantDenied`] when the PDP denies `erase`
    ///   on the target tenant.
    /// - [`DomainError::NotFound`] when the tenant does not exist, is
    ///   outside the caller's scope, or is `Provisioning`.
    /// - [`DomainError::Conflict`] when the tenant is not soft-deleted:
    ///   only a tenant under deletion can be erased. The root tenant
    ///   can never be deleted, so it always lands here.
    /// - [`DomainError::ServiceUnavailable`] when a hook failed
    ///   retryably, and [`DomainError::Internal`] when one failed
    ///   terminally. The name is already erased in both cases; a retry
    ///   delivers the event again.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-hierarchy-management-erase-tenant:p1:inst-flow-erase-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-tenant-erasure:p1:inst-dod-tenant-erasure-service
    pub async fn erase_tenant(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Tenant, DomainError> {
        let scope = self
            .authorize(ctx, pep::actions::ERASE, tenant_id, Some(tenant_id))
            .await?;
        let current = self
            .repo
            .find_by_id(&scope, tenant_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {tenant_id} not found"),
                resource: tenant_id.to_string(),
            })?;
        // `Provisioning` is AM-internal — surface as `NotFound`, as
        // every other public tenant operation does.
        match current.status {
            TenantStatus::Deleted => {}
            TenantStatus::Provisioning => {
                return Err(DomainError::NotFound {
                    detail: format!("tenant {tenant_id} not found"),
                    resource: tenant_id.to_string(),
                });
            }
            TenantStatus::Active | TenantStatus::Suspended => {
                return Err(DomainError::Conflict {
                    detail: format!(
                        "tenant {tenant_id} is not deleted; delete it before erasing it"
                    ),
                });
            }
        }
        // Read before the write, as in `update_tenant`.
        let tenant_type = self
            .resolve_tenant_type(ctx, current.tenant_type_uuid)
            .await?;
        let erased = self
            .repo
            .erase_tenant_name(&scope, tenant_id, OffsetDateTime::now_utc())
            .await?;
        if erased.name != current.name {
            // TODO(events): emit AM event when platform event-bus lands.
            tracing::info!(
                target: "am.events",
                kind = "tenantStateChanged",
                actor = "tenant_scoped",
                subject_id = %ctx.subject_id(),
                subject_tenant_id = %ctx.subject_tenant_id(),
                tenant_id = %tenant_id,
                event = "erased",
                status_from = current.status.as_str(),
                status_to = erased.status.as_str(),
                "am tenant state changed"
            );
        }
        self.notify_tenant_erased(ctx, tenant_id).await?;
        self.lower_to_tenant(&scope, erased, tenant_type).await
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-tenant-erasure:p1:inst-dod-tenant-erasure-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-hierarchy-management-erase-tenant:p1:inst-flow-erase-service

    /// Deliver `TenantErased` to every registered hook. Runs after the
    /// name is written, so a hook that reads the tenant back sees the
    /// erased name. A hook failure fails the request (503 retryable,
    /// 500 terminal) without undoing the write; the retry re-delivers.
    async fn notify_tenant_erased(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<(), DomainError> {
        let Some(lifecycle) = self.lifecycle.as_ref() else {
            return Ok(());
        };
        let event = PrincipalLifecycleEvent::TenantErased { tenant_id };
        lifecycle
            .notify(ctx, &event)
            .await
            .map_err(|err| principal_lifecycle::into_domain_error(&event, err))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "erasure_tests.rs"]
mod erasure_tests;
