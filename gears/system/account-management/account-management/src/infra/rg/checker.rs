//! `RgResourceOwnershipChecker` — the built-in
//! [`crate::domain::tenant::resource_checker::ResourceOwnershipChecker`],
//! answering from the tenant's resource groups in `resource-group`.
//!
//! The gear wires it when no tenant-ownership plugin vendor is configured
//! (`tenant_ownership.vendor` unset); with a vendor configured the
//! plugin answers instead. The placeholder
//! [`crate::domain::tenant::resource_checker::InertResourceOwnershipChecker`]
//! is reserved for unit tests.
//!
//! ## Probe shape
//!
//! `delete_tenant` only needs a boolean ("does the tenant own at least one
//! RG group?") to drive the [`DomainError::TenantHasResources`] rejection.
//! The implementation issues:
//!
//! ```text
//! list_groups($top=1, $filter=tenant_id eq <tenant>)
//! ```
//!
//! and reports `1` when the page has any items, `0` otherwise.
//!
//! ## Caller-independent read
//!
//! The probe goes through [`ResourceGroupReadHierarchy`], RG's in-process
//! read surface that bypasses the `PolicyEnforcer`, not through the
//! PEP-gated `ResourceGroupClient`. Through the gated client the answer
//! depended on the deleting caller's PDP scope: a group the caller could
//! not see read as "no groups", so a tenant still owning groups (and the
//! resources inside them) was deleted, while the same tenant was refused
//! for a caller who could see them. The `tenant_id` filter alone selects
//! the tenant; the answer is a single boolean, and the tenant-level
//! `DELETE` authorization has already run, so nothing is disclosed. The
//! `SecurityContext` passed to RG is AM's system actor and serves audit
//! correlation only — RG ignores it on this surface.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use resource_group_sdk::ResourceGroupReadHierarchy;
use toolkit_odata::ODataQuery;
use toolkit_odata::ast::{CompareOperator, Expr, Value};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::system_actor::for_ownership_probe;
use crate::domain::tenant::resource_checker::ResourceOwnershipChecker;

/// `OData` identifier for the tenant-scope column on `ResourceGroup`.
///
/// Matches `GroupFilterField::TenantId` as exposed by RG SDK (the
/// un-nested identifier `tenant_id`, not `hierarchy/tenant_id`). The
/// literal here keeps AM independent of the SDK's enum re-exports.
const TENANT_ID_FIELD: &str = "tenant_id";

/// Default RG probe timeout (ms). Tight enough that a hung RG never
/// stalls a tenant soft-delete past the 503 boundary, loose enough that
/// healthy RG round trips never spuriously time out.
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 2_000;

/// Built-in resource-ownership checker backed by RG's un-gated read
/// surface.
pub struct RgResourceOwnershipChecker {
    client: Arc<dyn ResourceGroupReadHierarchy + Send + Sync>,
    probe_timeout: Duration,
}

impl RgResourceOwnershipChecker {
    /// Construct a new checker around RG's read client resolved from
    /// `ClientHub`, using the default timeout.
    #[must_use]
    pub fn new(client: Arc<dyn ResourceGroupReadHierarchy + Send + Sync>) -> Self {
        Self::with_timeout(client, DEFAULT_PROBE_TIMEOUT_MS)
    }

    /// Construct a checker with an explicit probe timeout.
    #[must_use]
    pub fn with_timeout(
        client: Arc<dyn ResourceGroupReadHierarchy + Send + Sync>,
        probe_timeout_ms: u64,
    ) -> Self {
        Self {
            client,
            probe_timeout: Duration::from_millis(probe_timeout_ms.max(1)),
        }
    }
}

#[async_trait]
impl ResourceOwnershipChecker for RgResourceOwnershipChecker {
    // @cpt-begin:cpt-cf-account-management-algo-tenant-hierarchy-management-soft-delete-preconditions:p1:inst-algo-sdelpc-rg-probe
    async fn count_ownership_links(&self, tenant_id: Uuid) -> Result<u64, DomainError> {
        // `$top=1` — we only need a boolean answer; the actual count is
        // never read by `delete_tenant`, which compares against zero.
        let query = ODataQuery::default()
            .with_limit(1)
            .with_filter(Expr::Compare(
                Box::new(Expr::Identifier(TENANT_ID_FIELD.to_owned())),
                CompareOperator::Eq,
                Box::new(Expr::Value(Value::Uuid(tenant_id))),
            ));
        // `am.dependency_health` mirrors the IdP-path emission shape
        // (`target` / `op` / `outcome`) so an RG outage shows up on
        // the unified dependency-health dashboard alongside IdP and
        // GTS, not only as a tracing line.
        // Audit-correlation identity only; the read surface ignores it.
        let ctx = for_ownership_probe(tenant_id);
        match tokio::time::timeout(self.probe_timeout, self.client.list_groups(&ctx, &query)).await
        {
            Err(_elapsed) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "resource_group"),
                        ("op", "list_groups"),
                        ("outcome", "timeout"),
                    ],
                );
                Err(DomainError::service_unavailable(
                    "resource-group: timeout exceeded",
                ))
            }
            Ok(Err(err)) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "resource_group"),
                        ("op", "list_groups"),
                        ("outcome", "error"),
                    ],
                );
                // `ResourceGroupError` Display is forwarded into the
                // detail unredacted (no `redact_provider_detail` here).
                // This is intentional: RG is a CF-internal sibling
                // gear whose error surface is curated and safe to
                // expose to the caller, in contrast to the `IdP`
                // plugin path (`account_management_sdk::idp`) where the
                // error text comes from third-party vendor SDKs and
                // must be redacted before crossing the AM boundary.
                Err(DomainError::service_unavailable(format!(
                    "resource-group: {err}"
                )))
            }
            Ok(Ok(page)) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "resource_group"),
                        ("op", "list_groups"),
                        ("outcome", "success"),
                    ],
                );
                Ok(u64::from(!page.items.is_empty()))
            }
        }
    }
    // @cpt-end:cpt-cf-account-management-algo-tenant-hierarchy-management-soft-delete-preconditions:p1:inst-algo-sdelpc-rg-probe
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "test helpers")]
#[path = "checker_tests.rs"]
mod checker_tests;
