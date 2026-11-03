//! Capability Checker (DESIGN §3.2): capabilities, parameter policy, tool
//! checks, structured output schema, request limits.

use std::sync::Arc;

use llm_gateway_sdk::{CreateResponseBody, LlmGatewayError};
use model_registry_sdk::ModelInfoV1;
use serde_json::json;

use super::{CapabilityChecker, EffectiveRequest};
use crate::config::LimitsConfig;
use crate::domain::ports::ResolvedPlugin;
use crate::domain::provider_rules::ProviderRulesRegistry;
use crate::domain::test_support::{
    CallLog, FakePlugin, FakeRules, OPENAI_TYPE, body, sample_model_info_patched,
};

const OTHER_TYPE: &str = "gts.cf.genai.model.info.v1~cf.genai._.other.v1~";

// ── builders ────────────────────────────────────────────────────────────────

fn model(patch: impl FnOnce(&mut serde_json::Value)) -> ModelInfoV1 {
    sample_model_info_patched(OPENAI_TYPE, patch)
}

fn plugin(tool_namespace: &str, web_search: bool) -> ResolvedPlugin {
    let fake = FakePlugin::echo_id(Arc::new(CallLog::default()));
    fake.set_web_search(web_search);
    ResolvedPlugin::new(fake, tool_namespace)
}

fn checker_with(limits: &LimitsConfig, rules: FakeRules) -> CapabilityChecker {
    CapabilityChecker::new(
        limits,
        Arc::new(ProviderRulesRegistry::permissive().with_rules(OPENAI_TYPE, Arc::new(rules))),
    )
}

fn check_with(
    limits: &LimitsConfig,
    rules: FakeRules,
    model: &ModelInfoV1,
    plugin: &ResolvedPlugin,
    request: CreateResponseBody,
) -> Result<EffectiveRequest, LlmGatewayError> {
    checker_with(limits, rules).check(model, plugin, request)
}

fn check(
    model: &ModelInfoV1,
    request: CreateResponseBody,
) -> Result<EffectiveRequest, LlmGatewayError> {
    check_with(
        &LimitsConfig::default(),
        FakeRules::default(),
        model,
        &plugin("openai", false),
        request,
    )
}

#[allow(clippy::needless_pass_by_value)] // call sites build the patch inline with `json!`
fn request(patch: serde_json::Value) -> CreateResponseBody {
    let mut value = serde_json::to_value(body("openai::gpt-4o")).expect("body serializes");
    for (key, field) in patch.as_object().expect("patch is an object") {
        value[key] = field.clone();
    }
    serde_json::from_value(value).expect("request deserializes")
}

fn err_of(result: Result<EffectiveRequest, LlmGatewayError>) -> LlmGatewayError {
    result.expect_err("check must fail")
}

fn assert_capability(err: &LlmGatewayError, expected: &str) {
    assert!(
        matches!(err, LlmGatewayError::CapabilityNotSupported { capability, .. } if capability == expected),
        "expected capability_not_supported({expected}), got {err:?}"
    );
}

fn image_input() -> serde_json::Value {
    json!([{ "type": "message", "role": "user", "content": [
        { "type": "input_text", "text": "what is this" },
        { "type": "input_image", "image_url": "https://example.com/a.png" }
    ]}])
}

fn file_input() -> serde_json::Value {
    json!([{ "type": "message", "role": "user", "content": [
        { "type": "input_file", "file_url": "https://example.com/a.pdf" }
    ]}])
}

fn function_tool(name: &str) -> serde_json::Value {
    json!({ "type": "function", "name": name, "parameters": { "type": "object" } })
}

fn schema_format(schema: &serde_json::Value) -> serde_json::Value {
    json!({ "format": { "type": "json_schema", "name": "out", "schema": schema, "strict": true } })
}

// ── baseline ────────────────────────────────────────────────────────────────

