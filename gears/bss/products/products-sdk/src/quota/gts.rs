//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-contract:p1
//! The commercial quota GTS contract (P-D-267): the Products-owned schemas and the discovery
//! plugin specification. The toolkit inventory seeds types-registry with them; nothing registers
//! them by hand.
//!
//! Products registers no instance of these schemas. The adapter owner registers the metric, unit,
//! accounting-policy and scope-policy instances of its service in its own namespace, and its
//! discovery records reference them; Products judges every reference at intake.
//!
//! Each schema struct is also the wire type its records parse into: `deny_unknown_fields` refuses
//! a member the schema does not declare, and only `max` and the open maps may be absent or grow.
//! Every GTS reference of a record parses into a typed id, so a reference that is not a GTS id of
//! its kind (an instance or a type) is refused at the parse; the wire keeps plain strings.
#![allow(unknown_lints, de0901_gts_string_pattern)]
use std::collections::BTreeMap;

use gts::{GtsInstanceId, GtsTypeId, SELF_ID_REF};
use schemars::JsonSchema;
use serde_json::{Map, Value};
use toolkit_gts::{PluginV1, gts_id, gts_type_schema};
use uuid::Uuid;

/// The quota unit schema.
pub const UNIT_TYPE: &str = gts_id!("cf.bss.products.quota_unit.v1~");
/// The quota metric schema.
pub const METRIC_TYPE: &str = gts_id!("cf.bss.products.quota_metric.v1~");
/// The quota accounting-policy schema.
pub const ACCOUNTING_POLICY_TYPE: &str = gts_id!("cf.bss.products.quota_accounting_policy.v1~");
/// The quota scope-policy schema.
pub const SCOPE_POLICY_TYPE: &str = gts_id!("cf.bss.products.quota_scope_policy.v1~");
/// The quota capability schema; an adapter may register a schema derived from it.
pub const CAPABILITY_TYPE: &str = gts_id!("cf.bss.products.quota_capability.v1~");
/// The discovery plugin specification adapters register their plugin instances under.
pub const DISCOVERY_PLUGIN_TYPE: &str =
    gts_id!("cf.toolkit.plugins.plugin.v1~cf.bss.products.quota_discovery.v1~");

/// A positive canonical decimal integer of at most 39 digits.
const POSITIVE_DECIMAL: &str = r"^[1-9][0-9]{0,38}$";
/// A non-negative canonical decimal integer of at most 39 digits.
const DECIMAL: &str = r"^(0|[1-9][0-9]{0,38})$";

/// A named unit: exact rational multiplication into its canonical unit (quotas DESIGN §3.3.1).
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.bss.products.quota_unit.v1~"),
    description = "Commercial quota unit: symbol and exact rational factor into its canonical unit",
    properties = "id,symbol,canonical_unit_ref,numerator,denominator",
    gts_final = true,
)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaUnitV1 {
    /// The unit's own named instance id.
    #[schemars(with = "String", extend("x-gts-instance-ref" = SELF_ID_REF))]
    pub id: GtsInstanceId,
    /// Display symbol; never an identity.
    #[schemars(length(min = 1, max = 32))]
    pub symbol: String,
    /// The canonical unit; itself for a canonical unit.
    #[schemars(extend("x-gts-instance-ref" = UNIT_TYPE))]
    pub canonical_unit_ref: String,
    /// Positive decimal integer.
    #[schemars(regex(pattern = POSITIVE_DECIMAL))]
    pub numerator: String,
    /// Positive decimal integer.
    #[schemars(regex(pattern = POSITIVE_DECIMAL))]
    pub denominator: String,
}

