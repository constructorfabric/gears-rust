//! PEP gate and per-resource vocabulary for the usage-collector domain.
//!
//! Per ADR-0001 (`cpt-cf-usage-collector-adr-pdp-centric-authorization`) the
//! collector keeps NO local policy table and NO PDP-decision cache; every
//! decision is delegated to the bound `authz-resolver` client. Catalog
//! resources are platform-global per ADR-0012 / PRD §5.8 (no owning tenant,
//! no resource id, no per-row scoping), so catalog authz is subject-only;
//! the ingestion surface declares per-record attribution attributes
//! (tenant, optional subject, resource type and id) so policy can reason
//! over them. The catalog surface opts out of `require_constraints`
//! (subject-only authz, so an unconstrained `allow_all` permit is the
//! legitimate happy-path outcome); the per-record ingestion surface runs
//! under `require_constraints(true)` and gates each record's owning tenant
//! against the PDP-returned row scope, so a tenant-scoped caller cannot
//! attribute usage to — or read / deactivate a record of — a tenant outside
//! its closure.
//!
//! Fail-closed wiring (transport → `AuthorizationUnavailable`, deny /
//! compile-failed → `AuthorizationDenied`) lives here so it cannot drift
//! between call sites.

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_macros::domain_model;
use toolkit_odata::ast;
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, ScopeValue, SecurityContext, pep_properties,
};
use usage_collector_sdk::UsageRecord;
use uuid::Uuid;

use super::error::DomainError;

/// The full attribution tuple that determines a `UsageRecord` PDP request
/// under a fixed `(SecurityContext, action)` pair.
///
/// **Why this type exists.** The batch ingestion path
/// (`Service::create_usage_records`) deduplicates PDP round-trips by
/// grouping records with byte-identical PDP payloads. Correctness of that
/// dedup hinges on a single invariant: **every field the PDP payload
/// reads MUST be carried by this type, and every field this type carries
/// MUST be read by the payload composer.** If those two field sets ever
/// diverge, records that the PDP would have judged differently could
/// silently share a single decision — a bypass.
///
/// The invariant is enforced **structurally**, not by review: the
/// only PDP-composer entry point ([`authorize_attribution_tuple`]) takes
/// `&AttributionTupleKey` and nothing else, so it physically cannot
/// reference any record field outside this struct. Adding a new PEP
/// attribute therefore requires (a) a new field here, (b) an update to
/// [`AttributionTupleKey::from_record`], and (c) a corresponding
/// `.resource_property(...)` line in `authorize_attribution_tuple` —
/// the type system rejects any edit that touches the composer but not
/// the key.
///
/// `action` participates in the hash/eq contract so a batch carrying
/// records bound to different actions cannot collapse onto a single PDP
/// decision. Today every batch caller passes a constant
/// (`usage_record::actions::CREATE`); promoting `action` into the key
/// makes the safety property hold structurally for any future caller
/// that mixes actions in one batch.
#[domain_model]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct AttributionTupleKey {
    tenant_id: Uuid,
    resource_type: String,
    resource_id: String,
    subject_id: Option<String>,
    subject_type: Option<String>,
    action: &'static str,
}

impl AttributionTupleKey {
    /// Extract the tuple key from a record's caller-supplied attribution
    /// fields together with the PEP `action` the batch is authorising.
    /// The set of fields read here MUST match the set of
    /// `resource_property` / action writes in
    /// [`authorize_attribution_tuple`] — the field-by-field structural
    /// mirror is the load-bearing invariant.
    pub(crate) fn from_record(record: &UsageRecord, action: &'static str) -> Self {
        let (subject_id, subject_type) = match record.subject_ref.as_ref() {
            Some(s) => (
                Some(s.subject_id().to_owned()),
                s.subject_type().map(str::to_owned),
            ),
            None => (None, None),
        };
        Self {
            tenant_id: record.tenant_id,
            resource_type: record.resource_ref.resource_type().to_owned(),
            resource_id: record.resource_ref.resource_id().to_owned(),
            subject_id,
            subject_type,
            action,
        }
    }
}

/// PEP vocabulary for the `UsageType` catalog.
///
/// Platform-global resource (ADR-0012 / PRD §5.8): no owning tenant, no
/// resource id, no per-`UsageType` scoping. The PDP authorizes the subject
/// alone and the [`RESOURCE`] declares no attributes.
pub(crate) mod usage_type {
    use authz_resolver_sdk::pep::ResourceType;

    /// PEP resource type for the `UsageType` catalog.
    pub const RESOURCE: ResourceType =
        ResourceType::from_static("gts.cf.core.uc.usage_type.v1~", &[]);

    /// `UsageType` action vocabulary. Renaming any of these is a contract
    /// change against the PDP policy bundle.
    pub mod actions {
        pub const CREATE: &str = "create";
        pub const GET: &str = "get";
        pub const LIST: &str = "list";
        pub const DELETE: &str = "delete";
    }
}

