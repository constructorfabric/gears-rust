//! GTS definitions owned by quota-enforcement.
//!
//! Three concerns live here:
//!
//! 1. The **plugin specs**. Plugin gears register an instance of one spec
//!    (vendor + priority). The gear discovers the active plugin through the
//!    types registry and resolves its scoped `ClientHub` client under the
//!    instance id.
//! 2. The **resource identifiers** the canonical error envelope carries in
//!    `Problem.context.resource_type` (DESIGN section 3.3, "Error Model").
//! 3. The **projection contract bases** of ADR-0007: four abstract bases, the
//!    concrete scope-discriminator type, and its two P1 well-known instances.
//!    The type documents are generated from the Rust declarations below
//!    (`#[gts_type_schema]`), which are their only source.
//!    They reach `types-registry` two ways: through the link-time inventory
//!    the registry seeds itself from in the embedded profile, and through the
//!    gear's bootstrap, which re-asserts them idempotently for the deployed
//!    profile ([`owned_definitions`]).

use std::collections::BTreeMap;

use gts::{GtsTypeId, SELF_ID_REF};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use toolkit_gts::{
    GtsSchema, GtsTraitsSchema, InventoryInstance, PluginV1, gts_id, gts_type_schema,
};

/// GTS resource type for Quota records (declarative caps).
pub const QUOTA_RESOURCE: &str = gts_id!("cf.core.qe.quota.v1~");

/// GTS resource type for Quota Resolution Policy records and their versions.
pub const POLICY_RESOURCE: &str = gts_id!("cf.core.qe.policy.v1~");

/// GTS resource type for two-phase capacity leases.
pub const LEASE_RESOURCE: &str = gts_id!("cf.core.qe.lease.v1~");

/// GTS resource type for operation-log records.
pub const OPERATION_RESOURCE: &str = gts_id!("cf.core.qe.operation.v1~");

// ---------------------------------------------------------------------------
// Projection contract bases (ADR-0007)
// ---------------------------------------------------------------------------

// Owners derive subject, request/constraint, and optional resource contracts
// from these bases.

// @cpt-begin:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-author
/// Abstract base of the owner-published subject projections. Its required
/// traits are `scope` and `admitted_metrics`.
pub const SUBJECT_BASE: &str = gts_id!("cf.core.qe.subj.v1~");
// @cpt-end:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-author

// @cpt-begin:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-res
/// Abstract base of the owner-published resource projections.
pub const RESOURCE_BASE: &str = gts_id!("cf.core.qe.res.v1~");
// @cpt-end:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-res

// @cpt-begin:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-attrs
/// Abstract base of the per-metric request contracts. Its required traits are
/// `metric` and `constraint_contract`.
pub const REQUEST_BASE: &str = gts_id!("cf.core.qe.request.v1~");

/// Abstract base of the operator-authored arbitration constraint contracts.
pub const CONSTRAINT_BASE: &str = gts_id!("cf.core.qe.constraint.v1~");
// @cpt-end:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-attrs

/// The concrete scope-discriminator type. Its well-known instances classify
/// caller-supplied subject references.
pub const SCOPE_TYPE: &str = gts_id!("cf.core.qe.scope.v1~");

/// The `user` scope instance.
pub const SCOPE_USER: &str = gts_id!("cf.core.qe.scope.v1~cf.core.qe.user.v1");

/// The `tenant` scope instance. The gear materializes one subject of this
/// scope from every request's `tenant_id`.
pub const SCOPE_TENANT: &str = gts_id!("cf.core.qe.scope.v1~cf.core.qe.tenant.v1");

/// The platform metric base every admitted metric is an instance of.
///
/// Provisional: PRD section 3.2 names this id pending the platform-wide
/// metric naming decision (PRD section 13). The QE base schemas pin the same
/// id in their `x-gts-instance-ref` narrowing, so the two must change together. Metrics
/// are registry-owned; the gear never registers this base, it only checks
/// admitted metrics against it at bootstrap.
pub const METRIC_BASE_TYPE: &str = gts_id!("cf.core.qe.metric_type.v1~");

/// One QE-owned GTS definition as the registry receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedDefinition {
    /// The GTS id: a type id (trailing `~`) or an instance id.
    pub id: &'static str,
    /// The document registered under `id`.
    pub document: Value,
}

