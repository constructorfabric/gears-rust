//! Lazy vendor/priority selection of an [`IdpPluginClient`].
//!
//! Concrete `IdP` plugins may initialize after AM, so eager selection would race
//! their publication. [`LazyIdpProvider`] forwards calls to the first successful
//! selection cached by [`toolkit::plugins::GtsPluginSelector`]; failures are
//! not cached. List every catalogue page and keep only instances with a scoped
//! client in this process, excluding advertisements retained from earlier boots.
//!
//! With `cfg.idp.required = true`, selection failures map to the operation's
//! unavailable variant (`IdpProvisionFailure::CleanFailure`,
//! `IdpDeprovisionFailure::Retryable`, `IdpUserOperationFailure::Unavailable`),
//! preserving saga compensation and 503 responses. With `required = false`,
//! delegate to [`NoopIdpProvider`], which returns `UnsupportedOperation`.
//!
//! Plugins must use `ClientHub::register_scoped::<dyn IdpPluginClient>` with
//! `ClientScope::gts_id(&instance_id)` matching their published instance
//! (`cpt-cf-account-management-fr-idp-vendor-selection`).

use std::sync::Arc;

use account_management_sdk::idp_service_account::{
    IdpListServiceAccountsRequest, IdpProvisionServiceAccountRequest,
    IdpRevokeServiceAccountRequest, IdpRotateServiceAccountSecretRequest,
    IdpServiceAccountCredentials, IdpServiceAccountFailure, IdpServiceAccountSummary,
};
use account_management_sdk::{
    IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpDeprovisionUserRequest,
    IdpListUsersRequest, IdpPluginClient, IdpPluginSpecV1, IdpProvisionFailure, IdpProvisionResult,
    IdpProvisionTenantRequest, IdpProvisionUserRequest, IdpUpdateUserRequest, IdpUser,
    IdpUserOperationFailure,
};
use async_trait::async_trait;
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::plugins::{GtsPluginSelector, choose_registered_plugin_instance};
use toolkit_odata::Page;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::{
    GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};

use crate::infra::idp::NoopIdpProvider;

/// Lazy `IdP` plugin resolver — see gear docs for the contract.
pub struct LazyIdpProvider {
    hub: Arc<ClientHub>,
    registry: Arc<dyn PlatformTypesRegistryApi>,
    vendor: String,
    /// Mirrors `AccountManagementConfig::idp.required`. Determines
    /// the failure category emitted when the catalogue + scoped
    /// `ClientHub` lookup yield nothing: `required = true` surfaces
    /// the per-trait "unavailable" variant (retryable on the wire);
    /// `required = false` delegates to [`NoopIdpProvider`] (returns
    /// `UnsupportedOperation`, the existing dev / test posture).
    required: bool,
    selector: GtsPluginSelector,
    fallback: NoopIdpProvider,
}

impl std::fmt::Debug for LazyIdpProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyIdpProvider")
            .field("vendor", &self.vendor)
            .field("required", &self.required)
            .finish_non_exhaustive()
    }
}

impl LazyIdpProvider {
    /// Construct the lazy wrapper. No catalogue access happens here —
    /// the very first `IdP` method call triggers resolution.
    #[must_use]
    pub fn new(
        hub: Arc<ClientHub>,
        registry: Arc<dyn PlatformTypesRegistryApi>,
        vendor: String,
        required: bool,
    ) -> Self {
        Self {
            hub,
            registry,
            vendor,
            required,
            selector: GtsPluginSelector::new(),
            fallback: NoopIdpProvider,
        }
    }

