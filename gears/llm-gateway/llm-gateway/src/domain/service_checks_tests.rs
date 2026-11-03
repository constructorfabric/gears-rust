//! Slice 2 pipeline tests: Tool Resolver, Capability Checker and Output
//! Validator inside `create_response` (DESIGN §3.2 ordering, §4.1 codes).

use llm_gateway_sdk::{CreateResponseBody, LlmGatewayError, Tool};
use serde_json::json;

use crate::domain::test_support::{
    FakeModels, FakePlugin, FakeRules, Harness, OPENAI_TYPE, body, ctx, sample_model_info_patched,
    sample_response, sample_response_with_text,
};

const WEATHER: &str = "gts.cf.test.tools.weather.v1~";
const SENTINEL: &str = "SENTINEL-VALUE-5d2e";

#[allow(clippy::needless_pass_by_value)] // call sites build the patch inline with `json!`
fn request(patch: serde_json::Value) -> CreateResponseBody {
    let mut value = serde_json::to_value(body("openai::gpt-4o")).expect("body serializes");
    for (key, field) in patch.as_object().expect("patch is an object") {
        value[key] = field.clone();
    }
    serde_json::from_value(value).expect("request deserializes")
}

/// Serve a model patched by `patch`.
fn serve_model(h: &mut Harness, patch: impl FnOnce(&mut serde_json::Value)) {
    h.models = FakeModels::found(sample_model_info_patched(OPENAI_TYPE, patch), h.log.clone());
}

fn weather_schema() -> serde_json::Value {
    json!({ "title": "get_weather", "type": "object", "properties": { "city": { "type": "string" } } })
}

fn function_calling(v: &mut serde_json::Value) {
    v["capabilities"]["function_calling"] = json!(true);
}

fn response_schema(v: &mut serde_json::Value) {
    v["capabilities"]["response_schema"] = json!(true);
}

// ── ordering ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tools_are_resolved_after_plugin_selection_and_before_the_broker() {
    let mut h = Harness::new();
    serve_model(&mut h, function_calling);
    h.tool_schemas.insert(WEATHER, weather_schema());

    h.service()
        .create_response(
            &ctx(),
            request(json!({ "tools": [{ "type": "reference", "schema_id": WEATHER }] })),
        )
        .await
        .expect("succeeds");

    assert_eq!(
        h.log.entries(),
        vec![
            "authz",
            "models",
            "plugins",
            "tool_schemas",
            "credentials",
            "plugin"
        ]
    );
}

#[tokio::test]
async fn the_plugin_receives_unified_function_tools() {
    let mut h = Harness::new();
    serve_model(&mut h, function_calling);
    h.tool_schemas.insert(WEATHER, weather_schema());

    h.service()
        .create_response(
            &ctx(),
            request(json!({ "tools": [{ "type": "reference", "schema_id": WEATHER }] })),
        )
        .await
        .expect("succeeds");

    let sent = h.plugin.bodies();
    let tools = sent[0].tools.as_ref().expect("tools reach the plugin");
    assert!(
        matches!(&tools[0], Tool::Function(f) if f.name == "get_weather"),
        "{tools:?}"
    );
}

#[tokio::test]
async fn unresolvable_tool_schema_stops_before_the_broker_and_the_plugin() {
    let mut h = Harness::new();
    serve_model(&mut h, function_calling);

    let err = h
        .service()
        .create_response(
            &ctx(),
            request(json!({ "tools": [{ "type": "reference", "schema_id": WEATHER }] })),
        )
        .await
        .expect_err("unknown schema");

    assert!(
        matches!(&err, LlmGatewayError::SchemaNotFound { schema_id, .. } if schema_id == WEATHER),
        "{err:?}"
    );
    assert_eq!(
        h.log.entries(),
        vec!["authz", "models", "plugins", "tool_schemas"]
    );
}

#[tokio::test]
async fn tool_choice_may_name_a_function_from_a_gts_reference() {
    let mut h = Harness::new();
    serve_model(&mut h, function_calling);
    h.tool_schemas.insert(WEATHER, weather_schema());

    h.service()
        .create_response(
            &ctx(),
            request(json!({
                "tools": [{ "type": "reference", "schema_id": WEATHER }],
                "tool_choice": { "type": "function", "name": "get_weather" }
            })),
        )
        .await
        .expect("resolution precedes the tool_choice check");
}

#[tokio::test]
async fn gateway_and_provider_tools_cause_no_type_registry_call() {
    let mut h = Harness::new();
    serve_model(&mut h, |v| {
        function_calling(v);
        v["capabilities"]["web_search"]["enabled"] = json!(true);
    });
    h.plugin.set_web_search(true);

    h.service()
        .create_response(
            &ctx(),
            request(json!({ "tools": [
                function_tool_json("f"),
                { "type": "cf_gears:web_search" },
                { "type": "openai:shell" }
            ]})),
        )
        .await
        .expect("succeeds");

    assert_eq!(h.tool_schemas.calls(), 0);
    assert_eq!(h.plugin.bodies()[0].tools.as_ref().map(Vec::len), Some(3));
}

