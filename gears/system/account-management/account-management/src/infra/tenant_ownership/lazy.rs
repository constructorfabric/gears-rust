//! Lazy vendor/priority selection of a [`TenantOwnershipPluginClient`].
//!
//! The plugin's owning gear may initialize after AM, so eager selection would
//! race its publication. [`LazyTenantOwnershipChecker`] resolves the plugin on
//! first need through [`toolkit::plugins::GtsPluginSelector`], exactly like
//! the `IdP` plugin resolution in `crate::infra::idp`: list every catalogue
//! instance of [`TenantOwnershipPluginSpecV1`], keep only those with a scoped
//! client in this process, pick by vendor then priority. Only a successful
//! selection is cached; a failed one is retried on the next call.
//!
//! An unresolvable plugin never falls back to the built-in resource-group
//! check: the operator chose who answers "does this tenant own anything?",
//! so a missing answer fails the soft-delete with a retryable
//! `service_unavailable` and defers the hard-delete cleanup instead.

use std::sync::Arc;
use std::time::Duration;

use account_management_sdk::{
    TenantOwnershipFailure, TenantOwnershipPluginClient, TenantOwnershipPluginSpecV1,
};
use async_trait::async_trait;
use futures::FutureExt;
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::plugins::{GtsPluginSelector, choose_registered_plugin_instance};
use toolkit_security::PlatformSecurityContext;
use tracing::{info, warn};
use types_registry_sdk::{
    GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::tenant::hooks::{HookError, TenantHardDeleteHook};
use crate::domain::tenant::resource_checker::ResourceOwnershipChecker;

/// Upper bound on one `owns_anything` call. A soft-delete waits on it, so
/// a hung plugin must surface as a retryable 503 rather than a stuck
/// request.
const OWNS_ANYTHING_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on one `cleanup_tenant` call, so a slow plugin cannot
/// monopolise the retention pipeline; the tenant is retried next tick.
#[allow(
    clippy::duration_suboptimal_units,
    reason = "from_mins is unstable on workspace MSRV; keep from_secs"
)]
const CLEANUP_BUDGET: Duration = Duration::from_secs(120);

/// `target` label on the `am.dependency_health` counter for this plugin.
const METRIC_TARGET: &str = "tenant_ownership_plugin";

/// Lazily-resolved tenant-ownership plugin, used both as the soft-delete
/// [`ResourceOwnershipChecker`] and (through [`Self::cleanup_hook`]) as a
/// hard-delete hook.
pub struct LazyTenantOwnershipChecker {
    hub: Arc<ClientHub>,
    registry: Arc<dyn PlatformTypesRegistryApi>,
    vendor: String,
    selector: GtsPluginSelector,
    owns_anything_timeout: Duration,
    cleanup_budget: Duration,
}

impl std::fmt::Debug for LazyTenantOwnershipChecker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyTenantOwnershipChecker")
            .field("vendor", &self.vendor)
            .finish_non_exhaustive()
    }
}

impl LazyTenantOwnershipChecker {
    /// Construct the lazy wrapper. No catalogue access happens here — the
    /// first ownership question or cleanup triggers resolution.
    #[must_use]
    pub fn new(
        hub: Arc<ClientHub>,
        registry: Arc<dyn PlatformTypesRegistryApi>,
        vendor: String,
    ) -> Self {
        Self {
            hub,
            registry,
            vendor,
            selector: GtsPluginSelector::new(),
            owns_anything_timeout: OWNS_ANYTHING_TIMEOUT,
            cleanup_budget: CLEANUP_BUDGET,
        }
    }

