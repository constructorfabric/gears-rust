//! Domain errors and their single mapping to `CanonicalError` (DESIGN §4
//! Error Handling).
//!
//! Every reason code and field reason comes from the SDK (`reason`, `field`),
//! and the two resource scopes carry the SDK resource types, so the
//! `ModelRegistryError` projection of a consumer sees exactly what this gear
//! emits.

use std::fmt;

use authz_resolver_sdk::EnforcerError;
use model_registry_sdk::{
    ApprovalState, field,
    reason::{aborted, permission},
};
use sea_orm::DbErr;
use tenant_resolver_sdk::TenantResolverError;
use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_db::DbError;
use toolkit_db::contention::is_retryable_contention;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Error scope of model-addressed outcomes; its literal equals
/// `model_registry_sdk::gts::MODEL_RESOURCE_TYPE`.
#[domain_model]
#[resource_error(gts_id!("cf.genai.model_registry.model.v1~"))]
pub struct ModelScope;

/// Error scope of provider-addressed outcomes; its literal equals
/// `model_registry_sdk::gts::PROVIDER_RESOURCE_TYPE`.
#[domain_model]
#[resource_error(gts_id!("cf.genai.model_registry.provider.v1~"))]
pub struct ProviderScope;

/// A dependency that could not answer.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dependency {
    TenantResolver,
    Pdp,
    TypesRegistry,
    CredStore,
    Database,
}

impl fmt::Display for Dependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TenantResolver => "tenant-resolver",
            Self::Pdp => "authz-resolver",
            Self::TypesRegistry => "types-registry",
            Self::CredStore => "credstore",
            Self::Database => "database",
        })
    }
}

/// The entity an error is about; selects the error scope.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    Provider,
    Model,
}

/// One field violation of an `invalid_argument` error.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub field: String,
    pub description: String,
    pub reason: field::Reason,
}

impl Violation {
    #[must_use]
    pub fn new(
        field: impl Into<String>,
        description: impl Into<String>,
        reason: field::Reason,
    ) -> Self {
        Self {
            field: field.into(),
            description: description.into(),
            reason,
        }
    }
}

/// Domain-level error of every catalog operation.
#[domain_model]
#[derive(Debug, Error)]
pub enum DomainError {
    /// Not in the effective catalog, malformed canonical id, or an unknown or
    /// invisible model id. `name` is the canonical id or the id as supplied.
    #[error("model not found: {name}")]
    ModelNotFound { name: String },

    #[error("provider not found: {id}")]
    ProviderNotFound { id: Uuid },

    #[error("provider of {canonical_id} is disabled")]
    ProviderDisabled { canonical_id: String },

    #[error("model {canonical_id} is not approved ({})", state.as_str())]
    ModelNotApproved {
        canonical_id: String,
        state: ApprovalState,
    },

    #[error("not permitted: {operation}")]
    NotPermitted {
        resource: Resource,
        operation: &'static str,
    },

    #[error("{operation} on inherited entry {id} is not allowed")]
    InheritedReadOnly {
        resource: Resource,
        operation: &'static str,
        id: Uuid,
    },

    #[error("provider slug already exists: {slug}")]
    ProviderAlreadyExists { slug: String },

    #[error("model already exists: {canonical_id}")]
    ModelAlreadyExists { canonical_id: String },

    /// `expected_version` does not match, or a resent field or the row set of
    /// a type migration changed concurrently (ADR-0004).
    #[error("optimistic lock failure on {id}: {detail}")]
    OptimisticLockFailure {
        resource: Resource,
        id: Uuid,
        detail: &'static str,
    },

    /// The provider's `gts_type` changed between settings validation and the
    /// model write.
    #[error("provider {provider_id} changed type during the write")]
    ProviderTypeChanged { provider_id: Uuid },

    #[error("invalid request: {} violation(s)", violations.len())]
    Invalid {
        resource: Resource,
        violations: Vec<Violation>,
    },

    #[error(transparent)]
    OData(toolkit_odata::Error),

    /// The dependency did not answer within its deadline or failed. The
    /// diagnostic stays in the logs.
    #[error("{dependency} unavailable: {diagnostic}")]
    DependencyUnavailable {
        dependency: Dependency,
        diagnostic: String,
    },

    #[error("internal error: {detail}")]
    Internal { detail: String },
}

impl DomainError {
    #[must_use]
    pub fn model_not_found(name: impl Into<String>) -> Self {
        Self::ModelNotFound { name: name.into() }
    }

    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn unavailable(dependency: Dependency, diagnostic: impl fmt::Display) -> Self {
        Self::DependencyUnavailable {
            dependency,
            diagnostic: diagnostic.to_string(),
        }
    }