fn function_tool_json(name: &str) -> serde_json::Value {
    json!({ "type": "function", "name": name, "parameters": { "type": "object" } })
}

// ── every check fails before the broker and the plugin ──────────────────────

type Setup = fn(&mut Harness) -> CreateResponseBody;

fn image_without_vision(_: &mut Harness) -> CreateResponseBody {
    request(
        json!({ "input": [{ "type": "message", "role": "user", "content": [
            { "type": "input_image", "image_url": "https://example.com/a.png" }
        ]}]}),
    )
}

fn rejected_parameter(h: &mut Harness) -> CreateResponseBody {
    h.rules = FakeRules {
        unsupported_params: ["temperature"].into(),
        ..FakeRules::default()
    };
    request(json!({ "temperature": 0.5 }))
}

fn inexpressible_schema(h: &mut Harness) -> CreateResponseBody {
    serve_model(h, response_schema);
    h.rules = FakeRules {
        unsupported_construct: Some("oneOf"),
        ..FakeRules::default()
    };
    request(
        json!({ "text": { "format": { "type": "json_schema", "name": "o", "schema": { "type": "object" }, "strict": true } } }),
    )
}

fn unknown_tool_choice(h: &mut Harness) -> CreateResponseBody {
    serve_model(h, function_calling);
    request(
        json!({ "tools": [function_tool_json("a")], "tool_choice": { "type": "function", "name": "b" } }),
    )
}

fn foreign_provider_tool(h: &mut Harness) -> CreateResponseBody {
    h.set_tool_namespace("anthropic");
    request(json!({ "tools": [{ "type": "openai:shell" }] }))
}

fn too_many_input_items(h: &mut Harness) -> CreateResponseBody {
    h.limits.max_input_items = 1;
    request(json!({ "input": [
        { "type": "message", "role": "user", "content": "a" },
        { "type": "message", "role": "user", "content": "b" }
    ]}))
}

fn too_many_tools(h: &mut Harness) -> CreateResponseBody {
    h.limits.max_tools = 1;
    request(json!({ "tools": [{ "type": "openai:shell" }, { "type": "openai:shell" }] }))
}

fn oversized_schema(h: &mut Harness) -> CreateResponseBody {
    serve_model(h, response_schema);
    h.limits.max_schema_bytes = 16;
    request(
        json!({ "text": { "format": { "type": "json_schema", "name": "o", "schema": { "type": "object", "description": "long description" }, "strict": true } } }),
    )
}

#[tokio::test]
async fn every_check_failure_happens_before_the_broker_and_the_plugin() {
    let cases: [(&str, &str, Setup); 8] = [
        (
            "capability_not_supported",
            "image_without_vision",
            image_without_vision,
        ),
        (
            "unsupported_parameter",
            "rejected_parameter",
            rejected_parameter,
        ),
        (
            "unsupported_schema",
            "inexpressible_schema",
            inexpressible_schema,
        ),
        (
            "validation_error",
            "unknown_tool_choice",
            unknown_tool_choice,
        ),
        (
            "validation_error",
            "foreign_provider_tool",
            foreign_provider_tool,
        ),
        (
            "validation_error",
            "too_many_input_items",
            too_many_input_items,
        ),
        ("validation_error", "too_many_tools", too_many_tools),
        ("validation_error", "oversized_schema", oversized_schema),
    ];
    for (code, name, setup) in cases {
        let mut h = Harness::new();
        let input = setup(&mut h);

        let err = h
            .service()
            .create_response(&ctx(), input)
            .await
            .expect_err(name);

        assert_eq!(err.code(), code, "{name}: {err:?}");
        assert_eq!(
            h.log.entries(),
            vec!["authz", "models", "plugins"],
            "{name}"
        );
        assert_eq!(h.broker.calls(), 0, "{name}");
        assert_eq!(h.plugin.calls(), 0, "{name}");
    }
}

// ── effective request and ignored_parameters ────────────────────────────────

#[tokio::test]
async fn the_plugin_receives_the_effective_request_with_model_defaults() {
    let mut h = Harness::new();
    serve_model(&mut h, |v| {
        v["default_parameters"] = json!({ "temperature": 0.3 });
    });

    h.service()
        .create_response(&ctx(), request(json!({})))
        .await
        .expect("succeeds");

    assert_eq!(h.plugin.bodies()[0].temperature, Some(0.3));
}

#[tokio::test]
async fn dropped_and_overridden_parameters_are_listed_on_the_response() {
    let mut h = Harness::new();
    serve_model(&mut h, |v| {
        v["default_parameters"] = json!({ "temperature": 0.3 });
        v["allow_parameter_override"] = json!(false);
    });
    h.rules = FakeRules {
        unsupported_params: ["top_p"].into(),
        ..FakeRules::default()
    };

    let response = h
        .service()
        .create_response(
            &ctx(),
            request(json!({ "temperature": 0.9, "top_p": 0.4, "drop_unsupported_params": true })),
        )
        .await
        .expect("succeeds");

    assert_eq!(
        response.ignored_parameters,
        vec!["temperature".to_owned(), "top_p".to_owned()]
    );
    let sent = &h.plugin.bodies()[0];
    assert_eq!(sent.temperature, Some(0.3));
    assert_eq!(sent.top_p, None);
}

