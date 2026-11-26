// Created: 2026-08-06 by Constructor Tech
// Updated: 2026-11-05 by Constructor Tech
//! `OData` filter / order surface of the model lists
//! ([`list_available_models`] and [`list_models`]; DESIGN §3.3).
//!
//! [`list_available_models`]: crate::ModelRegistryClientV1::list_available_models
//! [`list_models`]: crate::ModelRegistryClientV1::list_models

use chrono::{DateTime, Utc};
use toolkit_odata::filter::FilterField as _;
use toolkit_odata::{FieldRef, Schema};
use toolkit_odata_macros::ODataFilterable;
use uuid::Uuid;

/// Filterable wire surface of both model lists. Every field maps to a real
/// `models` column; the capability fields are the effective values
/// (declared and not disabled), `api_*` the `supported_api` members, and
/// `max_input_tokens` / `max_output_tokens` the `context_window` limits.
/// Names are flat (`gts_type`, not `info.gts_type`). Fields stored inside
/// `provider_settings`, `default_parameters`, or `additional_info` are not
/// filterable.
///
/// Never constructed at runtime: the struct exists to carry the annotations;
/// the generated [`ModelFilterField`] enum is the usable contract.
#[derive(ODataFilterable)]
#[allow(clippy::struct_excessive_bools)]
pub struct ModelQuery {
    // Identity
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    #[odata(filter(kind = "Uuid"))]
    pub tenant_id: Uuid,
    #[odata(filter(kind = "Uuid"))]
    pub provider_id: Uuid,
    #[odata(filter(kind = "String"))]
    pub name: String,
    #[odata(filter(kind = "String"))]
    pub canonical_id: String,
    #[odata(filter(kind = "String"))]
    pub provider_model_id: String,
    #[odata(filter(kind = "String"))]
    pub gts_type: String,
    #[odata(filter(kind = "String"))]
    pub sharing_mode: String,
    // Approval
    #[odata(filter(kind = "String"))]
    pub approval_state: String,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub approval_decided_at: DateTime<Utc>,
    // Capabilities (effective values)
    #[odata(filter(kind = "Bool"))]
    pub vision: bool,
    #[odata(filter(kind = "Bool"))]
    pub reasoning_effort: bool,
    #[odata(filter(kind = "Bool"))]
    pub tools: bool,
    #[odata(filter(kind = "Bool"))]
    pub json_schema: bool,
    #[odata(filter(kind = "Bool"))]
    pub streaming: bool,
    #[odata(filter(kind = "Bool"))]
    pub file_input: bool,
    #[odata(filter(kind = "Bool"))]
    pub image_generation: bool,
    #[odata(filter(kind = "Bool"))]
    pub audio_input: bool,
    #[odata(filter(kind = "Bool"))]
    pub audio_output: bool,
    #[odata(filter(kind = "Bool"))]
    pub code_interpreter: bool,
    #[odata(filter(kind = "Bool"))]
    pub web_search: bool,
    // Supported API
    #[odata(filter(kind = "Bool"))]
    pub api_completion: bool,
    #[odata(filter(kind = "Bool"))]
    pub api_embedding: bool,
    #[odata(filter(kind = "Bool"))]
    pub api_batch: bool,
    // User-facing
    #[odata(filter(kind = "String"))]
    pub display_name: String,
    #[odata(filter(kind = "String", orderable = false))]
    pub family: String,
    #[odata(filter(kind = "String", orderable = false))]
    pub vendor: String,
    #[odata(filter(kind = "String", orderable = false))]
    pub hosted_by: String,
    #[odata(filter(kind = "String", orderable = false))]
    pub region: String,
    #[odata(filter(kind = "String", orderable = false))]
    pub model_version: String,
    #[odata(filter(kind = "Bool"))]
    pub managed: bool,
    #[odata(filter(kind = "String", orderable = false))]
    pub architecture: String,
    #[odata(filter(kind = "String", orderable = false))]
    pub format: String,
    #[odata(filter(kind = "I64"))]
    pub sort_order: i64,
    #[odata(filter(kind = "DateTimeUtc", orderable = false))]
    pub last_release_at: DateTime<Utc>,
    // Context window
    #[odata(filter(kind = "I64"))]
    pub max_input_tokens: i64,
    #[odata(filter(kind = "I64", orderable = false))]
    pub max_output_tokens: i64,
    // Audit
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: DateTime<Utc>,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub updated_at: DateTime<Utc>,
}

