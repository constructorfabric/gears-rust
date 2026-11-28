//! Authoritative mapping from domain rejections to canonical errors.
use crate::domain::error::DomainError;
use crate::domain::validation::ValidationReport;
use toolkit::api::canonical_prelude::{CanonicalError, resource_error};

#[resource_error(gts_id!("cf.bss.products.sku.v1~"))]
struct SkuResource;
#[resource_error(gts_id!("cf.bss.products.category.v1~"))]
struct CategoryResource;
#[resource_error(gts_id!("cf.bss.products.approval_unit.v1~"))]
struct ApprovalUnitResource;
#[resource_error(gts_id!("cf.bss.products.derived_usage_type.v1~"))]
struct DerivedUsageTypeResource;
#[resource_error(gts_id!("cf.bss.products.quota_definition.v1~"))]
struct QuotaDefinitionResource;
#[resource_error(gts_id!("cf.bss.products.quota_adapter_target.v1~"))]
struct QuotaAdapterTargetResource;

/// The resource a refusal names (RS-25): one of the gear's registered authz labels
/// (`crate::authz::labels`), picked from what the refusal carries. Every refusal used to name
/// `product.v1~`, a type this gear neither defines nor registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refused {
    Sku,
    Category,
    ApprovalUnit,
    DerivedUsageType,
    QuotaDefinition,
    QuotaAdapterTarget,
}

/// Build `$method($args)` on the resource `$resource` names; every arm has one builder type.
macro_rules! on {
    ($resource:expr, $method:ident($($arg:expr),*)) => {
        match $resource {
            Refused::Sku => SkuResource::$method($($arg),*),
            Refused::Category => CategoryResource::$method($($arg),*),
            Refused::ApprovalUnit => ApprovalUnitResource::$method($($arg),*),
            Refused::DerivedUsageType => DerivedUsageTypeResource::$method($($arg),*),
            Refused::QuotaDefinition => QuotaDefinitionResource::$method($($arg),*),
            Refused::QuotaAdapterTarget => QuotaAdapterTargetResource::$method($($arg),*),
        }
    };
}

impl Refused {
    /// A missing row names its kind (`what`); a reference is a SKU's. An approval refusal and a
    /// stale unit refuse the unit, a `CATEGORY_` code the category, and a `DERIVED_` code of the
    /// derived doors the derived usage type (P-D-231). The binding's `DERIVED_` codes refuse a
    /// SKU's write, so they name the SKU (P-D-232). A quota row's miss names its quota resource
    /// ([`Self::of_quota`], P-D-273); every other quota refusal is answered by its endpoint
    /// ([`quota_refusal`]). Every other refusal is a write or a read of a SKU, the registry's own
    /// resource.
    fn of(err: &DomainError) -> Self {
        if let Some(quota) = Self::of_quota(err) {
            return quota;
        }
        match err {
            DomainError::NotFound {
                what: "derived_usage_type",
                ..
            } => Self::DerivedUsageType,
            DomainError::Conflict { code, .. } | DomainError::Forbidden { code, .. }
                if names_the_derived_type(code) =>
            {
                Self::DerivedUsageType
            }
            DomainError::Validation(report)
                if report
                    .violations()
                    .iter()
                    .any(|v| names_the_derived_type(v.code)) =>
            {
                Self::DerivedUsageType
            }
            DomainError::NotFound {
                what: "category", ..
            } => Self::Category,
            DomainError::NotFound {
                what: "approval_unit",
                ..
            }
            | DomainError::Approval(_)
            | DomainError::StaleUnit { .. } => Self::ApprovalUnit,
            DomainError::Conflict { code, .. } | DomainError::Forbidden { code, .. }
                if code.starts_with("CATEGORY_") =>
            {
                Self::Category
            }
            _ => Self::Sku,
        }
    }
}

impl Refused {
    /// The quota row a miss names (P-D-273): a target's the adapter target, a candidate's or a
    /// definition's the quota definition; `None` for every other refusal. A quota code names no
    /// resource here: the quota endpoint that refuses it decides ([`quota_refusal`]).
    fn of_quota(err: &DomainError) -> Option<Self> {
        match err {
            DomainError::NotFound {
                what: "quota_adapter_target",
                ..
            } => Some(Self::QuotaAdapterTarget),
            DomainError::NotFound {
                what: "quota_candidate" | "quota_definition",
                ..
            } => Some(Self::QuotaDefinition),
            _ => None,
        }
    }
}

/// A `DERIVED_` code of the derived doors (P-D-231), not of a SKU's binding (P-D-232).
fn names_the_derived_type(code: &str) -> bool {
    code.starts_with("DERIVED_") && !crate::domain::derived::SKU_BINDING_CODES.contains(&code)
}