/// PEP vocabulary for the `UsageRecord` ingestion surface.
///
/// The PDP authorizes the subject together with the caller-supplied
/// attribution composites carried on each record: the owning tenant
/// (`UsageRecord::tenant_id` — caller-supplied, never derived from the
/// [`SecurityContext`]), the optional subject reference (`subject_id` plus
/// optional `subject_type` qualifier), and the mandatory resource reference.
/// Property keys are exported as `PROP_*` constants so call sites and policy
/// authors share one vocabulary.
pub(crate) mod usage_record {
    use authz_resolver_sdk::pep::ResourceType;
    use toolkit_security::pep_properties;

    /// PEP attribute key carrying the caller-supplied `resource_type`.
    pub const PROP_RESOURCE_TYPE: &str = "resource_type";

    /// PEP attribute key carrying the caller-supplied `resource_id`.
    pub const PROP_RESOURCE_ID: &str = "resource_id";

    /// PEP attribute key carrying the optional caller-supplied `subject_type`
    /// qualifier (present only when [`usage_collector_sdk::SubjectRef`] is
    /// supplied AND its `subject_type` field is populated).
    pub const PROP_SUBJECT_TYPE: &str = "subject_type";

    /// PEP resource type for the `UsageRecord` ingestion surface. Declares the
    /// attribution-tuple attributes the PDP may key its policy on.
    pub const RESOURCE: ResourceType = ResourceType::from_static(
        "gts.cf.core.uc.usage_record.v1~",
        &[
            pep_properties::OWNER_TENANT_ID,
            pep_properties::OWNER_ID,
            PROP_RESOURCE_TYPE,
            PROP_RESOURCE_ID,
            PROP_SUBJECT_TYPE,
        ],
    );

    /// `UsageRecord` action vocabulary. Renaming any of these is a contract
    /// change against the PDP policy bundle.
    pub mod actions {
        pub const CREATE: &str = "create";
        pub const DEACTIVATE: &str = "deactivate";
        pub const GET: &str = "get";
        pub const LIST: &str = "list";
    }
}

/// Run the PDP check for `(resource_type, action)` and lift the outcome into
/// [`DomainError`].
///
/// Subject-only authz: the request carries no resource attributes and opts
/// out of `require_constraints`, so a permit with no constraints (`allow_all`)
/// is the legitimate happy-path outcome. Deny / transport failure / compile
/// failure fail closed through the existing `From<EnforcerError>` mapping.
///
/// # Errors
///
/// * [`DomainError::AuthorizationDenied`] when the PDP denies or returns an
///   uncompilable constraint shape.
/// * [`DomainError::AuthorizationUnavailable`] when the PDP transport fails.
// @cpt-flow:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1
// @cpt-algo:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-pdp-centric-authorization:p2
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-fail-closed:p2
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-contract-authz-resolver:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-entity-pdp-decision:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-adr-pdp-centric-authorization:p2
pub(crate) async fn authorize(
    enforcer: &PolicyEnforcer,
    // @cpt-begin:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-input
    ctx: &SecurityContext,
    resource_type: &ResourceType,
    action: &str,
    // @cpt-end:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-input
) -> Result<(), DomainError> {
    // @cpt-begin:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-compose-tuple
    // @cpt-begin:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-compose
    // @cpt-begin:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-resolver-call
    // @cpt-begin:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-call
    // @cpt-begin:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-return
    // @cpt-begin:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-return
    enforcer
        .access_scope_with(
            ctx,
            resource_type,
            action,
            None,
            &AccessRequest::new().require_constraints(false),
        )
        .await
        .map(|_| ())
        .map_err(DomainError::from)
    // @cpt-end:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-return
    // @cpt-end:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-return
    // @cpt-end:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-call
    // @cpt-end:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-resolver-call
    // @cpt-end:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-compose
    // @cpt-end:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-compose-tuple
}

/// Run the PDP check for `(usage_record, action)` carrying the caller-supplied
/// (for `CREATE`) or plugin-loaded (for `DEACTIVATE`) attribution-tuple
/// attributes lifted off the [`UsageRecord`]: the owning tenant
/// (`record.tenant_id`), the optional subject reference (its mandatory
/// `subject_id` plus optional `subject_type` qualifier), and the mandatory
/// resource reference. `action` selects the verb the PDP authorizes against
/// (`actions::CREATE` for emission, `actions::DEACTIVATE` for event
/// deactivation); the per-verb PEP vocabulary is identical so policy authors
/// reason over a single attribute set. Unlike [`authorize`], this path runs
/// under `require_constraints(true)` and applies the per-record attribution
/// gate in [`scope_admits_attribution_tuple`]: `access_scope_with` fails
/// closed only on an
/// outright PDP deny / transport / compile error, so a *permit* carrying a
/// row-scope narrowing constraint (e.g. `OWNER_TENANT_ID In [caller's tenant
/// closure]`) is returned as `Ok(scope)` and the SDK does NOT auto-match it
/// against the request's resource properties. The record's owning tenant
/// must therefore be matched against the granted scope here, or cross-tenant
/// attribution (create) / cross-tenant read (get) / cross-tenant deactivate
/// would slip through.
///
/// # Errors
///
/// * [`DomainError::AuthorizationDenied`] when the PDP denies, returns an
///   uncompilable constraint shape, or grants a scope that does not cover
///   the record's owning tenant.
/// * [`DomainError::AuthorizationUnavailable`] when the PDP transport fails.
// @cpt-algo:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1
// @cpt-algo:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-tenant-attribution:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-resource-attribution:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-subject-attribution:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-ingestion-authorization:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-entity-resource-ref:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-entity-subject-ref:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-entity-security-context:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-principle-fail-closed:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-adr-caller-supplied-attribution:p1
pub(crate) async fn authorize_usage_record(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    record: &UsageRecord,
    action: &'static str,
) -> Result<(), DomainError> {
    let key = AttributionTupleKey::from_record(record, action);
    authorize_attribution_tuple(enforcer, ctx, &key).await
}