    #[must_use]
    pub fn invalid(violation: Violation) -> Self {
        Self::invalid_all(vec![violation])
    }

    #[must_use]
    pub fn invalid_all(violations: Vec<Violation>) -> Self {
        Self::Invalid {
            resource: Resource::Model,
            violations,
        }
    }

    #[must_use]
    pub fn not_permitted(operation: &'static str) -> Self {
        Self::NotPermitted {
            resource: Resource::Model,
            operation,
        }
    }

    /// Scopes `NotPermitted` and `Invalid` to the provider resource type;
    /// provider operations apply it to every error they return.
    #[must_use]
    pub fn for_provider(mut self) -> Self {
        if let Self::NotPermitted { resource, .. } | Self::Invalid { resource, .. } = &mut self {
            *resource = Resource::Provider;
        }
        self
    }

    /// Enforcer outcome mapping of DESIGN §4 Authorization rule 5.
    #[must_use]
    pub fn from_enforcer(err: EnforcerError, operation: &'static str) -> Self {
        match err {
            EnforcerError::Denied { deny_reason } => {
                tracing::debug!(operation, ?deny_reason, "pdp denied");
                Self::not_permitted(operation)
            }
            EnforcerError::CompileFailed(e) => {
                tracing::warn!(operation, error = %e, "pdp constraints could not be compiled");
                Self::not_permitted(operation)
            }
            EnforcerError::EvaluationFailed(e) => Self::unavailable(Dependency::Pdp, e),
        }
    }

    /// Tenant Resolver outcome mapping of DESIGN §4 Error Handling.
    #[must_use]
    pub fn from_tenant_resolver(err: TenantResolverError, operation: &'static str) -> Self {
        match err {
            TenantResolverError::TenantNotFound { .. } | TenantResolverError::Unauthorized => {
                tracing::warn!(operation, error = %err, "tenant resolver refused the caller's tenant");
                Self::not_permitted(operation)
            }
            TenantResolverError::NoPluginAvailable
            | TenantResolverError::ServiceUnavailable(_)
            | TenantResolverError::Internal(_) => {
                Self::unavailable(Dependency::TenantResolver, err)
            }
        }
    }

    /// A database failure the pagination helper reports as text: the
    /// `Display` of the `DbErr`, whose connectivity kinds are recognized by
    /// their prefix.
    #[must_use]
    pub fn from_db_message(message: &str) -> Self {
        if CONNECTIVITY_PREFIXES.iter().any(|p| message.starts_with(p)) {
            return Self::unavailable(Dependency::Database, message);
        }
        Self::from_db_err(&DbErr::Custom(message.to_owned()))
    }

    /// Database error classification: connectivity, pool, timeouts, and
    /// contention are dependency failures; everything else is internal.
    #[must_use]
    pub fn from_db_err(err: &DbErr) -> Self {
        if is_unavailable_db_err(err) {
            return Self::unavailable(Dependency::Database, err);
        }
        Self::internal(format!("database error: {err}"))
    }
}

/// `Display` prefixes of `DbErr::Conn` and `DbErr::ConnectionAcquire`.
const CONNECTIVITY_PREFIXES: [&str; 2] = [
    "Connection Error: ",
    "Failed to acquire connection from pool: ",
];

fn is_unavailable_db_err(err: &DbErr) -> bool {
    if matches!(err, DbErr::Conn(_) | DbErr::ConnectionAcquire(_)) {
        return true;
    }
    let backends = [
        sea_orm::DbBackend::Postgres,
        sea_orm::DbBackend::Sqlite,
        sea_orm::DbBackend::MySql,
    ];
    if backends.iter().any(|b| is_retryable_contention(*b, err)) {
        return true;
    }
    let msg = err.to_string().to_lowercase();
    msg.contains("statement timeout")
        || msg.contains("canceling statement")
        || msg.contains("pool timed out")
        || msg.contains("timed out")
}

impl From<DbError> for DomainError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::Sea(e) => Self::from_db_err(&e),
            // Driver-level errors reach the gear only through connect and
            // pool failures, which are dependency failures.
            DbError::Sqlx(e) => Self::unavailable(Dependency::Database, e),
            other => Self::internal(format!("database error: {other}")),
        }
    }
}

impl From<toolkit_odata::Error> for DomainError {
    fn from(err: toolkit_odata::Error) -> Self {
        Self::OData(err)
    }
}