    /// Resolve the underlying plugin via the types-registry catalogue
    /// then scoped `ClientHub` lookup. Returns `Some` only when both
    /// the catalogue selection AND the scoped registration are
    /// present; returns `None` (without caching) otherwise so the
    /// next call retries.
    ///
    /// `try_get_scoped` miss is logged at WARN through `or_else` for
    /// operator visibility; the wrapper still returns `None` so the
    /// IdP-trait methods surface the "unavailable" failure shape per
    /// `cfg.idp.required`.
    async fn resolve(&self) -> Option<Arc<dyn IdpPluginClient>> {
        // `get_or_init` caches only successful `Ok` returns — a
        // listing with no running candidate surfaces as `Err` and the
        // next call retries from scratch.
        let gts_id: Arc<str> = self
            .selector
            .get_or_init(|| async {
                let plugin_type_id = IdpPluginSpecV1::gts_type_id();
                let query =
                    ListEntitiesRequest::matching(GtsIdPattern::try_new(plugin_type_id.as_ref())?);
                // List every page or fail. Selection is process-wide and cached, so use the
                // platform API (P33), independent of whichever caller triggers it first.
                let instances = self
                    .registry
                    .list_instances(&PlatformSecurityContext::outbound_marker(), query)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("types-registry list_instances<IdpPluginSpecV1>: {e}")
                    })?;
                choose_registered_plugin_instance::<IdpPluginSpecV1, dyn IdpPluginClient>(
                    &self.hub,
                    &self.vendor,
                    instances
                        .iter()
                        .map(|instance| (instance.id.as_ref(), instance.content.as_ref())),
                )
                .map_err(|e| anyhow::anyhow!("choose_plugin_instance<IdpPluginSpecV1>: {e}"))
            })
            .await
            .map_err(|e| {
                tracing::warn!(
                    target: "am.idp.lazy",
                    vendor = %self.vendor,
                    error = %e,
                    "lazy IdP plugin resolution failed; will retry on next call"
                );
                e
            })
            .ok()?;

        let scope = ClientScope::gts_id(gts_id.as_ref());
        self.hub
            .try_get_scoped::<dyn IdpPluginClient>(&scope)
            .or_else(|| {
                tracing::warn!(
                    target: "am.idp.lazy",
                    vendor = %self.vendor,
                    gts_id = %gts_id,
                    "catalogue advertised plugin but no scoped IdpPluginClient is registered \
                     under this gts_id; will retry on next call"
                );
                None
            })
    }
}

