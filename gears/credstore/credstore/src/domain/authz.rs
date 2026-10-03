//! Authorization policy-enforcement helpers for typed credentials.
//!
//! Evaluates the PDP once per action on the base credential type and converts
//! the decision into an [`AccessScope`] (tenant plus row predicates on
//! `secret_type` and `reference`) or a fail-closed domain error. Create evaluates the requested
//! concrete type instead.

use authz_resolver_sdk::pep::{AccessRequest, EnforcerError, PolicyEnforcer, ResourceType};
use credstore_sdk::CREDENTIAL_RESOURCE_TYPE;
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, ScopeValue, SecurityContext, pep_properties,
};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// PDP property carrying the credential's type (ADR-0010). Its value is the
/// deterministic v5 UUID of the type's GTS id (`credstore_sdk::type_uuid`) -
/// the representation the row stores - and the entity maps it to the
/// `secret_type_uuid` column, so a PDP constraint on it compiles to a SQL
/// predicate.
pub const SECRET_TYPE_PROP: &str = "secret_type";

/// PDP property carrying the credential's reference (its per-tenant name).
/// Per-instance grants are expressed by reference, not by record id: a
/// re-created record gets a new id, so an id-based grant would silently stop
/// matching. The entity maps it to the `reference` column.
pub const REFERENCE_PROP: &str = "reference";

/// The row properties a PDP may constrain besides the tenant.
pub const ROW_PROPS: [&str; 2] = [SECRET_TYPE_PROP, REFERENCE_PROP];

/// PDP resource for every operation on an **existing** credential: the base
/// credential type. One evaluation per action decides which credential types
/// the caller may touch - the PDP answers with a constraint on
/// [`SECRET_TYPE_PROP`] - so the number of evaluations never depends on how
/// many types exist or which ones the tenant holds (ADR-0010).
pub const CREDENTIAL_RESOURCE: ResourceType = ResourceType::from_static(
    CREDENTIAL_RESOURCE_TYPE,
    &[
        pep_properties::OWNER_TENANT_ID,
        SECRET_TYPE_PROP,
        REFERENCE_PROP,
    ],
);

/// PDP resource type for a concrete credential type: the full derived GTS id
/// (design §5.4), e.g.
/// `gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~`.
///
/// Used only by **create**, where the type comes from the request and no row
/// exists yet to constrain; every other operation evaluates
/// [`CREDENTIAL_RESOURCE`]. The id comes from the per-operation
/// types-registry resolution
/// ([`crate::domain::secret::type_resolver::ResolvedSecretType::gts_id`]),
/// so dynamically registered types are addressable without a release.
#[must_use]
pub fn credential_type_resource(gts_id: &str) -> ResourceType {
    ResourceType::new(
        gts_id.to_owned(),
        &[pep_properties::OWNER_TENANT_ID, REFERENCE_PROP],
    )
}

/// The six PDP actions ADR-0004 replaces the shipped `read`/`write`/`delete`
/// with: plain verbs for the record, `_secret`-suffixed verbs for the value.
/// `LIST` is evaluated by the collection read (Phase 3, ADR-0005) — defined
/// here now so the vocabulary is complete and stable.
pub mod actions {
    /// Collection read (Phase 3). Also implied by `read` (a record reader
    /// can list what it can read), per ADR-0004's "Implications between the
    /// actions".
    pub const LIST: &str = "list";
    /// Point read of the credential record (`GET /credentials/{ref}`).
    pub const READ: &str = "read";
    /// `PUT`/`PATCH` of any metadata field (`sharing`, `fallback`,
    /// `expires_at`, `secret_type` on replace).
    pub const WRITE: &str = "write";
    /// `DELETE /credentials/{ref}`.
    pub const DELETE: &str = "delete";
    /// Point read of the value: `GET /credentials/{ref}` (or the collection)
    /// with `value` named in `$select` (ADR-0004 Amendment A).
    pub const READ_SECRET: &str = "read_secret";
    /// `PUT`/`PATCH` of the `value` field.
    pub const WRITE_SECRET: &str = "write_secret";
}

/// Map a PEP enforcement failure to a domain error (fail-closed).
/// `Denied` / `CompileFailed` → `AccessDenied` (403); `EvaluationFailed` → `ServiceUnavailable` (503).
#[must_use]
pub fn map_enforcer_err(err: EnforcerError) -> DomainError {
    match err {
        EnforcerError::Denied { .. } | EnforcerError::CompileFailed(_) => {
            DomainError::AccessDenied {
                cause: Some(Box::new(err)),
            }
        }
        EnforcerError::EvaluationFailed(source) => DomainError::ServiceUnavailable {
            detail: "authorization evaluation failed".to_owned(),
            retry_after: None,
            cause: Some(Box::new(EnforcerError::EvaluationFailed(source))),
        },
    }
}

