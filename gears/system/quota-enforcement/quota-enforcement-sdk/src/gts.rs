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
//!    They are declared as `gts_type_schema` structs in this crate's
//!    collector ([`crate::gts_declarations`]); quota-enforcement publishes
//!    them with `gts_publication(crates = [quota_enforcement_sdk])` before its
//!    `init`. The reviewed contract stays in `docs/schemas/`, and the crate's
//!    tests hold the generated documents to it.

use gts::{GtsInstanceId, GtsTypeId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use toolkit_gts::local::{gts_instance_raw, gts_type_schema};
use toolkit_gts::{GtsTraitsSchema, PluginV1, gts_id};

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

// @cpt-dod:cpt-cf-quota-enforcement-dod-base-registration:p1
// The QE-owned declarations, in this crate's collector (`gts_declarations`)
// and published by quota-enforcement with `gts_publication(crates = [...])`.
// `docs/schemas/` keeps the reviewed contract; `gts_tests.rs` holds these
// declarations to it.

/// A registered metric id under [`METRIC_BASE_TYPE`] (a trait value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(
    transparent,
    extend("x-gts-instance-ref" = "gts.cf.core.qe.metric_type.v1~")
)]
pub struct MetricInstanceRef(#[schemars(with = "String")] pub GtsInstanceId);

/// Traits of a subject projection ([`SubjectProjectionV1`]). Field docs are
/// the published schema descriptions, verbatim from `docs/schemas/`.
#[allow(
    clippy::doc_markdown,
    reason = "doc text is the published schema description"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct SubjectTraitsV1 {
    /// Required scope discriminator: a well-known instance of gts.cf.core.qe.scope.v1~. QE reads the registry-validated effective trait and maps caller-supplied SubjectRef kinds through the catalogue; scope is never encoded in the type-id name segment.
    #[schemars(
        with = "String",
        extend("x-gts-instance-ref" = "gts.cf.core.qe.scope.v1~")
    )]
    pub scope: GtsInstanceId,
    /// Metrics this projection admits. x-gts-instance-ref narrowing is pattern-level only: metric registration, the instance-of relationship to the metric base, and debit admission are checked by QE at bootstrap and Gateway ingress (ADR-0007). At most one projection admits a given metric per (metric, scope) pair per deployment; narrowing this set is a breaking contract change.
    #[schemars(length(min = 1))]
    pub admitted_metrics: Vec<MetricInstanceRef>,
}

/// Traits of a metric request contract ([`RequestContractV1`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestTraitsV1 {
    /// The single registered metric governed by this request contract.
    #[schemars(
        with = "String",
        extend("x-gts-instance-ref" = "gts.cf.core.qe.metric_type.v1~")
    )]
    pub metric: GtsInstanceId,
    /// The concrete arbitration-constraint contract attached to this metric request contract.
    #[schemars(
        with = "String",
        extend("x-gts-type-ref" = "gts.cf.core.qe.constraint.v1~")
    )]
    pub constraint_contract: GtsTypeId,
}

/// `gts.cf.core.qe.subj.v1~`: the abstract base of subject projections.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.qe.subj.v1~"),
    description = "Abstract base for subject projections (ADR-0007). The metric owner derives one concrete projection per scope; QE maps a caller-supplied SubjectRef kind to the unique projection for (metric, scope). Operation metadata is defined once by the metric request contract, not by each projection.",
    properties = "gts_type",
    base = true,
    traits_schema = inline(SubjectTraitsV1),
    gts_abstract = true
)]
#[derive(Debug)]
pub struct SubjectProjectionV1 {
    /// The concrete owner-declared subject projection. QE selects this type from the catalogue; consumers do not supply it.
    #[schemars(with = "String", extend("x-gts-type-ref" = "/$id"))]
    #[serde(rename = "type")]
    pub gts_type: GtsTypeId,
}