/// Shared canonical error constructor.
fn precondition(
    resource: Refused,
    field: &'static str,
    detail: &str,
    code: &'static str,
) -> CanonicalError {
    on!(resource, failed_precondition())
        .with_precondition_violation(field, detail, code)
        .create()
}

/// Shared canonical error constructor.
fn aborted(resource: Refused, detail: String, code: &'static str) -> CanonicalError {
    on!(resource, aborted(detail)).with_reason(code).create()
}

/// Shared canonical error constructor.
fn denied(resource: Refused, code: &'static str) -> CanonicalError {
    on!(resource, permission_denied())
        .with_reason(code)
        .create()
}

/// A PDP denial on the resource `label` names (one of `crate::authz::labels`); any other label is
/// the SKU's. The doors that authorize more than one resource build their 403 here (RS-25).
pub(crate) fn permission_denied(label: &str, reason: String) -> CanonicalError {
    use crate::authz::labels;
    let resource = if label == labels::CATEGORY {
        Refused::Category
    } else if label == labels::APPROVAL_UNIT {
        Refused::ApprovalUnit
    } else if label == labels::DERIVED_USAGE_TYPE {
        Refused::DerivedUsageType
    } else if label == labels::QUOTA_DEFINITION {
        Refused::QuotaDefinition
    } else if label == labels::QUOTA_ADAPTER_TARGET {
        Refused::QuotaAdapterTarget
    } else {
        Refused::Sku
    };
    on!(resource, permission_denied())
        .with_reason(reason)
        .create()
}

/// A dependency that did not answer: logged for the operator, a 503 for the caller, carrying the
/// code in its detail where the design names one.
fn unavailable(dependency: &str, detail: &str, code: Option<&str>) -> CanonicalError {
    tracing::error!(dependency, detail, "bss-products: dependency unavailable");
    let builder = CanonicalError::service_unavailable();
    match code {
        Some(code) => builder.with_detail(format!("{code}: {detail}")).create(),
        None => builder.create(),
    }
}

/// P-D-207: the usage-type catalog refused the caller. The detail is the gear's own sentence; the
/// collector's PDP reason stays in the operator log (`infra::usage_types`).
fn catalog_denied(detail: &str) -> CanonicalError {
    tracing::warn!(
        detail,
        "bss-products: the usage-type catalog refused the caller"
    );
    denied(Refused::Sku, "USAGE_TYPE_FORBIDDEN")
}

/// A validation report: a catalog's refusal of the caller is a 403 and a catalog outage a 503,
/// whichever stage reported them; otherwise every violation, in the order collected (P-D-202), on
/// the resource [`Refused::of`] picked.
fn validation(resource: Refused, report: &ValidationReport) -> CanonicalError {
    // A catalog that refused the caller is a permission, never a field fix (P-D-207).
    if report
        .violations()
        .iter()
        .any(|v| v.code == "USAGE_TYPE_FORBIDDEN")
    {
        return CanonicalError::from(DomainError::UsageTypeForbidden(
            "the usage-type catalog refused this caller".into(),
        ));
    }
    // Outages are retryable even when reported by the pure publish validator.
    if report
        .violations()
        .iter()
        .any(|v| v.code == "USAGE_TYPE_UNAVAILABLE")
    {
        return CanonicalError::from(DomainError::UsageTypeUnavailable(
            "the usage-type catalog did not answer".into(),
        ));
    }
    let mut violations = report.violations().iter();
    let Some(first) = violations.next() else {
        return CanonicalError::internal("products: validation failed with an empty report")
            .create();
    };
    let mut builder = on!(resource, failed_precondition()).with_precondition_violation(
        first.subject.clone(),
        first.detail.clone(),
        first.code,
    );
    for violation in violations {
        builder = builder.with_precondition_violation(
            violation.subject.clone(),
            violation.detail.clone(),
            violation.code,
        );
    }
    builder.create()
}

/// The quota resource a quota endpoint answers its refusals on (P-D-273).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuotaResource {
    /// The adapter target endpoints and the discovery.
    AdapterTarget,
    /// The candidate, review and definition endpoints.
    Definition,
}

impl From<QuotaResource> for Refused {
    fn from(resource: QuotaResource) -> Self {
        match resource {
            QuotaResource::AdapterTarget => Self::QuotaAdapterTarget,
            QuotaResource::Definition => Self::QuotaDefinition,
        }
    }
}