/// PDP-evaluate an [`AttributionTupleKey`] directly, bypassing
/// [`AttributionTupleKey::from_record`].
///
/// This is the sole composer of the `UsageRecord` PDP request. By taking
/// `&AttributionTupleKey` and no `&UsageRecord`, it makes "the dedup
/// grouping key is a complete description of the PDP payload" a
/// **structural** invariant rather than a coupling between two files —
/// see the type-level docs on [`AttributionTupleKey`].
///
/// # Errors
///
/// Same envelope as [`authorize_usage_record`]:
///
/// * [`DomainError::AuthorizationDenied`] on deny / uncompilable constraint.
/// * [`DomainError::AuthorizationUnavailable`] on PDP transport failure.
pub(crate) async fn authorize_attribution_tuple(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    key: &AttributionTupleKey,
) -> Result<(), DomainError> {
    // @cpt-begin:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-compose-tuple
    // @cpt-begin:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-compose
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-compose-tuple
    // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-compose-tuple
    let mut request = AccessRequest::new()
        .require_constraints(true)
        .resource_property(pep_properties::OWNER_TENANT_ID, key.tenant_id.to_string())
        .resource_property(usage_record::PROP_RESOURCE_TYPE, key.resource_type.clone())
        .resource_property(usage_record::PROP_RESOURCE_ID, key.resource_id.clone());

    if let Some(subject_id) = key.subject_id.as_ref() {
        request = request.resource_property(pep_properties::OWNER_ID, subject_id.clone());
        if let Some(subject_type) = key.subject_type.as_ref() {
            request =
                request.resource_property(usage_record::PROP_SUBJECT_TYPE, subject_type.clone());
        }
    }
    // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-compose-tuple
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-compose-tuple
    // @cpt-end:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-compose
    // @cpt-end:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-compose-tuple

    // @cpt-begin:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-resolver-call
    // @cpt-begin:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-call
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-pdp-deny
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-pdp-allow
    // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-call
    // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-deny
    // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-fail-closed
    // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-allow
    let scope = enforcer
        .access_scope_with(ctx, &usage_record::RESOURCE, key.action, None, &request)
        .await
        .map_err(DomainError::from)?;

    // Per-record attribution gate (see [`scope_admits_attribution_tuple`]). A
    // permit that narrows the grant (to the caller's tenant closure, and
    // possibly other attribution predicates) comes back as `Ok(scope)`, not a
    // bare yes — the record's attribution tuple must satisfy that scope or this
    // is cross-tenant / out-of-grant attribution / access and we fail closed.
    if scope_admits_attribution_tuple(&scope, key) {
        Ok(())
    } else {
        tracing::warn!(
            target: "authz",
            tenant_id = %key.tenant_id,
            action = key.action,
            "PDP permit did not authorize the record's owning tenant; \
             denying cross-tenant usage_record attribution"
        );
        Err(DomainError::AuthorizationDenied {
            reason: Some(format!(
                "caller is not authorized for usage_record owning tenant {}",
                key.tenant_id
            )),
        })
    }
    // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-allow
    // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-fail-closed
    // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-deny
    // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-call
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-pdp-allow
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-pdp-deny
    // @cpt-end:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-call
    // @cpt-end:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-resolver-call
}

