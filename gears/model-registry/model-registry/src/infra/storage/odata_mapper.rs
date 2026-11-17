//! Binding of the SDK filter fields to real columns (DESIGN §3.3 field
//! table) and the keyset cursor extraction.

use model_registry_sdk::odata::{ModelFilterField, ProviderFilterField};
use sea_orm::Value;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, SortDir};

use super::entity::{model, provider};

/// The query with the default order of DESIGN §3.3 injected when the caller
/// gave none and no cursor fixes the order. The `id` tie-breaker is appended
/// by the paginator.
pub fn with_default_order(query: &ODataQuery, field: &str) -> ODataQuery {
    let mut q = query.clone();
    if q.cursor.is_none() && q.order.is_empty() {
        q.order = ODataOrderBy(vec![OrderKey {
            field: field.to_owned(),
            dir: SortDir::Asc,
        }]);
    }
    q
}

pub struct ModelODataMapper;

impl FieldToColumn<ModelFilterField> for ModelODataMapper {
    type Column = model::Column;

    fn map_field(field: ModelFilterField) -> model::Column {
        use ModelFilterField as F;
        use model::Column as C;
        match field {
            F::Id => C::Id,
            F::TenantId => C::TenantId,
            F::ProviderId => C::ProviderId,
            F::Name => C::Name,
            F::CanonicalId => C::CanonicalId,
            F::ProviderModelId => C::ProviderModelId,
            F::GtsType => C::GtsType,
            F::SharingMode => C::SharingMode,
            F::ApprovalState => C::ApprovalState,
            F::ApprovalDecidedAt => C::ApprovalDecidedAt,
            F::Vision => C::CapVision,
            F::ReasoningEffort => C::CapReasoningEffort,
            F::Tools => C::CapTools,
            F::JsonSchema => C::CapJsonSchema,
            F::Streaming => C::CapStreaming,
            F::FileInput => C::CapFileInput,
            F::ImageGeneration => C::CapImageGeneration,
            F::AudioInput => C::CapAudioInput,
            F::AudioOutput => C::CapAudioOutput,
            F::CodeInterpreter => C::CapCodeInterpreter,
            F::WebSearch => C::CapWebSearch,
            F::ApiCompletion => C::ApiCompletion,
            F::ApiEmbedding => C::ApiEmbedding,
            F::ApiBatch => C::ApiBatch,
            F::DisplayName => C::DisplayName,
            F::Family => C::Family,
            F::Vendor => C::Vendor,
            F::HostedBy => C::HostedBy,
            F::Region => C::Region,
            F::ModelVersion => C::ModelVersion,
            F::Managed => C::Managed,
            F::Architecture => C::Architecture,
            F::Format => C::Format,
            F::SortOrder => C::SortOrder,
            F::LastReleaseAt => C::LastReleaseAt,
            F::MaxInputTokens => C::CtxMaxInputTokens,
            F::MaxOutputTokens => C::CtxMaxOutputTokens,
            F::CreatedAt => C::CreatedAt,
            F::UpdatedAt => C::UpdatedAt,
        }
    }

    fn is_orderable(field: ModelFilterField) -> bool {
        field.orderable()
    }
}

impl ODataFieldMapping<ModelFilterField> for ModelODataMapper {
    type Entity = model::Entity;

    fn extract_cursor_value(m: &model::Model, field: ModelFilterField) -> Value {
        use ModelFilterField as F;
        match field {
            F::Id => m.id.into(),
            F::TenantId => m.tenant_id.into(),
            F::ProviderId => m.provider_id.into(),
            F::Name => m.name.clone().into(),
            F::CanonicalId => m.canonical_id.clone().into(),
            F::ProviderModelId => m.provider_model_id.clone().into(),
            F::GtsType => m.gts_type.clone().into(),
            F::SharingMode => m.sharing_mode.clone().into(),
            F::ApprovalState => m.approval_state.clone().into(),
            F::ApprovalDecidedAt => m.approval_decided_at.into(),
            F::Vision => m.cap_vision.into(),
            F::ReasoningEffort => m.cap_reasoning_effort.into(),
            F::Tools => m.cap_tools.into(),
            F::JsonSchema => m.cap_json_schema.into(),
            F::Streaming => m.cap_streaming.into(),
            F::FileInput => m.cap_file_input.into(),
            F::ImageGeneration => m.cap_image_generation.into(),
            F::AudioInput => m.cap_audio_input.into(),
            F::AudioOutput => m.cap_audio_output.into(),
            F::CodeInterpreter => m.cap_code_interpreter.into(),
            F::WebSearch => m.cap_web_search.into(),
            F::ApiCompletion => m.api_completion.into(),
            F::ApiEmbedding => m.api_embedding.into(),
            F::ApiBatch => m.api_batch.into(),
            F::DisplayName => m.display_name.clone().into(),
            F::Family => m.family.clone().into(),
            F::Vendor => m.vendor.clone().into(),
            F::HostedBy => m.hosted_by.clone().into(),
            F::Region => m.region.clone().into(),
            F::ModelVersion => m.model_version.clone().into(),
            F::Managed => m.managed.into(),
            F::Architecture => m.architecture.clone().into(),
            F::Format => m.format.clone().into(),
            F::SortOrder => i64::from(m.sort_order).into(),
            F::LastReleaseAt => m.last_release_at.into(),
            F::MaxInputTokens => m.ctx_max_input_tokens.into(),
            F::MaxOutputTokens => m.ctx_max_output_tokens.into(),
            F::CreatedAt => m.created_at.into(),
            F::UpdatedAt => m.updated_at.into(),
        }
    }
}

