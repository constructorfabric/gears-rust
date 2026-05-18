//! `RgResourceOwnershipChecker` — production
//! [`crate::domain::tenant::resource_checker::ResourceOwnershipChecker`]
//! wired against `resource_group_sdk::ResourceGroupClient` resolved
//! from `ClientHub`.
//!
//! Production binding for the soft-delete ownership probe (DESIGN §3.5):
//! `resource-group` is a hard `deps` of AM, so this checker is always
//! the one wired in production. The placeholder
//! [`crate::domain::tenant::resource_checker::InertResourceOwnershipChecker`]
//! is reserved for unit tests.
//!
//! ## Probe shape
//!
//! `delete_tenant` only needs a boolean ("does the child tenant own at
//! least one RG row?") to drive the [`DomainError::TenantHasResources`]
//! rejection. The implementation issues:
//!
//! ```text
//! list_groups(ctx, $top=1, $filter=tenant_id eq <child>)
//! ```
//!
//! and reports `1` when the page has any items, `0` otherwise. The
//! caller's [`SecurityContext`] is propagated so RG-side `AuthZ` +
//! `SecureORM` apply the parent's `AccessScope` (which already covers
//! descendants per RG PRD §4); the `OData` filter narrows the answer to
//! the *specific* child tenant rather than the whole reachable subtree
//! — without that filter an unfiltered `list_groups($top=1)` would
//! return non-empty whenever any sibling owns RG rows, over-blocking
//! soft-delete.
//!
//! ## Coordination
//!
//! The `tenant_id` filter field was added to RG's `GroupFilterField`
//! whitelist in cyberfabric/cyberware-rust#1626 (closed). RG ships
//! the field as the un-nested identifier `tenant_id` (not
//! `hierarchy/tenant_id`); this checker matches that wire shape.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use modkit_odata::ODataQuery;
use modkit_odata::ast::{CompareOperator, Expr, Value};
use modkit_security::SecurityContext;
use resource_group_sdk::ResourceGroupClient;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::tenant::resource_checker::ResourceOwnershipChecker;

/// `OData` identifier for the tenant-scope column on `ResourceGroup`.
///
/// Matches `GroupFilterField::TenantId` as exposed by RG SDK; RG
/// shipped the field with the un-nested name `tenant_id` per the
/// `#1626` whitelist work. The literal here keeps AM independent of
/// the SDK's enum re-exports.
const TENANT_ID_FIELD: &str = "tenant_id";

/// Default RG probe timeout (ms). Picked to be tight enough that a
/// hung RG never stalls a tenant soft-delete saga past the 503
/// fail-something boundary, and loose enough that healthy RG round
/// trips never spuriously time out. Mirrors the donor-branch default.
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 2_000;

/// Production resource-ownership checker backed by the Resource Group
/// SDK.
pub struct RgResourceOwnershipChecker {
    client: Arc<dyn ResourceGroupClient + Send + Sync>,
    probe_timeout: Duration,
}

impl RgResourceOwnershipChecker {
    /// Construct a new checker around an RG client resolved from
    /// `ClientHub`, using the backward-compatible default timeout.
    #[must_use]
    pub fn new(client: Arc<dyn ResourceGroupClient + Send + Sync>) -> Self {
        Self::with_timeout(client, DEFAULT_PROBE_TIMEOUT_MS)
    }

    /// Construct a checker with the configured probe timeout.
    #[must_use]
    pub fn with_timeout(
        client: Arc<dyn ResourceGroupClient + Send + Sync>,
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
    async fn count_ownership_links(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<u64, DomainError> {
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
        match tokio::time::timeout(self.probe_timeout, self.client.list_groups(ctx, &query)).await {
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
                // module whose error surface is curated and safe to
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
