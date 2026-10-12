//! Authorization abstraction for the control plane.
//!
//! `TenantOnlyAuthorizer` enforces the tenant boundary only (tests, safe default);
//! `PolicyEnforcerAuthorizer` (`infra::authz`) delegates per-type decisions to the
//! platform Authorization Service over `gts.cf.fstorage.file.type.v1~`.

use async_trait::async_trait;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Actions checked against a file's GTS type.
pub mod actions {
    pub const READ: &str = "read";
    pub const WRITE: &str = "write";
    pub const DELETE: &str = "delete";
    /// Cross-owner / tenant-wide policy and retention-rule administration.
    ///
    /// Distinct from `WRITE` so ordinary file access does not grant access to another
    /// subject's policy or retention rules; same resource type, distinct action.
    pub const ADMIN_POLICY: &str = "admin_policy";
}

/// Resolves an access decision into the [`AccessScope`] used for tenant-scoped
/// queries, or [`DomainError::Forbidden`] when access is denied.
#[async_trait]
pub trait Authorizer: Send + Sync {
    /// Authorize `action` on a file of `gts_file_type` (optionally a specific
    /// `file_id`) and return the scope to apply to the backing query.
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        gts_file_type: &str,
        file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError>;
}

/// Enforces only the tenant boundary: any authenticated caller may act within
/// their own tenant; cross-tenant access is impossible because every query is
/// scoped to `ctx.subject_tenant_id()`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Default)]
pub struct TenantOnlyAuthorizer;

#[async_trait]
impl Authorizer for TenantOnlyAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        _action: &str,
        _gts_file_type: &str,
        _file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}