/// `err` as a quota endpoint answers it (P-D-273): the endpoint decides the resource, not the
/// code, so a generic refusal (`VALIDATION`, `FIELD_TOO_LONG`, `CONTENDED`, a key's refusal, a
/// stale revision) names the endpoint's quota resource and never the SKU. A miss names the quota
/// row that is missing.
pub(crate) fn quota_refusal(err: DomainError, resource: QuotaResource) -> CanonicalError {
    let named = match &err {
        DomainError::NotFound { .. } => Refused::of_quota(&err).unwrap_or(resource.into()),
        _ => resource.into(),
    };
    canonical(err, named)
}

/// A stored quota row that does not read (P-D-274): a data-loss 500 on the endpoint's quota
/// resource, whose wire detail is the fixed `detail`.
pub(crate) fn quota_data_loss(resource: QuotaResource, detail: &str) -> CanonicalError {
    on!(Refused::from(resource), data_loss(detail))
        .with_resource("quota catalog")
        .create()
}

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        let resource = Refused::of(&err);
        canonical(err, resource)
    }
}

/// `err` on the resource it refuses.
fn canonical(err: DomainError, resource: Refused) -> CanonicalError {
    use DomainError as D;
    match err {
        D::Conflict { code, detail } => aborted(resource, detail, code),
        D::Forbidden { code, .. } => denied(resource, code),
        D::NotFound { what, id } => on!(resource, not_found(format!("{what} {id}")))
            .with_resource(id.to_string())
            .create(),
        D::StaleUnit { generation } => precondition(
            resource,
            "unit",
            &format!("the unit was refreshed; review generation {generation}"),
            "UNIT_STALE",
        ),
        // InvalidSubmit and ApplyRefused enter through Validation/Conflict
        // so their static subject-specific codes and fields survive intact.
        // Database errors are mapped only after the transaction retry loop.
        D::Approval(r) => match r.code {
            "SOD_VIOLATION" | "NOT_SUBMITTER" => denied(resource, r.code),
            "NOTE_REQUIRED" => {
                precondition(resource, "note", "a reject needs a note", "NOTE_REQUIRED")
            }
            "NOTE_TOO_LONG" => precondition(resource, "note", &r.detail, "NOTE_TOO_LONG"),
            "VALIDATION" => precondition(resource, "items", &r.detail, "VALIDATION"),
            "GENERATION_MISMATCH" => {
                precondition(resource, "generation", &r.detail, "GENERATION_MISMATCH")
            }
            // The cause stays in the log, never on the wire (RS-01): an outbox enqueue, a
            // repository failure the store wrapped, a unit row that does not read.
            "DB" | "STORE" => {
                tracing::error!(
                    code = r.code,
                    detail = %r.detail,
                    "bss-products: approval store failure"
                );
                CanonicalError::internal(format!("products: approval store: {}", r.detail)).create()
            }
            other => aborted(resource, r.detail, other),
        },
        D::Validation(report) => validation(resource, &report),
        D::StaleRevision { expected, found } => aborted(
            resource,
            format!("expected {expected}, found {found}"),
            "STALE_REVISION",
        ),
        D::IdempotencyConflict(detail) => aborted(resource, detail, "IDEMPOTENCY_CONFLICT"),
        D::IdempotencyKeyInFlight(detail) => aborted(resource, detail, "IDEMPOTENCY_KEY_IN_FLIGHT"),
        D::AuditUnavailable(detail) => unavailable("audit_log", &detail, None),
        D::UsageTypeUnresolved(detail) => {
            precondition(resource, "usage_type_ref", &detail, "USAGE_TYPE_UNRESOLVED")
        }
        D::UsageTypeUnavailable(detail) => unavailable(
            "usage_type_catalog",
            &detail,
            Some("USAGE_TYPE_UNAVAILABLE"),
        ),
        D::UsageTypeForbidden(detail) => catalog_denied(&detail),
        D::UsageUnavailable(detail) => {
            unavailable("sku_usage_port", &detail, Some("USAGE_UNAVAILABLE"))
        }
        D::UnrecognizedUnit(detail) => {
            precondition(resource, "meter", &detail, "UNRECOGNIZED_UNIT")
        }
        D::MeterDeclarationIncomplete(detail) => {
            precondition(resource, "meter", &detail, "METER_DECLARATION_INCOMPLETE")
        }
        D::QuotaUnavailable { code, detail } => unavailable("quota", &detail, Some(code)),
        // No adapter is built for the plugin instance: the capability is absent, not down.
        D::QuotaClosed { code, detail } => {
            on!(resource, unimplemented(format!("{code}: {detail}"))).create()
        }
    }
}

#[cfg(test)]
#[path = "error_mapping_tests.rs"]
mod error_mapping_tests;