/// `gts.cf.core.qe.res.v1~`: the abstract base of resource projections.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.qe.res.v1~"),
    description = "Abstract base for resource projections (ADR-0007). Completes the request contract for resource-aware selection; descriptive input only, and not part of the P1 counter key.",
    properties = "gts_type,id,metadata",
    base = true,
    gts_abstract = true
)]
#[derive(Debug)]
pub struct ResourceProjectionV1 {
    /// The concrete owner-declared resource projection.
    #[schemars(with = "String", extend("x-gts-type-ref" = "/$id"))]
    #[serde(rename = "type")]
    pub gts_type: GtsTypeId,
    /// Optional resource identity: the contract may describe a resource class or a concrete resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub id: Option<String>,
    /// Schematized resource properties, refined by the derived type. Required on the wire per the base contract rule.
    pub metadata: Map<String, Value>,
}

/// `gts.cf.core.qe.request.v1~`: the abstract base of metric request contracts.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.qe.request.v1~"),
    description = "Abstract base for a metric owner's operation request contract (ADR-0007). One concrete contract is published per metric and validates the operation-level metadata object shared by every subject scope and caller.",
    properties = "gts_type,metadata",
    base = true,
    traits_schema = inline(RequestTraitsV1),
    gts_abstract = true
)]
#[derive(Debug)]
pub struct RequestContractV1 {
    /// The concrete owner-published metric request contract selected by QE; consumers do not supply this type.
    #[schemars(with = "String", extend("x-gts-type-ref" = "/$id"))]
    #[serde(rename = "type")]
    pub gts_type: GtsTypeId,
    /// The operation-level metadata object refined by the concrete contract and supplied once per evaluation request.
    pub metadata: Map<String, Value>,
}

/// `gts.cf.core.qe.constraint.v1~`: the abstract base of arbitration constraint contracts.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.qe.constraint.v1~"),
    description = "Abstract base for operator-authored arbitration constraints (ADR-0007). The metric owner attaches one derived constraint contract to the metric request contract; operators populate Quota.metadata at create/update, and QE validates it once before persistence (snapshot timing per ADR-0003). The request schema MUST NOT be reused here.",
    properties = "gts_type,metadata",
    base = true,
    gts_abstract = true
)]
#[derive(Debug)]
pub struct ConstraintContractV1 {
    /// The concrete owner-declared constraint contract.
    #[schemars(with = "String", extend("x-gts-type-ref" = "/$id"))]
    #[serde(rename = "type")]
    pub gts_type: GtsTypeId,
    /// Operator-populated arbitration object (quota.metadata), refined by the derived type; may carry fields such as regions or weight with no request-side counterpart.
    pub metadata: Map<String, Value>,
}

/// `gts.cf.core.qe.scope.v1~`: the concrete scope-discriminator type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.qe.scope.v1~"),
    description = "Concrete QE scope-discriminator type (ADR-0007). Its well-known instances classify caller-supplied subject references; each subject projection carries one as its required scope trait.",
    properties = "id,gts_type",
    base = true
)]
#[derive(Debug)]
pub struct ScopeV1 {
    /// The well-known scope instance id.
    #[schemars(with = "String", extend("x-gts-instance-ref" = "/$id"))]
    pub id: GtsInstanceId,
    // A `String`: the schema macro takes a typed id or a typed type field on
    // a base, not both.
    /// Always the scope-discriminator type itself.
    #[schemars(with = "String", extend("x-gts-type-ref" = "/$id"))]
    #[serde(rename = "type")]
    pub gts_type: String,
}

gts_instance_raw!({
    "id": gts_id!("cf.core.qe.scope.v1~cf.core.qe.user.v1"),
    "type": gts_id!("cf.core.qe.scope.v1~")
});
gts_instance_raw!({
    "id": gts_id!("cf.core.qe.scope.v1~cf.core.qe.tenant.v1"),
    "type": gts_id!("cf.core.qe.scope.v1~")
});

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
