//! Provider rules registry: permissive default and per-provider lookup.

use std::sync::Arc;

use super::{
    GENERIC_PARAMETERS, PROMPT_CACHE_KEY, PermissiveRules, ProviderRules, ProviderRulesRegistry,
};
use model_registry_sdk::ModelInfoV1;

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

// ── pricing hook (DESIGN §3.2 Quota Guard "Amount") ─────────────────────────

mod pricing {
    use super::*;
    use crate::domain::pricing::ModelPricing;
    use crate::domain::test_support::{remove_cost, sample_model_info_patched, set_cost};

    fn pricing_of(patch: impl FnOnce(&mut serde_json::Value)) -> ModelPricing {
        let model = sample_model_info_patched(OPENAI_TYPE, patch);
        PermissiveRules.pricing(&model)
    }

    #[test]
    fn permissive_rules_parse_both_rates_from_provider_settings_cost() {
        let pricing = pricing_of(|v| set_cost(v, Some(150), Some(600)));
        assert_eq!(
            pricing,
            ModelPricing {
                input_per_1k_micro: Some(150),
                output_per_1k_micro: Some(600)
            }
        );
    }

    #[test]
    fn explicit_zero_rates_are_present_not_missing() {
        let pricing = pricing_of(|v| set_cost(v, Some(0), Some(0)));
        assert_eq!(pricing.input_per_1k_micro, Some(0));
        assert_eq!(pricing.output_per_1k_micro, Some(0));
    }

    #[test]
    fn a_missing_cost_object_gives_no_rates() {
        assert_eq!(pricing_of(remove_cost), ModelPricing::default());
    }

    #[test]
    fn a_null_rate_is_missing_and_the_other_rate_is_kept() {
        let pricing = pricing_of(|v| set_cost(v, Some(150), None));
        assert_eq!(pricing.input_per_1k_micro, Some(150));
        assert_eq!(pricing.output_per_1k_micro, None);
    }

    #[test]
    fn unparseable_rates_are_missing() {
        for bad in [
            serde_json::json!("150"),
            serde_json::json!(-5),
            serde_json::json!(1.5),
            serde_json::json!({ "x": 1 }),
            serde_json::json!(true),
        ] {
            let pricing = pricing_of(|v| {
                v["provider_settings"]["cost"] =
                    serde_json::json!({ "input_per_1k_micro": bad, "output_per_1k_micro": 600 });
            });
            assert_eq!(pricing.input_per_1k_micro, None, "{bad}");
            assert_eq!(pricing.output_per_1k_micro, Some(600), "{bad}");
        }
    }

    #[test]
    fn a_non_object_cost_gives_no_rates() {
        let pricing = pricing_of(|v| v["provider_settings"]["cost"] = serde_json::json!("free"));
        assert_eq!(pricing, ModelPricing::default());
    }

    #[test]
    fn other_cost_fields_are_ignored() {
        let pricing = pricing_of(|v| {
            v["provider_settings"]["cost"] = serde_json::json!({
                "input_per_1k_micro": 1,
                "output_per_1k_micro": 2,
                "cached_input_per_1k_micro": 3,
                "web_search_per_1k_calls_micro": 4
            });
        });
        assert_eq!(pricing.input_per_1k_micro, Some(1));
        assert_eq!(pricing.output_per_1k_micro, Some(2));
    }

    #[test]
    fn the_registry_fallback_and_fake_rules_use_the_permissive_parser() {
        let model = sample_model_info_patched(OPENAI_TYPE, |v| set_cost(v, Some(7), Some(9)));
        let registry = ProviderRulesRegistry::permissive();
        assert_eq!(
            registry
                .rules_for(OTHER_TYPE)
                .pricing(&model)
                .input_per_1k_micro,
            Some(7)
        );
        assert_eq!(
            FakeRules::default().pricing(&model).output_per_1k_micro,
            Some(9)
        );
    }

    #[test]
    fn a_provider_specific_rule_set_can_override_pricing() {
        struct Flat;
        impl ProviderRules for Flat {
            fn supports_parameter(&self, _: &ModelInfoV1, _: &str) -> bool {
                true
            }
            fn supports_cache_control(&self, _: &ModelInfoV1, _: &str) -> bool {
                true
            }
            fn supports_dimensions(&self, _: &ModelInfoV1, _: u32) -> bool {
                true
            }
            fn unsupported_schema_construct(
                &self,
                _: &ModelInfoV1,
                _: &serde_json::Value,
            ) -> Option<String> {
                None
            }
            fn pricing(&self, _: &ModelInfoV1) -> ModelPricing {
                ModelPricing {
                    input_per_1k_micro: Some(42),
                    output_per_1k_micro: Some(43),
                }
            }
        }
        let registry = ProviderRulesRegistry::permissive().with_rules(OPENAI_TYPE, Arc::new(Flat));
        let model = sample_model_info(OPENAI_TYPE);
        assert_eq!(
            registry
                .rules_for(OPENAI_TYPE)
                .pricing(&model)
                .input_per_1k_micro,
            Some(42)
        );
    }
}