/// Decide whether a PDP-returned [`AccessScope`] authorizes a per-record
/// operation on the record described by `key`.
///
/// [`authorize_attribution_tuple`] runs under `require_constraints(true)`, so
/// a permit comes back as a compiled scope rather than a bare yes/no, and the
/// SDK does NOT auto-match that scope against the request's resource
/// properties — confirming the record falls inside the granted scope is the
/// gear's responsibility. Without this check a `/tenants/{A}`-scoped caller
/// could create / read / deactivate records attributed to any other tenant:
/// the resolver returns a permit plus an `OWNER_TENANT_ID In [A's closure]`
/// narrowing, but nothing otherwise rejects an out-of-closure record.
///
/// Constraints are OR-ed (one per independent access path) and the filters
/// within a constraint are AND-ed — the same shape [`scope_to_odata_filter`]
/// projects for LIST. This gate is the point-operation analogue of that
/// projection: rather than pushing the scope into a query, it evaluates the
/// single record's attribution tuple against it directly. **A constraint
/// admits the record iff** every one of its filters is satisfied by the tuple
/// AND at least one filter pins the owning tenant; the scope admits iff *some*
/// constraint admits.
///
/// * **Tenant isolation is mandatory.** A constraint that does not pin the
///   record's `OWNER_TENANT_ID` (via `Eq` / `In`) never admits, even if its
///   other predicates match — `usage_record` grants are tenant-scoped, so an
///   admitting path must always name the tenant. UUID-as-`String` values are
///   accepted, mirroring [`AccessScope::contains_uuid`] / [`scope_value_to_ast`].
/// * **Every other predicate also constrains.** A constraint that additionally
///   narrows by `resource_type` / `resource_id` / `subject` only admits a
///   record whose tuple satisfies those filters too. The previous tenant-only
///   gate ignored them and granted more than the PDP intended (within tenant);
///   honouring them closes that gap.
/// * **Unevaluable filters fail closed.** A tree predicate
///   ([`ScopeFilter::InGroup`] / [`ScopeFilter::InGroupSubtree`] /
///   [`ScopeFilter::InTenantSubtree`]) or a filter over a property outside the
///   [`usage_record::RESOURCE`] attribute set cannot be evaluated against a
///   flat per-record tuple, so the enclosing constraint cannot admit — as
///   defensive as the LIST path, which rejects the same shapes. Other (OR-ed)
///   constraints may still admit; dropping an unevaluable disjunct only
///   narrows access, never widens it.
/// * **An unconstrained (`allow_all`) scope is denied.** Under
///   `require_constraints(true)` a legitimate permit always carries the
///   `OWNER_TENANT_ID In [..]` narrowing (admin included, as `In [all
///   tenants]`); `allow_all` only arises from a degenerate empty-predicate
///   permit, so this gate short-circuits [`AccessScope::is_unconstrained`]
///   to a denial. The LIST/aggregate [`scope_to_odata_filter`] path now
///   fails closed on the same degenerate permit, so both read gates share
///   one fail-closed posture for an unconstrained scope.
fn scope_admits_attribution_tuple(scope: &AccessScope, key: &AttributionTupleKey) -> bool {
    if scope.is_unconstrained() {
        return false;
    }
    scope
        .constraints()
        .iter()
        .any(|constraint| constraint_admits_tuple(constraint, key))
}

/// Whether a single PDP [`ScopeConstraint`] (an AND of filters) admits the
/// record's attribution tuple. Requires the owning tenant to be pinned and
/// every filter satisfied; an empty constraint (an allow-all disjunct) and any
/// unevaluable filter both fail closed. See [`scope_admits_attribution_tuple`].
fn constraint_admits_tuple(constraint: &ScopeConstraint, key: &AttributionTupleKey) -> bool {
    let mut tenant_pinned = false;
    for filter in constraint.filters() {
        match evaluate_filter(filter, key) {
            FilterOutcome::TenantSatisfied => tenant_pinned = true,
            FilterOutcome::Satisfied => {}
            FilterOutcome::Rejected => return false,
        }
    }
    // An empty constraint matches every row (an allow-all disjunct); under
    // require_constraints(true) we refuse to honour it, so the absence of a
    // satisfied tenant filter is itself a denial.
    tenant_pinned
}

/// Outcome of checking one PDP filter against the record's attribution tuple.
#[domain_model]
enum FilterOutcome {
    /// An `OWNER_TENANT_ID` filter the record's owning tenant satisfies.
    TenantSatisfied,
    /// A non-tenant, gate-understood filter the record satisfies.
    Satisfied,
    /// The record does not satisfy the filter, or the filter cannot be
    /// evaluated against a flat per-record tuple (tree predicate, unknown
    /// property, or un-comparable value). Either way the enclosing constraint
    /// cannot admit — fail closed.
    Rejected,
}

/// Evaluate one [`ScopeFilter`] against the record's tuple. The property →
/// value mapping mirrors the `resource_property` writes in
/// [`authorize_attribution_tuple`]; keep the two in sync.
fn evaluate_filter(filter: &ScopeFilter, key: &AttributionTupleKey) -> FilterOutcome {
    let matched = match filter {
        ScopeFilter::Eq(eq) => {
            let property = eq.property();
            // Resolve the value kind from the shared `pep_field` registry and
            // the record's value from `tuple_value_for` (both keyed on the same
            // property set); either miss means the property is outside the
            // gear's vocabulary -> fail closed.
            let (Some(field), Some(value)) = (pep_field(property), tuple_value_for(property, key))
            else {
                return rejected_unknown_property(property);
            };
            value_matches(value, field.kind, eq.value())
        }
        ScopeFilter::In(in_filter) => {
            let property = in_filter.property();
            let (Some(field), Some(value)) = (pep_field(property), tuple_value_for(property, key))
            else {
                return rejected_unknown_property(property);
            };
            in_filter
                .values()
                .iter()
                .any(|v| value_matches(value, field.kind, v))
        }
        ScopeFilter::InGroup(_)
        | ScopeFilter::InGroupSubtree(_)
        | ScopeFilter::InTenantSubtree(_) => {
            tracing::warn!(
                target: "authz",
                property = %filter.property(),
                "PDP returned an unsupported tree predicate on the per-record \
                 usage_record gate: usage_records is a flat resource with no \
                 resource-group or tenant-closure membership; failing closed"
            );
            return FilterOutcome::Rejected;
        }
    };
    if !matched {
        return FilterOutcome::Rejected;
    }
    // Consult the shared classifier so the per-record gate and the LIST
    // projection agree on what counts as tenant narrowing.
    if is_owner_tenant_filter(filter) {
        FilterOutcome::TenantSatisfied
    } else {
        FilterOutcome::Satisfied
    }
}

