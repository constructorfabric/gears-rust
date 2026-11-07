//! Handlers of the maintenance surface.
//!
//! One pass of what the managed lifecycle otherwise runs on every replica,
//! asked for by an external scheduler so the work runs once per cluster. The
//! caller is a machine job: no step-up is asked and a service principal is
//! accepted. Authorization is the platform scope's alone: the policy decision
//! on the declaration resource, the platform-wide one, with the action
//! `maintain`, and a caller whose own tenant is the root tenant.

use std::sync::Arc;
use std::time::Duration;

use axum::{Extension, Json};
use tokio_util::sync::CancellationToken;
use toolkit::api::canonical_prelude::*;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;

use crate::api::authz::{self, resource};
use crate::api::rest::maintenance_dto::{PruneResultDto, SweepResultDto};
use crate::domain::error::DomainError;
use crate::domain::ports::LifecycleMetrics;
use crate::gear::ConcreteResolver;
use crate::infra::maintenance::{self, PruneBatches};
use crate::infra::value_writes::{SWEEP_LIMIT, WriteCoordinator};

/// The action the policy decision point is asked about.
pub const MAINTAIN: &str = "maintain";

/// What the passes need besides the coordinator and the database.
pub struct MaintenanceSettings {
    /// The configured default audit retention.
    pub retention: Duration,
    /// The budget of one prune request.
    pub batches: PruneBatches,
    /// Where the retention pass reports.
    pub metrics: Arc<dyn LifecycleMetrics>,
}

/// The platform scope's gate: the policy decision, then the caller's own
/// tenant being the root.
///
/// Neither step-up nor an interactive caller is required: the caller is a
/// machine job holding a machine token.
async fn authorize(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    resolver: &ConcreteResolver,
    ctx: &SecurityContext,
) -> Result<(), DomainError> {
    authz::access_scope(enforcer, ctx, &resource::DECLARATION, MAINTAIN, None).await?;
    let root = resolver.root_tenant().await?;
    if ctx.subject_tenant_id() != root {
        return Err(DomainError::Unauthorized {
            resource: settings_service_sdk::gts::DECLARATION_SCHEMA,
        });
    }
    Ok(())
}

/// Run one pending-secret sweep pass.
///
/// `200` with how many staged secrets were released and whether more may
/// remain. With no secret store bound the pass does nothing and answers zero.
///
/// # Errors
/// `403` for a caller outside the root tenant or denied by the policy;
/// `503` when the database cannot answer.
pub async fn sweep_pending_secrets(
    Extension(ctx): Extension<SecurityContext>,
    Extension(writes): Extension<Arc<WriteCoordinator>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
) -> ApiResult<impl IntoResponse> {
    authorize(&enforcer, &resolver, &ctx).await?;
    // A request is not the lifecycle: nothing here stops the pass but its end.
    let page = writes
        .sweep_page(SWEEP_LIMIT, &CancellationToken::new())
        .await?;
    Ok(Json(SweepResultDto {
        processed: u64::try_from(page.released).unwrap_or(u64::MAX),
        more: page.more,
    }))
}

/// Run one bounded audit retention pass.
///
/// `200` with how many records were deleted and whether the request's budget
/// ran out with more possibly waiting. The budget is a tenth of a lifecycle
/// tick's, so a request stays short.
///
/// # Errors
/// `403` as for the sweep; a failure of the pass is its own error and what
/// earlier batches deleted stays deleted.
pub async fn prune_audit(
    Extension(ctx): Extension<SecurityContext>,
    Extension(db): Extension<Arc<DBProvider<DbError>>>,
    Extension(settings): Extension<Arc<MaintenanceSettings>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
) -> ApiResult<impl IntoResponse> {
    authorize(&enforcer, &resolver, &ctx).await?;
    let run = maintenance::prune_pass(
        &db,
        settings.retention,
        time::OffsetDateTime::now_utc(),
        settings.batches,
        &CancellationToken::new(),
        settings.metrics.as_ref(),
    )
    .await;
    if let Some(failure) = run.failure {
        return Err(failure.into());
    }
    Ok(Json(PruneResultDto {
        deleted: run.pruned,
        more: run.more,
    }))
}
