//! REST DTOs (DESIGN §3.3 REST Endpoints). Entry DTOs flatten the SDK
//! entries: the provider or model fields plus the flags at the top level.
//! The nested Model Info objects travel as JSON and are parsed into the SDK
//! types by `parse`.

use chrono::{DateTime, Utc};
use model_registry_sdk::{
    ApprovalV1, ModelEntryV1, ModelInfoV1, ModelV1, ProviderEntryV1, ProviderV1,
};
use serde_json::Value as JsonValue;
use toolkit_macros::api_dto;
use uuid::Uuid;

/// Deserializes `Option<Option<T>>` so that an absent key is `None`, an
/// explicit `null` is `Some(None)`, and a value is `Some(Some(v))`.
#[allow(clippy::option_option)]
pub(crate) mod nullable_opt {
    use serde::Deserialize;

    pub fn deserialize<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

/// `ProviderEntryV1`, flattened.
#[allow(clippy::struct_excessive_bools)]
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ProviderEntryDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub slug: String,
    pub name: String,
    pub gts_type: String,
    pub enabled: bool,
    pub sharing_mode: String,
    pub managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
    /// Row version; the `ETag` of single-entry responses.
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Uuid,
    pub inherited: bool,
    pub shadowed: bool,
    pub available_for_eval: bool,
}

impl From<ProviderEntryV1> for ProviderEntryDto {
    fn from(e: ProviderEntryV1) -> Self {
        let ProviderV1 {
            id,
            tenant_id,
            slug,
            name,
            gts_type,
            enabled,
            sharing_mode,
            managed,
            metadata,
            version,
            created_at,
            created_by,
            updated_at,
            updated_by,
            ..
        } = e.provider;
        Self {
            id,
            tenant_id,
            slug,
            name,
            gts_type: gts_type.to_string(),
            enabled,
            sharing_mode: sharing_mode.as_str().to_owned(),
            managed,
            metadata,
            version,
            created_at,
            created_by,
            updated_at,
            updated_by,
            inherited: e.inherited,
            shadowed: e.shadowed,
            available_for_eval: e.available_for_eval,
        }
    }
}

#[api_dto(request)]
#[serde(deny_unknown_fields)]
#[derive(Debug, Clone)]
pub struct CreateProviderRequestDto {
    pub slug: String,
    pub name: String,
    pub gts_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
}

/// `UpdateProviderRequestV1` on the wire. The precondition comes from the
/// `If-Match` header, not the body.
#[api_dto(request)]
#[serde(deny_unknown_fields)]
#[derive(Debug, Clone, Default)]
#[allow(clippy::option_option)]
pub struct UpdateProviderRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A value different from the stored one is a type migration of the
    /// provider and all its models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gts_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    /// `null` clears the metadata.
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<Option<JsonValue>>,
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ApprovalDto {
    pub state: String,
    pub decided_by: Uuid,
    pub decided_at: DateTime<Utc>,
}

impl From<ApprovalV1> for ApprovalDto {
    fn from(a: ApprovalV1) -> Self {
        Self {
            state: a.state.as_str().to_owned(),
            decided_by: a.decided_by,
            decided_at: a.decided_at,
        }
    }
}

/// `ModelInfoV1` on the wire. Nested objects keep the SDK JSON shape.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ModelInfoDto {
    pub gts_type: String,
    pub provider_model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    pub managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosted_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_release_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_level: Option<String>,
    /// The vendor's release version of the model; not the row `version`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    pub sort_order: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier_display: Option<String>,
    /// Cost in micro-credits; absent rates are omitted.
    pub cost: JsonValue,
    pub performance: JsonValue,
    pub additional_info: JsonValue,
    pub supported_api: Vec<String>,
    pub capabilities: JsonValue,
    pub disabled_capabilities: JsonValue,
    pub context_window: JsonValue,
    pub default_parameters: JsonValue,
    pub allow_parameter_override: bool,
    pub allow_extra_params: Vec<String>,
    pub provider_settings: JsonValue,
}