fn rejected_unknown_property(property: &str) -> FilterOutcome {
    tracing::warn!(
        target: "authz",
        property = %property,
        "PDP returned a constraint over an unknown property on the per-record \
         usage_record gate: refuse to admit under an unrecognised attribute"
    );
    FilterOutcome::Rejected
}

/// The record's value for a PEP property, in a form comparable to a
/// [`ScopeValue`]. `Absent` denotes an optional attribute the record does not
/// carry, so a constraint filtering on it cannot be satisfied.
#[domain_model]
#[derive(Clone, Copy)]
enum TupleValue<'a> {
    Uuid(Uuid),
    Str(&'a str),
    Absent,
}

/// Resolve a PEP property to the record's tuple value, or `None` when the
/// property is outside the [`usage_record::RESOURCE`] attribute set. Mirrors
/// the `resource_property` writes in [`authorize_attribution_tuple`].
fn tuple_value_for<'a>(property: &str, key: &'a AttributionTupleKey) -> Option<TupleValue<'a>> {
    if property == pep_properties::OWNER_TENANT_ID {
        return Some(TupleValue::Uuid(key.tenant_id));
    }
    if property == pep_properties::OWNER_ID {
        return Some(
            key.subject_id
                .as_deref()
                .map_or(TupleValue::Absent, TupleValue::Str),
        );
    }
    match property {
        usage_record::PROP_RESOURCE_TYPE => Some(TupleValue::Str(&key.resource_type)),
        usage_record::PROP_RESOURCE_ID => Some(TupleValue::Str(&key.resource_id)),
        usage_record::PROP_SUBJECT_TYPE => Some(
            key.subject_type
                .as_deref()
                .map_or(TupleValue::Absent, TupleValue::Str),
        ),
        _ => None,
    }
}

/// Whether the record's `tuple_value` (already in its native kind) equals the
/// PDP `scope_value` once coerced to the field's `kind`. Routes the coercion
/// through the shared [`coerce_scope_value`] so the per-record gate and the
/// LIST projection ([`scope_value_to_ast`]) cannot disagree on value typing —
/// a UUID-shaped `resource_id` is accepted on both, a value that does not
/// coerce to the field's kind (or an absent optional attribute) fails closed.
fn value_matches(tuple_value: TupleValue, kind: OdataFieldKind, scope_value: &ScopeValue) -> bool {
    match (tuple_value, coerce_scope_value(kind, scope_value)) {
        (TupleValue::Uuid(u), Some(CanonicalValue::Uuid(v))) => u == v,
        (TupleValue::Str(s), Some(CanonicalValue::Str(v))) => s == v.as_str(),
        _ => false,
    }
}

