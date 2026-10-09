//! Consumer-required immutable meter evidence. E1's authoritative provider is external.
pub use crate::terms::MeterRef;
use crate::{Digest, terms::Fold};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

/// An exact historical declaration, including source integration provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterSemantics {
    /// Exact requested usage identity and immutable version.
    pub meter: MeterRef,
    /// Canonical integrated quantity unit.
    pub canonical_unit: String,
    /// Declared additive fold.
    pub fold: Fold,
    /// Immutable source accrual definition.
    pub accrual_policy_version: String,
    /// Whether quantities already integrate the source over time.
    pub source_integrated: bool,
    /// Provider's immutable evidence digest, retained in validation audit.
    pub digest: Digest,
}

/// Authorized exact-version semantic reads; never substitute the latest declaration.
#[async_trait::async_trait]
pub trait UsageMeterSemanticsV1: Send + Sync {
    /// Resolve as the caller. Denial and outage remain canonical errors.
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        meter: MeterRef,
    ) -> Result<MeterSemantics, CanonicalError>;
}

/// An authoritative provider has not been registered; distinct from a configured outage.
#[derive(Debug, thiserror::Error)]
#[error("UsageMeterSemanticsV1 is unconfigured (external dependency E1)")]
pub struct UnconfiguredMeterSemantics;
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.pricing.plan.v1~"))]
struct PlanResource;
impl From<UnconfiguredMeterSemantics> for CanonicalError {
    fn from(_error: UnconfiguredMeterSemantics) -> Self {
        PlanResource::failed_precondition()
            .with_precondition_violation(
                "UsageMeterSemanticsV1",
                "UsageMeterSemanticsV1 is unconfigured (external dependency E1)",
                "UNCONFIGURED_DEPENDENCY",
            )
            .create()
    }
}
