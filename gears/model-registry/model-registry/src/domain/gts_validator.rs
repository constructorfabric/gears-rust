//! GTS checks on writes (DESIGN §4 GTS Checks): provider type checks on
//! provider create and type migration, `provider_settings` validation on
//! model create, settings update, and for every model of a provider on type
//! migration. Not called on the resolution path.
//!
//! Settings are validated by the Types Registry SDK payload validator of the
//! provider type, on the `{"gts_type": …, "provider_settings": …}` envelope.

use async_trait::async_trait;
use model_registry_sdk::field;
use model_registry_sdk::gts::PROVIDER_BASE_TYPE;
use toolkit_security::SecurityContext;
use types_registry_sdk::{
    CompilePayloadValidatorError, GtsCompilePayloadValidatorError, PayloadValidator, TypeSchema,
};

use super::error::{DomainError, Violation};

/// Where provider type schemas come from (Types Registry in production, a
/// map in tests).
#[async_trait]
pub trait SchemaSource: Send + Sync {
    /// The active Type Schema of `type_id` with its authored content and
    /// resolved schema selected, or `None` when the registry has no active
    /// schema for it.
    async fn type_schema(
        &self,
        ctx: &SecurityContext,
        type_id: &gts::GtsTypeId,
    ) -> Result<Option<TypeSchema>, DomainError>;
}

/// Provider type checks 1 and 2: the id parses as a GTS type id, and it is
/// derived from the provider base type.
pub fn check_provider_type(gts_type: &gts::GtsTypeId) -> Result<(), DomainError> {
    let raw = gts_type.as_ref();
    if let Err(e) = gts::GtsTypeId::try_new(raw) {
        return Err(DomainError::invalid(Violation::new(
            "gts_type",
            format!("not a GTS type id: {e}"),
            field::Reason::InvalidGtsType,
        )));
    }
    let derived = raw
        .strip_prefix(PROVIDER_BASE_TYPE)
        .is_some_and(|rest| !rest.is_empty() && rest.ends_with('~'));
    if !derived {
        return Err(DomainError::invalid(Violation::new(
            "gts_type",
            format!("must be derived from the provider base type {PROVIDER_BASE_TYPE}"),
            field::Reason::NotAProviderType,
        )));
    }
    Ok(())
}

/// Fetches the provider type for a write and compiles its payload validator.
/// `field_name` names the request field reported when the type is not
/// registered or is abstract.
pub async fn provider_type_validator(
    source: &dyn SchemaSource,
    ctx: &SecurityContext,
    gts_type: &gts::GtsTypeId,
    field_name: &'static str,
) -> Result<PayloadValidator, DomainError> {
    let schema = source.type_schema(ctx, gts_type).await?.ok_or_else(|| {
        DomainError::invalid(Violation::new(
            field_name,
            format!("provider type {gts_type} is not registered"),
            field::Reason::GtsTypeNotRegistered,
        ))
    })?;
    compile_validator(&schema, field_name)
}

fn compile_validator(
    schema: &TypeSchema,
    field_name: &'static str,
) -> Result<PayloadValidator, DomainError> {
    schema.compile_payload_validator().map_err(|e| match e {
        CompilePayloadValidatorError::Upstream(GtsCompilePayloadValidatorError::Abstract(_)) => {
            DomainError::invalid(Violation::new(
                field_name,
                format!("provider type {} is abstract", schema.type_id),
                field::Reason::NotAProviderType,
            ))
        }
        other => DomainError::internal(format!(
            "provider type {} cannot compile a payload validator: {other}",
            schema.type_id
        )),
    })
}

/// Checks one `provider_settings` document against the provider type. A
/// violation is reported on `field_name`: `provider_settings` on model
/// writes, or `gts_type` with the model's canonical id in the description on
/// a type migration (`context`).
#[must_use]
pub fn check_settings(
    validator: &PayloadValidator,
    settings: &serde_json::Value,
    field_name: &str,
    context: Option<&str>,
) -> Vec<Violation> {
    let describe = |message: &str| match context {
        Some(ctx) => format!("{ctx}: provider_settings: {message}"),
        None => message.to_owned(),
    };
    if !settings.is_object() {
        return vec![Violation::new(
            field_name,
            describe("must be a JSON object"),
            field::Reason::SchemaMismatch,
        )];
    }
    let envelope = serde_json::json!({
        "gts_type": validator.type_id(),
        "provider_settings": settings,
    });
    match validator.validate(&envelope) {
        Ok(()) => Vec::new(),
        Err(e) => vec![Violation::new(
            field_name,
            describe(&e.to_string()),
            field::Reason::SchemaMismatch,
        )],
    }
}