fn to_json<T: serde::Serialize>(value: &T) -> JsonValue {
    serde_json::to_value(value).unwrap_or(JsonValue::Null)
}

impl From<ModelInfoV1> for ModelInfoDto {
    fn from(i: ModelInfoV1) -> Self {
        let mut supported_api: Vec<String> = i
            .supported_api
            .iter()
            .map(|a| a.as_str().to_owned())
            .collect();
        supported_api.sort();
        Self {
            gts_type: i.gts_type.to_string(),
            provider_model_id: i.provider_model_id,
            base_url: i.base_url,
            secret_ref: i.secret_ref,
            display_name: i.display_name,
            description: i.description,
            family: i.family,
            vendor: i.vendor,
            managed: i.managed,
            architecture: i.architecture,
            size_bytes: i.size_bytes,
            format: i.format,
            region: i.region,
            hosted_by: i.hosted_by,
            last_release_at: i.last_release_at,
            reasoning_level: i.reasoning_level,
            model_version: i.model_version,
            sort_order: i.sort_order,
            icon: i.icon,
            multiplier_display: i.multiplier_display,
            cost: to_json(&i.cost),
            performance: to_json(&i.performance),
            additional_info: to_json(&i.additional_info),
            supported_api,
            capabilities: to_json(&i.capabilities),
            disabled_capabilities: to_json(&i.disabled_capabilities),
            context_window: to_json(&i.context_window),
            default_parameters: to_json(&i.default_parameters),
            allow_parameter_override: i.allow_parameter_override,
            allow_extra_params: i.allow_extra_params,
            provider_settings: i.provider_settings,
        }
    }
}

/// `ModelV1` on the wire: the full record the resolution routes return.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ModelDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub provider_id: Uuid,
    pub name: String,
    pub canonical_id: String,
    pub sharing_mode: String,
    pub approval: ApprovalDto,
    pub info: ModelInfoDto,
    /// Row version; the `ETag` of single-entry responses.
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Uuid,
}

impl From<ModelV1> for ModelDto {
    fn from(m: ModelV1) -> Self {
        Self {
            id: m.id,
            tenant_id: m.tenant_id,
            provider_id: m.provider_id,
            name: m.name,
            canonical_id: m.canonical_id,
            sharing_mode: m.sharing_mode.as_str().to_owned(),
            approval: m.approval.into(),
            info: m.info.into(),
            version: m.version,
            created_at: m.created_at,
            created_by: m.created_by,
            updated_at: m.updated_at,
            updated_by: m.updated_by,
        }
    }
}

/// `ModelEntryV1`, flattened: the `ModelDto` fields plus the flags.
#[allow(clippy::struct_excessive_bools)]
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ModelEntryDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub provider_id: Uuid,
    pub name: String,
    pub canonical_id: String,
    pub sharing_mode: String,
    pub approval: ApprovalDto,
    pub info: ModelInfoDto,
    /// Row version; the `ETag` of single-entry responses.
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Uuid,
    pub inherited: bool,
    pub shadowed: bool,
    pub provider_enabled: bool,
    pub available_for_eval: bool,
}

impl From<ModelEntryV1> for ModelEntryDto {
    fn from(e: ModelEntryV1) -> Self {
        let m = ModelDto::from(e.model);
        Self {
            id: m.id,
            tenant_id: m.tenant_id,
            provider_id: m.provider_id,
            name: m.name,
            canonical_id: m.canonical_id,
            sharing_mode: m.sharing_mode,
            approval: m.approval,
            info: m.info,
            version: m.version,
            created_at: m.created_at,
            created_by: m.created_by,
            updated_at: m.updated_at,
            updated_by: m.updated_by,
            inherited: e.inherited,
            shadowed: e.shadowed,
            provider_enabled: e.provider_enabled,
            available_for_eval: e.available_for_eval,
        }
    }
}