pub struct ProviderODataMapper;

impl FieldToColumn<ProviderFilterField> for ProviderODataMapper {
    type Column = provider::Column;

    fn map_field(field: ProviderFilterField) -> provider::Column {
        use ProviderFilterField as F;
        use provider::Column as C;
        match field {
            F::Id => C::Id,
            F::TenantId => C::TenantId,
            F::Slug => C::Slug,
            F::Name => C::Name,
            F::GtsType => C::GtsType,
            F::Enabled => C::Enabled,
            F::SharingMode => C::SharingMode,
            F::Managed => C::Managed,
            F::CreatedAt => C::CreatedAt,
            F::UpdatedAt => C::UpdatedAt,
        }
    }
}

impl ODataFieldMapping<ProviderFilterField> for ProviderODataMapper {
    type Entity = provider::Entity;

    fn extract_cursor_value(p: &provider::Model, field: ProviderFilterField) -> Value {
        use ProviderFilterField as F;
        match field {
            F::Id => p.id.into(),
            F::TenantId => p.tenant_id.into(),
            F::Slug => p.slug.clone().into(),
            F::Name => p.name.clone().into(),
            F::GtsType => p.gts_type.clone().into(),
            F::Enabled => p.enabled.into(),
            F::SharingMode => p.sharing_mode.clone().into(),
            F::Managed => p.managed.into(),
            F::CreatedAt => p.created_at.into(),
            F::UpdatedAt => p.updated_at.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Iden;
    use toolkit_odata::filter::FilterField;

    /// DESIGN §3.3: `cap_<field>` for capabilities, `ctx_<field>` for the
    /// context window, the same name otherwise.
    #[test]
    fn model_columns_follow_the_design_naming() {
        for field in ModelFilterField::FIELDS {
            let column = ModelODataMapper::map_field(*field).to_string();
            let name = field.name();
            let expected = match name {
                "vision" | "reasoning_effort" | "tools" | "json_schema" | "streaming"
                | "file_input" | "image_generation" | "audio_input" | "audio_output"
                | "code_interpreter" | "web_search" => format!("cap_{name}"),
                "max_input_tokens" | "max_output_tokens" => format!("ctx_{name}"),
                other => other.to_owned(),
            };
            assert_eq!(column, expected, "{name}");
        }
        for field in ProviderFilterField::FIELDS {
            assert_eq!(
                ProviderODataMapper::map_field(*field).to_string(),
                field.name()
            );
        }
    }

    #[test]
    fn nullable_model_fields_are_not_orderable() {
        for field in model_registry_sdk::odata::MODEL_FILTER_ONLY_FIELDS {
            assert!(!ModelODataMapper::is_orderable(*field), "{field:?}");
        }
        assert!(ModelODataMapper::is_orderable(
            ModelFilterField::CanonicalId
        ));
        assert!(ProviderODataMapper::is_orderable(ProviderFilterField::Slug));
    }

    #[test]
    fn default_order_is_injected_only_without_order_and_cursor() {
        let q = with_default_order(&ODataQuery::default(), "canonical_id");
        assert_eq!(q.order.0.len(), 1);
        assert_eq!(q.order.0[0].field, "canonical_id");

        let ordered = ODataQuery::default().with_order(ODataOrderBy(vec![OrderKey {
            field: "vendor".into(),
            dir: SortDir::Desc,
        }]));
        assert_eq!(
            with_default_order(&ordered, "canonical_id").order.0[0].field,
            "vendor"
        );

        let with_cursor = ODataQuery::default().with_cursor(toolkit_odata::CursorV1 {
            k: vec![],
            o: SortDir::Asc,
            s: String::new(),
            f: None,
            d: "fwd".into(),
        });
        assert!(
            with_default_order(&with_cursor, "canonical_id")
                .order
                .is_empty()
        );
    }
}