/// Validates `provider_settings` of one model write against the provider
/// type.
pub fn validate_settings(
    validator: &PayloadValidator,
    settings: &serde_json::Value,
) -> Result<(), DomainError> {
    let violations = check_settings(validator, settings, "provider_settings", None);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(DomainError::invalid_all(violations))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_registry_sdk::ProviderTypeV1;
    use serde_json::json;
    use toolkit_gts::gts_id;

    fn reasons(err: &DomainError) -> Vec<(String, field::Reason)> {
        match err {
            DomainError::Invalid { violations: v, .. } => {
                v.iter().map(|v| (v.field.clone(), v.reason)).collect()
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn provider_type_checks() {
        check_provider_type(&gts::GtsTypeId::new(gts_id!(
            "cf.genai.model.provider.v1~cf.genai._.openai.v1~"
        )))
        .unwrap();

        let err = check_provider_type(&gts::GtsTypeId::new("not a gts id")).unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![("gts_type".to_owned(), field::Reason::InvalidGtsType)]
        );

        let err = check_provider_type(&gts::GtsTypeId::new(gts_id!(
            "cf.genai.model.provider.v1~cf.genai._.openai.v1"
        )))
        .unwrap_err();
        assert_eq!(reasons(&err)[0].1, field::Reason::InvalidGtsType);

        for bad in [
            gts_id!("cf.genai.model.provider.v1~"),
            gts_id!("cf.toolkit.authz.permission.v1~cf.genai._.openai.v1~"),
        ] {
            let err = check_provider_type(&gts::GtsTypeId::new(bad)).unwrap_err();
            assert_eq!(reasons(&err)[0].1, field::Reason::NotAProviderType, "{bad}");
        }
    }

    const BASE: &str = gts_id!("cf.genai.model.provider.v1~");
    const DERIVED: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.validator_test.v1~");
    const MID: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.mid.v1~");
    const LEAF: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.mid.v1~cf.genai._.leaf.v1~");

    #[toolkit_macros::domain_model]
    #[derive(Debug)]
    #[toolkit_gts::gts_type_schema(
        dir_path = "schemas",
        base = ProviderTypeV1,
        type_id = gts_id!("cf.genai.model.provider.v1~cf.genai._.validator_test.v1~"),
        description = "Validator test provider settings",
        properties = "oagw_alias"
    )]
    pub struct ValidatorTestSettingsV1 {
        pub oagw_alias: String,
    }

    fn inventory_schema(type_id: &str) -> serde_json::Value {
        let entry = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
            .into_iter()
            .find(|e| e.type_id == type_id)
            .unwrap_or_else(|| panic!("{type_id} is in the GTS inventory"));
        serde_json::from_str(&(entry.schema_fn)()).unwrap()
    }

    /// Resolves `type_id` the way Types Registry does, over `schemas`.
    fn resolved(type_id: &str, schemas: &[(&str, serde_json::Value)]) -> PayloadValidator {
        let mut store = gts::GtsStore::new();
        for (id, schema) in schemas {
            store.register_schema(id, schema).unwrap();
        }
        let resolved = store.validate_schema(type_id).unwrap();
        let schema = crate::testing::provider_type_schema(type_id, resolved.schema, false);
        compile_validator(&schema, "gts_type").unwrap()
    }

    fn generated() -> PayloadValidator {
        resolved(
            DERIVED,
            &[
                (BASE, inventory_schema(BASE)),
                (DERIVED, inventory_schema(DERIVED)),
            ],
        )
    }

    #[test]
    fn generated_provider_type_validates_the_settings_envelope() {
        let validator = generated();
        validate_settings(&validator, &json!({ "oagw_alias": "p" })).unwrap();

        let err = validate_settings(&validator, &json!({ "oagw_alias": 7 })).unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![(
                "provider_settings".to_owned(),
                field::Reason::SchemaMismatch
            )]
        );
        let err = validate_settings(&validator, &json!({})).unwrap_err();
        assert_eq!(reasons(&err)[0].1, field::Reason::SchemaMismatch);
    }

    /// A `#/definitions/...` reference from an inherited parent resolves.
    #[test]
    fn definitions_of_an_inherited_parent_resolve() {
        let mid = json!({
            "$id": format!("gts://{MID}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "definitions": {
                "Endpoint": {
                    "type": "object",
                    "required": ["port"],
                    "properties": { "port": { "type": "integer" } }
                }
            },
            "allOf": [
                { "$ref": format!("gts://{BASE}") },
                {
                    "type": "object",
                    "properties": {
                        "provider_settings": {
                            "type": "object",
                            "required": ["endpoint"],
                            "properties": { "endpoint": { "$ref": "#/definitions/Endpoint" } }
                        }
                    }
                }
            ]
        });
        let leaf = json!({
            "$id": format!("gts://{LEAF}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [
                { "$ref": format!("gts://{MID}") },
                {
                    "type": "object",
                    "properties": {
                        "provider_settings": {
                            "type": "object",
                            "required": ["oagw_alias"],
                            "properties": { "oagw_alias": { "type": "string" } }
                        }
                    }
                }
            ]
        });
        let validator = resolved(
            LEAF,
            &[(BASE, inventory_schema(BASE)), (MID, mid), (LEAF, leaf)],
        );
        validate_settings(
            &validator,
            &json!({ "oagw_alias": "p", "endpoint": { "port": 443 } }),
        )
        .unwrap();
        validate_settings(
            &validator,
            &json!({ "oagw_alias": "p", "endpoint": { "port": "https" } }),
        )
        .unwrap_err();
        validate_settings(&validator, &json!({ "endpoint": { "port": 443 } })).unwrap_err();
    }

    #[test]
    fn abstract_type_is_not_a_provider_type() {
        let schema =
            crate::testing::provider_type_schema(DERIVED, json!({ "type": "object" }), true);
        let err = compile_validator(&schema, "gts_type").unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![("gts_type".to_owned(), field::Reason::NotAProviderType)]
        );
    }

    #[test]
    fn open_type_accepts_any_object_and_rejects_non_objects() {
        let open = crate::testing::provider_type_schema(
            DERIVED,
            json!({ "type": "object", "properties": { "provider_settings": { "type": "object" } } }),
            false,
        );
        let validator = compile_validator(&open, "gts_type").unwrap();
        validate_settings(&validator, &json!({ "anything": [1, 2] })).unwrap();
        validate_settings(&validator, &json!({})).unwrap();
        let err = validate_settings(&validator, &json!("str")).unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![(
                "provider_settings".to_owned(),
                field::Reason::SchemaMismatch
            )]
        );
    }

    /// A type migration reports a model's violation on `gts_type`, naming
    /// the model.
    #[test]
    fn migration_violations_land_on_gts_type_with_context() {
        let validator = generated();
        let ok = check_settings(
            &validator,
            &json!({ "oagw_alias": "p" }),
            "gts_type",
            Some("openai::gpt-4o"),
        );
        assert!(ok.is_empty());
        let bad = check_settings(
            &validator,
            &json!({ "oagw_alias": 1 }),
            "gts_type",
            Some("openai::gpt-4o"),
        );
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].field, "gts_type");
        assert_eq!(bad[0].reason, field::Reason::SchemaMismatch);
        assert!(
            bad[0]
                .description
                .starts_with("openai::gpt-4o: provider_settings:"),
            "{}",
            bad[0].description
        );
    }

    #[tokio::test]
    async fn unregistered_type_is_reported_on_the_named_field() {
        let source = crate::testing::StaticSchemaSource::new();
        let ctx = crate::testing::security_context(uuid::Uuid::new_v4());
        let err = provider_type_validator(
            &source,
            &ctx,
            &gts::GtsTypeId::new(gts_id!("cf.genai.model.provider.v1~cf.genai._.x.v1~")),
            "provider_settings",
        )
        .await
        .unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![(
                "provider_settings".to_owned(),
                field::Reason::GtsTypeNotRegistered
            )]
        );
    }
}
