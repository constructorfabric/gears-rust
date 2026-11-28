//! Tenant-ownership plugin contract.
//!
//! Account Management refuses to soft-delete a tenant that still owns
//! something (`TENANT_HAS_RESOURCES`), and must clean up whatever is left
//! behind when the tenant is hard-deleted after its retention window. What
//! "owns something" means is product-specific: AM itself only knows about the
//! resource groups it can see in Resource Group, while a product such as a
//! resource-management gear knows which of its own records are still live.
//!
//! This module lets such a product answer both questions for AM. A product
//! registers an instance of [`crate::TenantOwnershipPluginSpecV1`] with its
//! vendor and binds a [`TenantOwnershipPluginClient`] under that instance's
//! scope; AM selects the instance whose vendor matches its configured
//! `tenant_ownership.vendor` on first need, the same way it selects its `IdP`
//! plugin. With no vendor configured, AM keeps its built-in check over the
//! tenant's resource groups and calls no plugin.
//!
//! # No caller identity, on purpose
//!
//! Neither method takes a `SecurityContext`. The answer to "does tenant T
//! still own anything?" must be the same whoever asks: a check that runs with
//! the deleting caller's visibility lets a caller who cannot see a tenant's
//! resources delete the tenant out from under them, and blocks a caller who
//! can. The plugin therefore answers from the full, unscoped view of its own
//! data. This is safe because the answer is a single boolean (no resource is
//! disclosed) and AM only asks after the caller has passed the tenant-level
//! delete authorization.

use async_trait::async_trait;
use uuid::Uuid;

/// Why a tenant-ownership plugin call did not produce an answer.
///
/// AM maps the variants onto its own lifecycle: during the soft-delete
/// precondition both variants surface as a retryable `service_unavailable`
/// (the delete is refused, nothing is written); during the hard-delete
/// cleanup [`Self::Retryable`] defers the tenant to the next retention tick
/// and [`Self::Terminal`] parks it for an operator.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TenantOwnershipFailure {
    /// Transient failure (storage or dependency outage, timeout); calling
    /// again later may succeed.
    #[error("tenant ownership plugin failed transiently: {detail}")]
    Retryable {
        /// Human-readable diagnostic, safe to log.
        detail: String,
    },
    /// The plugin cannot complete the call for this tenant without operator
    /// intervention (for example the cleanup finds the tenant still owns
    /// live resources).
    #[error("tenant ownership plugin cannot proceed: {detail}")]
    Terminal {
        /// Human-readable diagnostic, safe to log.
        detail: String,
    },
}

impl TenantOwnershipFailure {
    /// Stable, snake-case metric-label form of this variant.
    #[must_use]
    pub const fn as_metric_label(&self) -> &'static str {
        match self {
            Self::Retryable { .. } => "retryable",
            Self::Terminal { .. } => "terminal",
        }
    }
}

/// Product-supplied answers to AM's tenant-ownership questions.
///
/// Implementations MUST answer from their own data with full (unscoped)
/// visibility — see the module docs for why no caller identity is passed.
///
/// # `ClientHub` registration
///
/// A plugin registers itself as `Arc<dyn TenantOwnershipPluginClient>` under
/// `ClientScope::gts_id(<instance id>)`, where the instance id is the one it
/// publishes for [`crate::TenantOwnershipPluginSpecV1`]
/// (`toolkit::plugins::bind_plugin_instances` does both steps).
#[async_trait]
pub trait TenantOwnershipPluginClient: Send + Sync + 'static {
    /// Whether `tenant_id` still owns anything that must be removed before
    /// the tenant may be soft-deleted.
    ///
    /// Implementations SHOULD count work that is still in flight (for
    /// example a resource whose delete was requested but has not yet been
    /// confirmed by its provider) as owned, so a tenant is never deleted
    /// while a provider object may still exist.
    ///
    /// # Errors
    ///
    /// [`TenantOwnershipFailure`] when the answer cannot be determined; AM
    /// then refuses the delete with a retryable error rather than guessing.
    async fn owns_anything(&self, tenant_id: Uuid) -> Result<bool, TenantOwnershipFailure>;

    /// Remove the plugin's leftovers for a tenant that AM is hard-deleting.
    ///
    /// Called once per retention tick for a tenant whose retention window has
    /// elapsed, before AM removes the tenant row. The tenant is already in
    /// the `Deleted` state. The call MUST be idempotent: a lost claim or a
    /// failure of a later hook re-runs it, so "already gone" is success.
    ///
    /// # Errors
    ///
    /// [`TenantOwnershipFailure::Retryable`] defers the tenant to the next
    /// tick; [`TenantOwnershipFailure::Terminal`] parks it until an operator
    /// intervenes. Either way the tenant row is kept.
    async fn cleanup_tenant(&self, tenant_id: Uuid) -> Result<(), TenantOwnershipFailure>;
}
