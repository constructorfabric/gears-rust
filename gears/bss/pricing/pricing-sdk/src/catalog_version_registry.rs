//! The `CatalogVersion` registry contract (`CatalogVersionRegistryV1`).
//!
//! The Product & SKU registry is the **sole** incrementer of `CatalogVersion`:
//! this gear requests addressability at publish and is told, later, which
//! committed version its publish landed in. The registry MAY batch approved
//! publishes, so the request returns a **pending** ref and the commit arrives
//! asynchronously — the shape the design set fixes (`design/01-foundation.md`
//! §3.6, D-47).
//!
//! The contract lives here, in the catalog's own SDK, because the registry gear
//! has no code in this repository yet. That is a temporary asymmetry, not a
//! claim of ownership: when the registry publishes its own SDK this trait
//! becomes an adapter over it. What must not happen in the meantime is the
//! catalog inventing versions locally — it would become a second incrementer,
//! and two incrementers make `CatalogVersion` unordered.
//!
//! The default [`UnconfiguredCatalogVersionRegistryV1`] is fail-closed: with no
//! registry wired, publish stops rather than producing rows no consumer can
//! address.

use async_trait::async_trait;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_security::SecurityContext;

use crate::catalog_version::CatalogVersion;

/// The two registry ports file their canonical errors under one identity —
/// [`product_catalog`](crate::product_catalog)'s, because they are the same
/// relationship seen from two sides and a reader matching on the type URI must
/// not have to know which side raised.
///
/// Not the **plan** label. The two dispositions that carry a resource at all —
/// [`unconfigured_registry`], which reports that no registry is wired, and
/// [`registry_rejected`], which reports the registry's own refusal of a version
/// — are about a dependency this gear does not own, and filing them under the
/// authoring data plane makes each read as a fault of the plan the caller was
/// publishing. ([`registry_unreachable`] carries no resource: it is a bare
/// `service_unavailable`, so it is untouched by the choice either way.)
///
/// That neither declared label actually *names* the registry dependency is the
/// design set's to settle (§05's table declares the labels); borrowing the
/// nearer of the two is what this crate can do without minting vocabulary of
/// its own.

#[resource_error(gts_id!("cf.bss.pricing.config.v1~"))]
struct RegistryResource;

/// A registry-side acknowledgement that a publish will be assigned a version.
///
/// The `request_id` is the catalog's own idempotency handle: re-requesting with
/// the same id after a crash must not enqueue a second publish for versioning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingVersionRef {
    /// The catalog-supplied request id this pending ref answers.
    pub request_id: String,
    /// The registry's handle for the pending assignment, carried on
    /// `PlanPublished` and replaced by the committed version when
    /// `CatalogVersionPublished` fires.
    pub pending_ref: String,
}

/// The wire-string discriminator that tells the registry's **refusal** apart from
/// its **outage** after the canonical round trip.
///
/// A refusal is a decision and will be made identically for as long as the
/// request is unchanged; an outage is a deployment state a retry may find
/// changed. Both are `FailedPrecondition`-shaped at the boundary only because the
/// refusal is, so the constant is what carries the difference — see
/// [`CatalogVersionRegistryError::Rejected`].
pub const CATALOG_VERSION_REJECTED: &str = "CATALOG_VERSION_REJECTED";

/// A registry failure — the typed view over [`CanonicalError`].
///
/// Opt-in: the trait contract is canonical, and this projection exists because
/// the gear dispatches on the refusal/outage split rather than propagating.
/// Semantic — never an HTTP status here; the gear maps it to its fail-closed
/// publish rejection.
#[derive(Debug, thiserror::Error)]
pub enum CatalogVersionRegistryError {
    /// No registry client is wired.
    #[error("no catalog-version registry configured")]
    Unconfigured,
    /// The registry could not be reached.
    #[error("registry unreachable: {0}")]
    Unreachable(String),
    /// The registry refused the request (an unknown SKU, a closed version).
    #[error("registry rejected the request: {0}")]
    Rejected(String),
    /// Anything else, preserved verbatim for the operator.
    #[error("internal: {0}")]
    Internal(String),
    /// A category this port does not emit today, carried whole.
    #[error("registry error: {0}")]
    Other(CanonicalError),
}