#[test]
fn plain_text_request_passes_unchanged() {
    let original = request(json!({}));
    let effective = check(&model(|_| {}), original.clone()).expect("plain request passes");
    assert_eq!(effective.body, original);
    assert!(effective.ignored_parameters.is_empty());
}

// ── capabilities ────────────────────────────────────────────────────────────

#[test]
fn model_without_completion_api_is_not_supported() {
    let embedding_only = model(|v| v["supported_api"] = json!(["embedding"]));
    assert_capability(
        &err_of(check(&embedding_only, request(json!({})))),
        "completion",
    );
}

#[test]
fn image_input_needs_vision() {
    let input = request(json!({ "input": image_input() }));

    assert_capability(&err_of(check(&model(|_| {}), input.clone())), "vision");

    let enabled = model(|v| v["capabilities"]["vision"]["enabled"] = json!(true));
    check(&enabled, input.clone()).expect("vision enabled");

    let disabled = model(|v| {
        v["capabilities"]["vision"]["enabled"] = json!(true);
        v["disabled_capabilities"]["vision"]["disabled"] = json!(true);
    });
    assert_capability(&err_of(check(&disabled, input)), "vision");
}

#[test]
fn file_input_needs_file_input() {
    let input = request(json!({ "input": file_input() }));

    assert_capability(&err_of(check(&model(|_| {}), input.clone())), "file_input");

    let enabled = model(|v| v["capabilities"]["file_input"]["enabled"] = json!(true));
    check(&enabled, input.clone()).expect("file input enabled");

    let disabled = model(|v| {
        v["capabilities"]["file_input"]["enabled"] = json!(true);
        v["disabled_capabilities"]["file_input"]["disabled"] = json!(true);
    });
    assert_capability(&err_of(check(&disabled, input)), "file_input");
}

#[test]
fn function_tools_need_function_calling() {
    let input = request(json!({ "tools": [function_tool("f")] }));

    assert_capability(
        &err_of(check(&model(|_| {}), input.clone())),
        "function_calling",
    );

    let enabled = model(|v| v["capabilities"]["function_calling"] = json!(true));
    check(&enabled, input.clone()).expect("function calling enabled");

    let disabled = model(|v| {
        v["capabilities"]["function_calling"] = json!(true);
        v["disabled_capabilities"]["function_calling"] = json!(true);
    });
    assert_capability(&err_of(check(&disabled, input)), "function_calling");
}

#[test]
fn web_search_needs_the_model_capability_and_the_plugin_capability() {
    let input = request(json!({ "tools": [{ "type": "cf_gears:web_search" }] }));
    let searching = model(|v| v["capabilities"]["web_search"]["enabled"] = json!(true));
    let limits = LimitsConfig::default();

    // Plugin without web search.
    let err = err_of(check_with(
        &limits,
        FakeRules::default(),
        &searching,
        &plugin("openai", false),
        input.clone(),
    ));
    assert_capability(&err, "web_search");

    // Model without web search.
    let err = err_of(check_with(
        &limits,
        FakeRules::default(),
        &model(|_| {}),
        &plugin("openai", true),
        input.clone(),
    ));
    assert_capability(&err, "web_search");

    // Both.
    check_with(
        &limits,
        FakeRules::default(),
        &searching,
        &plugin("openai", true),
        input.clone(),
    )
    .expect("model and plugin support web search");

    // Both, but administratively disabled.
    let disabled = model(|v| {
        v["capabilities"]["web_search"]["enabled"] = json!(true);
        v["disabled_capabilities"]["web_search"]["disabled"] = json!(true);
    });
    let err = err_of(check_with(
        &limits,
        FakeRules::default(),
        &disabled,
        &plugin("openai", true),
        input,
    ));
    assert_capability(&err, "web_search");
}

