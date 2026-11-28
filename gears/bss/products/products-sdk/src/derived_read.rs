//! The read of one stored derived usage declaration through `ClientHub` (P-D-277).
//!
//! [`DerivedUsageTypeReadV1`] answers one stored version of a derived usage type, named by its
//! meter id, in a catalog tenant the caller names: the declaration as stored, its stored digest,
//! and the canonical unit and accrual policy version a pricing usage policy names. It is the
//! in-process twin of `GET /bss-products/v1/derived-usage-types/{code}/versions/{n}` and answers
//! with the same refusals. It is a general read: any caller that holds `sku:read` in the catalog
//! tenant may use it.
//!
//! The declaration is [`crate::derived`]'s, so a consumer evaluates it with
//! [`crate::derived::evaluate`] and can check the digest against
//! [`crate::derived::canonical_bytes`].

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::derived::{DerivedUsageDeclaration, MeterId};

/// One stored version of a derived usage type, as [`DerivedUsageTypeReadV1::declaration`]
/// answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDerivedDeclaration {
    /// The meter id asked for: `products.derived/<code>@<n>`.
    pub meter: MeterId,
    /// The declaration as the version stores it: inputs at their exact versions, the formula and
    /// the output's unit, scale and rounding.
    pub declaration: DerivedUsageDeclaration,
    /// The STORED digest, 64 lowercase hex digits: the SHA-256 of the declaration's canonical
    /// bytes, taken once at the write. Nothing recomputes it, and the REST version read serves the
    /// same text.
    pub digest: String,
    /// The declaration's output unit, the unit a pricing usage policy names.
    pub canonical_unit: String,
    /// `derived-v1:<digest>`, the accrual policy version a pricing usage policy names.
    pub accrual_policy_version: String,
}

/// Reads one stored derived usage declaration in a named catalog tenant (P-D-277). Products
/// registers it in `ClientHub` at its init.
#[async_trait]
pub trait DerivedUsageTypeReadV1: Send + Sync {
    /// The version that `meter` names, in `catalog_tenant_id`.
    ///
    /// The caller is authorized by `sku:read` in `catalog_tenant_id` through the PDP, which may be
    /// a tenant other than the caller's own, for example a seller tenant read by a service homed at
    /// the platform root. A tenant the caller holds no grant in answers nothing.
    ///
    /// # Errors
    /// 403 for a caller with no subject or tenant, a nil catalog tenant, or a caller the PDP does
    /// not grant `sku:read` in `catalog_tenant_id`; 503 when the PDP or the store does not answer;
    /// 404 when the catalog tenant holds no type with the meter's code, or the type no such
    /// version; 500 for a stored row that does not read.
    async fn declaration(
        &self,
        ctx: &SecurityContext,
        catalog_tenant_id: Uuid,
        meter: &MeterId,
    ) -> Result<StoredDerivedDeclaration, CanonicalError>;
}