impl From<CanonicalError> for CatalogVersionRegistryError {
    fn from(err: CanonicalError) -> Self {
        let detail = err.detail().to_owned();
        match &err {
            CanonicalError::Unimplemented { .. } => Self::Unconfigured,
            CanonicalError::ServiceUnavailable { .. } => Self::Unreachable(detail),
            CanonicalError::Internal { .. } => Self::Internal(detail),
            // Matched on the violation type rather than on the category alone:
            // `FailedPrecondition` is a shape the registry could raise for
            // something other than a refusal, and folding those onto `Rejected`
            // would hand the gear a 400 for a fact it never decided.
            //
            // The sentence is taken from the **violation**, not from the
            // envelope's `detail`: [`registry_rejected`] puts the registry's own
            // words there, and reading `detail` instead loses the SKU the 400
            // exists to name — which is the whole difference between a 400 the
            // caller can act on and one it can only retry.
            CanonicalError::FailedPrecondition { ctx, .. } => ctx
                .violations
                .iter()
                .find(|v| v.type_ == CATALOG_VERSION_REJECTED)
                .map_or_else(
                    || Self::Other(err.clone()),
                    |v| Self::Rejected(v.description.clone()),
                ),
            _ => Self::Other(err),
        }
    }
}

/// The canonical error every implementation raises when no registry is wired.
#[must_use]
pub fn unconfigured_registry() -> CanonicalError {
    RegistryResource::unimplemented("no catalog-version registry configured").create()
}

/// The canonical error an implementation raises when the registry did not answer.
#[must_use]
pub fn registry_unreachable(detail: impl Into<String>) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(detail)
        .create()
}

/// The canonical error an implementation raises for the registry's own refusal.
#[must_use]
pub fn registry_rejected(detail: impl Into<String>) -> CanonicalError {
    RegistryResource::failed_precondition()
        .with_precondition_violation("catalog_version", detail, CATALOG_VERSION_REJECTED)
        .create()
}

/// The `CatalogVersion` increment contract
/// (`cpt-cf-bss-pricing-contract-registry-catalogversion`).
#[async_trait]
pub trait CatalogVersionRegistryV1: Send + Sync {
    /// Request addressability for a committed publish.
    ///
    /// Idempotent on `request_id`: a retry after a crash returns the same
    /// pending ref rather than queueing a second assignment.
    ///
    /// # Errors
    /// [`CanonicalError`] when no registry is configured, it is
    /// unreachable, or it refuses the request. Project with
    /// [`CatalogVersionRegistryError::from`] to tell a refusal from an outage. Every case blocks the publish
    /// from becoming consumer-visible; none of them is retried into a locally
    /// invented version.
    async fn request_version(
        &self,
        ctx: &SecurityContext,
        request_id: &str,
    ) -> Result<PendingVersionRef, CanonicalError>;

    /// Resolve a pending ref to its committed version, or `None` while the
    /// registry has not yet committed the batch it belongs to.
    ///
    /// # Errors
    /// [`CanonicalError`] when the registry is unavailable. A
    /// pending ref that stays unresolved past the batching SLO is an alarm, not
    /// an error here — the caller decides that, since only it knows how long
    /// the ref has been outstanding.
    async fn committed_version(
        &self,
        ctx: &SecurityContext,
        pending_ref: &str,
    ) -> Result<Option<CatalogVersion>, CanonicalError>;
}

/// Fail-safe default until the registry gear is wired: every request fails, so
/// no publish becomes addressable and none is silently assigned a version this
/// gear made up. Mirrors the sibling ledger's `UnconfiguredRateProviderV1`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredCatalogVersionRegistryV1;

#[async_trait]
impl CatalogVersionRegistryV1 for UnconfiguredCatalogVersionRegistryV1 {
    async fn request_version(
        &self,
        _ctx: &SecurityContext,
        _request_id: &str,
    ) -> Result<PendingVersionRef, CanonicalError> {
        Err(unconfigured_registry())
    }

    async fn committed_version(
        &self,
        _ctx: &SecurityContext,
        _pending_ref: &str,
    ) -> Result<Option<CatalogVersion>, CanonicalError> {
        Err(unconfigured_registry())
    }
}