impl From<DomainError> for CanonicalError {
    // One flat match is the whole point of this mapping.
    #[allow(clippy::cognitive_complexity)]
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::ModelNotFound { name } => ModelScope::not_found("model not found")
                .with_resource(name)
                .create(),
            DomainError::ProviderNotFound { id } => ProviderScope::not_found("provider not found")
                .with_resource(id.to_string())
                .create(),
            DomainError::ProviderDisabled { canonical_id } => {
                tracing::debug!(canonical_id, "provider disabled");
                ModelScope::permission_denied()
                    .with_reason(permission::PROVIDER_DISABLED)
                    .create()
            }
            DomainError::ModelNotApproved {
                canonical_id,
                state,
            } => {
                tracing::debug!(canonical_id, state = state.as_str(), "model not approved");
                ModelScope::permission_denied()
                    .with_reason(permission::MODEL_NOT_APPROVED)
                    .create()
            }
            // Logged by the service as a security event.
            DomainError::NotPermitted { resource, .. } => match resource {
                Resource::Provider => ProviderScope::permission_denied()
                    .with_reason(permission::NOT_PERMITTED)
                    .create(),
                Resource::Model => ModelScope::permission_denied()
                    .with_reason(permission::NOT_PERMITTED)
                    .create(),
            },
            DomainError::InheritedReadOnly { resource, .. } => match resource {
                Resource::Provider => ProviderScope::permission_denied()
                    .with_reason(permission::INHERITED_READ_ONLY)
                    .create(),
                Resource::Model => ModelScope::permission_denied()
                    .with_reason(permission::INHERITED_READ_ONLY)
                    .create(),
            },
            DomainError::ProviderAlreadyExists { slug } => {
                ProviderScope::already_exists("provider slug already exists in the tenant")
                    .with_resource(slug)
                    .create()
            }
            DomainError::ModelAlreadyExists { canonical_id } => {
                ModelScope::already_exists("model already exists in the tenant")
                    .with_resource(canonical_id)
                    .create()
            }
            DomainError::OptimisticLockFailure {
                resource,
                id,
                detail,
            } => {
                tracing::debug!(%id, detail, "optimistic lock failure");
                match resource {
                    Resource::Provider => ProviderScope::aborted(detail)
                        .with_reason(aborted::OPTIMISTIC_LOCK_FAILURE)
                        .with_resource(id.to_string())
                        .create(),
                    Resource::Model => ModelScope::aborted(detail)
                        .with_reason(aborted::OPTIMISTIC_LOCK_FAILURE)
                        .with_resource(id.to_string())
                        .create(),
                }
            }
            DomainError::ProviderTypeChanged { provider_id } => {
                tracing::debug!(%provider_id, "provider type changed during the write");
                ModelScope::aborted("the provider type changed during the write")
                    .with_reason(aborted::PROVIDER_TYPE_CHANGED)
                    .create()
            }
            DomainError::Invalid {
                resource,
                violations,
            } => {
                let mut iter = violations.into_iter();
                let Some(first) = iter.next() else {
                    return match resource {
                        Resource::Provider => ProviderScope::invalid_argument()
                            .with_format("invalid request")
                            .create(),
                        Resource::Model => ModelScope::invalid_argument()
                            .with_format("invalid request")
                            .create(),
                    };
                };
                let mut builder = match resource {
                    Resource::Provider => ProviderScope::invalid_argument(),
                    Resource::Model => ModelScope::invalid_argument(),
                }
                .with_field_violation(
                    first.field,
                    first.description,
                    first.reason.as_wire(),
                );
                for v in iter {
                    builder =
                        builder.with_field_violation(v.field, v.description, v.reason.as_wire());
                }
                builder.create()
            }
            DomainError::OData(e) => e.into(),
            DomainError::DependencyUnavailable {
                dependency,
                diagnostic,
            } => {
                tracing::warn!(%dependency, diagnostic, "dependency unavailable");
                CanonicalError::service_unavailable()
                    .with_detail(format!("{dependency} unavailable"))
                    .create()
            }
            DomainError::Internal { detail } => {
                tracing::error!(detail, "model-registry internal error");
                CanonicalError::internal("internal error").create()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_registry_sdk::gts::{MODEL_RESOURCE_TYPE, PROVIDER_RESOURCE_TYPE};
    use model_registry_sdk::{ModelRegistryError, gts::Resource as SdkResource};
    use toolkit_canonical_errors::Problem;

    fn problem(err: DomainError) -> serde_json::Value {
        serde_json::to_value(Problem::from(CanonicalError::from(err))).unwrap()
    }

    #[test]
    fn scopes_carry_the_sdk_resource_types() {
        let json = problem(DomainError::model_not_found("openai::gpt-4o"));
        assert_eq!(json["context"]["resource_type"], MODEL_RESOURCE_TYPE);
        assert_eq!(json["context"]["resource_name"], "openai::gpt-4o");
        assert_eq!(json["status"], 404);

        let id = Uuid::new_v4();
        let json = problem(DomainError::ProviderNotFound { id });
        assert_eq!(json["context"]["resource_type"], PROVIDER_RESOURCE_TYPE);
        assert_eq!(json["context"]["resource_name"], id.to_string());
    }

    #[test]
    fn permission_reasons_are_the_sdk_constants() {
        let cases = [
            (
                DomainError::ProviderDisabled {
                    canonical_id: "a::b".into(),
                },
                permission::PROVIDER_DISABLED,
            ),
            (
                DomainError::ModelNotApproved {
                    canonical_id: "a::b".into(),
                    state: ApprovalState::Revoked,
                },
                permission::MODEL_NOT_APPROVED,
            ),
            (
                DomainError::not_permitted("resolve_model"),
                permission::NOT_PERMITTED,
            ),
            (
                DomainError::InheritedReadOnly {
                    resource: Resource::Model,
                    operation: "update_model",
                    id: Uuid::nil(),
                },
                permission::INHERITED_READ_ONLY,
            ),
        ];
        for (err, reason) in cases {
            let json = problem(err);
            assert_eq!(json["status"], 403);
            assert_eq!(json["context"]["reason"], reason);
        }
    }

    #[test]
    fn already_exists_projects_typed_resource() {
        let err =
            ModelRegistryError::from(CanonicalError::from(DomainError::ProviderAlreadyExists {
                slug: "openai".into(),
            }));
        assert!(matches!(
            err,
            ModelRegistryError::AlreadyExists {
                resource: SdkResource::Provider,
                ..
            }
        ));
        let err = ModelRegistryError::from(CanonicalError::from(DomainError::ModelAlreadyExists {
            canonical_id: "openai::gpt-4o".into(),
        }));
        assert!(matches!(
            err,
            ModelRegistryError::AlreadyExists {
                resource: SdkResource::Model,
                ..
            }
        ));
    }

    #[test]
    fn aborted_reasons_project_to_optimistic_lock_failure() {
        let id = Uuid::new_v4();
        for (err, reason, resource_type) in [
            (
                DomainError::OptimisticLockFailure {
                    resource: Resource::Provider,
                    id,
                    detail: "stale",
                },
                aborted::OPTIMISTIC_LOCK_FAILURE,
                PROVIDER_RESOURCE_TYPE,
            ),
            (
                DomainError::OptimisticLockFailure {
                    resource: Resource::Model,
                    id,
                    detail: "stale",
                },
                aborted::OPTIMISTIC_LOCK_FAILURE,
                MODEL_RESOURCE_TYPE,
            ),
            (
                DomainError::ProviderTypeChanged { provider_id: id },
                aborted::PROVIDER_TYPE_CHANGED,
                MODEL_RESOURCE_TYPE,
            ),
        ] {
            let canonical = CanonicalError::from(err);
            let json = serde_json::to_value(Problem::from(canonical.clone())).unwrap();
            assert_eq!(json["status"], 409);
            assert_eq!(json["context"]["reason"], reason);
            assert_eq!(json["context"]["resource_type"], resource_type);
            assert!(matches!(
                ModelRegistryError::from(canonical),
                ModelRegistryError::OptimisticLockFailure { .. }
            ));
        }
    }

    #[test]
    fn inherited_read_only_uses_the_entry_scope() {
        let json = problem(DomainError::InheritedReadOnly {
            resource: Resource::Provider,
            operation: "update_provider",
            id: Uuid::nil(),
        });
        assert_eq!(json["context"]["resource_type"], PROVIDER_RESOURCE_TYPE);
        assert_eq!(json["context"]["reason"], permission::INHERITED_READ_ONLY);
    }

    #[test]
    fn provider_operations_carry_the_provider_scope() {
        let json = problem(DomainError::not_permitted("update_provider").for_provider());
        assert_eq!(json["context"]["resource_type"], PROVIDER_RESOURCE_TYPE);
        assert_eq!(json["context"]["reason"], permission::NOT_PERMITTED);
        let json = problem(
            DomainError::invalid(Violation::new("slug", "d", field::Reason::ImmutableField))
                .for_provider(),
        );
        assert_eq!(json["context"]["resource_type"], PROVIDER_RESOURCE_TYPE);
        let json = problem(DomainError::not_permitted("update_model"));
        assert_eq!(json["context"]["resource_type"], MODEL_RESOURCE_TYPE);
        // Entry-addressed errors keep their own scope.
        let json = problem(DomainError::model_not_found("x").for_provider());
        assert_eq!(json["context"]["resource_type"], MODEL_RESOURCE_TYPE);
    }

    #[test]
    fn field_violations_carry_every_sdk_reason() {
        let violations: Vec<Violation> = field::Reason::ALL
            .iter()
            .map(|r| Violation::new("f", "d", *r))
            .collect();
        let json = problem(DomainError::invalid_all(violations));
        assert_eq!(json["status"], 400);
        let fv = json["context"]["field_violations"].as_array().unwrap();
        assert_eq!(fv.len(), field::Reason::ALL.len());
        for (v, r) in fv.iter().zip(field::Reason::ALL) {
            assert_eq!(v["reason"], r.as_wire());
            assert_eq!(v["field"], "f");
        }
    }

    #[test]
    fn dependency_unavailable_is_503_without_diagnostic() {
        let json = problem(DomainError::unavailable(
            Dependency::TenantResolver,
            "connection refused to 10.0.0.1",
        ));
        assert_eq!(json["status"], 503);
        assert!(!json["detail"].as_str().unwrap().contains("10.0.0.1"));
    }

    #[test]
    fn internal_strips_detail() {
        let json = problem(DomainError::internal("SELECT * FROM models failed"));
        assert_eq!(json["status"], 500);
        assert!(!json["detail"].as_str().unwrap().contains("SELECT"));
    }

    #[test]
    fn enforcer_mapping() {
        let e = DomainError::from_enforcer(EnforcerError::Denied { deny_reason: None }, "op");
        assert!(matches!(e, DomainError::NotPermitted { .. }));
        let e = DomainError::from_enforcer(
            EnforcerError::EvaluationFailed(CanonicalError::service_unavailable().create()),
            "op",
        );
        assert!(matches!(
            e,
            DomainError::DependencyUnavailable {
                dependency: Dependency::Pdp,
                ..
            }
        ));
    }

    #[test]
    fn tenant_resolver_mapping() {
        let e = DomainError::from_tenant_resolver(TenantResolverError::Unauthorized, "op");
        assert!(matches!(e, DomainError::NotPermitted { .. }));
        let e = DomainError::from_tenant_resolver(
            TenantResolverError::TenantNotFound {
                tenant_id: tenant_resolver_sdk::TenantId(Uuid::nil()),
            },
            "op",
        );
        assert!(matches!(e, DomainError::NotPermitted { .. }));
        for err in [
            TenantResolverError::NoPluginAvailable,
            TenantResolverError::ServiceUnavailable("x".into()),
            TenantResolverError::Internal("x".into()),
        ] {
            let e = DomainError::from_tenant_resolver(err, "op");
            assert!(matches!(
                e,
                DomainError::DependencyUnavailable {
                    dependency: Dependency::TenantResolver,
                    ..
                }
            ));
        }
    }

    #[test]
    fn db_classification() {
        let e = DomainError::from_db_err(&DbErr::Custom("deadlock detected".into()));
        assert!(matches!(
            e,
            DomainError::DependencyUnavailable {
                dependency: Dependency::Database,
                ..
            }
        ));
        let e = DomainError::from_db_err(&DbErr::Custom("syntax error".into()));
        assert!(matches!(e, DomainError::Internal { .. }));
        // A list page reports its failure as text; the connectivity kinds
        // keep their classification.
        for err in [
            DbErr::Conn(sea_orm::RuntimeErr::Internal("connection reset".into())),
            DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::ConnectionClosed),
        ] {
            let e = DomainError::from_db_message(&err.to_string());
            assert!(
                matches!(
                    e,
                    DomainError::DependencyUnavailable {
                        dependency: Dependency::Database,
                        ..
                    }
                ),
                "{err}"
            );
        }
        let e = DomainError::from_db_message("syntax error");
        assert!(matches!(e, DomainError::Internal { .. }));
        let e = DomainError::from_db_err(&DbErr::Custom(
            "canceling statement due to statement timeout".into(),
        ));
        assert!(matches!(e, DomainError::DependencyUnavailable { .. }));
    }

    #[test]
    fn odata_errors_are_invalid_argument() {
        let json = problem(DomainError::OData(toolkit_odata::Error::InvalidCursor));
        assert_eq!(json["status"], 400);
    }
}
