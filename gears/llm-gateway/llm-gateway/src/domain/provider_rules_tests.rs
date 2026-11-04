//! Provider rules registry: permissive default and per-provider lookup.

use std::sync::Arc;

use super::{
    GENERIC_PARAMETERS, PROMPT_CACHE_KEY, PermissiveRules, ProviderRules, ProviderRulesRegistry,
};
use crate::domain::test_support::{FakeRules, OPENAI_TYPE, sample_model_info};

const OTHER_TYPE: &str = "gts.cf.genai.model.info.v1~cf.genai._.other.v1~";

#[test]
fn permissive_rules_support_every_generic_parameter() {
    let model = sample_model_info(OPENAI_TYPE);
    for parameter in GENERIC_PARAMETERS {
        assert!(
            PermissiveRules.supports_parameter(&model, parameter),
            "{parameter}"
        );
    }
}

#[test]
fn permissive_rules_support_cache_control_hints() {
    let model = sample_model_info(OPENAI_TYPE);
    assert!(PermissiveRules.supports_cache_control(&model, PROMPT_CACHE_KEY));
}

#[test]
fn permissive_rules_express_every_schema() {
    let model = sample_model_info(OPENAI_TYPE);
    let schema = serde_json::json!({ "oneOf": [{ "type": "string" }], "patternProperties": {} });
    assert_eq!(
        PermissiveRules.unsupported_schema_construct(&model, &schema),
        None
    );
}

#[test]
fn registry_returns_the_rules_registered_for_the_provider_type() {
    let rules = FakeRules {
        unsupported_params: ["temperature"].into(),
        ..FakeRules::default()
    };
    let registry = ProviderRulesRegistry::permissive().with_rules(OPENAI_TYPE, Arc::new(rules));
    let model = sample_model_info(OPENAI_TYPE);

    assert!(
        !registry
            .rules_for(OPENAI_TYPE)
            .supports_parameter(&model, "temperature")
    );
    assert!(
        registry
            .rules_for(OPENAI_TYPE)
            .supports_parameter(&model, "top_p")
    );
}

#[test]
fn unknown_provider_type_gets_the_permissive_default() {
    let rules = FakeRules {
        unsupported_params: ["temperature"].into(),
        unsupported_construct: Some("oneOf"),
        ..FakeRules::default()
    };
    let registry = ProviderRulesRegistry::permissive().with_rules(OPENAI_TYPE, Arc::new(rules));
    let model = sample_model_info(OTHER_TYPE);
    let schema = serde_json::json!({ "type": "object" });

    let fallback = registry.rules_for(OTHER_TYPE);
    assert!(fallback.supports_parameter(&model, "temperature"));
    assert_eq!(fallback.unsupported_schema_construct(&model, &schema), None);
}

#[test]
fn permissive_rules_support_every_dimensions_value() {
    let model = sample_model_info(OPENAI_TYPE);
    assert!(PermissiveRules.supports_dimensions(&model, 1));
    assert!(PermissiveRules.supports_dimensions(&model, 3_072));
}

#[test]
fn permissive_rules_support_the_input_type_parameter() {
    let model = sample_model_info(OPENAI_TYPE);
    assert!(PermissiveRules.supports_parameter(&model, super::INPUT_TYPE));
    assert_eq!(super::INPUT_TYPE, "input_type");
}

#[test]
fn registry_serves_provider_specific_dimensions_rules() {
    let registry = ProviderRulesRegistry::permissive().with_rules(
        OPENAI_TYPE,
        Arc::new(FakeRules {
            reject_all_dimensions: true,
            ..FakeRules::default()
        }),
    );
    let model = sample_model_info(OPENAI_TYPE);
    assert!(
        !registry
            .rules_for(OPENAI_TYPE)
            .supports_dimensions(&model, 8)
    );
    assert!(
        registry
            .rules_for(OTHER_TYPE)
            .supports_dimensions(&model, 8)
    );
}