/// `OData` filter / order field enum of the model lists, generated from
/// [`ModelQuery`].
pub use ModelQueryFilterField as ModelFilterField;

/// Nullable model fields: filterable, but a `$orderby` on one is rejected
/// with `invalid_argument` (DESIGN §3.3). Declared `orderable = false` on
/// [`ModelQuery`]; the tests pin this list to those declarations.
pub const MODEL_FILTER_ONLY_FIELDS: &[ModelFilterField] = &[
    ModelFilterField::Family,
    ModelFilterField::Vendor,
    ModelFilterField::HostedBy,
    ModelFilterField::Region,
    ModelFilterField::ModelVersion,
    ModelFilterField::Architecture,
    ModelFilterField::Format,
    ModelFilterField::LastReleaseAt,
    ModelFilterField::MaxOutputTokens,
];

/// Schema marker binding [`ModelFilterField`] to the typed
/// [`QueryBuilder`](toolkit_odata::QueryBuilder).
#[derive(Debug, Clone, Copy)]
pub struct ModelSchema;

impl Schema for ModelSchema {
    type Field = ModelFilterField;

    fn field_name(field: Self::Field) -> &'static str {
        field.name()
    }
}

// ---------------------------------------------------------------------------
// Typed field references
// ---------------------------------------------------------------------------
//
// The type parameter gates the string-only operators (`contains` /
// `startswith` / `endswith`); it does not constrain the value passed to
// `eq` / `ne` / `gt` / …, which accept any `IntoODataValue`. Enum-valued
// fields take their SDK enum directly via the impls in `super::values`.

pub const MODEL_ID: FieldRef<ModelSchema, Uuid> = FieldRef::new(ModelFilterField::Id);
pub const MODEL_TENANT_ID: FieldRef<ModelSchema, Uuid> = FieldRef::new(ModelFilterField::TenantId);
pub const MODEL_PROVIDER_ID: FieldRef<ModelSchema, Uuid> =
    FieldRef::new(ModelFilterField::ProviderId);
pub const MODEL_NAME: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::Name);
pub const MODEL_CANONICAL_ID: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::CanonicalId);
pub const MODEL_PROVIDER_MODEL_ID: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::ProviderModelId);
pub const MODEL_GTS_TYPE: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::GtsType);
pub const MODEL_SHARING_MODE: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::SharingMode);
pub const MODEL_APPROVAL_STATE: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::ApprovalState);
pub const MODEL_APPROVAL_DECIDED_AT: FieldRef<ModelSchema, DateTime<Utc>> =
    FieldRef::new(ModelFilterField::ApprovalDecidedAt);
pub const MODEL_VISION: FieldRef<ModelSchema, bool> = FieldRef::new(ModelFilterField::Vision);
pub const MODEL_REASONING_EFFORT: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::ReasoningEffort);
pub const MODEL_TOOLS: FieldRef<ModelSchema, bool> = FieldRef::new(ModelFilterField::Tools);
pub const MODEL_JSON_SCHEMA: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::JsonSchema);
pub const MODEL_STREAMING: FieldRef<ModelSchema, bool> = FieldRef::new(ModelFilterField::Streaming);
pub const MODEL_FILE_INPUT: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::FileInput);
pub const MODEL_IMAGE_GENERATION: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::ImageGeneration);
pub const MODEL_AUDIO_INPUT: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::AudioInput);
pub const MODEL_AUDIO_OUTPUT: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::AudioOutput);
pub const MODEL_CODE_INTERPRETER: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::CodeInterpreter);
pub const MODEL_WEB_SEARCH: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::WebSearch);
pub const MODEL_API_COMPLETION: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::ApiCompletion);
pub const MODEL_API_EMBEDDING: FieldRef<ModelSchema, bool> =
    FieldRef::new(ModelFilterField::ApiEmbedding);