#[test]
fn reasoning_config_needs_reasoning() {
    let input = request(json!({ "reasoning": { "effort": "high" } }));

    assert_capability(&err_of(check(&model(|_| {}), input.clone())), "reasoning");

    let enabled = model(|v| v["capabilities"]["reasoning"]["effort"] = json!(true));
    check(&enabled, input.clone()).expect("reasoning enabled");

    let disabled = model(|v| {
        v["capabilities"]["reasoning"]["effort"] = json!(true);
        v["disabled_capabilities"]["reasoning"]["effort"] = json!(true);
    });
    assert_capability(&err_of(check(&disabled, input)), "reasoning");
}

#[test]
fn json_schema_output_needs_response_schema() {
    let input = request(json!({ "text": schema_format(&json!({ "type": "object" })) }));

    assert_capability(
        &err_of(check(&model(|_| {}), input.clone())),
        "response_schema",
    );

    let enabled = model(|v| v["capabilities"]["response_schema"] = json!(true));
    check(&enabled, input.clone()).expect("response schema enabled");

    let disabled = model(|v| {
        v["capabilities"]["response_schema"] = json!(true);
        v["disabled_capabilities"]["response_schema"] = json!(true);
    });
    assert_capability(&err_of(check(&disabled, input)), "response_schema");
}

// ── parameter policy ────────────────────────────────────────────────────────

fn with_defaults(allow_override: bool) -> ModelInfoV1 {
    model(move |v| {
        v["default_parameters"] = json!({ "temperature": 0.3, "max_output_tokens": 512 });
        v["allow_parameter_override"] = json!(allow_override);
    })
}

#[test]
fn model_defaults_fill_omitted_parameters() {
    for allow_override in [true, false] {
        let effective = check(&with_defaults(allow_override), request(json!({}))).expect("passes");
        assert_eq!(effective.body.temperature, Some(0.3));
        assert_eq!(effective.body.max_output_tokens, Some(512));
        assert!(
            effective.ignored_parameters.is_empty(),
            "defaults are not ignored values"
        );
    }
}

#[test]
fn request_value_wins_when_parameter_override_is_allowed() {
    let effective =
        check(&with_defaults(true), request(json!({ "temperature": 0.9 }))).expect("passes");
    assert_eq!(effective.body.temperature, Some(0.9));
    assert_eq!(effective.body.max_output_tokens, Some(512));
    assert!(effective.ignored_parameters.is_empty());
}

#[test]
fn default_is_kept_and_listed_when_parameter_override_is_not_allowed() {
    let effective = check(
        &with_defaults(false),
        request(json!({ "temperature": 0.9 })),
    )
    .expect("passes");
    assert_eq!(effective.body.temperature, Some(0.3));
    assert_eq!(effective.ignored_parameters, vec!["temperature".to_owned()]);
}

#[test]
fn request_value_without_a_model_default_is_kept_even_without_override() {
    let effective = check(&with_defaults(false), request(json!({ "top_p": 0.5 }))).expect("passes");
    assert_eq!(effective.body.top_p, Some(0.5));
    assert!(effective.ignored_parameters.is_empty());
}

#[test]
fn unsupported_parameter_is_rejected_naming_it() {
    let rules = FakeRules {
        unsupported_params: ["top_p"].into(),
        ..FakeRules::default()
    };
    let err = err_of(check_with(
        &LimitsConfig::default(),
        rules,
        &model(|_| {}),
        &plugin("openai", false),
        request(json!({ "temperature": 0.5, "top_p": 0.5 })),
    ));
    assert!(
        matches!(&err, LlmGatewayError::UnsupportedParameter { param, .. } if param == "top_p"),
        "{err:?}"
    );
}