/// Returns the PDP `AccessScope` for `action` on `resource` for the
/// caller's tenant.
///
/// # Errors
///
/// Returns `DomainError::AccessDenied` if the PDP denies access or fails to compile constraints.
/// Returns `DomainError::ServiceUnavailable` if the PDP evaluation call fails.
pub async fn scope_for(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    resource: &ResourceType,
    action: &str,
) -> Result<AccessScope, DomainError> {
    let tenant = ctx.subject_tenant_id();
    let request = AccessRequest::new()
        .resource_property(pep_properties::OWNER_TENANT_ID, tenant)
        .require_constraints(true);
    let scope = enforcer
        .access_scope_with(ctx, resource, action, None, &request)
        .await
        .map_err(map_enforcer_err)?;
    Ok(normalize_reference_filters(&scope))
}

/// Rewrite an `Eq`/`In` filter on [`REFERENCE_PROP`] so its values are
/// `ScopeValue::String`: the PEP compiler turns a UUID-shaped string into
/// `ScopeValue::Uuid`, which SQL would bind as a uuid parameter against the
/// text `reference` column. A UUID becomes its lowercase hyphenated string.
/// Every other filter is returned unchanged.
fn normalize_reference_filter(filter: &ScopeFilter) -> ScopeFilter {
    if filter.property() != REFERENCE_PROP {
        return filter.clone();
    }
    let to_string = |v: &ScopeValue| match v {
        ScopeValue::Uuid(u) => ScopeValue::String(u.hyphenated().to_string()),
        other => other.clone(),
    };
    match filter {
        ScopeFilter::Eq(f) => ScopeFilter::eq(REFERENCE_PROP, to_string(f.value())),
        ScopeFilter::In(f) => {
            ScopeFilter::r#in(REFERENCE_PROP, f.values().iter().map(to_string).collect())
        }
        other => other.clone(),
    }
}

/// [`normalize_reference_filter`] over every filter of a PDP scope.
#[must_use]
pub fn normalize_reference_filters(scope: &AccessScope) -> AccessScope {
    if scope.is_unconstrained() || scope.constraints().is_empty() {
        return scope.clone();
    }
    AccessScope::from_constraints(
        scope
            .constraints()
            .iter()
            .map(|c| {
                ScopeConstraint::new(c.filters().iter().map(normalize_reference_filter).collect())
            })
            .collect(),
    )
}

// ── Scope algebra over the tenant and row properties ────────────────────────

/// Whether a single constraint affirms `tenant`, ignoring the row properties.
///
/// Fail-closed tenant gate: every non-row filter must be a plain
/// `owner_tenant_id` `Eq`/`In` containing `tenant`, and at least one such
/// filter must be present. A sibling filter that narrows below tenant
/// granularity (`owner_id`, `id`, group membership, ...) or one this gate
/// cannot evaluate makes the constraint non-admitting. Row filters narrow
/// rows, not tenants - they are applied by SQL (or [`RowClamp`]).
fn constraint_admits_tenant(constraint: &ScopeConstraint, tenant: Uuid) -> bool {
    let mut affirmed = false;
    for filter in constraint.filters() {
        if ROW_PROPS.contains(&filter.property()) {
            continue;
        }
        if filter.property() != pep_properties::OWNER_TENANT_ID {
            return false;
        }
        match filter {
            ScopeFilter::Eq(_) | ScopeFilter::In(_)
                if filter.values().iter().any(|v| v.as_uuid() == Some(tenant)) =>
            {
                affirmed = true;
            }
            // Structured subtree/group predicates are a capability-contract
            // breach (credstore advertises no capabilities), and any
            // variant added later is one this build cannot resolve.
            _ => return false,
        }
    }
    affirmed
}

/// True iff `tenant` is within `scope`, ignoring row predicates (the
/// fail-closed own-tenant gate; see [`constraint_admits_tenant`]).
#[must_use]
pub fn scope_admits_tenant(scope: &AccessScope, tenant: Uuid) -> bool {
    if scope.is_unconstrained() {
        return true;
    }
    scope
        .constraints()
        .iter()
        .any(|c| constraint_admits_tenant(c, tenant))
}

/// The rows a scope admits for the caller's own tenant, as the PDP's row
/// predicates (`secret_type` and/or `reference`) kept in their OR-of-ANDs
/// shape - "type A OR reference x" is not "type in {A} AND reference in {x}",
/// so the structure cannot be flattened into sets.
#[derive(Debug, Clone, PartialEq)]
pub enum RowClamp {
    /// No row narrowing: every row of the tenant.
    Any,
    /// Alternatives (OR), each a conjunction (AND) of row-property filters;
    /// empty = no row is admitted.
    Constraints(Vec<ScopeConstraint>),
}

