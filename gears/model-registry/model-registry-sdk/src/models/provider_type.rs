// Created: 2026-10-26 by Constructor Tech
// Updated: 2026-11-12 by Constructor Tech
//! [`ProviderTypeV1`] — the provider base type `gts.cf.genai.model.provider.v1~`
//! (DESIGN §3.3 Provider Base Type).

use toolkit_gts::gts_type_schema;

/// Base GTS type of every AI model provider type.
///
/// Each LLM Gateway provider plugin publishes a derived type whose schema
/// narrows `provider_settings` to the plugin's settings:
///
/// ```ignore
/// #[toolkit_gts::gts_type_schema(
///     dir_path = "schemas",
///     base = ProviderTypeV1,
///     type_id = gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~"),
///     description = "OpenAI provider settings",
///     properties = "oagw_alias",
/// )]
/// pub struct OpenAiProviderSettingsV1 {
///     pub oagw_alias: String,
/// }
/// ```
///
/// Cost is not a provider setting: it is
/// [`ModelInfoV1::cost`](crate::models::ModelInfoV1::cost), one shape for
/// every provider type.
///
/// The derived struct's fields are the `provider_settings` object. A
/// provider's `gts_type` must be such a derived type: the base type is
/// abstract. On write, the registry validates
/// `{"gts_type": …, "provider_settings": …}` as an instance of the provider
/// type, with the Types Registry SDK payload validator of the type. A
/// violation's field is `provider_settings`.
///
/// Schema carrier only; the registry never constructs this type. The
/// `gts_type` discriminator field is required by the GTS macro on a base
/// type.
#[derive(Debug)]
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.genai.model.provider.v1~"),
    description = "AI model provider base type; derived provider types narrow provider_settings",
    properties = "gts_type,provider_settings",
    gts_abstract = true,
    base = true
)]
pub struct ProviderTypeV1<P: gts::GtsSchema = serde_json::Value> {
    /// The provider type id.
    pub gts_type: gts::GtsTypeId,
    /// Provider-specific settings; an open object on the base type.
    pub provider_settings: P,
}

#[cfg(test)]
mod tests {
    use toolkit_gts::gts_id;

    use super::ProviderTypeV1;
    use crate::gts::PROVIDER_BASE_TYPE;

    const DERIVED_TYPE: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.test.v1~");

    #[derive(Debug)]
    #[toolkit_gts::gts_type_schema(
        dir_path = "schemas",
        base = ProviderTypeV1,
        type_id = gts_id!("cf.genai.model.provider.v1~cf.genai._.test.v1~"),
        description = "Test provider settings",
        properties = "oagw_alias"
    )]
    pub struct TestProviderSettingsV1 {
        pub oagw_alias: String,
    }

    fn inventory_schema(type_id: &str) -> serde_json::Value {
        let entry = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
            .into_iter()
            .find(|e| e.type_id == type_id)
            .expect("type registered in the GTS inventory");
        serde_json::from_str(&(entry.schema_fn)()).expect("schema is valid JSON")
    }

    #[test]
    fn base_type_id_matches_constant() {
        assert_eq!(
            <ProviderTypeV1 as gts::GtsSchema>::TYPE_ID,
            PROVIDER_BASE_TYPE
        );
    }

    #[test]
    fn base_type_is_in_inventory_with_open_provider_settings() {
        let schema = inventory_schema(PROVIDER_BASE_TYPE);
        let settings = &schema["properties"]["provider_settings"];
        assert_eq!(settings["type"], "object", "provider_settings: {schema}");
        assert!(
            settings.get("properties").is_none(),
            "base type must not constrain provider_settings: {schema}"
        );
    }

    #[test]
    fn derived_type_narrows_provider_settings_inside_all_of() {
        let schema = inventory_schema(DERIVED_TYPE);
        assert!(
            schema.pointer("/properties/provider_settings").is_none(),
            "derived type has no root provider_settings: {schema}"
        );
        let branches = schema["allOf"].as_array().expect("allOf array");
        let narrowed: Vec<_> = branches
            .iter()
            .filter_map(|b| b.pointer("/properties/provider_settings"))
            .collect();
        assert_eq!(narrowed.len(), 1, "one narrowed subschema: {schema}");
        assert_eq!(narrowed[0]["type"], "object");
        assert_eq!(narrowed[0]["properties"]["oagw_alias"]["type"], "string");
        assert_eq!(narrowed[0]["required"], serde_json::json!(["oagw_alias"]));
    }
}