    /// Override the per-call time limits (tests only).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_timeouts(mut self, owns_anything: Duration, cleanup: Duration) -> Self {
        self.owns_anything_timeout = owns_anything;
        self.cleanup_budget = cleanup;
        self
    }

    /// Resolve the plugin: catalogue selection, then the scoped `ClientHub`
    /// lookup. The error string is a diagnostic for logs and the 503 detail.
    async fn resolve(&self) -> Result<Arc<dyn TenantOwnershipPluginClient>, String> {
        let gts_id: Arc<str> = self
            .selector
            .get_or_init(|| async {
                let plugin_type_id = TenantOwnershipPluginSpecV1::gts_type_id();
                let query =
                    ListEntitiesRequest::matching(GtsIdPattern::try_new(plugin_type_id.as_ref())?);
                // Selection is process-wide and cached, so use the platform API,
                // independent of whichever caller triggers it first.
                let instances = self
                    .registry
                    .list_instances(&PlatformSecurityContext::outbound_marker(), query)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "types-registry list_instances<TenantOwnershipPluginSpecV1>: {e}"
                        )
                    })?;
                choose_registered_plugin_instance::<
                    TenantOwnershipPluginSpecV1,
                    dyn TenantOwnershipPluginClient,
                >(
                    &self.hub,
                    &self.vendor,
                    instances
                        .iter()
                        .map(|instance| (instance.id.as_ref(), instance.content.as_ref())),
                )
                .map_err(|e| {
                    anyhow::anyhow!("choose_plugin_instance<TenantOwnershipPluginSpecV1>: {e}")
                })
            })
            .await
            .map_err(|e: anyhow::Error| {
                warn!(
                    target: "am.tenant_ownership",
                    vendor = %self.vendor,
                    error = %e,
                    "tenant-ownership plugin resolution failed; will retry on next call"
                );
                format!(
                    "tenant-ownership plugin (vendor {}) unavailable: {e}",
                    self.vendor
                )
            })?;

        self.hub
            .try_get_scoped::<dyn TenantOwnershipPluginClient>(&ClientScope::gts_id(
                gts_id.as_ref(),
            ))
            .ok_or_else(|| {
                warn!(
                    target: "am.tenant_ownership",
                    vendor = %self.vendor,
                    gts_id = %gts_id,
                    "catalogue advertised the plugin but no scoped client is registered \
                     under its gts_id; will retry on next call"
                );
                format!(
                    "tenant-ownership plugin {gts_id} is advertised but not registered in this process"
                )
            })
    }

    /// Build the hard-delete hook that asks the plugin to clean up a tenant.
    ///
    /// `Retryable` plugin failures, an unresolvable plugin and exceeding the
    /// cleanup budget defer the tenant to the next retention tick;
    /// `Terminal` plugin failures park it for an operator.
    #[must_use]
    pub fn cleanup_hook(self: &Arc<Self>) -> TenantHardDeleteHook {
        let this = Arc::clone(self);
        Arc::new(move |tenant_id: Uuid| {
            let this = Arc::clone(&this);
            async move { this.cleanup(tenant_id).await }.boxed()
        })
    }

    /// Body of [`Self::cleanup_hook`].
    async fn cleanup(&self, tenant_id: Uuid) -> Result<(), HookError> {
        let plugin = self.resolve().await.map_err(|detail| {
            record("cleanup_tenant", "unresolved");
            HookError::Retryable { detail }
        })?;
        match tokio::time::timeout(self.cleanup_budget, plugin.cleanup_tenant(tenant_id)).await {
            Ok(Ok(())) => {
                record("cleanup_tenant", "success");
                info!(
                    target: "am.tenant_ownership",
                    tenant_id = %tenant_id,
                    "tenant-ownership plugin cleanup completed"
                );
                Ok(())
            }
            Ok(Err(failure)) => {
                record("cleanup_tenant", failure.as_metric_label());
                Err(match failure {
                    TenantOwnershipFailure::Terminal { detail } => HookError::Terminal { detail },
                    // `Retryable` and any variant added later: deferring is the
                    // safe default, the tenant row stays either way.
                    other => HookError::Retryable {
                        detail: other.to_string(),
                    },
                })
            }
            Err(_elapsed) => {
                record("cleanup_tenant", "timeout");
                Err(HookError::Retryable {
                    detail: format!(
                        "tenant-ownership cleanup for tenant {tenant_id} exceeded its budget of \
                         {}s; retrying on next retention tick",
                        self.cleanup_budget.as_secs()
                    ),
                })
            }
        }
    }
}

#[async_trait]
impl ResourceOwnershipChecker for LazyTenantOwnershipChecker {
    async fn count_ownership_links(&self, tenant_id: Uuid) -> Result<u64, DomainError> {
        let plugin = self.resolve().await.map_err(|detail| {
            record("owns_anything", "unresolved");
            DomainError::service_unavailable(detail)
        })?;
        match tokio::time::timeout(self.owns_anything_timeout, plugin.owns_anything(tenant_id))
            .await
        {
            Ok(Ok(owns)) => {
                record("owns_anything", "success");
                Ok(u64::from(owns))
            }
            Ok(Err(failure)) => {
                record("owns_anything", failure.as_metric_label());
                Err(DomainError::service_unavailable(failure.to_string()))
            }
            Err(_elapsed) => {
                record("owns_anything", "timeout");
                Err(DomainError::service_unavailable(
                    "tenant-ownership plugin: timeout exceeded",
                ))
            }
        }
    }
}

/// Emit one outcome on the shared dependency-health counter.
fn record(op: &'static str, outcome: &'static str) {
    emit_metric(
        AM_DEPENDENCY_HEALTH,
        MetricKind::Counter,
        &[("target", METRIC_TARGET), ("op", op), ("outcome", outcome)],
    );
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "test helpers")]
#[path = "lazy_tests.rs"]
mod lazy_tests;
