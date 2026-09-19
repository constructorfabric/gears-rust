//! `From<DomainError> for CanonicalError`: the boundary lift of DESIGN
//! section 3.3, "Error Model". The HTTP status is a property of the canonical
//! category; fine-grained discriminators ride as reason tokens.
//!
//! Kept out of `domain/`: the lift names GTS resource types and, later, may
//! classify backend failures with `toolkit-db` helpers the domain layer must
//! not import.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::{DomainError, ResourceKind};

#[resource_error(gts_id!("cf.qe.resource.quota.v1~"))]
pub(crate) struct QuotaResource;

#[resource_error(gts_id!("cf.qe.resource.policy.v1~"))]
pub(crate) struct PolicyResource;

#[resource_error(gts_id!("cf.qe.resource.lease.v1~"))]
pub(crate) struct LeaseResource;

#[resource_error(gts_id!("cf.qe.resource.operation.v1~"))]
pub(crate) struct OperationResource;

/// Closed reason tokens of the canonical envelope.
pub mod reason {
    /// PDP denial or constraint compile failure.
    pub const AUTHZ: &str = "AUTHZ";
    /// Bootstrap has not completed.
    pub const NOT_READY: &str = "NOT_READY";
    /// A dependency is unreachable.
    pub const DEPENDENCY_UNAVAILABLE: &str = "DEPENDENCY_UNAVAILABLE";
    /// A reserved capability (`rate` Quotas in P1).
    pub const NOT_YET_IMPLEMENTED: &str = "NOT_YET_IMPLEMENTED";
    /// An engine Decision broke the closed Debit-Plan invariant set. Carried
    /// as the leading sub-token of an `Internal` detail (DESIGN section 3.3).
    pub const INVARIANT_VIOLATION: &str = "INVARIANT_VIOLATION";
}

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            // --- quota lifecycle, decided before storage (400 / 501) ---
            DomainError::CapMustBeNonNegative { .. }
            | DomainError::ThresholdsRequireBoundedCap
            | DomainError::ConstraintContractMismatch { .. }
            | DomainError::MetricClassificationInvalid { .. }
            | DomainError::NotYetImplemented { .. } => quota_lifecycle(err),

            DomainError::InvalidPolicy {
                field,
                reason,
                detail,
            } => PolicyResource::invalid_argument()
                .with_field_violation(field, detail, reason)
                .create(),

            // --- 400 InvalidArgument ---
            DomainError::InvalidArgument { field, reason } => QuotaResource::invalid_argument()
                .with_field_violation(field, format!("invalid argument {field}: {reason}"), reason)
                .create(),
            DomainError::ProjectionNotRegistered { projection } => {
                QuotaResource::invalid_argument()
                    .with_field_violation(
                        "projection_type",
                        format!("projection {projection} is not registered"),
                        "PROJECTION_NOT_REGISTERED",
                    )
                    .with_resource(projection)
                    .create()
            }

            // --- 400 FailedPrecondition ---
            DomainError::LeaseNotActive { token } => LeaseResource::failed_precondition()
                .with_precondition_violation(
                    token.to_string(),
                    format!("lease {token} is not active"),
                    "LEASE_NOT_ACTIVE",
                )
                .create(),
            DomainError::OverCommitNotAuthorized { reserved, actual } => {
                LeaseResource::failed_precondition()
                    .with_precondition_violation(
                        "actual_amount",
                        format!("commit amount {actual} exceeds reserved amount {reserved}"),
                        "OVER_COMMIT_NOT_AUTHORIZED",
                    )
                    .create()
            }
            DomainError::CapBelowConsumed { new_cap, consumed } => {
                QuotaResource::failed_precondition()
                    .with_precondition_violation(
                        "cap",
                        format!("cap {new_cap} is below the consumed amount {consumed}"),
                        "CAP_BELOW_CONSUMED",
                    )
                    .create()
            }
            DomainError::QuotaDeactivated { id } => QuotaResource::failed_precondition()
                .with_precondition_violation(
                    id.clone(),
                    format!("quota {id} is deactivated"),
                    "QUOTA_DEACTIVATED",
                )
                .create(),
            DomainError::PeriodClosed => QuotaResource::failed_precondition()
                .with_precondition_violation(
                    "period",
                    "the target period is closed",
                    "PERIOD_CLOSED",
                )
                .create(),
            DomainError::MetricNotRegistered { metric } => QuotaResource::failed_precondition()
                .with_precondition_violation(
                    metric.clone(),
                    format!("metric {metric} is not registered"),
                    "METRIC_NOT_REGISTERED",
                )
                .create(),
            DomainError::MetricNotQuotaGated { metric } => QuotaResource::failed_precondition()
                .with_precondition_violation(
                    metric.clone(),
                    format!("metric {metric} is not quota-gated"),
                    "METRIC_NOT_QUOTA_GATED",
                )
                .create(),
            DomainError::ProjectionNotResolvable { projection } => {
                QuotaResource::failed_precondition()
                    .with_precondition_violation(
                        "subject.projection_type",
                        format!(
                            "projection {projection} is not resolvable in the configured catalogue"
                        ),
                        DomainError::PROJECTION_NOT_RESOLVABLE,
                    )
                    .create()
            }
            // --- engine evaluation (504 / 429 / 500) ---
            DomainError::EngineTimeout { .. }
            | DomainError::EngineCostExceeded { .. }
            | DomainError::InvariantViolation { .. }
            | DomainError::EngineFailure { .. } => engine(err),

            // --- policy lifecycle, operator surface (404 / 409 / 400) ---
            DomainError::PolicyScopeOccupied { .. }
            | DomainError::PolicyNotFound { .. }
            | DomainError::PolicyDeleted { .. }
            | DomainError::CannotDeleteSeededGlobalPolicy
            | DomainError::UnknownPolicyVersion { .. }
            | DomainError::VersionRolledBack { .. } => policy(err),

            // --- 403 PermissionDenied (no PDP detail on the wire) ---
            DomainError::PdpDenied { .. } => QuotaResource::permission_denied()
                .with_reason(reason::AUTHZ)
                .create(),

            // --- 404 NotFound ---
            DomainError::NotFound { kind, id } => {
                let detail = format!("{kind} {id} not found");
                match kind {
                    ResourceKind::Quota => {
                        QuotaResource::not_found(detail).with_resource(id).create()
                    }
                    ResourceKind::Policy => {
                        PolicyResource::not_found(detail).with_resource(id).create()
                    }
                    ResourceKind::Lease => {
                        LeaseResource::not_found(detail).with_resource(id).create()
                    }
                    ResourceKind::Operation => OperationResource::not_found(detail)
                        .with_resource(id)
                        .create(),
                }
            }

            // --- 409 Aborted (safe to retry) ---
            DomainError::IdempotencyPayloadMismatch => {
                OperationResource::aborted("idempotency key replayed with a different payload")
                    .with_reason("IDEMPOTENCY_PAYLOAD_MISMATCH")
                    .create()
            }
            DomainError::VersionConflict { expected, actual } => PolicyResource::aborted(format!(
                "version conflict: expected {expected}, found {actual}"
            ))
            .with_reason("VERSION_CONFLICT")
            .create(),
            DomainError::LeaseContentionTimeout => {
                LeaseResource::aborted("acquisition contention timeout elapsed")
                    .with_reason("LEASE_CONTENTION_TIMEOUT")
                    .create()
            }

            // --- 429 ResourceExhausted ---
            DomainError::LeaseInflightLimitExceeded => LeaseResource::resource_exhausted(
                "active-lease cap reached for the (tenant, metric) pair",
            )
            .with_quota_violation("(tenant, metric)", "LEASE_INFLIGHT_LIMIT_EXCEEDED")
            .create(),

            // --- 503 ServiceUnavailable ---
            DomainError::NotReady { dependency } => CanonicalError::service_unavailable()
                .with_detail(format!(
                    "{}: quota enforcement is not ready ({dependency})",
                    reason::NOT_READY
                ))
                .create(),
            DomainError::PdpUnavailable(_) => CanonicalError::service_unavailable()
                .with_detail(format!(
                    "{}: authorization service unavailable",
                    reason::DEPENDENCY_UNAVAILABLE
                ))
                .create(),
            DomainError::BackendUnavailable(_) => CanonicalError::service_unavailable()
                .with_detail(format!(
                    "{}: storage backend unavailable",
                    reason::DEPENDENCY_UNAVAILABLE
                ))
                .create(),
            DomainError::TypesRegistryUnavailable(_) => CanonicalError::service_unavailable()
                .with_detail(format!(
                    "{}: types registry unavailable",
                    reason::DEPENDENCY_UNAVAILABLE
                ))
                .create(),
            DomainError::PluginNotFound { kind, .. }
            | DomainError::PluginClientNotRegistered { kind, .. }
            | DomainError::InvalidPluginInstance { kind, .. } => {
                CanonicalError::service_unavailable()
                    .with_detail(format!(
                        "{}: {kind} plugin unavailable",
                        reason::DEPENDENCY_UNAVAILABLE
                    ))
                    .create()
            }
            DomainError::ClusterUnavailable(_) => CanonicalError::service_unavailable()
                .with_detail(format!(
                    "{}: cluster unavailable",
                    reason::DEPENDENCY_UNAVAILABLE
                ))
                .create(),
            // A rejected catalogue never leaves bootstrap; the gear is not ready.
            DomainError::CatalogInvalid { .. } => CanonicalError::service_unavailable()
                .with_detail(format!(
                    "{}: projection contract catalogue rejected",
                    reason::NOT_READY
                ))
                .create(),

            // --- 500 Internal (opaque; never carries internal detail) ---
            DomainError::SchemaVersionMismatch { .. } | DomainError::Internal(_) => {
                CanonicalError::internal("internal error").create()
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "canonical_mapping_tests.rs"]
mod canonical_mapping_tests;

