//! Whole-row projection between the SDK records and the table rows,
//! including the promoted filter columns, and the update application of
//! DESIGN §3.3 (a present field replaces the stored value as a whole).

use chrono::{DateTime, Utc};
use model_registry_sdk::{
    ApprovalState, ApprovalV1, ContextWindow, ModelInfoV1, ModelV1, ProviderV1, SharingMode,
    SupportedApi, UpdateModelRequestV1, UpdateProviderRequestV1,
};
use sea_orm::{NotSet, Set};
use uuid::Uuid;

use super::entity::{model, provider};
use crate::domain::error::DomainError;

fn drift(column: &str, value: &str) -> DomainError {
    DomainError::internal(format!("stored {column} `{value}` is outside its domain"))
}

fn json<T: serde::Serialize>(column: &str, value: &T) -> Result<serde_json::Value, DomainError> {
    serde_json::to_value(value)
        .map_err(|e| DomainError::internal(format!("cannot serialize {column}: {e}")))
}

fn from_json<T: serde::de::DeserializeOwned>(
    column: &str,
    value: serde_json::Value,
) -> Result<T, DomainError> {
    serde_json::from_value(value)
        .map_err(|e| DomainError::internal(format!("stored {column} does not deserialize: {e}")))
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

#[must_use]
pub fn provider_to_active(p: &ProviderV1) -> provider::ActiveModel {
    provider::ActiveModel {
        id: Set(p.id),
        tenant_id: Set(p.tenant_id),
        slug: Set(p.slug.clone()),
        name: Set(p.name.clone()),
        gts_type: Set(p.gts_type.to_string()),
        enabled: Set(p.enabled),
        sharing_mode: Set(p.sharing_mode.as_str().to_owned()),
        managed: Set(p.managed),
        metadata: Set(p.metadata.clone()),
        version: Set(p.version),
        created_at: Set(p.created_at),
        created_by: Set(p.created_by),
        updated_at: Set(p.updated_at),
        updated_by: Set(p.updated_by),
    }
}

/// The columns a versioned update rewrites: everything except the identity and
/// creation columns. `tenant_id` stays `NotSet`: the scope filter clamps it.
#[must_use]
pub fn provider_to_update_active(p: &ProviderV1) -> provider::ActiveModel {
    provider::ActiveModel {
        id: NotSet,
        tenant_id: NotSet,
        created_at: NotSet,
        created_by: NotSet,
        ..provider_to_active(p)
    }
}

pub fn provider_from_row(row: provider::Model) -> Result<ProviderV1, DomainError> {
    let sharing_mode = SharingMode::from_wire(&row.sharing_mode)
        .ok_or_else(|| drift("providers.sharing_mode", &row.sharing_mode))?;
    let mut p = ProviderV1::new(
        row.id,
        row.tenant_id,
        row.slug,
        row.name,
        gts::GtsTypeId::new(&row.gts_type),
        row.created_by,
        row.created_at,
    );
    p.enabled = row.enabled;
    p.sharing_mode = sharing_mode;
    p.managed = row.managed;
    p.metadata = row.metadata;
    p.version = row.version;
    p.updated_at = row.updated_at;
    p.updated_by = row.updated_by;
    Ok(p)
}

/// Applies the present fields of an update, increments `version`, and stamps
/// the audit columns. `expected_version` is checked by the service, never
/// applied. A present `gts_type` is applied as-is; the service decides whether
/// it is a type migration.
pub fn apply_provider_update(
    p: &mut ProviderV1,
    req: &UpdateProviderRequestV1,
    subject: Uuid,
    now: DateTime<Utc>,
) {
    if let Some(name) = &req.name {
        p.name.clone_from(name);
    }
    if let Some(gts_type) = &req.gts_type {
        p.gts_type.clone_from(gts_type);
    }
    if let Some(enabled) = req.enabled {
        p.enabled = enabled;
    }
    if let Some(mode) = req.sharing_mode {
        p.sharing_mode = mode;
    }
    if let Some(managed) = req.managed {
        p.managed = managed;
    }
    if let Some(metadata) = &req.metadata {
        p.metadata.clone_from(metadata);
    }
    p.version += 1;
    p.updated_at = now;
    p.updated_by = subject;
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// The promoted filter columns derived from the JSON sources.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promoted {
    pub api_completion: bool,
    pub api_embedding: bool,
    pub api_batch: bool,
    pub cap_vision: bool,
    pub cap_reasoning_effort: bool,
    pub cap_tools: bool,
    pub cap_json_schema: bool,
    pub cap_streaming: bool,
    pub cap_file_input: bool,
    pub cap_image_generation: bool,
    pub cap_audio_input: bool,
    pub cap_audio_output: bool,
    pub cap_code_interpreter: bool,
    pub cap_web_search: bool,
    pub ctx_max_input_tokens: i64,
    pub ctx_max_output_tokens: Option<i64>,
    pub ctx_output_vector_size: Option<i64>,
}

impl Promoted {
    /// Effective capability = declared and not disabled.
    #[must_use]
    pub fn from_info(info: &ModelInfoV1) -> Self {
        let c = &info.capabilities;
        let d = &info.disabled_capabilities;
        Self {
            api_completion: info.supported_api.contains(&SupportedApi::Completion),
            api_embedding: info.supported_api.contains(&SupportedApi::Embedding),
            api_batch: info.supported_api.contains(&SupportedApi::Batch),
            cap_vision: c.vision.enabled && !d.vision.disabled,
            cap_reasoning_effort: c.reasoning.effort && !d.reasoning.effort,
            cap_tools: c.tools.enabled && !d.tools.disabled,
            cap_json_schema: c.output.json_schema && !d.output.json_schema,
            cap_streaming: c.streaming && !d.streaming,
            cap_file_input: c.file_input.enabled && !d.file_input.disabled,
            cap_image_generation: c.image_generation.enabled && !d.image_generation.disabled,
            cap_audio_input: c.audio_input.enabled && !d.audio_input.disabled,
            cap_audio_output: c.audio_output.enabled && !d.audio_output.disabled,
            cap_code_interpreter: c.code_interpreter && !d.code_interpreter,
            cap_web_search: c.web_search.enabled && !d.web_search.disabled,
            ctx_max_input_tokens: i64::from(info.context_window.max_input_tokens),
            ctx_max_output_tokens: info.context_window.max_output_tokens.map(i64::from),
            ctx_output_vector_size: info.context_window.output_vector_size.map(i64::from),
        }
    }

    /// The promoted columns as stored on a row.
    #[must_use]
    pub fn from_row(row: &model::Model) -> Self {
        Self {
            api_completion: row.api_completion,
            api_embedding: row.api_embedding,
            api_batch: row.api_batch,
            cap_vision: row.cap_vision,
            cap_reasoning_effort: row.cap_reasoning_effort,
            cap_tools: row.cap_tools,
            cap_json_schema: row.cap_json_schema,
            cap_streaming: row.cap_streaming,
            cap_file_input: row.cap_file_input,
            cap_image_generation: row.cap_image_generation,
            cap_audio_input: row.cap_audio_input,
            cap_audio_output: row.cap_audio_output,
            cap_code_interpreter: row.cap_code_interpreter,
            cap_web_search: row.cap_web_search,
            ctx_max_input_tokens: row.ctx_max_input_tokens,
            ctx_max_output_tokens: row.ctx_max_output_tokens,
            ctx_output_vector_size: row.ctx_output_vector_size,
        }
    }
}

/// Every column is `Set`, so a save rewrites the whole row and the promoted
/// columns never drift from their JSON sources.
pub fn model_to_active(m: &ModelV1) -> Result<model::ActiveModel, DomainError> {
    let i = &m.info;
    let p = Promoted::from_info(i);
    let size_bytes = i
        .size_bytes
        .map(|s| i64::try_from(s).map_err(|_| DomainError::internal("size_bytes exceeds i64")))
        .transpose()?;
    let mut sorted_api: Vec<SupportedApi> = i.supported_api.iter().copied().collect();
    sorted_api.sort_by_key(|a| a.as_str());
    Ok(model::ActiveModel {
        id: Set(m.id),
        tenant_id: Set(m.tenant_id),
        provider_id: Set(m.provider_id),
        name: Set(m.name.clone()),
        provider_model_id: Set(i.provider_model_id.clone()),
        canonical_id: Set(m.canonical_id.clone()),
        gts_type: Set(i.gts_type.to_string()),
        sharing_mode: Set(m.sharing_mode.as_str().to_owned()),
        approval_state: Set(m.approval.state.as_str().to_owned()),
        approval_decided_by: Set(m.approval.decided_by),
        approval_decided_at: Set(m.approval.decided_at),
        base_url: Set(i.base_url.clone()),
        secret_ref: Set(i.secret_ref.clone()),
        display_name: Set(i.display_name.clone()),
        description: Set(i.description.clone()),
        family: Set(i.family.clone()),
        vendor: Set(i.vendor.clone()),
        managed: Set(i.managed),
        architecture: Set(i.architecture.clone()),
        size_bytes: Set(size_bytes),
        format: Set(i.format.clone()),
        region: Set(i.region.clone()),
        hosted_by: Set(i.hosted_by.clone()),
        last_release_at: Set(i.last_release_at),
        reasoning_level: Set(i.reasoning_level.clone()),
        model_version: Set(i.model_version.clone()),
        sort_order: Set(i.sort_order),
        icon: Set(i.icon.clone()),
        multiplier_display: Set(i.multiplier_display.clone()),
        cost: Set(json("cost", &i.cost)?),
        performance: Set(json("performance", &i.performance)?),
        additional_info: Set(json("additional_info", &i.additional_info)?),
        supported_api: Set(json("supported_api", &sorted_api)?),
        api_completion: Set(p.api_completion),
        api_embedding: Set(p.api_embedding),
        api_batch: Set(p.api_batch),
        capabilities: Set(json("capabilities", &i.capabilities)?),
        disabled_capabilities: Set(json("disabled_capabilities", &i.disabled_capabilities)?),
        cap_vision: Set(p.cap_vision),
        cap_reasoning_effort: Set(p.cap_reasoning_effort),
        cap_tools: Set(p.cap_tools),
        cap_json_schema: Set(p.cap_json_schema),
        cap_streaming: Set(p.cap_streaming),
        cap_file_input: Set(p.cap_file_input),
        cap_image_generation: Set(p.cap_image_generation),
        cap_audio_input: Set(p.cap_audio_input),
        cap_audio_output: Set(p.cap_audio_output),
        cap_code_interpreter: Set(p.cap_code_interpreter),
        cap_web_search: Set(p.cap_web_search),
        ctx_max_input_tokens: Set(p.ctx_max_input_tokens),
        ctx_max_output_tokens: Set(p.ctx_max_output_tokens),
        ctx_output_vector_size: Set(p.ctx_output_vector_size),
        default_parameters: Set(json("default_parameters", &i.default_parameters)?),
        allow_parameter_override: Set(i.allow_parameter_override),
        allow_extra_params: Set(json("allow_extra_params", &i.allow_extra_params)?),
        provider_settings: Set(i.provider_settings.clone()),
        version: Set(m.version),
        created_at: Set(m.created_at),
        created_by: Set(m.created_by),
        updated_at: Set(m.updated_at),
        updated_by: Set(m.updated_by),
    })
}

/// The columns a versioned update rewrites: everything except the identity and
/// creation columns. `tenant_id` stays `NotSet`: the scope filter clamps it.
pub fn model_to_update_active(m: &ModelV1) -> Result<model::ActiveModel, DomainError> {
    Ok(model::ActiveModel {
        id: NotSet,
        tenant_id: NotSet,
        created_at: NotSet,
        created_by: NotSet,
        ..model_to_active(m)?
    })
}

/// The JSON columns are the source of truth on read.
pub fn model_from_row(row: model::Model) -> Result<ModelV1, DomainError> {
    let sharing_mode = SharingMode::from_wire(&row.sharing_mode)
        .ok_or_else(|| drift("models.sharing_mode", &row.sharing_mode))?;
    let state = ApprovalState::from_wire(&row.approval_state)
        .ok_or_else(|| drift("models.approval_state", &row.approval_state))?;
    let size_bytes = row
        .size_bytes
        .map(|s| u64::try_from(s).map_err(|_| drift("models.size_bytes", &s.to_string())))
        .transpose()?;
    let max_input_tokens = u32::try_from(row.ctx_max_input_tokens).map_err(|_| {
        drift(
            "models.ctx_max_input_tokens",
            &row.ctx_max_input_tokens.to_string(),
        )
    })?;
    let to_u32 = |column: &str, v: Option<i64>| {
        v.map(|n| u32::try_from(n).map_err(|_| drift(column, &n.to_string())))
            .transpose()
    };
    let mut context_window = ContextWindow::new(max_input_tokens);
    context_window.max_output_tokens =
        to_u32("models.ctx_max_output_tokens", row.ctx_max_output_tokens)?;
    context_window.output_vector_size =
        to_u32("models.ctx_output_vector_size", row.ctx_output_vector_size)?;
    let mut info = ModelInfoV1::new(
        gts::GtsTypeId::new(&row.gts_type),
        row.provider_model_id,
        row.display_name,
        context_window,
    );
    info.base_url = row.base_url;
    info.secret_ref = row.secret_ref;
    info.description = row.description;
    info.family = row.family;
    info.vendor = row.vendor;
    info.managed = row.managed;
    info.architecture = row.architecture;
    info.size_bytes = size_bytes;
    info.format = row.format;
    info.region = row.region;
    info.hosted_by = row.hosted_by;
    info.last_release_at = row.last_release_at;
    info.reasoning_level = row.reasoning_level;
    info.model_version = row.model_version;
    info.sort_order = row.sort_order;
    info.icon = row.icon;
    info.multiplier_display = row.multiplier_display;
    info.cost = from_json("models.cost", row.cost)?;
    info.performance = from_json("models.performance", row.performance)?;
    info.additional_info = from_json("models.additional_info", row.additional_info)?;
    info.supported_api = from_json("models.supported_api", row.supported_api)?;
    info.capabilities = from_json("models.capabilities", row.capabilities)?;
    info.disabled_capabilities =
        from_json("models.disabled_capabilities", row.disabled_capabilities)?;
    info.default_parameters = from_json("models.default_parameters", row.default_parameters)?;
    info.allow_parameter_override = row.allow_parameter_override;
    info.allow_extra_params = from_json("models.allow_extra_params", row.allow_extra_params)?;
    info.provider_settings = row.provider_settings;
    let mut m = ModelV1::new(
        row.id,
        row.tenant_id,
        row.provider_id,
        row.name,
        row.canonical_id,
        info,
        row.created_by,
        row.created_at,
    );
    m.sharing_mode = sharing_mode;
    m.approval = ApprovalV1::new(state, row.approval_decided_by, row.approval_decided_at);
    m.version = row.version;
    m.updated_at = row.updated_at;
    m.updated_by = row.updated_by;
    Ok(m)
}

/// Applies the present fields of an update (whole-value replacement), records
/// an approval decision when `approval_state` is present, increments
/// `version`, and stamps the audit columns. `expected_version` is checked by
/// the service, never applied.
// The field list is the whole point of this function.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
pub fn apply_model_update(
    m: &mut ModelV1,
    req: &UpdateModelRequestV1,
    subject: Uuid,
    now: DateTime<Utc>,
) {
    if let Some(mode) = req.sharing_mode {
        m.sharing_mode = mode;
    }
    if let Some(state) = req.approval_state {
        m.approval = ApprovalV1::new(state, subject, now);
    }
    let i = &mut m.info;
    macro_rules! replace {
        ($($field:ident),* $(,)?) => {
            $( if let Some(v) = &req.$field { i.$field.clone_from(v); } )*
        };
    }
    replace!(
        provider_model_id,
        base_url,
        secret_ref,
        display_name,
        description,
        family,
        vendor,
        managed,
        architecture,
        size_bytes,
        format,
        region,
        hosted_by,
        last_release_at,
        reasoning_level,
        model_version,
        sort_order,
        icon,
        multiplier_display,
        cost,
        performance,
        additional_info,
        supported_api,
        capabilities,
        disabled_capabilities,
        context_window,
        default_parameters,
        allow_parameter_override,
        allow_extra_params,
        provider_settings,
    );
    m.version += 1;
    m.updated_at = now;
    m.updated_by = subject;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{model_update, provider_update};
    use model_registry_sdk::{ContextWindow, MediaCapability, ModelCapabilities};
    use sea_orm::{ActiveValue, Iterable, TryIntoModel};
    use std::collections::{HashMap, HashSet};
    use toolkit_gts::gts_id;

    fn all_true_caps() -> ModelCapabilities {
        let mut media = MediaCapability::default();
        media.enabled = true;
        media.supported_mime_types = vec!["image/png".into()];
        let mut caps = ModelCapabilities::default();
        caps.vision = media.clone();
        let reasoning = &mut caps.reasoning;
        reasoning.effort = true;
        reasoning.toggle = true;
        reasoning.resume = true;
        reasoning.budget = true;
        reasoning.summary = true;
        let tools = &mut caps.tools;
        tools.enabled = true;
        tools.parallel_tool_calls = true;
        tools.max_tool_calls = true;
        tools.tool_choice_required = true;
        tools.tool_choice_none = true;
        tools.tool_choice_named = true;
        let output = &mut caps.output;
        output.json_mode = true;
        output.json_schema = true;
        output.strict_schema = true;
        output.verbosity = true;
        let sampling = &mut caps.sampling;
        sampling.temperature = true;
        sampling.top_p = true;
        sampling.seed = true;
        sampling.presence_penalty = true;
        sampling.frequency_penalty = true;
        sampling.logprobs = true;
        caps.streaming = true;
        caps.file_input = media.clone();
        caps.image_generation = media.clone();
        caps.audio_input = media.clone();
        caps.audio_output = media;
        caps.code_interpreter = true;
        let web_search = &mut caps.web_search;
        web_search.enabled = true;
        web_search.allowed_domains = true;
        web_search.excluded_domains = true;
        caps.truncation = true;
        caps.instructions = true;
        caps
    }

    fn sample() -> ModelV1 {
        let now = Utc::now();
        let mut context_window = ContextWindow::new(128_000);
        context_window.max_output_tokens = Some(16_384);
        let mut i = ModelInfoV1::new(
            gts::GtsTypeId::new(gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~")),
            "gpt-4o",
            "GPT-4o",
            context_window,
        );
        i.base_url = Some("https://api.openai.com/v1".into());
        i.secret_ref = Some("key".into());
        i.description = Some("d".into());
        i.family = Some("gpt-4".into());
        i.vendor = Some("OpenAI".into());
        i.managed = true;
        i.architecture = Some("transformer".into());
        i.size_bytes = Some(1 << 40);
        i.format = Some("api".into());
        i.region = Some("eu".into());
        i.hosted_by = Some("Azure".into());
        i.last_release_at = Some(now);
        i.reasoning_level = Some("high".into());
        i.model_version = Some("2026-01".into());
        i.sort_order = 3;
        i.icon = Some("https://x/i.png".into());
        i.multiplier_display = Some("3x".into());
        i.cost.input_per_1m_micro = Some(2_500_000);
        i.performance.response_latency_ms = Some(200);
        i.performance.tokens_per_second = Some(90);
        i.additional_info = HashMap::from([("k".to_owned(), serde_json::json!(1))]);
        i.supported_api = HashSet::from([SupportedApi::Completion, SupportedApi::Batch]);
        i.capabilities = all_true_caps();
        i.disabled_capabilities.vision.disabled = true;
        i.disabled_capabilities.streaming = true;
        i.default_parameters.temperature = Some(0.5);
        i.allow_parameter_override = true;
        i.allow_extra_params = vec!["seed".into()];
        i.provider_settings = serde_json::json!({ "oagw_alias": "openai" });
        let mut m = ModelV1::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "gpt-4o",
            "openai::gpt-4o",
            i,
            Uuid::new_v4(),
            now,
        );
        m.sharing_mode = SharingMode::Shared;
        m.approval = ApprovalV1::new(ApprovalState::Approved, Uuid::new_v4(), now);
        m.updated_by = Uuid::new_v4();
        m
    }

    fn sample_provider() -> ProviderV1 {
        ProviderV1::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "openai",
            "OpenAI",
            gts::GtsTypeId::new(gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~")),
            Uuid::new_v4(),
            Utc::now(),
        )
    }

    fn row_of(m: &ModelV1) -> model::Model {
        model_to_active(m).unwrap().try_into_model().unwrap()
    }

    #[test]
    fn model_round_trips_through_the_row() {
        let m = sample();
        let back = model_from_row(row_of(&m)).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn every_column_is_set_on_write() {
        let am = model_to_active(&sample()).unwrap();
        for col in model::Column::iter() {
            assert!(
                matches!(
                    sea_orm::ActiveModelTrait::get(&am, col),
                    ActiveValue::Set(_)
                ),
                "{col:?} is not Set"
            );
        }
    }

    #[test]
    fn promoted_columns_are_effective_values() {
        let m = sample();
        let row = row_of(&m);
        let p = Promoted::from_row(&row);
        assert_eq!(p, Promoted::from_info(&m.info));
        assert!(!p.cap_vision, "declared but disabled");
        assert!(!p.cap_streaming, "declared but disabled");
        assert!(p.cap_reasoning_effort && p.cap_tools && p.cap_json_schema);
        assert!(
            p.cap_file_input && p.cap_image_generation && p.cap_audio_input && p.cap_audio_output
        );
        assert!(p.cap_code_interpreter && p.cap_web_search);
        assert!(p.api_completion && p.api_batch && !p.api_embedding);
        assert_eq!(p.ctx_max_input_tokens, 128_000);
        assert_eq!(p.ctx_max_output_tokens, Some(16_384));
        assert_eq!(p.ctx_output_vector_size, None);
    }

    #[test]
    fn update_replaces_whole_values_and_records_decisions() {
        let mut m = sample();
        let subject = Uuid::new_v4();
        let now = Utc::now() + chrono::Duration::hours(1);
        let req = model_update(|r| {
            r.approval_state = Some(ApprovalState::Revoked);
            r.base_url = Some(None);
            r.capabilities = Some(ModelCapabilities::default());
            r.supported_api = Some(HashSet::from([SupportedApi::Embedding]));
            let mut context_window = ContextWindow::new(8);
            context_window.output_vector_size = Some(1536);
            r.context_window = Some(context_window);
        });
        apply_model_update(&mut m, &req, subject, now);
        assert_eq!(m.approval.state, ApprovalState::Revoked);
        assert_eq!(m.approval.decided_by, subject);
        assert_eq!(m.approval.decided_at, now);
        assert_eq!(m.info.base_url, None);
        assert_eq!(
            m.info.secret_ref.as_deref(),
            Some("key"),
            "absent fields stay"
        );
        assert_eq!(m.updated_by, subject);
        assert_eq!(m.version, 2, "every update increments the row version");
        let p = Promoted::from_row(&row_of(&m));
        assert!(!p.cap_tools && !p.cap_web_search);
        assert!(p.api_embedding && !p.api_completion && !p.api_batch);
        assert_eq!(p.ctx_max_input_tokens, 8);
        assert_eq!(p.ctx_output_vector_size, Some(1536));

        let before = m.approval;
        apply_model_update(
            &mut m,
            &model_update(|r| r.display_name = Some("x".into())),
            Uuid::new_v4(),
            now,
        );
        assert_eq!(m.approval, before, "no decision without approval_state");
    }

    #[test]
    fn provider_round_trip_and_update() {
        let now = Utc::now();
        let mut p = sample_provider();
        p.metadata = Some(serde_json::json!({ "a": 1 }));
        p.updated_by = Uuid::new_v4();
        let row = provider_to_active(&p).try_into_model().unwrap();
        assert_eq!(provider_from_row(row).unwrap(), p);

        let subject = Uuid::new_v4();
        apply_provider_update(
            &mut p,
            &provider_update(|r| {
                r.enabled = Some(false);
                r.sharing_mode = Some(SharingMode::Shared);
                r.metadata = Some(None);
            }),
            subject,
            now,
        );
        assert!(!p.enabled);
        assert_eq!(p.sharing_mode, SharingMode::Shared);
        assert_eq!(p.metadata, None);
        assert_eq!(p.name, "OpenAI");
        assert_eq!(p.updated_by, subject);
        assert_eq!(p.version, 2);

        apply_provider_update(
            &mut p,
            &provider_update(|r| {
                r.gts_type = Some(gts::GtsTypeId::new(gts_id!(
                    "cf.genai.model.provider.v1~cf.genai._.azure.v1~"
                )));
            }),
            subject,
            now,
        );
        assert_eq!(
            p.gts_type.as_ref(),
            gts_id!("cf.genai.model.provider.v1~cf.genai._.azure.v1~")
        );
        assert_eq!(p.version, 3);
    }

    /// The update rows leave the identity and creation columns alone;
    /// `tenant_id` in particular stays `NotSet` so the scope filter is the
    /// only tenant clamp.
    #[test]
    fn update_rows_leave_identity_and_creation_columns_unset() {
        let m = sample();
        let am = model_to_update_active(&m).unwrap();
        for col in [
            model::Column::Id,
            model::Column::TenantId,
            model::Column::CreatedAt,
            model::Column::CreatedBy,
        ] {
            assert!(
                matches!(
                    sea_orm::ActiveModelTrait::get(&am, col),
                    ActiveValue::NotSet
                ),
                "{col:?} must be NotSet"
            );
        }
        assert!(matches!(
            sea_orm::ActiveModelTrait::get(&am, model::Column::Version),
            ActiveValue::Set(_)
        ));
        let p = sample_provider();
        let am = provider_to_update_active(&p);
        assert!(matches!(
            sea_orm::ActiveModelTrait::get(&am, provider::Column::TenantId),
            ActiveValue::NotSet
        ));
        assert!(matches!(
            sea_orm::ActiveModelTrait::get(&am, provider::Column::Name),
            ActiveValue::Set(_)
        ));
    }

    #[test]
    fn drifted_enum_values_fail_closed() {
        let mut row = row_of(&sample());
        row.approval_state = "maybe".into();
        assert!(matches!(
            model_from_row(row).unwrap_err(),
            DomainError::Internal { .. }
        ));
    }
}