/// Authorize a `list_usage_records` request and return the compiled
/// [`AccessScope`] for downstream `OData` composition.
///
/// `require_constraints(true)` so the PDP MUST return row-scope narrowing
/// for a tenant-scoped caller rather than short-circuiting to an
/// unconstrained `AccessScope::allow_all`. The authz-resolver materializes
/// the caller's tenant closure into a flat `OWNER_TENANT_ID In [..]`
/// constraint — `usage_record` does not advertise
/// `Capability::TenantHierarchy`, so the closure is expanded eagerly rather
/// than pushed down as an `InTenantSubtree` predicate this flat resource
/// cannot consume. A platform-admin (`Global` scope) still resolves to a
/// constraint over the full tenant set (never `allow_all`), so requiring
/// constraints does not deny admin. The compiled scope is projected through
/// [`scope_to_odata_filter`] at the call site and AND-merged into the
/// user's filter before plugin dispatch (see
/// [`crate::domain::service::Service::list_usage_records`]); with
/// `require_constraints(false)` the PDP returned `allow_all`, leaving LIST
/// unscoped across tenants (no tenant isolation). As defence in depth, a
/// degenerate `allow_all` that still slips through (a non-empty constraints
/// list whose constraints are all empty compiles to `allow_all`) is denied
/// by [`scope_to_odata_filter`], never passed through as "no row narrowing".
///
/// The request carries no per-record attribution attributes (LIST is
/// pre-row), so the composed PEP request is action+resource-type only —
/// the PDP returns row-scope narrowing via the `AccessScope`
/// constraints, not via a tuple match.
///
/// # Errors
///
/// * [`DomainError::AuthorizationDenied`] when the PDP denies or returns
///   an uncompilable constraint shape.
/// * [`DomainError::AuthorizationUnavailable`] when the PDP transport
///   fails.
// @cpt-algo:cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read:p2
pub(crate) async fn authorize_list_usage_records(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<AccessScope, DomainError> {
    enforcer
        .access_scope_with(
            ctx,
            &usage_record::RESOURCE,
            usage_record::actions::LIST,
            None,
            &AccessRequest::new().require_constraints(true),
        )
        .await
        .map_err(DomainError::from)
}

/// Project an [`AccessScope`] into an `OData` filter expression over the
/// `UsageRecord` raw-read filter surface.
///
/// Constraints are OR-ed at the [`AccessScope`] level (one constraint per
/// independent access path) and filters within a constraint are AND-ed —
/// see [`AccessScope`] docs. The returned expression mirrors that shape:
/// `(f1 and f2 and ...) or (g1 and g2 and ...)`. PEP property names are
/// translated to the `OData` wire fields declared on
/// [`usage_collector_sdk::UsageRecordFilterField`]:
///
/// | PEP property                          | `OData` field   | Value kind |
/// |---------------------------------------|-----------------|------------|
/// | `pep_properties::OWNER_TENANT_ID`     | `tenant_id`     | UUID       |
/// | `pep_properties::OWNER_ID`            | `subject_id`    | string     |
/// | `usage_record::PROP_RESOURCE_TYPE`    | `resource_type` | string     |
/// | `usage_record::PROP_RESOURCE_ID`      | `resource_id`   | string     |
/// | `usage_record::PROP_SUBJECT_TYPE`     | `subject_type`  | string     |
///
/// Always projects to a narrowing predicate (`Ok(expr)`) or fails closed.
/// There is **no** "no row narrowing" pass-through: under
/// `require_constraints(true)` (see [`authorize_list_usage_records`]) a
/// legitimate permit always carries `OWNER_TENANT_ID In [..]` narrowing
/// (admin included, as `In [all tenants]`), so an unconstrained /
/// empty-constraint scope is a degenerate empty-predicate permit and is
/// denied rather than allowed to leak every tenant's rows — mirroring the
/// per-record [`scope_admits_attribution_tuple`] gate.
///
/// # Errors
///
/// * [`DomainError::AuthorizationDenied`] when the scope is unconstrained
///   ([`AccessScope::is_unconstrained`]) or carries an empty (filter-less)
///   constraint disjunct — both match every row, so collapsing to "no row
///   narrowing" would breach tenant isolation. Fail closed instead, per
///   [`cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`].
/// * [`DomainError::AuthorizationDenied`] when a constraint is non-empty but
///   carries no `OWNER_TENANT_ID` `Eq`/`In` filter — a constraint narrowing
///   only by some other property (e.g. `resource_type`) would AND into the
///   user query as a cross-tenant predicate, so it is denied. This mirrors the
///   per-record gate's tenant-pinning requirement (see
///   [`is_owner_tenant_filter`]).
/// * [`DomainError::AuthorizationDenied`] when the scope is deny-all
///   ([`AccessScope::is_deny_all`]) — the PDP explicitly authorized no
///   rows, which is observationally indistinguishable from a deny on
///   this surface.
/// * [`DomainError::AuthorizationDenied`] when a constraint carries a
///   tree predicate ([`ScopeFilter::InGroup`] /
///   [`ScopeFilter::InGroupSubtree`] / [`ScopeFilter::InTenantSubtree`])
///   — `usage_records` is a flat resource without resource-group or
///   tenant-closure membership tables, so a tree predicate cannot be
///   compiled against this plugin's storage. Surfacing a policy
///   shape this gear can't honour as `AuthorizationDenied` is
///   fail-closed by construction.
/// * [`DomainError::AuthorizationDenied`] when a constraint names a
///   PEP property outside the
///   [`usage_record::RESOURCE`] attribute set — same fail-closed
///   rationale.
// @cpt-algo:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2
pub(crate) fn scope_to_odata_filter(scope: &AccessScope) -> Result<ast::Expr, DomainError> {
    if scope.is_unconstrained() {
        tracing::warn!(
            target: "authz",
            "PDP returned an unconstrained (allow_all) scope on the usage_record \
             query path under require_constraints(true); failing closed"
        );
        return Err(DomainError::AuthorizationDenied {
            reason: Some("PDP returned an unconstrained scope".to_owned()),
        });
    }
    if scope.is_deny_all() {
        tracing::warn!(
            target: "authz",
            "PDP returned a deny-all scope on the usage_record query path"
        );
        return Err(DomainError::AuthorizationDenied {
            reason: Some("PDP returned a deny-all scope".to_owned()),
        });
    }

    let mut disjunction: Option<ast::Expr> = None;
    for constraint in scope.constraints() {
        let constraint_expr = constraint_to_odata_conjunction(constraint)?;
        disjunction = Some(match disjunction {
            None => constraint_expr,
            Some(acc) => acc.or(constraint_expr),
        });
    }
    // Unreachable in practice — the `is_unconstrained` / `is_deny_all` guards
    // above leave `constraints()` non-empty, and a non-empty constraint always
    // yields a `Some` disjunct (an empty one fails closed). Deny anyway so the
    // projection can never silently emit "no row narrowing".
    disjunction.ok_or_else(|| DomainError::AuthorizationDenied {
        reason: Some("PDP returned a scope with no usable constraints".to_owned()),
    })
}

/// Project one [`ScopeConstraint`] (an AND of filters) into an `OData`
/// conjunction, or fail closed.
///
/// A constraint fails closed under `require_constraints(true)` two ways:
///
/// * **Empty (filter-less)** — matches every row, an allow-all disjunct.
/// * **Not tenant-pinned** — carries no `OWNER_TENANT_ID` `Eq`/`In` filter.
///   A constraint narrowing only by, say, `resource_type` would AND into the
///   user query as a *cross-tenant* predicate.
///
/// Either would collapse the projection to "no tenant narrowing" and leak
/// every tenant's records, so both are denied — mirroring the per-record gate
/// [`constraint_admits_tuple`], which likewise requires the owning tenant to be
/// pinned. Both consult the shared [`is_owner_tenant_filter`] so the two paths
/// cannot drift on what counts as tenant narrowing.
fn constraint_to_odata_conjunction(constraint: &ScopeConstraint) -> Result<ast::Expr, DomainError> {
    let mut conjunction: Option<ast::Expr> = None;
    let mut tenant_pinned = false;
    for filter in constraint.filters() {
        tenant_pinned |= is_owner_tenant_filter(filter);
        let predicate = scope_filter_to_expr(filter)?;
        conjunction = Some(match conjunction {
            None => predicate,
            Some(acc) => acc.and(predicate),
        });
    }
    let Some(conjunction) = conjunction else {
        tracing::warn!(
            target: "authz",
            "PDP returned an empty (allow-all) constraint on the usage_record \
             query path under require_constraints(true); failing closed"
        );
        return Err(DomainError::AuthorizationDenied {
            reason: Some("PDP returned an empty constraint".to_owned()),
        });
    };
    if !tenant_pinned {
        tracing::warn!(
            target: "authz",
            "PDP returned a usage_record LIST constraint without OWNER_TENANT_ID \
             narrowing under require_constraints(true); failing closed"
        );
        return Err(DomainError::AuthorizationDenied {
            reason: Some("PDP returned a constraint without tenant narrowing".to_owned()),
        });
    }
    Ok(conjunction)
}

/// Whether `filter` pins the owning tenant — an `Eq`/`In` on
/// [`pep_properties::OWNER_TENANT_ID`]. Tree predicates over the tenant
/// property never pin (they fail closed upstream as unsupported on a flat
/// resource). Shared by the per-record gate ([`evaluate_filter`]) and the LIST
/// projection ([`constraint_to_odata_conjunction`]) so neither can drift on
/// what counts as tenant narrowing.
fn is_owner_tenant_filter(filter: &ScopeFilter) -> bool {
    match filter {
        ScopeFilter::Eq(eq) => eq.property() == pep_properties::OWNER_TENANT_ID,
        ScopeFilter::In(in_filter) => in_filter.property() == pep_properties::OWNER_TENANT_ID,
        ScopeFilter::InGroup(_)
        | ScopeFilter::InGroupSubtree(_)
        | ScopeFilter::InTenantSubtree(_) => false,
    }
}

/// Map a single [`ScopeFilter`] to an `OData` [`ast::Expr`].
fn scope_filter_to_expr(filter: &ScopeFilter) -> Result<ast::Expr, DomainError> {
    match filter {
        ScopeFilter::Eq(eq) => {
            let field = pep_property_to_field(eq.property())?;
            let value = scope_value_to_ast(field, eq.value())?;
            Ok(ast::Expr::Compare(
                Box::new(ast::Expr::Identifier(field.name.to_owned())),
                ast::CompareOperator::Eq,
                Box::new(ast::Expr::Value(value)),
            ))
        }
        ScopeFilter::In(in_filter) => {
            let field = pep_property_to_field(in_filter.property())?;
            let values: Vec<ast::Expr> = in_filter
                .values()
                .iter()
                .map(|v| scope_value_to_ast(field, v).map(ast::Expr::Value))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ast::Expr::In(
                Box::new(ast::Expr::Identifier(field.name.to_owned())),
                values,
            ))
        }
        ScopeFilter::InGroup(_)
        | ScopeFilter::InGroupSubtree(_)
        | ScopeFilter::InTenantSubtree(_) => {
            tracing::warn!(
                target: "authz",
                property = %filter.property(),
                "PDP returned an unsupported tree predicate: usage_records is a \
                 flat resource with no resource-group or tenant-closure membership"
            );
            Err(DomainError::AuthorizationDenied {
                reason: Some(format!(
                    "PDP returned an unsupported tree predicate on property `{}`: \
                     usage_records is a flat resource with no resource-group or \
                     tenant-closure membership",
                    filter.property()
                )),
            })
        }
    }
}