#[test]
fn unsupported_parameter_is_dropped_and_listed_with_drop_unsupported_params() {
    let rules = FakeRules {
        unsupported_params: ["top_p"].into(),
        ..FakeRules::default()
    };
    let effective = check_with(
        &LimitsConfig::default(),
        rules,
        &model(|_| {}),
        &plugin("openai", false),
        request(json!({ "temperature": 0.5, "top_p": 0.5, "drop_unsupported_params": true })),
    )
    .expect("dropped, not rejected");
    assert_eq!(effective.body.top_p, None);
    assert_eq!(effective.body.temperature, Some(0.5));
    assert_eq!(effective.ignored_parameters, vec!["top_p".to_owned()]);
}

#[test]
fn unsupported_parameter_that_is_not_sent_is_no_problem() {
    let rules = FakeRules {
        unsupported_params: ["top_p"].into(),
        ..FakeRules::default()
    };
    let effective = check_with(
        &LimitsConfig::default(),
        rules,
        &model(|_| {}),
        &plugin("openai", false),
        request(json!({ "temperature": 0.5 })),
    )
    .expect("passes");
    assert!(effective.ignored_parameters.is_empty());
}

#[test]
fn unsupported_cache_control_hint_is_always_dropped_and_listed() {
    let rules = FakeRules {
        unsupported_hints: ["prompt_cache_key"].into(),
        ..FakeRules::default()
    };
    let effective = check_with(
        &LimitsConfig::default(),
        rules,
        &model(|_| {}),
        &plugin("openai", false),
        request(json!({ "prompt_cache_key": "k1" })),
    )
    .expect("a cache-control hint is never an error");
    assert_eq!(effective.body.prompt_cache_key, None);
    assert_eq!(
        effective.ignored_parameters,
        vec!["prompt_cache_key".to_owned()]
    );
}

#[test]
fn supported_cache_control_hint_is_kept() {
    let effective =
        check(&model(|_| {}), request(json!({ "prompt_cache_key": "k1" }))).expect("passes");
    assert_eq!(effective.body.prompt_cache_key.as_deref(), Some("k1"));
    assert!(effective.ignored_parameters.is_empty());
}

#[test]
fn extra_fields_are_allowed_only_when_the_model_lists_them() {
    let allowing = model(|v| v["allow_extra_params"] = json!(["openai:web_search_options"]));
    let allowed = request(json!({ "extra_fields": { "openai:web_search_options": { "k": 1 } } }));
    let effective = check(&allowing, allowed).expect("listed extra field is allowed");
    assert!(
        effective
            .body
            .extra_fields
            .as_ref()
            .is_some_and(|m| m.contains_key("openai:web_search_options"))
    );

    let err = err_of(check(
        &model(|_| {}),
        request(json!({ "extra_fields": { "foo": 1 } })),
    ));
    assert!(
        matches!(&err, LlmGatewayError::UnsupportedParameter { param, .. } if param == "foo"),
        "{err:?}"
    );
}

#[test]
fn unlisted_extra_fields_are_dropped_and_listed_with_drop_unsupported_params() {
    let allowing = model(|v| v["allow_extra_params"] = json!(["kept"]));
    let effective = check(
        &allowing,
        request(
            json!({ "extra_fields": { "kept": 1, "foo": 2 }, "drop_unsupported_params": true }),
        ),
    )
    .expect("dropped, not rejected");
    let extras = effective.body.extra_fields.expect("allowed key survives");
    assert!(extras.contains_key("kept"));
    assert!(!extras.contains_key("foo"));
    assert_eq!(effective.ignored_parameters, vec!["foo".to_owned()]);
}

#[test]
fn ignored_parameters_are_sorted_and_free_of_duplicates() {
    let rules = FakeRules {
        unsupported_params: ["top_p"].into(),
        unsupported_hints: ["prompt_cache_key"].into(),
        ..FakeRules::default()
    };
    let overriding_off = model(|v| {
        v["default_parameters"] = json!({ "temperature": 0.3 });
        v["allow_parameter_override"] = json!(false);
    });
    let effective = check_with(
        &LimitsConfig::default(),
        rules,
        &overriding_off,
        &plugin("openai", false),
        request(json!({
            "temperature": 0.9,
            "top_p": 0.5,
            "prompt_cache_key": "k",
            "drop_unsupported_params": true
        })),
    )
    .expect("passes");
    assert_eq!(
        effective.ignored_parameters,
        vec![
            "prompt_cache_key".to_owned(),
            "temperature".to_owned(),
            "top_p".to_owned()
        ]
    );
}

