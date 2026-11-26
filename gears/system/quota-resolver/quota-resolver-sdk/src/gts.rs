//! GTS types owned by the quota resolver: the quota subject base and the tenant
//! subject type, the metric base with its traits, and the plugin spec.

use schemars::JsonSchema;
use toolkit_gts::local::gts_type_schema;
use toolkit_gts::{GtsTraitsSchema, PluginV1, gts_id};

/// GTS type definition for quota resolver plugin instances.
///
/// # Instance ID Format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~<vendor>.<package>.quota_resolver.plugin.v1~
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.quota_resolver.plugin.v1~"),
    description = "Quota Resolver plugin specification",
    properties = "",
)]
pub struct QuotaResolverPluginSpecV1;

/// Id of the quota subject base type.
pub const QUOTA_SUBJECT_TYPE_ID: &str = gts_id!("cf.core.quota.subj.v1~");
/// Id of the tenant quota subject type.
pub const QUOTA_TENANT_SUBJECT_TYPE_ID: &str =
    gts_id!("cf.core.quota.subj.v1~cf.core.quota.tenant.v1~");
/// Id of the quota metric base type.
pub const QUOTA_METRIC_TYPE_ID: &str = gts_id!("cf.core.quota.metric.v1~");

// Narrows `type` from `GtsTypeId`'s open `x-gts-type-ref: "gts.*"` to `/$id`:
// an object names the derived type it instantiates.
fn self_type_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = <gts::GtsTypeId as JsonSchema>::json_schema(generator);
    schema.remove("title");
    schema.insert(gts::X_GTS_TYPE_REF.to_owned(), gts::SELF_ID_REF.into());
    schema
}

/// Quota subject base, `gts.cf.core.quota.subj.v1~`. Abstract and closed.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.quota.subj.v1~"),
    description = "Abstract base of quota subject types: whom a quota applies to. A request and a quota name a subject as its type, derived from this base, and an id. The operator registers the subject types of principals; a module registers the subject types of what it charges.",
    properties = "gts_type,id",
    gts_abstract = true
)]
pub struct QuotaSubjectV1 {
    /// The concrete quota subject type.
    #[serde(rename = "type")]
    #[schemars(schema_with = "self_type_schema")]
    pub gts_type: gts::GtsTypeId,
    /// Opaque subject id. A derived type may narrow its format; a derived type that restates id restates minLength too.
    #[schemars(length(min = 1))]
    pub id: String,
}

// The tenant type narrows `id` of the base without adding a property, which a
// derived `#[gts_type_schema]` cannot express: the macro nests a derived
// type's fields under a generic slot that the closed base would reject.
fn tenant_subject_schema() -> String {
    serde_json::json!({
        "$id": format!("gts://{QUOTA_TENANT_SUBJECT_TYPE_ID}"),
        "$schema": gts::JSON_SCHEMA_DRAFT_07,
        "description": "The tenant quota subject type. Its id is the tenant id; in a request it must name the request's tenant. Final: no type derives from it.",
        "x-gts-final": true,
        "type": "object",
        "allOf": [
            { "$ref": format!("gts://{QUOTA_SUBJECT_TYPE_ID}") },
            {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "minLength": 1,
                        "format": "uuid",
                        "description": "The tenant id."
                    }
                }
            }
        ]
    })
    .to_string()
}

const TENANT_SUBJECT_DECLARATION: toolkit_gts::InventoryTypeSchema =
    toolkit_gts::InventoryTypeSchema {
        type_id: QUOTA_TENANT_SUBJECT_TYPE_ID,
        schema_fn: tenant_subject_schema,
    };

toolkit_gts::inventory::submit! { TENANT_SUBJECT_DECLARATION }
toolkit_gts::inventory::submit! {
    crate::__toolkit_gts_inventory::TypeSchema(TENANT_SUBJECT_DECLARATION)
}

// A single admitted quota subject type id. `x-gts-type-ref` validates the
// shape of the id, not that the type is registered.
#[allow(dead_code)]
#[derive(serde::Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-type-ref" = gts_id!("cf.core.quota.subj.v1~")))]
pub struct SubjectTypeRef(pub String);

/// How a metric counts usage.
#[derive(JsonSchema, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Counter,
    Gauge,
}

/// Whether operations on a metric are checked against quotas.
#[derive(JsonSchema, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricEnforcement {
    QuotaGated,
    Direct,
}

// Traits of `gts.cf.core.quota.metric.v1~`, emitted as its `x-gts-traits-schema`.
// The field docs are schema descriptions, not markdown.
#[allow(clippy::doc_markdown)]
#[derive(JsonSchema, serde::Serialize, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct QuotaMetricTraits {
    /// counter: usage adds up over time, for example tokens. gauge: usage is a level held at a moment, for example concurrent sessions.
    pub kind: MetricKind,
    /// Quota subject types the metric's quotas may count; a type derived from an admitted type is admitted. The tenant subject type is always admitted, so the empty default admits tenant quotas only. The list guards against a wrong configuration when the owner knows the metric's kinds of subject; an owner that does not know them admits the base gts.cf.core.quota.subj.v1~, which admits every subject type.
    #[serde(default)]
    pub admitted_subjects: Vec<SubjectTypeRef>,
    /// quota_gated: operations on the metric are checked against quotas. direct: the metric is not checked against quotas, and the backend rejects the operations that take capacity on it (debit, lease acquisition), while commit, release, and return still run; a quota can still be created for it and has no effect.
    #[serde(default = "quota_gated")]
    pub enforcement: MetricEnforcement,
}

fn quota_gated() -> MetricEnforcement {
    MetricEnforcement::QuotaGated
}

/// Quota metric base, `gts.cf.core.quota.metric.v1~`. Abstract and closed.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.quota.metric.v1~"),
    description = "Abstract base of quota metric types: what a quota counts. A metric is a type derived from this base, registered by the module that owns it and named under that module. A request names a metric as its type, an optional id, and metadata; a quota names the metric type.",
    properties = "gts_type,id,metadata",
    traits_schema = inline(QuotaMetricTraits),
    gts_abstract = true
)]
pub struct QuotaMetricV1<M: gts::GtsSchema> {
    /// The concrete quota metric type.
    #[serde(rename = "type")]
    #[schemars(schema_with = "self_type_schema")]
    pub gts_type: gts::GtsTypeId,
    /// Optional id of the one thing the amount is counted for, for example a model or a virtual machine; without it the request names the metric as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(min = 1))]
    pub id: Option<String>,
    /// Attributes of the charged operation that the backend's policies read, refined by the derived type.
    pub metadata: M,
}

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
