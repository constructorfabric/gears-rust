//! Domain errors and their canonical mapping (DESIGN §3.3, Error model).

use license_enforcement_sdk::error::{PRECONDITION_TYPE, aborted, precondition};
use license_enforcement_sdk::{DelegationId, GrantId, RejectionReason};
use toolkit_canonical_errors::{CanonicalError, FieldViolation, resource_error};
use toolkit_macros::domain_model;

#[resource_error(gts_id!("cf.core.license_enforcement.grant.v1~"))]
pub(crate) struct GrantResourceError;

#[resource_error(gts_id!("cf.core.license_enforcement.pack.v1~"))]
struct PackResourceError;

#[resource_error(gts_id!("cf.core.license_enforcement.delegation.v1~"))]
struct DelegationResourceError;

/// What a [`DomainError::NotFound`] did not find.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    Grant,
    Pack,
    Delegation,
}

#[domain_model]
#[derive(thiserror::Error, Debug)]
pub enum DomainError {
    #[error("{what:?} {id} not found")]
    NotFound { what: Missing, id: String },

    #[error("permission denied")]
    PermissionDenied,

    #[error("invalid argument: {} violation(s)", .0.len())]
    InvalidArgument(Vec<FieldViolation>),

    /// A query that does not fit the fields of the gear; mapped as the `OData`
    /// extractor maps it, with the field and reason that `toolkit-odata` names.
    #[error("invalid query: {0}")]
    Query(toolkit_odata::Error),

    #[error("invalid transition: {0}")]
    InvalidTransition(String),

    /// Replacement cannot carry the bindings of these grants.
    #[error("{} binding(s) without a successor", .0.len())]
    /// Each grant whose binding has no successor, with the delegation in its
    /// chain that does not fit the successor, when one is the cause.
    BindingsWithoutSuccessor(Vec<(GrantId, Option<DelegationId>)>),

    #[error("expected_version differs")]
    VersionMismatch,

    #[error("a concurrent write changed the plugin material")]
    AdmissionConflict,

    #[error("a concurrent replacement moved a tree")]
    TreeMoved,

    /// The plugin admitted a root whose id is already stored: a retry admits
    /// it again.
    #[error("root {0} is already stored")]
    RootAlreadyAdmitted(GrantId),

    #[error("idempotency token reused with a different payload")]
    IdempotencyConflict,

    #[error("evidence rejected: {0}")]
    EvidenceRejected(RejectionReason),

    /// `precondition::REPLACED_EVIDENCE_MISSING` or `EVIDENCE_ALREADY_REPLACED`.
    #[error("evidence chain broken: {0}")]
    EvidenceChainBroken(&'static str),

    #[error("the linked plugin does not serve the intake of this operation")]
    IntakeNotServed,

    /// `renew` changes the stored window, and a plugin that verifies evidence
    /// takes the window from the evidence: the renewal would not apply.
    #[error("renew in place needs a plugin that does not verify evidence")]
    RenewNeedsNewEvidence,

    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),

    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),

    /// The caller's credential was refused (a dependency's 401 for a
    /// request's read). Not an outage: a retry does not cure it.
    #[error("unauthenticated")]
    Unauthenticated,

    #[error("internal error: {0}")]
    Internal(String),
}

impl DomainError {
    pub fn invalid(
        field: impl Into<String>,
        description: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::InvalidArgument(vec![FieldViolation::new(field, description, reason)])
    }

    pub fn transition(detail: impl Into<String>) -> Self {
        Self::InvalidTransition(detail.into())
    }

    pub fn store(e: impl std::fmt::Display) -> Self {
        Self::DependencyUnavailable(format!("store: {e}"))
    }
}

impl From<toolkit_db::DbError> for DomainError {
    fn from(e: toolkit_db::DbError) -> Self {
        Self::store(e)
    }
}

impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        match e {
            authz_resolver_sdk::EnforcerError::AccessDenied(_) => Self::PermissionDenied,
            // Fail closed as a deny, but log it: a permit whose constraints do
            // not compile is a PDP misconfiguration, not a decision.
            authz_resolver_sdk::EnforcerError::CompileFailed(e) => {
                tracing::warn!(error = %e, "authz constraint compilation failed; denied");
                Self::PermissionDenied
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(e) => {
                Self::DependencyUnavailable(format!("PDP: {e}"))
            }
            // An unknown outcome class is an internal failure, never a denial.
            unknown => {
                tracing::error!(error = %unknown, "authz failed with an unknown outcome");
                Self::Internal("authorization failed with an unknown outcome".to_owned())
            }
        }
    }
}

impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::NotFound { what, id } => match what {
                Missing::Grant => GrantResourceError::not_found(format!("grant {id} not found"))
                    .with_resource(id)
                    .create(),
                Missing::Pack => PackResourceError::not_found(format!("pack {id} not found"))
                    .with_resource(id)
                    .create(),
                Missing::Delegation => {
                    DelegationResourceError::not_found(format!("delegation {id} not found"))
                        .with_resource(id)
                        .create()
                }
            },
            DomainError::PermissionDenied => GrantResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),
            // The listener's token, never a dependency's text.
            DomainError::Unauthenticated => CanonicalError::unauthenticated()
                .with_reason("AUTHN_FAILED")
                .create(),
            DomainError::InvalidArgument(violations) => {
                let mut violations = violations.into_iter();
                let Some(first) = violations.next() else {
                    return CanonicalError::internal("empty violation list").create();
                };
                let mut builder = GrantResourceError::invalid_argument().with_field_violation(
                    first.field,
                    first.description,
                    first.reason,
                );
                for v in violations {
                    builder = builder.with_field_violation(v.field, v.description, v.reason);
                }
                builder.create()
            }
            DomainError::Query(e) => e.into(),
            DomainError::InvalidTransition(detail) => GrantResourceError::failed_precondition()
                .with_precondition_violation(
                    precondition::INVALID_TRANSITION,
                    detail,
                    PRECONDITION_TYPE,
                )
                .create(),
            DomainError::BindingsWithoutSuccessor(ids) => {
                let mut ids = ids.into_iter();
                let Some(first) = ids.next() else {
                    return CanonicalError::internal("empty binding list").create();
                };
                let violation = |(id, blocking): (GrantId, Option<DelegationId>)| {
                    let detail = match blocking {
                        Some(d) => format!(
                            "the binding of grant {id} has no successor: delegation {d} does not fit it"
                        ),
                        None => format!("the binding of grant {id} has no successor"),
                    };
                    (
                        precondition::BINDING_WITHOUT_SUCCESSOR,
                        detail,
                        PRECONDITION_TYPE,
                    )
                };
                let (s, d, t) = violation(first);
                let mut builder =
                    GrantResourceError::failed_precondition().with_precondition_violation(s, d, t);
                for v in ids {
                    let (s, d, t) = violation(v);
                    builder = builder.with_precondition_violation(s, d, t);
                }
                builder.create()
            }
            DomainError::VersionMismatch => {
                GrantResourceError::aborted("expected_version differs from the current version")
                    .with_reason(aborted::OPTIMISTIC_LOCK_FAILURE)
                    .create()
            }
            DomainError::AdmissionConflict => GrantResourceError::aborted(
                "a concurrent admission changed the plugin material; retry with the same token",
            )
            .with_reason(aborted::ADMISSION_CONFLICT)
            .create(),
            DomainError::TreeMoved => GrantResourceError::aborted(
                "a concurrent replacement moved a tree; retry with the same token",
            )
            .with_reason(aborted::TREE_MOVED)
            .create(),
            DomainError::RootAlreadyAdmitted(id) => GrantResourceError::already_exists(
                "the evidence names a root that is already stored",
            )
            .with_resource(id.to_string())
            .create(),
            DomainError::IdempotencyConflict => GrantResourceError::already_exists(
                "the idempotency token was used with a different payload",
            )
            .with_resource("idempotency_token")
            .create(),
            DomainError::EvidenceRejected(reason) => GrantResourceError::invalid_argument()
                .with_field_violation("evidence", reason.to_string(), reason.reason())
                .create(),
            DomainError::EvidenceChainBroken(code) => GrantResourceError::failed_precondition()
                .with_precondition_violation(
                    code,
                    "the evidence that this evidence replaces is missing or already replaced",
                    PRECONDITION_TYPE,
                )
                .create(),
            DomainError::IntakeNotServed => GrantResourceError::unimplemented(
                "the linked admission plugin does not serve the intake of this operation",
            )
            .create(),
            DomainError::RenewNeedsNewEvidence => GrantResourceError::unimplemented(
                "renew in place needs an admission plugin that does not verify evidence; \
                 new evidence replaces the grant: `replace` when the plugin also admits \
                 `issue` requests, evidence supersession (not implemented yet) otherwise",
            )
            .create(),
            DomainError::NotImplemented(op) => {
                GrantResourceError::unimplemented(format!("{op} is not implemented yet")).create()
            }
            DomainError::DependencyUnavailable(detail) => {
                tracing::warn!(%detail, "dependency unavailable");
                CanonicalError::service_unavailable().create()
            }
            DomainError::Internal(detail) => {
                tracing::error!(%detail, "internal error");
                CanonicalError::internal("internal error").create()
            }
        }
    }
}
