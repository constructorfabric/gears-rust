//! Request body → SDK request conversion, with the REST-only checks of
//! DESIGN §3.3: enum parsing, nested JSON parsing, the `IMMUTABLE_FIELD`
//! and `null` rejections on update bodies, and the `If-Match` precondition.

use std::collections::{HashMap, HashSet};

use axum::http::HeaderMap;
use axum::http::header::IF_MATCH;
use model_registry_sdk::{
    ApprovalState, CreateModelRequestV1, CreateProviderRequestV1, SharingMode, SupportedApi,
    UpdateModelRequestV1, UpdateProviderRequestV1, field,
};

use super::dto::{
    CreateModelRequestDto, CreateProviderRequestDto, UpdateModelRequestDto,
    UpdateProviderRequestDto,
};
use crate::domain::error::{DomainError, Violation};
use crate::domain::precondition::Precondition;

/// Fields no provider update body may carry
/// (`cpt-cf-model-registry-constraint-immutable-identity`, `version`, and the
/// audit fields). `gts_type` is accepted: it is a type migration.
pub const PROVIDER_IMMUTABLE_FIELDS: &[&str] = &[
    "id",
    "tenant_id",
    "slug",
    "version",
    "created_at",
    "created_by",
    "updated_at",
    "updated_by",
];

/// Fields no model update body may carry: the identity fields, `canonical_id`,
/// the model `gts_type`, `version`, and the decision and audit fields.
/// `provider_model_id` is mutable.
pub const MODEL_IMMUTABLE_FIELDS: &[&str] = &[
    "id",
    "tenant_id",
    "provider_id",
    "name",
    "canonical_id",
    "gts_type",
    "version",
    "approval",
    "approval_decided_by",
    "approval_decided_at",
    "created_at",
    "created_by",
    "updated_at",
    "updated_by",
];

/// Provider update fields that `null` clears; `null` on any other field is
/// rejected (ADR-0004).
pub const PROVIDER_NULLABLE_FIELDS: &[&str] = &["metadata"];

/// Model update fields that `null` clears.
pub const MODEL_NULLABLE_FIELDS: &[&str] = &[
    "base_url",
    "secret_ref",
    "description",
    "family",
    "vendor",
    "architecture",
    "size_bytes",
    "format",
    "region",
    "hosted_by",
    "last_release_at",
    "reasoning_level",
    "model_version",
    "icon",
    "multiplier_display",
];

fn invalid(field: &str, description: impl Into<String>, reason: field::Reason) -> DomainError {
    DomainError::invalid(Violation::new(field, description, reason))
}

/// Rejects an update body that names an immutable field or sets a required
/// field to `null`, before it is parsed.
pub fn check_update_body(
    body: &serde_json::Value,
    immutable: &[&str],
    nullable: &[&str],
) -> Result<(), DomainError> {
    let Some(map) = body.as_object() else {
        return Err(invalid(
            "body",
            "must be a JSON object",
            field::Reason::InvalidFormat,
        ));
    };
    let mut violations: Vec<Violation> = immutable
        .iter()
        .filter(|f| map.contains_key(**f))
        .map(|f| Violation::new(*f, "field is immutable", field::Reason::ImmutableField))
        .collect();
    violations.extend(
        map.iter()
            .filter(|(key, value)| {
                value.is_null()
                    && !immutable.contains(&key.as_str())
                    && !nullable.contains(&key.as_str())
            })
            .map(|(key, _)| Violation::new(key, "must not be null", field::Reason::InvalidFormat)),
    );
    if violations.is_empty() {
        Ok(())
    } else {
        Err(DomainError::invalid_all(violations))
    }
}