// ── tools ───────────────────────────────────────────────────────────────────

fn calling_model() -> ModelInfoV1 {
    model(|v| v["capabilities"]["function_calling"] = json!(true))
}

#[test]
fn tool_choice_may_name_a_function_in_the_tool_set() {
    let input = request(json!({
        "tools": [function_tool("get_weather")],
        "tool_choice": { "type": "function", "name": "get_weather" }
    }));
    check(&calling_model(), input).expect("named function exists");
}

#[test]
fn tool_choice_naming_an_absent_function_is_a_validation_error() {
    let input = request(json!({
        "tools": [function_tool("get_weather")],
        "tool_choice": { "type": "function", "name": "get_time" }
    }));
    let err = err_of(check(&calling_model(), input));
    assert!(
        matches!(&err, LlmGatewayError::Validation { param, .. } if param.as_deref() == Some("tool_choice")),
        "{err:?}"
    );
}

#[test]
fn tool_choice_naming_a_function_without_any_tools_is_a_validation_error() {
    let input = request(json!({ "tool_choice": { "type": "function", "name": "get_time" } }));
    let err = err_of(check(&calling_model(), input));
    assert!(matches!(err, LlmGatewayError::Validation { .. }), "{err:?}");
}

#[test]
fn provider_tool_with_the_plugin_namespace_passes() {
    let input = request(json!({ "tools": [{ "type": "openai:shell", "env": "default" }] }));
    let effective = check(&model(|_| {}), input.clone()).expect("namespace matches");
    assert_eq!(effective.body.tools, input.tools);
}

#[test]
fn provider_tool_with_a_foreign_namespace_is_a_validation_error() {
    let input = request(json!({ "tools": [{ "type": "anthropic:bash" }] }));
    let err = err_of(check(&model(|_| {}), input));
    assert!(matches!(err, LlmGatewayError::Validation { .. }), "{err:?}");
}

// ── structured output schema ────────────────────────────────────────────────

fn schema_model() -> ModelInfoV1 {
    model(|v| v["capabilities"]["response_schema"] = json!(true))
}

#[test]
fn schema_not_expressible_in_the_provider_subset_is_unsupported_schema() {
    let rules = FakeRules {
        unsupported_construct: Some("oneOf"),
        ..FakeRules::default()
    };
    let err = err_of(check_with(
        &LimitsConfig::default(),
        rules,
        &schema_model(),
        &plugin("openai", false),
        request(json!({ "text": schema_format(&json!({ "oneOf": [{ "type": "string" }] })) })),
    ));
    assert!(
        matches!(&err, LlmGatewayError::UnsupportedSchema { construct, .. } if construct == "oneOf"),
        "{err:?}"
    );
}

#[test]
fn provider_without_rules_accepts_any_schema() {
    // The OPENAI_TYPE rules reject every schema; a provider type without
    // rules falls back to the permissive default.
    let rules = FakeRules {
        unsupported_construct: Some("oneOf"),
        ..FakeRules::default()
    };
    let other = sample_model_info_patched(OTHER_TYPE, |v| {
        v["capabilities"]["response_schema"] = json!(true);
    });
    check_with(
        &LimitsConfig::default(),
        rules,
        &other,
        &plugin("openai", false),
        request(json!({ "text": schema_format(&json!({ "oneOf": [{ "type": "string" }] })) })),
    )
    .expect("permissive default accepts the schema");
}

// ── request limits ──────────────────────────────────────────────────────────