impl RowClamp {
    /// Whether a row of `secret_type_uuid` and `reference` is admitted.
    ///
    /// Fail-closed: a filter of a kind or value type this build cannot
    /// evaluate does not match.
    #[must_use]
    pub fn admits(&self, secret_type_uuid: Uuid, reference: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Constraints(constraints) => constraints.iter().any(|c| {
                c.filters()
                    .iter()
                    .all(|f| filter_admits_row(f, secret_type_uuid, reference))
            }),
        }
    }

    /// True when no row is admitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Constraints(c) if c.is_empty())
    }

    /// Narrow by the caller's own `$filter type in (...)` set: an
    /// `In(secret_type, types)` filter is AND-ed into every alternative.
    #[must_use]
    pub fn restrict_to(self, types: Option<&[Uuid]>) -> Self {
        let Some(types) = types else {
            return self;
        };
        let type_filter = ScopeFilter::in_uuids(SECRET_TYPE_PROP, types.to_vec());
        match self {
            Self::Any => Self::Constraints(vec![ScopeConstraint::new(vec![type_filter])]),
            Self::Constraints(constraints) => Self::Constraints(
                constraints
                    .into_iter()
                    .map(|c| {
                        let mut filters = c.filters().to_vec();
                        filters.push(type_filter.clone());
                        ScopeConstraint::new(filters)
                    })
                    .collect(),
            ),
        }
    }

    /// The clamp as a row-only [`AccessScope`] for the repo's SQL queries
    /// (deny-all when empty).
    #[must_use]
    pub fn to_scope(&self) -> AccessScope {
        match self {
            Self::Any => AccessScope::allow_all(),
            Self::Constraints(c) if c.is_empty() => AccessScope::deny_all(),
            Self::Constraints(c) => AccessScope::from_constraints(c.clone()),
        }
    }
}

/// Whether one row-property filter admits the row. Only `Eq`/`In` on
/// [`SECRET_TYPE_PROP`] (UUID values) and [`REFERENCE_PROP`] (string values)
/// are evaluable; anything else fails closed.
fn filter_admits_row(filter: &ScopeFilter, secret_type_uuid: Uuid, reference: &str) -> bool {
    if !matches!(filter, ScopeFilter::Eq(_) | ScopeFilter::In(_)) {
        return false;
    }
    match filter.property() {
        SECRET_TYPE_PROP => filter
            .values()
            .iter()
            .any(|v| v.as_uuid() == Some(secret_type_uuid)),
        // Exact, case-sensitive string comparison; scopes are normalised to
        // string reference values (see [`normalize_reference_filters`]).
        REFERENCE_PROP => filter
            .values()
            .iter()
            .any(|v| matches!(v, ScopeValue::String(s) if s == reference)),
        _ => false,
    }
}

/// Derive the row clamp for `tenant` from a PDP scope: the alternatives, over
/// the constraints that admit the tenant, each reduced to its row-property
/// filters. A constraint without a row filter admits every row.
#[must_use]
pub fn row_clamp(scope: &AccessScope, tenant: Uuid) -> RowClamp {
    if scope.is_unconstrained() {
        return RowClamp::Any;
    }
    let mut alternatives = Vec::new();
    for constraint in scope
        .constraints()
        .iter()
        .filter(|c| constraint_admits_tenant(c, tenant))
    {
        let filters: Vec<ScopeFilter> = constraint
            .filters()
            .iter()
            .filter(|f| ROW_PROPS.contains(&f.property()))
            .map(normalize_reference_filter)
            .collect();
        match ScopeConstraint::try_new(filters) {
            Ok(c) => alternatives.push(c),
            Err(_) => return RowClamp::Any,
        }
    }
    RowClamp::Constraints(alternatives)
}

/// Intersection of two scopes (AND of two OR-of-ANDs): the cross product of
/// their constraints, each a conjunction of both sides' filters. Combines the
/// scopes of several actions into the one scope a lookup must satisfy.
#[must_use]
pub fn intersect_scopes(a: &AccessScope, b: &AccessScope) -> AccessScope {
    if a.is_unconstrained() {
        return b.clone();
    }
    if b.is_unconstrained() {
        return a.clone();
    }
    let mut constraints = Vec::new();
    for ca in a.constraints() {
        for cb in b.constraints() {
            let filters: Vec<ScopeFilter> =
                ca.filters().iter().chain(cb.filters()).cloned().collect();
            if let Ok(c) = ScopeConstraint::try_new(filters) {
                constraints.push(c);
            }
        }
    }
    AccessScope::from_constraints(constraints)
}

#[cfg(test)]
#[path = "authz_tests.rs"]
mod authz_tests;
