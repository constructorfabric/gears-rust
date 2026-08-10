//! The service-principal SPI trait, resolved via `ClientHub`.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::ServicePrincipalFailure;
use crate::models::{
    CreateServicePrincipalRequest, ServicePrincipalCredentials, ServicePrincipalSummary, TenantId,
};

/// Lifecycle of tenant-scoped machine identities (confidential OAuth
/// `client_credentials` clients).
///
/// Contract:
/// - Callers are trusted platform modules; caller authorization
///   (including parent→child tenant subtree semantics) happens in the
///   consumer's RBAC/PDP BEFORE calling. `ctx` is for audit.
/// - `(tenant_id, client_id)` is the scoped resource address; an
///   address that does not resolve within the tenant yields `NotFound`.
/// - The secret is returned only by `create`/`rotate_secret`. Persist
///   it immediately (credstore); a lost secret is recovered by rotate.
/// - Calls run outside any DB transaction. The adapter owns transport
///   resilience and reports transport uncertainty as `Ambiguous`,
///   never as success.
/// - Deployments without an adapter simply have no `ClientHub`
///   registration — `get::<dyn ServicePrincipalClientV1>()` fails.
#[async_trait]
pub trait ServicePrincipalClientV1: Send + Sync + 'static {
    /// Create a confidential `client_credentials`-only client owned by
    /// `req.tenant_id`. Tokens carry `tenant_id` and a service-subject
    /// `user_type`.
    ///
    /// A taken name yields `InvalidInput` — including a half-created
    /// principal left behind by an earlier `Ambiguous` failure; recover
    /// via `revoke` + `create`. Principals are deleted when their owning
    /// tenant is deprovisioned.
    async fn create(
        &self,
        ctx: &SecurityContext,
        req: &CreateServicePrincipalRequest,
    ) -> Result<ServicePrincipalCredentials, ServicePrincipalFailure>;

    /// Regenerate the secret; the old one stops working.
    async fn rotate_secret(
        &self,
        ctx: &SecurityContext,
        tenant_id: TenantId,
        client_id: &str,
    ) -> Result<ServicePrincipalCredentials, ServicePrincipalFailure>;

    /// Delete the client. Repeat revokes yield `NotFound`, which callers treat as success-equivalent.
    async fn revoke(
        &self,
        ctx: &SecurityContext,
        tenant_id: TenantId,
        client_id: &str,
    ) -> Result<(), ServicePrincipalFailure>;

    /// List the tenant's service principals (no secrets). Backs audit
    /// and future management surfaces.
    async fn list(
        &self,
        ctx: &SecurityContext,
        tenant_id: TenantId,
    ) -> Result<Vec<ServicePrincipalSummary>, ServicePrincipalFailure>;
}