#[tokio::test]
async fn a_request_without_ignored_parameters_returns_an_empty_list() {
    let h = Harness::new();
    let response = h
        .service()
        .create_response(&ctx(), body("m"))
        .await
        .expect("succeeds");
    assert!(response.ignored_parameters.is_empty());
}

#[tokio::test]
async fn plugin_reported_ignored_parameters_are_merged_without_duplicates() {
    let mut h = Harness::new();
    serve_model(&mut h, |v| {
        v["default_parameters"] = json!({ "temperature": 0.3 });
        v["allow_parameter_override"] = json!(false);
    });
    h.set_plugin(FakePlugin::with(
        |c| {
            let mut resource = sample_response(&c.request_id, "gpt-4o");
            resource.ignored_parameters = vec!["temperature".to_owned(), "metadata".to_owned()];
            Ok(resource)
        },
        h.log.clone(),
        false,
    ));

    let response = h
        .service()
        .create_response(&ctx(), request(json!({ "temperature": 0.9 })))
        .await
        .expect("succeeds");

    assert_eq!(
        response.ignored_parameters,
        vec!["metadata".to_owned(), "temperature".to_owned()]
    );
}

// ── structured output validation ────────────────────────────────────────────

fn json_schema_request() -> CreateResponseBody {
    request(json!({ "text": { "format": {
        "type": "json_schema", "name": "person", "strict": true,
        "schema": { "type": "object", "properties": { "age": { "type": "integer" } }, "required": ["age"] }
    }}}))
}

fn plugin_answering(h: &mut Harness, text: &'static str) {
    h.set_plugin(FakePlugin::with(
        move |c| Ok(sample_response_with_text(&c.request_id, "gpt-4o", text)),
        h.log.clone(),
        false,
    ));
}

#[tokio::test]
async fn valid_structured_output_is_returned_unchanged() {
    let mut h = Harness::new();
    serve_model(&mut h, response_schema);
    plugin_answering(&mut h, r#"{"age":36}"#);

    let response = h
        .service()
        .create_response(&ctx(), json_schema_request())
        .await
        .expect("valid");

    let expected = sample_response_with_text(&response.id, "gpt-4o", r#"{"age":36}"#);
    assert_eq!(response, expected);
}

#[tokio::test]
async fn invalid_structured_output_is_an_output_validation_error_without_content() {
    let mut h = Harness::new();
    serve_model(&mut h, response_schema);
    h.set_plugin(FakePlugin::with(
        |c| {
            let text = json!({ "age": SENTINEL }).to_string();
            Ok(sample_response_with_text(&c.request_id, "gpt-4o", &text))
        },
        h.log.clone(),
        false,
    ));

    let err = h
        .service()
        .create_response(&ctx(), json_schema_request())
        .await
        .expect_err("invalid");

    let LlmGatewayError::OutputValidation { message, .. } = &err else {
        panic!("expected output_validation_error, got {err:?}");
    };
    assert!(message.contains("/age"), "{message}");
    assert!(!message.contains(SENTINEL), "content leaked: {message}");
    assert_eq!(
        h.plugin.calls(),
        1,
        "validation happens after the provider call"
    );
}

#[tokio::test]
async fn non_json_structured_output_is_an_output_validation_error() {
    let mut h = Harness::new();
    serve_model(&mut h, response_schema);
    plugin_answering(&mut h, "definitely not json");

    let err = h
        .service()
        .create_response(&ctx(), json_schema_request())
        .await
        .expect_err("not JSON");
    assert!(
        matches!(err, LlmGatewayError::OutputValidation { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn free_text_responses_are_not_validated() {
    let mut h = Harness::new();
    plugin_answering(&mut h, "plain words, not json");

    let response = h
        .service()
        .create_response(&ctx(), request(json!({})))
        .await
        .expect("not validated");

    assert_eq!(response.output.len(), 1);
}

// ── tool count guard ────────────────────────────────────────────────────────

#[tokio::test]
async fn too_many_tools_is_rejected_before_any_type_registry_call() {
    let mut h = Harness::new();
    serve_model(&mut h, function_calling);
    h.limits.max_tools = 1;
    h.tool_schemas.insert(WEATHER, weather_schema());

    let err = h
        .service()
        .create_response(
            &ctx(),
            request(json!({ "tools": [
                { "type": "reference", "schema_id": WEATHER },
                { "type": "reference", "schema_id": WEATHER }
            ]})),
        )
        .await
        .expect_err("over the tool limit");

    assert!(
        matches!(&err, LlmGatewayError::Validation { param, .. } if param.as_deref() == Some("tools")),
        "{err:?}"
    );
    assert_eq!(h.tool_schemas.calls(), 0);
    assert_eq!(h.broker.calls(), 0);
    assert_eq!(h.plugin.calls(), 0);
}