/// What an allocation quota counts, with its display text and canonical unit (quotas DESIGN
/// §3.3.1).
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.bss.products.quota_metric.v1~"),
    description = "Commercial quota metric: what an allocation quota counts and its canonical unit",
    properties = "id,name,display_name,description,quota_kind,canonical_unit_ref,properties",
)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaMetricV1 {
    /// The metric's own named instance id.
    #[schemars(with = "String", extend("x-gts-instance-ref" = SELF_ID_REF))]
    pub id: GtsInstanceId,
    /// Stable name for operators; never a key.
    #[schemars(length(min = 1, max = 200))]
    pub name: String,
    /// Display name by locale tag; English is required at intake.
    #[schemars(extend("minProperties" = 1))]
    pub display_name: BTreeMap<String, String>,
    /// Description by locale tag; English is required at intake.
    #[schemars(extend("minProperties" = 1))]
    pub description: BTreeMap<String, String>,
    /// The kind of quota; only allocation in this version.
    pub quota_kind: QuotaKind,
    /// The canonical unit every unit of this metric converts into.
    #[schemars(extend("x-gts-instance-ref" = UNIT_TYPE))]
    pub canonical_unit_ref: String,
    /// Open, schema-validated extension members of a derived metric schema.
    pub properties: Map<String, Value>,
}

/// The kind of quantity a metric counts. Closed in this version.
//
// Variants carry `//` comments, not `///`: a variant doc would make schemars emit a `oneOf`
// of `const` subschemas instead of the flat `enum` the registry reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum QuotaKind {
    // Capacity the service allocates to a subject and enforces.
    Allocation,
}

/// How a metric's quantity is counted and aggregated (quotas DESIGN §3.3.1.3).
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.bss.products.quota_accounting_policy.v1~"),
    description = "Commercial quota accounting policy: which quantity counts and how it aggregates",
    properties = "id,quantity_basis,aggregation,description",
    gts_final = true,
)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaAccountingPolicyV1 {
    /// The policy's own named instance id.
    #[schemars(with = "String", extend("x-gts-instance-ref" = SELF_ID_REF))]
    pub id: GtsInstanceId,
    /// The quantity that counts.
    pub quantity_basis: QuantityBasis,
    /// How the counted quantities combine.
    pub aggregation: AccountingAggregation,
    /// What the policy counts, by locale tag; English is required at intake.
    #[schemars(extend("minProperties" = 1))]
    pub description: BTreeMap<String, String>,
}

/// The quantity an accounting policy counts. Closed in this version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum QuantityBasis {
    // The capacity allocated, whether or not it is in use.
    AllocatedCapacity,
}

/// How an accounting policy combines quantities. Closed in this version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum AccountingAggregation {
    // The quantities add up.
    Sum,
}

/// Which subject a quota applies to and in which scope it is shared (quotas DESIGN §3.3.1.3).
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.bss.products.quota_scope_policy.v1~"),
    description = "Commercial quota scope policy: the subject a quota applies to and its scope",
    properties = "id,subject_selector,aggregation,required_dimensions",
    gts_final = true,
)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaScopePolicyV1 {
    /// The policy's own named instance id.
    #[schemars(with = "String", extend("x-gts-instance-ref" = SELF_ID_REF))]
    pub id: GtsInstanceId,
    /// The subject the quota applies to.
    pub subject_selector: SubjectSelector,
    /// How the subject's quantities are scoped.
    pub aggregation: ScopeAggregation,
    /// The dimensions that select the one verified scope; unique.
    #[schemars(extend("uniqueItems" = true))]
    pub required_dimensions: Vec<ScopeDimension>,
}

/// The subject a scope policy selects. Closed in this version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum SubjectSelector {
    // The owner of the counted resources.
    ResourceOwner,
}

/// How a scope policy scopes quantities. Closed in this version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum ScopeAggregation {
    // One verified scope, selected by the required dimensions.
    SingleVerifiedScope,
}

/// A dimension that selects a scope. Closed in this version.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum ScopeDimension {
    // The region the resources run in.
    Region,
    // The resource pool the resources draw from.
    ResourcePool,
}

