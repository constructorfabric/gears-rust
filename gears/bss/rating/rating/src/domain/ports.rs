//! Ports the foundation's domain depends on: the clock, the metrics port (DESIGN §4.7) and the
//! service grant of Rating's identity per tenant (DESIGN §4.8).

use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::inbox::source::InboxSource;

/// The time source of every stored instant.
pub trait Clock: Send + Sync {
    /// The current instant, truncated to the microsecond the database stores.
    fn now(&self) -> OffsetDateTime;
}

/// The dependency a `rating_dependency_errors_total` sample is about.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dependency {
    /// The PDP behind `PolicyEnforcer`.
    Pdp,
    /// The Cluster leader-election backend.
    Cluster,
    /// The types registry (GTS label registration, meter declarations).
    TypesRegistry,
    /// The usage-collector feed.
    UsageFeed,
    /// The tenant hierarchy (tenant-resolver), asked whether a tenant lies in a configured scope.
    TenantHierarchy,
    /// The database.
    Database,
}

impl Dependency {
    /// The `dependency` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pdp => "pdp",
            Self::Cluster => "cluster",
            Self::TypesRegistry => "types_registry",
            Self::UsageFeed => "usage_feed",
            Self::TenantHierarchy => "tenant_hierarchy",
            Self::Database => "database",
        }
    }
}

/// The `code` label of `rating_dependency_errors_total`: what failed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureCode {
    /// The leader election could not be resolved or joined, or it closed.
    LeadershipUnavailable,
    /// The PDP could not be evaluated.
    EvaluationFailed,
    /// A database statement or transaction failed.
    StorageFailed,
    /// A read of the dependency could not be answered.
    Unavailable,
}

impl FailureCode {
    /// The `code` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LeadershipUnavailable => "leadership_unavailable",
            Self::EvaluationFailed => "evaluation_failed",
            Self::StorageFailed => "storage_failed",
            Self::Unavailable => "unavailable",
        }
    }
}

/// What one exception sweep did; the sweeper returns it and the metrics port records it.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Tenants whose scope the sweep bound.
    pub tenants: usize,
    /// Subjects handed back to their retrier.
    pub retried: usize,
    /// Retries that failed (logged; tried again next sweep).
    pub failed: usize,
}

/// How an exception sweep ended: the `outcome` label of `rating_exception_sweeps_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SweepOutcome {
    /// Every open exception a retrier handles was visited.
    Completed,
    /// The term ended during the sweep.
    Cancelled,
    /// A page read or the PDP failed; the next sweep starts over.
    Failed,
}

impl SweepOutcome {
    /// The `outcome` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// The foundation's metrics (DESIGN §4.7). Every label value is one of a closed set; no tenant id
/// or other unbounded value is ever a label.
pub trait RatingMetricsPort: Send + Sync {
    /// `rating_inbox_quarantined_total{source}`: an inbox entry was quarantined (an alert pages on
    /// any increase).
    fn inbox_quarantined(&self, source: InboxSource);

    /// `rating_dependency_errors_total{dependency, code}`: a dependency failed.
    fn dependency_error(&self, dependency: Dependency, code: FailureCode);

    /// One exception sweep: `rating_exception_sweeps_total{outcome}`,
    /// `rating_exception_sweep_retries_total{result}` (`retried`, `failed`) and
    /// `rating_exception_sweep_duration_seconds`.
    fn exception_sweep(&self, outcome: SweepOutcome, report: &SweepReport, duration: Duration);
}

/// Metrics that go nowhere, for contexts with no meter.
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl RatingMetricsPort for NoopMetrics {
    fn inbox_quarantined(&self, _source: InboxSource) {}

    fn dependency_error(&self, _dependency: Dependency, _code: FailureCode) {}

    fn exception_sweep(&self, _outcome: SweepOutcome, _report: &SweepReport, _duration: Duration) {}
}

/// Rating's service grant per tenant (DESIGN §4.8): the scope background work and intake bind for
/// one tenant, or a refusal when the tenant is outside the grant.
#[async_trait]
pub trait TenantGrant: Send + Sync {
    /// The `AccessScope` of Rating's service identity narrowed to `tenant_id`.
    ///
    /// # Errors
    ///
    /// [`DomainError::OutsideGrant`] when the PDP does not grant the tenant;
    /// [`DomainError::AuthzUnavailable`] when the PDP cannot be evaluated.
    async fn scope_for(&self, tenant_id: Uuid) -> Result<AccessScope, DomainError>;
}