/// The `If-Match` precondition of DESIGN §3.3: `*` or a missing optional
/// header is an unconditional write; one validator is taken as sent and
/// compared with the row's `ETag` as a string. A missing required header is
/// `IF_MATCH_REQUIRED`; more than one validator is `INVALID_FORMAT`.
pub fn if_match(headers: &HeaderMap, required: bool) -> Result<Precondition, DomainError> {
    let malformed = || {
        invalid(
            "If-Match",
            "must be `*` or one ETag, for example `\"3\"`",
            field::Reason::InvalidFormat,
        )
    };
    let mut tags: Vec<&str> = Vec::new();
    for raw in headers.get_all(IF_MATCH) {
        let line = raw.to_str().map_err(|_| malformed())?;
        tags.extend(line.split(',').map(str::trim).filter(|t| !t.is_empty()));
    }
    match tags.as_slice() {
        [] if required => Err(invalid(
            "If-Match",
            "send the current ETag, or `*` for an unconditional update",
            field::Reason::IfMatchRequired,
        )),
        [] | ["*"] => Ok(Precondition::None),
        [tag] => Ok(Precondition::Tag((*tag).to_owned())),
        _ => Err(malformed()),
    }
}

/// Parses a body into a DTO; a shape error is `INVALID_FORMAT` on `body`.
pub fn body<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, DomainError> {
    serde_json::from_value(value)
        .map_err(|e| invalid("body", e.to_string(), field::Reason::InvalidFormat))
}

fn sharing_mode(raw: &str) -> Result<SharingMode, DomainError> {
    SharingMode::from_wire(raw).ok_or_else(|| {
        invalid(
            "sharing_mode",
            format!("unknown value `{raw}`, expected one of: tenant, shared"),
            field::Reason::InvalidFormat,
        )
    })
}

fn approval_state(raw: &str) -> Result<ApprovalState, DomainError> {
    ApprovalState::from_wire(raw).ok_or_else(|| {
        invalid(
            "approval_state",
            format!("unknown value `{raw}`, expected one of: pending, approved, rejected, revoked"),
            field::Reason::InvalidFormat,
        )
    })
}

fn supported_api(raw: &[String]) -> Result<HashSet<SupportedApi>, DomainError> {
    raw.iter()
        .map(|s| {
            SupportedApi::from_wire(s).ok_or_else(|| {
                invalid(
                    "supported_api",
                    format!("unknown value `{s}`, expected one of: completion, embedding, batch"),
                    field::Reason::InvalidFormat,
                )
            })
        })
        .collect()
}

/// Parses a nested request document into its SDK type. The SDK types ignore
/// unknown keys so that readers stay compatible; a request must not carry
/// any, so each ignored key is a violation on its JSON pointer.
fn nested<T: serde::de::DeserializeOwned>(
    field_name: &str,
    value: serde_json::Value,
) -> Result<T, DomainError> {
    let mut unknown = Vec::new();
    let parsed = serde_ignored::deserialize(value, |path| {
        unknown.push(Violation::new(
            pointer(field_name, &path),
            "unknown field",
            field::Reason::InvalidFormat,
        ));
    })
    .map_err(|e| invalid(field_name, e.to_string(), field::Reason::InvalidFormat))?;
    if unknown.is_empty() {
        Ok(parsed)
    } else {
        Err(DomainError::invalid_all(unknown))
    }
}

/// `field_name` plus the JSON pointer of an ignored key.
fn pointer(field_name: &str, path: &serde_ignored::Path<'_>) -> String {
    use serde_ignored::Path;
    match path {
        Path::Root => field_name.to_owned(),
        Path::Seq { parent, index } => format!("{}/{index}", pointer(field_name, parent)),
        Path::Map { parent, key } => format!("{}/{key}", pointer(field_name, parent)),
        Path::Some { parent }
        | Path::NewtypeStruct { parent }
        | Path::NewtypeVariant { parent } => pointer(field_name, parent),
    }
}

fn nested_or_default<T: serde::de::DeserializeOwned + Default>(
    field_name: &str,
    value: Option<serde_json::Value>,
) -> Result<T, DomainError> {
    value.map_or_else(|| Ok(T::default()), |v| nested(field_name, v))
}