/// The lifts of the quota-lifecycle rejections decided before storage.
fn quota_lifecycle(err: DomainError) -> CanonicalError {
    match err {
        // --- 400 InvalidArgument ---
        DomainError::CapMustBeNonNegative { cap } => QuotaResource::invalid_argument()
            .with_field_violation(
                "cap",
                format!("cap {cap} is negative; caps live in 0..=i64::MAX"),
                "CAP_MUST_BE_NON_NEGATIVE",
            )
            .create(),
        // --- 400 FailedPrecondition ---
        DomainError::ThresholdsRequireBoundedCap => QuotaResource::failed_precondition()
            .with_precondition_violation(
                "notification_thresholds",
                "notification thresholds require a bounded cap",
                "THRESHOLDS_REQUIRE_BOUNDED_CAP",
            )
            .create(),
        DomainError::ConstraintContractMismatch { contract } => {
            QuotaResource::failed_precondition()
                .with_precondition_violation(
                    "metadata",
                    format!("metadata violates constraint contract {contract}"),
                    "CONSTRAINT_CONTRACT_MISMATCH",
                )
                .with_resource(contract)
                .create()
        }
        DomainError::MetricClassificationInvalid { metric } => QuotaResource::failed_precondition()
            .with_precondition_violation(
                metric.clone(),
                format!("metric {metric} carries no usable classification"),
                "METRIC_CLASSIFICATION_INVALID",
            )
            .create(),
        // --- 501 Unimplemented ---
        // The builder has no reason slot; the token leads the detail, as the
        // 503 lifts do.
        DomainError::NotYetImplemented { feature } => QuotaResource::unimplemented(format!(
            "{}: {feature} is not yet implemented",
            reason::NOT_YET_IMPLEMENTED
        ))
        .create(),
        other => CanonicalError::from(other),
    }
}