/// `CreateModelRequestV1` on the wire: every Model Info field except
/// `gts_type`, plus `provider_id`, `name`, `sharing_mode`, and
/// `approval_state`.
#[api_dto(request)]
#[serde(deny_unknown_fields)]
#[derive(Debug, Clone)]
pub struct CreateModelRequestDto {
    pub provider_id: Uuid,
    /// Right part of the canonical id; immutable after create.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_state: Option<String>,
    pub provider_model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_release_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiplier_display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_api: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_capabilities: Option<JsonValue>,
    pub context_window: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_parameters: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_parameter_override: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_extra_params: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_settings: Option<JsonValue>,
}

/// `UpdateModelRequestV1` on the wire. Nullable fields accept `null` to
/// clear. The precondition comes from the `If-Match` header, not the body.
#[api_dto(request)]
#[serde(deny_unknown_fields)]
#[derive(Debug, Clone, Default)]
#[allow(clippy::option_option)]
pub struct UpdateModelRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_model_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub base_url: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub secret_ref: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub family: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub vendor: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub architecture: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub size_bytes: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub format: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub region: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub hosted_by: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_release_at: Option<Option<DateTime<Utc>>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_level: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub model_version: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i32>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub icon: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "nullable_opt::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub multiplier_display: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_api: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_capabilities: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_parameters: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_parameter_override: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_extra_params: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_settings: Option<JsonValue>,
}

#[cfg(test)]
mod tests {
    use model_registry_sdk::{ApprovalState, ContextWindow, SharingMode};

    use super::*;
    use toolkit_gts::gts_id;

    fn sample_model() -> ModelV1 {
        let now = DateTime::parse_from_rfc3339("2026-10-28T00:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);
        let mut info = ModelInfoV1::new(
            gts::GtsTypeId::new(gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~")),
            "gpt-4o",
            "GPT-4o",
            ContextWindow::default(),
        );
        info.base_url = Some("https://api.openai.com/v1".into());
        info.secret_ref = Some("openai-key".into());
        info.model_version = Some("2026-05".into());
        info.cost.input_per_1m_micro = Some(2_500_000);
        info.provider_settings = serde_json::json!({ "oagw_alias": "openai" });
        let mut model = ModelV1::new(
            Uuid::nil(),
            Uuid::nil(),
            Uuid::nil(),
            "gpt-4o",
            "openai::gpt-4o",
            info,
            Uuid::nil(),
            now,
        );
        model.sharing_mode = SharingMode::Shared;
        model.approval = ApprovalV1::new(ApprovalState::Approved, Uuid::nil(), now);
        model.version = 3;
        model
    }

    /// Both the resolution and the management routes return the full record
    /// (DESIGN §4 Tenant Isolation and Data Protection); the entry adds the
    /// flags only.
    #[test]
    fn model_dto_and_entry_dto_carry_the_full_record() {
        let model = sample_model();
        let mut entry = ModelEntryV1::new(model.clone());
        entry.provider_enabled = true;
        entry.available_for_eval = true;
        let entry = serde_json::to_value(ModelEntryDto::from(entry)).expect("serializes");
        let full = serde_json::to_value(ModelDto::from(model)).expect("serializes");

        for key in ["base_url", "secret_ref", "provider_settings", "cost"] {
            assert!(full["info"].get(key).is_some(), "{key}");
        }
        assert_eq!(full["info"], entry["info"]);
        assert_eq!(full["info"]["model_version"], "2026-05");
        assert_eq!(full["info"]["cost"]["input_per_1m_micro"], 2_500_000);
        assert!(full["info"].get("version").is_none());
        assert_eq!(full["name"], "gpt-4o");
        assert_eq!(full["version"], 3);
        assert_eq!(entry["version"], 3);
        assert!(full.get("inherited").is_none());
        assert_eq!(entry["inherited"], false);
        assert_eq!(
            entry.as_object().map(serde_json::Map::len),
            full.as_object().map(|o| o.len() + 4)
        );
    }
}