pub fn create_provider(
    dto: CreateProviderRequestDto,
) -> Result<CreateProviderRequestV1, DomainError> {
    let mut b =
        CreateProviderRequestV1::builder(dto.slug, dto.name, gts::GtsTypeId::new(&dto.gts_type));
    if let Some(enabled) = dto.enabled {
        b = b.enabled(enabled);
    }
    if let Some(mode) = dto.sharing_mode.as_deref() {
        b = b.sharing_mode(sharing_mode(mode)?);
    }
    if let Some(managed) = dto.managed {
        b = b.managed(managed);
    }
    if let Some(metadata) = dto.metadata {
        b = b.metadata(metadata);
    }
    Ok(b.build())
}

pub fn update_provider(
    dto: UpdateProviderRequestDto,
) -> Result<UpdateProviderRequestV1, DomainError> {
    let mut req = UpdateProviderRequestV1::default();
    req.name = dto.name;
    req.gts_type = dto.gts_type.map(|t| gts::GtsTypeId::new(&t));
    req.enabled = dto.enabled;
    req.sharing_mode = dto.sharing_mode.as_deref().map(sharing_mode).transpose()?;
    req.managed = dto.managed;
    req.metadata = dto.metadata;
    Ok(req)
}

pub fn create_model(dto: CreateModelRequestDto) -> Result<CreateModelRequestV1, DomainError> {
    let mut req = CreateModelRequestV1::new(
        dto.provider_id,
        dto.name,
        dto.provider_model_id,
        dto.display_name,
        nested("context_window", dto.context_window)?,
    );
    req.sharing_mode = dto.sharing_mode.as_deref().map(sharing_mode).transpose()?;
    req.approval_state = dto
        .approval_state
        .as_deref()
        .map(approval_state)
        .transpose()?;
    req.base_url = dto.base_url;
    req.secret_ref = dto.secret_ref;
    req.description = dto.description;
    req.family = dto.family;
    req.vendor = dto.vendor;
    req.managed = dto.managed.unwrap_or(false);
    req.architecture = dto.architecture;
    req.size_bytes = dto.size_bytes;
    req.format = dto.format;
    req.region = dto.region;
    req.hosted_by = dto.hosted_by;
    req.last_release_at = dto.last_release_at;
    req.reasoning_level = dto.reasoning_level;
    req.model_version = dto.model_version;
    req.sort_order = dto.sort_order.unwrap_or(0);
    req.icon = dto.icon;
    req.multiplier_display = dto.multiplier_display;
    req.cost = nested_or_default("cost", dto.cost)?;
    req.performance = nested_or_default("performance", dto.performance)?;
    req.additional_info = nested_or_default::<HashMap<String, serde_json::Value>>(
        "additional_info",
        dto.additional_info,
    )?;
    req.supported_api = dto
        .supported_api
        .as_deref()
        .map(supported_api)
        .transpose()?
        .unwrap_or_default();
    req.capabilities = nested_or_default("capabilities", dto.capabilities)?;
    req.disabled_capabilities =
        nested_or_default("disabled_capabilities", dto.disabled_capabilities)?;
    req.default_parameters = nested_or_default("default_parameters", dto.default_parameters)?;
    req.allow_parameter_override = dto.allow_parameter_override.unwrap_or(false);
    req.allow_extra_params = dto.allow_extra_params.unwrap_or_default();
    if let Some(settings) = dto.provider_settings {
        req.provider_settings = settings;
    }
    Ok(req)
}