fn limits(patch: impl FnOnce(&mut LimitsConfig)) -> LimitsConfig {
    let mut limits = LimitsConfig::default();
    patch(&mut limits);
    limits
}

fn check_limited(
    limits: &LimitsConfig,
    model: &ModelInfoV1,
    input: CreateResponseBody,
) -> Result<EffectiveRequest, LlmGatewayError> {
    check_with(
        limits,
        FakeRules::default(),
        model,
        &plugin("openai", false),
        input,
    )
}

fn message_items(count: usize) -> serde_json::Value {
    let items: Vec<_> = (0..count)
        .map(|i| json!({ "type": "message", "role": "user", "content": format!("m{i}") }))
        .collect();
    json!(items)
}

fn assert_validation(err: &LlmGatewayError, expected_param: &str) {
    assert!(
        matches!(err, LlmGatewayError::Validation { param, .. } if param.as_deref() == Some(expected_param)),
        "expected validation_error(param={expected_param}), got {err:?}"
    );
}

#[test]
fn too_many_input_items_is_a_validation_error() {
    let small = limits(|l| l.max_input_items = 2);
    let model = model(|_| {});

    check_limited(
        &small,
        &model,
        request(json!({ "input": message_items(2) })),
    )
    .expect("at the limit");
    let err = err_of(check_limited(
        &small,
        &model,
        request(json!({ "input": message_items(3) })),
    ));
    assert_validation(&err, "input");
}

#[test]
fn too_many_tools_is_a_validation_error() {
    let small = limits(|l| l.max_tools = 1);
    let model = model(|_| {});
    let shell = json!({ "type": "openai:shell" });

    check_limited(&small, &model, request(json!({ "tools": [shell] }))).expect("at the limit");
    let err = err_of(check_limited(
        &small,
        &model,
        request(json!({ "tools": [shell, shell] })),
    ));
    assert_validation(&err, "tools");
}

#[test]
fn oversized_function_schema_is_a_validation_error() {
    let small = limits(|l| l.max_schema_bytes = 64);
    let big = json!({
        "type": "function", "name": "f",
        "parameters": { "type": "object", "description": "x".repeat(200) }
    });
    let err = err_of(check_limited(
        &small,
        &calling_model(),
        request(json!({ "tools": [big] })),
    ));
    assert_validation(&err, "tools");
}

#[test]
fn oversized_output_schema_is_a_validation_error() {
    let small = limits(|l| l.max_schema_bytes = 64);
    let big = json!({ "type": "object", "description": "x".repeat(200) });
    let err = err_of(check_limited(
        &small,
        &schema_model(),
        request(json!({ "text": schema_format(&big) })),
    ));
    assert_validation(&err, "text");
}

#[test]
fn empty_plugin_namespace_serves_no_provider_tool() {
    let input = request(json!({ "tools": [{ "type": ":x" }] }));
    let result = check_with(
        &LimitsConfig::default(),
        FakeRules::default(),
        &model(|_| {}),
        &plugin("", false),
        input,
    );
    let err = err_of(result);
    assert_validation(&err, "tools");
}

#[test]
fn duplicate_function_tool_names_are_a_validation_error() {
    let input = request(json!({
        "tools": [function_tool("get_weather"), function_tool("get_weather")]
    }));
    let err = err_of(check(&calling_model(), input));
    assert_validation(&err, "tools");
}

#[test]
fn tool_defaults_are_not_applied_without_tools() {
    let model = model(|v| {
        v["capabilities"]["function_calling"] = json!(true);
        v["default_parameters"] = json!({
            "tool_choice": { "type": "function", "name": "get_weather" },
            "parallel_tool_calls": true,
            "max_tool_calls": 3
        });
    });
    let effective = check(&model, request(json!({}))).expect("passes");
    assert_eq!(effective.body.tool_choice, None);
    assert_eq!(effective.body.parallel_tool_calls, None);
    assert_eq!(effective.body.max_tool_calls, None);
}