/// One capability an adapter discovers or an administrator authors: what it counts, in which unit,
/// within which bounds, and which operations the adapter supports (quotas DESIGN §3.3.1).
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.bss.products.quota_capability.v1~"),
    description = "Commercial quota capability: what an adapter counts, in which unit and bounds",
    properties = "id,gts_type,capability_key,metric_ref,unit_ref,subject_type_ref,\
                  resource_type_refs,limits,operations,mapping_key,accounting_policy_ref,\
                  scope_policy_ref,change_support,properties",
)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaCapabilityV1 {
    /// The record's own id, stable across snapshots of one adapter.
    pub id: Uuid,
    // Named `gts_type` here and `type_id` on the wire: the schema generator takes a base type
    // only with a `GtsInstanceId` named `id` or a `GtsTypeId` type member, and a record's `id`
    // is a UUID. A plain comment, so it stays out of the schema.
    /// The capability schema or a schema an adapter derived from it.
    #[serde(rename = "type_id")]
    #[schemars(with = "String", extend("x-gts-type-ref" = CAPABILITY_TYPE))]
    pub gts_type: GtsTypeId,
    /// The adapter's key of the capability, stable across snapshots.
    #[schemars(length(min = 1, max = 255))]
    pub capability_key: String,
    /// The metric the capability counts.
    #[schemars(with = "String", extend("x-gts-instance-ref" = METRIC_TYPE))]
    pub metric_ref: GtsInstanceId,
    /// The unit `limits` are stated in.
    #[schemars(with = "String", extend("x-gts-instance-ref" = UNIT_TYPE))]
    pub unit_ref: GtsInstanceId,
    /// The type of the subject the quota applies to.
    #[schemars(with = "String", extend("x-gts-type-ref" = gts_id!("*")))]
    pub subject_type_ref: GtsTypeId,
    /// The types of the resources the quota counts.
    #[schemars(
        with = "Vec<String>",
        length(min = 1, max = 200),
        extend("items" = { "type": "string", "x-gts-type-ref": gts_id!("*") })
    )]
    pub resource_type_refs: Vec<GtsTypeId>,
    /// The bounds the adapter enforces, in `unit_ref`.
    pub limits: QuotaLimits,
    /// The operations the adapter supports for this capability.
    pub operations: QuotaOperations,
    /// The adapter's key of its enforcement mapping.
    #[schemars(length(min = 1, max = 255))]
    pub mapping_key: String,
    /// How the quantity is counted.
    #[schemars(with = "String", extend("x-gts-instance-ref" = ACCOUNTING_POLICY_TYPE))]
    pub accounting_policy_ref: GtsInstanceId,
    /// Which subject and scope the quota applies to.
    #[schemars(with = "String", extend("x-gts-instance-ref" = SCOPE_POLICY_TYPE))]
    pub scope_policy_ref: GtsInstanceId,
    /// Which changes of an installed quota the adapter supports.
    pub change_support: QuotaChangeSupport,
    /// Open, schema-validated extension members of a derived capability schema.
    pub properties: Map<String, Value>,
}

/// A capability's bounds, as canonical decimal strings in its unit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct QuotaLimits {
    /// The smallest amount.
    #[schemars(regex(pattern = DECIMAL))]
    pub min: String,
    /// The granularity of an amount; positive.
    #[schemars(regex(pattern = POSITIVE_DECIMAL))]
    pub step: String,
    /// The largest amount; absent when unbounded.
    // One spelling of "unbounded": the member is absent. `null` is refused at the parse and never
    // written, so two records that mean the same thing never carry different digests.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_string"
    )]
    #[schemars(with = "String", regex(pattern = DECIMAL))]
    pub max: Option<String>,
}

/// A member that, when present, is a string: `null` is not a second spelling of "absent".
fn present_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <String as serde::Deserialize>::deserialize(deserializer).map(Some)
}

/// The quota operations, each stated explicitly; an absent flag is refused.
// Named flags are the package's wire contract (quotas DESIGN §3.3.1), not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct QuotaOperations {
    /// Install a quota.
    pub apply: bool,
    /// Read an installed quota back.
    pub read_application: bool,
    /// Recover the outcome of an interrupted operation.
    pub recover_operation: bool,
    /// Restrict new allocations.
    pub restrict: bool,
    /// Remove an installed quota.
    pub revoke: bool,
    /// Lift a restriction.
    pub resume: bool,
}

/// Which changes of an installed quota an adapter supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct QuotaChangeSupport {
    /// The first installation of a quota.
    pub initial_set: bool,
    /// Raising an installed quota.
    pub increase: bool,
}

/// The discovery plugin specification adapters register instances of (P-D-267).
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.bss.products.quota_discovery.v1~"),
    description = "Commercial quota capability discovery plugin specification",
    properties = "",
)]
pub struct QuotaDiscoveryPluginSpecV1;

#[cfg(test)]
#[path = "gts_tests.rs"]
mod tests;