pub fn update_model(dto: UpdateModelRequestDto) -> Result<UpdateModelRequestV1, DomainError> {
    let mut req = UpdateModelRequestV1::default();
    req.sharing_mode = dto.sharing_mode.as_deref().map(sharing_mode).transpose()?;
    req.approval_state = dto
        .approval_state
        .as_deref()
        .map(approval_state)
        .transpose()?;
    req.provider_model_id = dto.provider_model_id;
    req.base_url = dto.base_url;
    req.secret_ref = dto.secret_ref;
    req.display_name = dto.display_name;
    req.description = dto.description;
    req.family = dto.family;
    req.vendor = dto.vendor;
    req.managed = dto.managed;
    req.architecture = dto.architecture;
    req.size_bytes = dto.size_bytes;
    req.format = dto.format;
    req.region = dto.region;
    req.hosted_by = dto.hosted_by;
    req.last_release_at = dto.last_release_at;
    req.reasoning_level = dto.reasoning_level;
    req.model_version = dto.model_version;
    req.sort_order = dto.sort_order;
    req.icon = dto.icon;
    req.multiplier_display = dto.multiplier_display;
    req.cost = dto.cost.map(|v| nested("cost", v)).transpose()?;
    req.performance = dto
        .performance
        .map(|v| nested("performance", v))
        .transpose()?;
    req.additional_info = dto
        .additional_info
        .map(|v| nested("additional_info", v))
        .transpose()?;
    req.supported_api = dto
        .supported_api
        .as_deref()
        .map(supported_api)
        .transpose()?;
    req.capabilities = dto
        .capabilities
        .map(|v| nested("capabilities", v))
        .transpose()?;
    req.disabled_capabilities = dto
        .disabled_capabilities
        .map(|v| nested("disabled_capabilities", v))
        .transpose()?;
    req.context_window = dto
        .context_window
        .map(|v| nested("context_window", v))
        .transpose()?;
    req.default_parameters = dto
        .default_parameters
        .map(|v| nested("default_parameters", v))
        .transpose()?;
    req.allow_parameter_override = dto.allow_parameter_override;
    req.allow_extra_params = dto.allow_extra_params;
    req.provider_settings = dto.provider_settings;
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde_json::json;
    use toolkit_gts::gts_id;

    fn violations(err: &DomainError) -> Vec<(String, String)> {
        match err {
            DomainError::Invalid { violations, .. } => violations
                .iter()
                .map(|v| (v.field.clone(), v.reason.as_wire().to_owned()))
                .collect(),
            other => panic!("{other:?}"),
        }
    }

    fn headers(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(IF_MATCH, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn unknown_keys_in_a_nested_document_are_rejected() {
        use model_registry_sdk::{ModelCapabilities, ModelCostV1};

        nested::<ModelCostV1>("cost", json!({ "input_per_1m_micro": 2 })).unwrap();
        let err = nested::<ModelCostV1>(
            "cost",
            json!({ "input_per_1M_micro": 2_500_000, "output_per_1m_micro": 1 }),
        )
        .unwrap_err();
        assert_eq!(
            violations(&err),
            vec![(
                "cost/input_per_1M_micro".to_owned(),
                field::INVALID_FORMAT.to_owned()
            )]
        );
        let err =
            nested::<ModelCapabilities>("capabilities", json!({ "vision": { "enabeld": true } }))
                .unwrap_err();
        assert_eq!(
            violations(&err),
            vec![(
                "capabilities/vision/enabeld".to_owned(),
                field::INVALID_FORMAT.to_owned()
            )]
        );
    }

    /// Known gap (DESIGN §3.3): serde buffers a tagged enum before parsing
    /// it, so keys inside its variants are not reported.
    #[test]
    fn unknown_keys_inside_tagged_variants_are_not_seen() {
        use model_registry_sdk::DefaultInferenceParametersV1;

        let err = nested::<DefaultInferenceParametersV1>(
            "default_parameters",
            json!({ "text": { "format": { "type": "text" }, "bogus": 1 } }),
        )
        .unwrap_err();
        assert_eq!(
            violations(&err),
            vec![(
                "default_parameters/text/bogus".to_owned(),
                field::INVALID_FORMAT.to_owned()
            )]
        );
        for inside_a_variant in [
            json!({ "text": { "format": { "type": "json_schema", "name": "n", "strict": true, "bogus": 1 } } }),
            json!({ "tool_choice": { "type": "function", "name": "f", "bogus": 1 } }),
        ] {
            nested::<DefaultInferenceParametersV1>("default_parameters", inside_a_variant).unwrap();
        }
    }

    #[test]
    fn immutable_fields_are_rejected_with_the_sdk_reason() {
        let err = check_update_body(
            &json!({ "name": "x", "slug": "y", "version": 2, "created_at": "z" }),
            PROVIDER_IMMUTABLE_FIELDS,
            PROVIDER_NULLABLE_FIELDS,
        )
        .unwrap_err();
        assert_eq!(
            violations(&err),
            vec![
                ("slug".to_owned(), field::IMMUTABLE_FIELD.to_owned()),
                ("version".to_owned(), field::IMMUTABLE_FIELD.to_owned()),
                ("created_at".to_owned(), field::IMMUTABLE_FIELD.to_owned())
            ]
        );
        // A provider `gts_type` is a type migration, not an immutable field.
        check_update_body(
            &json!({ "gts_type": "x" }),
            PROVIDER_IMMUTABLE_FIELDS,
            PROVIDER_NULLABLE_FIELDS,
        )
        .unwrap();
        // `provider_model_id` is mutable; `name` and the model `gts_type` are not.
        check_update_body(
            &json!({ "provider_model_id": "gpt-4o-2026" }),
            MODEL_IMMUTABLE_FIELDS,
            MODEL_NULLABLE_FIELDS,
        )
        .unwrap();
        let err = check_update_body(
            &json!({ "name": "x", "gts_type": "y", "version": 1 }),
            MODEL_IMMUTABLE_FIELDS,
            MODEL_NULLABLE_FIELDS,
        )
        .unwrap_err();
        assert_eq!(violations(&err).len(), 3);
        assert!(
            check_update_body(&json!([]), MODEL_IMMUTABLE_FIELDS, MODEL_NULLABLE_FIELDS).is_err()
        );
    }

    #[test]
    fn null_is_rejected_on_required_fields_only() {
        let err = check_update_body(
            &json!({ "name": null, "enabled": null, "metadata": null, "slug": null }),
            PROVIDER_IMMUTABLE_FIELDS,
            PROVIDER_NULLABLE_FIELDS,
        )
        .unwrap_err();
        let mut found = violations(&err);
        found.sort();
        assert_eq!(
            found,
            vec![
                ("enabled".to_owned(), field::INVALID_FORMAT.to_owned()),
                ("name".to_owned(), field::INVALID_FORMAT.to_owned()),
                ("slug".to_owned(), field::IMMUTABLE_FIELD.to_owned()),
            ]
        );
        let err = check_update_body(
            &json!({ "display_name": null, "provider_settings": null, "base_url": null }),
            MODEL_IMMUTABLE_FIELDS,
            MODEL_NULLABLE_FIELDS,
        )
        .unwrap_err();
        let mut found = violations(&err);
        found.sort();
        assert_eq!(
            found,
            vec![
                ("display_name".to_owned(), field::INVALID_FORMAT.to_owned()),
                (
                    "provider_settings".to_owned(),
                    field::INVALID_FORMAT.to_owned()
                ),
            ]
        );
        // Every nullable field of the DTOs is in its list.
        let all_null: serde_json::Map<String, serde_json::Value> = MODEL_NULLABLE_FIELDS
            .iter()
            .map(|f| ((*f).to_owned(), serde_json::Value::Null))
            .collect();
        let body_value = serde_json::Value::Object(all_null);
        check_update_body(&body_value, MODEL_IMMUTABLE_FIELDS, MODEL_NULLABLE_FIELDS).unwrap();
        let dto: UpdateModelRequestDto = body(body_value).unwrap();
        let req = update_model(dto).unwrap();
        assert_eq!(req.base_url, Some(None));
        assert_eq!(req.multiplier_display, Some(None));
    }

    #[test]
    fn if_match_precondition() {
        assert_eq!(
            if_match(&headers(&["\"3\""]), true).unwrap(),
            Precondition::Tag("\"3\"".into())
        );
        // A validator is taken as sent; one that is no ETag never matches.
        assert_eq!(
            if_match(&headers(&["W/\"3\""]), true).unwrap(),
            Precondition::Tag("W/\"3\"".into())
        );
        assert_eq!(
            if_match(&headers(&["*"]), true).unwrap(),
            Precondition::None
        );
        assert_eq!(if_match(&headers(&[]), false).unwrap(), Precondition::None);

        let err = if_match(&headers(&[]), true).unwrap_err();
        assert_eq!(
            violations(&err),
            vec![("If-Match".to_owned(), field::IF_MATCH_REQUIRED.to_owned())]
        );
        for bad in [vec!["\"1\", \"2\""], vec!["\"1\"", "\"2\""]] {
            let err = if_match(&headers(&bad), false).unwrap_err();
            assert_eq!(
                violations(&err),
                vec![("If-Match".to_owned(), field::INVALID_FORMAT.to_owned())],
                "{bad:?}"
            );
        }
    }

    #[test]
    fn update_model_body_distinguishes_absent_null_and_value() {
        let dto: UpdateModelRequestDto = body(json!({
            "base_url": null, "description": "d", "approval_state": "revoked",
            "provider_model_id": "gpt-4o-2026", "cost": { "input_per_1m_micro": 5 }
        }))
        .unwrap();
        let req = update_model(dto).unwrap();
        assert_eq!(req.base_url, Some(None));
        assert_eq!(req.description, Some(Some("d".into())));
        assert_eq!(req.secret_ref, None);
        assert_eq!(req.approval_state, Some(ApprovalState::Revoked));
        assert_eq!(req.provider_model_id.as_deref(), Some("gpt-4o-2026"));
        assert_eq!(req.cost.unwrap().input_per_1m_micro, Some(5));
    }

    #[test]
    fn enums_and_nested_objects_are_validated() {
        let dto: UpdateModelRequestDto = body(json!({ "sharing_mode": "subtree" })).unwrap();
        let err = update_model(dto).unwrap_err();
        assert_eq!(
            violations(&err)[0],
            ("sharing_mode".to_owned(), field::INVALID_FORMAT.to_owned())
        );

        let dto: CreateModelRequestDto = body(json!({
            "provider_id": uuid::Uuid::nil(),
            "name": "gpt",
            "provider_model_id": "gpt",
            "display_name": "GPT",
            "context_window": { "max_input_tokens": "many" },
            "supported_api": ["completion"]
        }))
        .unwrap();
        let err = create_model(dto).unwrap_err();
        assert_eq!(violations(&err)[0].0, "context_window");

        let dto: CreateModelRequestDto = body(json!({
            "provider_id": uuid::Uuid::nil(),
            "name": "gpt",
            "provider_model_id": "gpt",
            "display_name": "GPT",
            "context_window": { "max_input_tokens": 8 },
            "supported_api": ["completion", "batch"],
            "performance": { "tokens_per_second": 90 },
            "cost": { "output_per_1m_micro": 10 }
        }))
        .unwrap();
        let req = create_model(dto).unwrap();
        assert_eq!(req.name, "gpt");
        assert_eq!(req.supported_api.len(), 2);
        assert_eq!(req.performance.tokens_per_second, Some(90));
        assert_eq!(req.cost.output_per_1m_micro, Some(10));
        assert!(!req.capabilities.streaming);
        assert_eq!(req.provider_settings, json!({}));
        assert!(!req.managed);
    }

    #[test]
    fn provider_bodies_parse() {
        let dto: CreateProviderRequestDto = body(json!({
            "slug": "openai", "name": "OpenAI", "gts_type": gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~"), "sharing_mode": "shared"
        }))
        .unwrap();
        let req = create_provider(dto).unwrap();
        assert_eq!(req.sharing_mode(), SharingMode::Shared);
        assert!(req.enabled());

        let dto: UpdateProviderRequestDto =
            body(json!({ "metadata": null, "enabled": false, "gts_type": gts_id!("cf.genai.model.provider.v1~cf.genai._.azure.v1~") })).unwrap();
        let req = update_provider(dto).unwrap();
        assert_eq!(req.metadata, Some(None));
        assert_eq!(req.enabled, Some(false));
        assert_eq!(
            req.gts_type.map(|t| t.to_string()).as_deref(),
            Some(gts_id!("cf.genai.model.provider.v1~cf.genai._.azure.v1~"))
        );
    }
}