/// The QE-owned GTS definitions in registration order: the four abstract
/// bases, the scope-discriminator type, then its two well-known instances.
///
/// Registration touches only these definitions. Concrete owner projections
/// are published by their owning gears, and the gear seeds no platform-wide
/// subject instances (ADR-0007).
///
// @cpt-dod:cpt-cf-quota-enforcement-dod-base-registration:p1
#[must_use]
pub fn owned_definitions() -> Vec<OwnedDefinition> {
    let mut out = Vec::with_capacity(7);
    for (id, document) in [
        (SUBJECT_BASE, SubjectV1::gts_schema_with_refs()),
        (RESOURCE_BASE, ResourceV1::gts_schema_with_refs()),
        (REQUEST_BASE, RequestV1::gts_schema_with_refs()),
        (CONSTRAINT_BASE, ConstraintV1::gts_schema_with_refs()),
        (SCOPE_TYPE, ScopeV1::gts_schema_with_refs()),
    ] {
        out.push(OwnedDefinition { id, document });
    }
    for id in [SCOPE_USER, SCOPE_TENANT] {
        out.push(OwnedDefinition {
            id,
            document: scope_instance(id),
        });
    }
    out
}

/// The identity-only document of a well-known scope instance.
fn scope_instance(id: &str) -> Value {
    json!({ "id": id, "type": SCOPE_TYPE })
}

inventory::submit! {
    InventoryInstance {
        type_id: SCOPE_TYPE,
        instance_id: SCOPE_USER,
        payload_fn: || scope_instance(SCOPE_USER),
    }
}
inventory::submit! {
    InventoryInstance {
        type_id: SCOPE_TYPE,
        instance_id: SCOPE_TENANT,
        payload_fn: || scope_instance(SCOPE_TENANT),
    }
}

// A well-known scope instance id. Narrowing is pattern-level only: the registry
// checks the id matches the scope type, not that the instance is registered.
#[derive(Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-instance-ref" = gts_id!("cf.core.qe.scope.v1~")))]
pub struct ScopeRef(pub String);

// A registered metric id under the platform metric base (provisional, see
// [`METRIC_BASE_TYPE`]). Narrowing is pattern-level only: metric registration,
// the instance-of relationship to the metric base, and debit admission are
// checked by QE at bootstrap and Gateway ingress (ADR-0007).
#[derive(Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-instance-ref" = gts_id!("cf.core.qe.metric_type.v1~")))]
pub struct MetricRef(pub String);

// A concrete arbitration-constraint contract: a type derived from the
// constraint base.
#[derive(Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-type-ref" = gts_id!("cf.core.qe.constraint.v1~")))]
pub struct ConstraintRef(pub String);

// Traits of the subject base, emitted as its `x-gts-traits-schema`. Schema-only
// keywords: no caller can send them. A `///` here would become a description on
// the trait-schema object, so the struct carries `//`; field docs are wanted.
#[allow(clippy::doc_markdown)]
#[derive(Serialize, JsonSchema, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct SubjectTraits {
    /// Required scope discriminator: a well-known instance of gts.cf.core.qe.scope.v1~. QE reads the registry-validated effective trait and maps caller-supplied SubjectRef kinds through the catalogue; scope is never encoded in the type-id name segment.
    pub scope: ScopeRef,
    /// Metrics this projection admits. x-gts-instance-ref narrowing is pattern-level only: metric registration, the instance-of relationship to the metric base, and debit admission are checked by QE at bootstrap and Gateway ingress (ADR-0007). At most one projection admits a given metric per (metric, scope) pair per deployment; narrowing this set is a breaking contract change.
    #[schemars(length(min = 1))]
    pub admitted_metrics: Vec<MetricRef>,
}

// Traits of the request base, emitted as its `x-gts-traits-schema`.
#[allow(clippy::doc_markdown)]
#[derive(Serialize, JsonSchema, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestTraits {
    /// The single registered metric governed by this request contract.
    pub metric: MetricRef,
    /// The concrete arbitration-constraint contract attached to this metric request contract.
    pub constraint_contract: ConstraintRef,
}

// @cpt-begin:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-author
/// Abstract base for subject projections (ADR-0007). The metric owner derives
/// one concrete projection per scope; QE maps a caller-supplied `SubjectRef`
/// kind to the unique projection for (metric, scope). Operation metadata is
/// defined once by the metric request contract, not by each projection.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.qe.subj.v1~"),
    description = "Abstract base for subject projections (ADR-0007). The metric owner derives one concrete projection per scope; QE maps a caller-supplied SubjectRef kind to the unique projection for (metric, scope). Operation metadata is defined once by the metric request contract, not by each projection.",
    properties = "r#type",
    traits_schema = inline(SubjectTraits),
    gts_abstract = true,
)]
pub struct SubjectV1 {
    /// The concrete owner-declared subject projection. QE selects this type from the catalogue; consumers do not supply it.
    #[schemars(with = "String", extend("x-gts-type-ref" = SELF_ID_REF))]
    pub r#type: GtsTypeId,
}
// @cpt-end:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-author