/// Wire description of a PDP property's `OData` projection.
#[domain_model]
#[derive(Clone, Copy)]
struct OdataField {
    /// `OData` identifier visible on the [`usage_collector_sdk::UsageRecordFilterField`] surface.
    name: &'static str,
    /// Expected [`ScopeValue`] variant. Anything else lifts to a fail-closed deny.
    kind: OdataFieldKind,
}

#[domain_model]
#[derive(Clone, Copy)]
enum OdataFieldKind {
    Uuid,
    String,
}

/// THE single registry of the `usage_record` PEP property set: each recognized
/// property's `OData` wire field name and canonical value [`OdataFieldKind`].
///
/// Both authz gates resolve properties through this one function — the
/// per-record gate ([`evaluate_filter`]) reads the `kind`, the LIST projection
/// ([`pep_property_to_field`]) reads the `name` and `kind` — so the recognized
/// property set and its typing **cannot drift** between the point-operation
/// path and the LIST path (Architecture finding #1). Returns `None` for a
/// property outside the [`usage_record::RESOURCE`] attribute set; each caller
/// lifts that into its own fail-closed denial.
fn pep_field(property: &str) -> Option<OdataField> {
    if property == pep_properties::OWNER_TENANT_ID {
        return Some(OdataField {
            name: "tenant_id",
            kind: OdataFieldKind::Uuid,
        });
    }
    if property == pep_properties::OWNER_ID {
        return Some(OdataField {
            name: "subject_id",
            kind: OdataFieldKind::String,
        });
    }
    match property {
        usage_record::PROP_RESOURCE_TYPE => Some(OdataField {
            name: "resource_type",
            kind: OdataFieldKind::String,
        }),
        usage_record::PROP_RESOURCE_ID => Some(OdataField {
            name: "resource_id",
            kind: OdataFieldKind::String,
        }),
        usage_record::PROP_SUBJECT_TYPE => Some(OdataField {
            name: "subject_type",
            kind: OdataFieldKind::String,
        }),
        _ => None,
    }
}