pub const MODEL_API_BATCH: FieldRef<ModelSchema, bool> = FieldRef::new(ModelFilterField::ApiBatch);
pub const MODEL_DISPLAY_NAME: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::DisplayName);
pub const MODEL_FAMILY: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::Family);
pub const MODEL_VENDOR: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::Vendor);
pub const MODEL_HOSTED_BY: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::HostedBy);
pub const MODEL_REGION: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::Region);
pub const MODEL_MODEL_VERSION: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::ModelVersion);
pub const MODEL_MANAGED: FieldRef<ModelSchema, bool> = FieldRef::new(ModelFilterField::Managed);
pub const MODEL_ARCHITECTURE: FieldRef<ModelSchema, String> =
    FieldRef::new(ModelFilterField::Architecture);
pub const MODEL_FORMAT: FieldRef<ModelSchema, String> = FieldRef::new(ModelFilterField::Format);
pub const MODEL_SORT_ORDER: FieldRef<ModelSchema, i64> = FieldRef::new(ModelFilterField::SortOrder);
pub const MODEL_LAST_RELEASE_AT: FieldRef<ModelSchema, DateTime<Utc>> =
    FieldRef::new(ModelFilterField::LastReleaseAt);
pub const MODEL_MAX_INPUT_TOKENS: FieldRef<ModelSchema, i64> =
    FieldRef::new(ModelFilterField::MaxInputTokens);
pub const MODEL_MAX_OUTPUT_TOKENS: FieldRef<ModelSchema, i64> =
    FieldRef::new(ModelFilterField::MaxOutputTokens);
pub const MODEL_CREATED_AT: FieldRef<ModelSchema, DateTime<Utc>> =
    FieldRef::new(ModelFilterField::CreatedAt);
pub const MODEL_UPDATED_AT: FieldRef<ModelSchema, DateTime<Utc>> =
    FieldRef::new(ModelFilterField::UpdatedAt);

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical field list of DESIGN §3.3, pinned.
    const DESIGN_FIELDS: &[&str] = &[
        "id",
        "tenant_id",
        "provider_id",
        "name",
        "canonical_id",
        "provider_model_id",
        "gts_type",
        "sharing_mode",
        "approval_state",
        "approval_decided_at",
        "vision",
        "reasoning_effort",
        "tools",
        "json_schema",
        "streaming",
        "file_input",
        "image_generation",
        "audio_input",
        "audio_output",
        "code_interpreter",
        "web_search",
        "api_completion",
        "api_embedding",
        "api_batch",
        "display_name",
        "family",
        "vendor",
        "hosted_by",
        "region",
        "model_version",
        "managed",
        "architecture",
        "format",
        "sort_order",
        "last_release_at",
        "max_input_tokens",
        "max_output_tokens",
        "created_at",
        "updated_at",
    ];

    #[test]
    fn filter_fields_match_the_design_list() {
        let names: Vec<&str> = ModelFilterField::FIELDS
            .iter()
            .map(ModelFilterField::name)
            .collect();
        assert_eq!(names, DESIGN_FIELDS);
    }

    #[test]
    fn filter_only_fields_are_the_nullable_subset() {
        for field in MODEL_FILTER_ONLY_FIELDS {
            assert!(ModelFilterField::FIELDS.contains(field), "{field:?}");
            assert!(!field.orderable(), "{field:?}");
        }
        let orderable = ModelFilterField::FIELDS
            .iter()
            .filter(|f| f.orderable())
            .count();
        assert_eq!(
            orderable,
            ModelFilterField::FIELDS.len() - MODEL_FILTER_ONLY_FIELDS.len()
        );
        assert!(ModelFilterField::CanonicalId.orderable());
        assert!(ModelFilterField::Name.orderable());
        assert!(!ModelFilterField::ModelVersion.orderable());
        assert!(!ModelFilterField::MaxOutputTokens.orderable());
    }
}