// @cpt-begin:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-res
/// Abstract base for resource projections (ADR-0007). Completes the request
/// contract for resource-aware selection; descriptive input only, and not part
/// of the P1 counter key.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.qe.res.v1~"),
    description = "Abstract base for resource projections (ADR-0007). Completes the request contract for resource-aware selection; descriptive input only, and not part of the P1 counter key.",
    properties = "r#type,id,metadata",
    gts_abstract = true,
)]
pub struct ResourceV1 {
    /// The concrete owner-declared resource projection.
    #[schemars(with = "String", extend("x-gts-type-ref" = SELF_ID_REF))]
    pub r#type: GtsTypeId,
    /// Optional resource identity: the contract may describe a resource class or a concrete resource.
    #[serde(default)]
    #[schemars(with = "String")]
    pub id: Option<String>,
    /// Schematized resource properties, refined by the derived type. Required on the wire per the base contract rule.
    pub metadata: BTreeMap<String, serde_json::Value>,
}
// @cpt-end:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-res

// @cpt-begin:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-attrs
/// Abstract base for a metric owner's operation request contract (ADR-0007).
/// One concrete contract is published per metric and validates the
/// operation-level metadata object shared by every subject scope and caller.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.qe.request.v1~"),
    description = "Abstract base for a metric owner's operation request contract (ADR-0007). One concrete contract is published per metric and validates the operation-level metadata object shared by every subject scope and caller.",
    properties = "r#type,metadata",
    traits_schema = inline(RequestTraits),
    gts_abstract = true,
)]
pub struct RequestV1 {
    /// The concrete owner-published metric request contract selected by QE; consumers do not supply this type.
    #[schemars(with = "String", extend("x-gts-type-ref" = SELF_ID_REF))]
    pub r#type: GtsTypeId,
    /// The operation-level metadata object refined by the concrete contract and supplied once per evaluation request.
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Abstract base for operator-authored arbitration constraints (ADR-0007). The
/// metric owner attaches one derived constraint contract to the metric request
/// contract; operators populate `Quota.metadata` at create/update, and QE
/// validates it once before persistence (snapshot timing per ADR-0003). The
/// request schema MUST NOT be reused here.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.qe.constraint.v1~"),
    description = "Abstract base for operator-authored arbitration constraints (ADR-0007). The metric owner attaches one derived constraint contract to the metric request contract; operators populate Quota.metadata at create/update, and QE validates it once before persistence (snapshot timing per ADR-0003). The request schema MUST NOT be reused here.",
    properties = "r#type,metadata",
    gts_abstract = true,
)]
pub struct ConstraintV1 {
    /// The concrete owner-declared constraint contract.
    #[schemars(with = "String", extend("x-gts-type-ref" = SELF_ID_REF))]
    pub r#type: GtsTypeId,
    /// Operator-populated arbitration object (quota.metadata), refined by the derived type; may carry fields such as regions or weight with no request-side counterpart.
    pub metadata: BTreeMap<String, serde_json::Value>,
}
// @cpt-end:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-attrs

/// Concrete QE scope-discriminator type (ADR-0007). Its well-known instances
/// classify caller-supplied subject references; each subject projection
/// carries one as its required scope trait. Registered idempotently by QE at
/// bootstrap together with the P1 well-known instances.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.qe.scope.v1~"),
    description = "Concrete QE scope-discriminator type (ADR-0007). Its well-known instances classify caller-supplied subject references; each subject projection carries one as its required scope trait. Registered idempotently by QE at bootstrap together with the P1 well-known instances.",
    properties = "id,r#type",
)]
pub struct ScopeV1 {
    /// The well-known scope instance id. A `String` rather than `GtsInstanceId`: the macro refuses a base declaring both an instance-id and a type-id field.
    #[schemars(extend("x-gts-instance-ref" = SELF_ID_REF))]
    pub id: String,
    /// Always the scope-discriminator type itself.
    #[schemars(with = "String", extend("x-gts-type-ref" = SELF_ID_REF))]
    pub r#type: GtsTypeId,
}

// ---------------------------------------------------------------------------
// Plugin specs
// ---------------------------------------------------------------------------

/// GTS plugin specification for quota-enforcement storage backends.
///
/// Instance id shape:
/// `gts.cf.toolkit.plugins.plugin.v1~cf.core.qe.storage_plugin.v1~<vendor>.<pkg>.<ns>.<name>.v1`
// @cpt-dod:cpt-cf-quota-enforcement-dod-sdk-contracts:p1
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.qe.storage_plugin.v1~"),
    description = "Quota Enforcement storage plugin specification",
    properties = "",
)]
pub struct QuotaEnforcementStoragePluginSpecV1;

/// GTS plugin specification for notification sinks. Unlike storage, every
/// registered instance of every vendor receives every event.
///
/// Instance id shape:
/// `gts.cf.toolkit.plugins.plugin.v1~cf.core.qe.notification_sink.v1~<vendor>.<pkg>.<ns>.<name>.v1`
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.qe.notification_sink.v1~"),
    description = "Quota Enforcement notification sink specification",
    properties = "",
)]
pub struct QuotaNotificationSinkSpecV1;

// Coordination uses the platform cluster profile rather than a plugin spec.

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_tests.rs"]
mod gts_tests;