#[async_trait]
impl IdpPluginClient for LazyIdpProvider {
    async fn provision_tenant(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionTenantRequest,
    ) -> Result<IdpProvisionResult, IdpProvisionFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.provision_tenant(ctx, req).await;
        }
        if self.required {
            // `CleanFailure` maps to `CanonicalError::ServiceUnavailable`
            // (HTTP 503) at the AM saga boundary — same shape clients
            // already retry on for transient IdP outages, which is the
            // correct treatment here: the plugin will appear once
            // types-registry settles.
            Err(IdpProvisionFailure::CleanFailure {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable; saga \
                     compensated, retry once types-registry catalogue settles",
                    self.vendor
                ),
            })
        } else {
            self.fallback.provision_tenant(ctx, req).await
        }
    }

    async fn deprovision_tenant(
        &self,
        ctx: &SecurityContext,
        req: &IdpDeprovisionTenantRequest,
    ) -> Result<(), IdpDeprovisionFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.deprovision_tenant(ctx, req).await;
        }
        if self.required {
            Err(IdpDeprovisionFailure::Retryable {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable; deprovision \
                     deferred to next retention / reaper tick",
                    self.vendor
                ),
            })
        } else {
            self.fallback.deprovision_tenant(ctx, req).await
        }
    }

    async fn provision_user(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.provision_user(ctx, req).await;
        }
        if self.required {
            Err(IdpUserOperationFailure::Unavailable {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable",
                    self.vendor
                ),
            })
        } else {
            self.fallback.provision_user(ctx, req).await
        }
    }

    async fn deprovision_user(
        &self,
        ctx: &SecurityContext,
        req: &IdpDeprovisionUserRequest,
    ) -> Result<(), IdpUserOperationFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.deprovision_user(ctx, req).await;
        }
        if self.required {
            Err(IdpUserOperationFailure::Unavailable {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable",
                    self.vendor
                ),
            })
        } else {
            self.fallback.deprovision_user(ctx, req).await
        }
    }

    async fn update_user(
        &self,
        ctx: &SecurityContext,
        req: &IdpUpdateUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.update_user(ctx, req).await;
        }
        if self.required {
            Err(IdpUserOperationFailure::Unavailable {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable",
                    self.vendor
                ),
            })
        } else {
            self.fallback.update_user(ctx, req).await
        }
    }

    async fn list_users(
        &self,
        ctx: &SecurityContext,
        req: &IdpListUsersRequest,
    ) -> Result<Page<IdpUser>, IdpUserOperationFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.list_users(ctx, req).await;
        }
        if self.required {
            Err(IdpUserOperationFailure::Unavailable {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable",
                    self.vendor
                ),
            })
        } else {
            self.fallback.list_users(ctx, req).await
        }
    }

    // ---- Service accounts ------------------------------------------
    //
    // These MUST be forwarded explicitly. `IdpPluginClient`'s
    // service-account methods carry default bodies returning
    // `UnsupportedOperation` so tenant/user-only adapters compile without
    // stubbing them — which means an override omitted HERE is not a compile
    // error, it silently answers 501 for every plugin, however capable.
    // `ServiceAccountService` holds this wrapper (not the resolved plugin),
    // so this is the only path the whole machine-identity REST surface has.

    async fn provision_service_account(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionServiceAccountRequest,
    ) -> Result<IdpServiceAccountCredentials, IdpServiceAccountFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.provision_service_account(ctx, req).await;
        }
        if self.required {
            // `CleanFailure` (503), not `Ambiguous`: nothing was sent, so
            // retrying the same request once the catalogue settles is safe.
            Err(IdpServiceAccountFailure::CleanFailure {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable; retry once \
                     types-registry catalogue settles",
                    self.vendor
                ),
            })
        } else {
            self.fallback.provision_service_account(ctx, req).await
        }
    }

    async fn rotate_service_account_secret(
        &self,
        ctx: &SecurityContext,
        req: &IdpRotateServiceAccountSecretRequest,
    ) -> Result<IdpServiceAccountCredentials, IdpServiceAccountFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.rotate_service_account_secret(ctx, req).await;
        }
        if self.required {
            Err(IdpServiceAccountFailure::CleanFailure {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable; retry once \
                     types-registry catalogue settles",
                    self.vendor
                ),
            })
        } else {
            self.fallback.rotate_service_account_secret(ctx, req).await
        }
    }

    async fn revoke_service_account(
        &self,
        ctx: &SecurityContext,
        req: &IdpRevokeServiceAccountRequest,
    ) -> Result<(), IdpServiceAccountFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.revoke_service_account(ctx, req).await;
        }
        if self.required {
            // NOT `NotFound`: AM treats that as success-equivalent, and an
            // unresolvable plugin has proven nothing about whether the
            // account exists. Reporting a successful revoke here would let a
            // live machine credential survive a caller's DELETE.
            Err(IdpServiceAccountFailure::CleanFailure {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable; retry once \
                     types-registry catalogue settles",
                    self.vendor
                ),
            })
        } else {
            self.fallback.revoke_service_account(ctx, req).await
        }
    }

    async fn list_service_accounts(
        &self,
        ctx: &SecurityContext,
        req: &IdpListServiceAccountsRequest,
    ) -> Result<Vec<IdpServiceAccountSummary>, IdpServiceAccountFailure> {
        if let Some(plugin) = self.resolve().await {
            return plugin.list_service_accounts(ctx, req).await;
        }
        if self.required {
            Err(IdpServiceAccountFailure::CleanFailure {
                detail: format!(
                    "idp provider plugin (vendor `{}`) not yet resolvable; retry once \
                     types-registry catalogue settles",
                    self.vendor
                ),
            })
        } else {
            self.fallback.list_service_accounts(ctx, req).await
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "lazy_tests.rs"]
mod lazy_tests;