/// A PDP [`ScopeValue`] coerced to the canonical comparable form for a field's
/// [`OdataFieldKind`]. Produced by [`coerce_scope_value`] and consumed by both
/// gates — the per-record gate compares it against the record's value, the
/// LIST projection lowers it to an [`ast::Value`].
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
enum CanonicalValue {
    Uuid(Uuid),
    Str(String),
}

/// THE single `ScopeValue` coercion policy shared by both authz gates
/// (Architecture finding #1).
///
/// * A **UUID** field accepts a `Uuid` or a UUID-shaped `String`, mirroring
///   [`ScopeValue::as_uuid`] / [`AccessScope::contains_uuid`].
/// * A **String** field accepts a `String` or a `Uuid` rendered to its
///   canonical string form, so a UUID-shaped `resource_id` matches regardless
///   of how the PEP compiler typed it.
/// * Anything else is a type mismatch and yields `None`; both gates then fail
///   closed.
///
/// Keeping this in one place is what stops the per-record gate and the LIST
/// projection from disagreeing on value typing (finding #2: the LIST path used
/// to deny a `ScopeValue::Uuid` on a String field while the per-record gate
/// admitted it).
fn coerce_scope_value(kind: OdataFieldKind, value: &ScopeValue) -> Option<CanonicalValue> {
    match kind {
        OdataFieldKind::Uuid => value.as_uuid().map(CanonicalValue::Uuid),
        OdataFieldKind::String => match value {
            ScopeValue::String(s) => Some(CanonicalValue::Str(s.clone())),
            ScopeValue::Uuid(u) => Some(CanonicalValue::Str(u.to_string())),
            ScopeValue::Int(_) | ScopeValue::Bool(_) => None,
        },
    }
}

/// LIST-side property resolution: [`pep_field`] lifted into the fail-closed
/// [`DomainError::AuthorizationDenied`] the projection surfaces for an
/// attribute outside the [`usage_record::RESOURCE`] set.
fn pep_property_to_field(property: &str) -> Result<OdataField, DomainError> {
    pep_field(property).ok_or_else(|| {
        tracing::warn!(
            target: "authz",
            property = %property,
            "PDP returned a constraint over an unknown property for the \
             usage_record resource: refuse to widen scope under an \
             unrecognised attribute"
        );
        DomainError::AuthorizationDenied {
            reason: Some(format!(
                "PDP returned a constraint over unknown property `{property}` for the \
                 usage_record resource — refuse to widen scope under an \
                 unrecognised attribute"
            )),
        }
    })
}

/// Lower a PDP [`ScopeValue`] to the `OData` value for `field`, sharing the
/// [`coerce_scope_value`] policy with the per-record gate so the two cannot
/// drift on value typing.
fn scope_value_to_ast(field: OdataField, value: &ScopeValue) -> Result<ast::Value, DomainError> {
    match coerce_scope_value(field.kind, value) {
        Some(CanonicalValue::Uuid(u)) => Ok(ast::Value::Uuid(u)),
        Some(CanonicalValue::Str(s)) => Ok(ast::Value::String(s)),
        None => {
            let expected = match field.kind {
                OdataFieldKind::Uuid => "UUID",
                OdataFieldKind::String => "string",
            };
            let actual = describe_scope_value(value);
            tracing::warn!(
                target: "authz",
                field = %field.name,
                expected = %expected,
                actual = %actual,
                "PDP returned a value that cannot be coerced to the constraint field's type"
            );
            Err(DomainError::AuthorizationDenied {
                reason: Some(format!(
                    "PDP returned a {actual} value for field `{}` typed as {expected}",
                    field.name,
                )),
            })
        }
    }
}

fn describe_scope_value(v: &ScopeValue) -> &'static str {
    match v {
        ScopeValue::Uuid(_) => "UUID",
        ScopeValue::String(_) => "string",
        ScopeValue::Int(_) => "integer",
        ScopeValue::Bool(_) => "boolean",
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "authz_tests.rs"]
mod authz_tests;