/// The engine-evaluation arms (DESIGN section 3.3). The response never carries
/// what the engine said: engine detail can quote operator config or request
/// values, so it goes to the log and the wire gets the class of failure.
fn engine(err: DomainError) -> CanonicalError {
    match err {
        DomainError::EngineTimeout { engine_id } => PolicyResource::deadline_exceeded(format!(
            "engine {engine_id} exceeded its evaluation budget"
        ))
        .create(),
        DomainError::EngineCostExceeded { engine_id } => PolicyResource::resource_exhausted(
            format!("engine {engine_id} exceeded its evaluation cost limit"),
        )
        .with_quota_violation("evaluation cost", "ENGINE_COST_EXCEEDED")
        .create(),
        // The token leads the detail, as the 501 lift does; the invariant name
        // is from the closed set of four and identifies no caller data.
        DomainError::InvariantViolation { invariant, .. } => {
            CanonicalError::internal(format!("{}: {invariant}", reason::INVARIANT_VIOLATION))
                .create()
        }
        DomainError::EngineFailure { .. } => {
            CanonicalError::internal("engine evaluation failed").create()
        }
        other => CanonicalError::from(other),
    }
}

/// The policy-lifecycle arms, split out to keep `From` inside the line budget.
fn policy(err: DomainError) -> CanonicalError {
    match err {
        DomainError::PolicyScopeOccupied { scope } => {
            PolicyResource::already_exists(format!("POLICY_SCOPE_OCCUPIED: {scope:?}"))
                .with_resource(format!("{scope:?}"))
                .create()
        }
        DomainError::PolicyNotFound { policy_id } => {
            PolicyResource::not_found(format!("policy {policy_id} not found"))
                .with_resource(policy_id.to_string())
                .create()
        }
        DomainError::PolicyDeleted { policy_id } => PolicyResource::failed_precondition()
            .with_precondition_violation(
                policy_id.to_string(),
                "policy is deleted",
                "POLICY_DELETED",
            )
            .create(),
        DomainError::CannotDeleteSeededGlobalPolicy => PolicyResource::failed_precondition()
            .with_precondition_violation(
                "global",
                "global policy cannot be deleted",
                "CANNOT_DELETE_SEEDED_GLOBAL_POLICY",
            )
            .create(),
        DomainError::UnknownPolicyVersion { policy_id, version } => {
            PolicyResource::failed_precondition()
                .with_precondition_violation(
                    format!("{policy_id}@{version}"),
                    format!("unknown version {version} of policy {policy_id}"),
                    "UNKNOWN_POLICY_VERSION",
                )
                .create()
        }
        DomainError::VersionRolledBack { policy_id, version } => {
            PolicyResource::failed_precondition()
                .with_precondition_violation(
                    format!("{policy_id}@{version}"),
                    format!("version {version} of policy {policy_id} was rolled back"),
                    "VERSION_ROLLED_BACK",
                )
                .create()
        }
        other => CanonicalError::from(other),
    }
}
