//! Tool Resolver (DESIGN §3.2, `cpt-cf-llm-gateway-seq-tool-calling-v1`).

use std::sync::Arc;

use llm_gateway_sdk::{LlmGatewayError, Tool};
use serde_json::json;
use uuid::Uuid;

use super::ToolResolver;
use crate::domain::test_support::{CallLog, FakeToolSchemas, ctx};

const WEATHER: &str = "gts.cf.test.tools.weather.v1~";
const ADDRESS: &str = "gts.cf.test.tools.address.v1~";
const GEO: &str = "gts.cf.test.tools.geo.v1~";

fn tool(value: serde_json::Value) -> Tool {
    serde_json::from_value(value).expect("tool deserializes")
}

fn setup() -> (Arc<FakeToolSchemas>, ToolResolver) {
    let source = FakeToolSchemas::new(Arc::new(CallLog::default()));
    let resolver = ToolResolver::new(source.clone(), 262_144);
    (source, resolver)
}

fn weather_schema() -> serde_json::Value {
    json!({
        "title": "get_weather",
        "description": "Current weather",
        "type": "object",
        "properties": { "city": { "type": "string" } },
        "required": ["city"]
    })
}

fn function_of(tool: &Tool) -> &llm_gateway_sdk::FunctionTool {
    let Tool::Function(function) = tool else {
        panic!("expected a function tool, got {tool:?}");
    };
    function
}

#[tokio::test]
async fn reference_tool_resolves_to_a_function_tool() {
    let (source, resolver) = setup();
    source.insert(WEATHER, weather_schema());

    let got = resolver
        .resolve(
            &ctx(),
            vec![tool(json!({ "type": "reference", "schema_id": WEATHER }))],
        )
        .await
        .expect("reference resolves");

    assert_eq!(got.len(), 1);
    let function = function_of(&got[0]);
    assert_eq!(function.name, "get_weather");
    assert_eq!(function.description.as_deref(), Some("Current weather"));
    assert_eq!(function.parameters.as_ref(), Some(&weather_schema()));
    assert_eq!(function.strict, None);
}

#[tokio::test]
async fn schemas_are_requested_under_the_caller_context() {
    let (source, resolver) = setup();
    source.insert(WEATHER, weather_schema());

    resolver
        .resolve(
            &ctx(),
            vec![tool(json!({ "type": "reference", "schema_id": WEATHER }))],
        )
        .await
        .expect("reference resolves");

    assert_eq!(source.requested(), vec![WEATHER.to_owned()]);
    assert_eq!(source.subjects(), vec![Uuid::from_u128(1)]);
}

#[tokio::test]
async fn inline_gts_tool_resolves_to_a_function_tool() {
    let (source, resolver) = setup();

    let got = resolver
        .resolve(
            &ctx(),
            vec![tool(
                json!({ "type": "inline_gts", "schema": { "json_schema": weather_schema() } }),
            )],
        )
        .await
        .expect("inline schema resolves");

    let function = function_of(&got[0]);
    assert_eq!(function.name, "get_weather");
    assert_eq!(function.parameters.as_ref(), Some(&weather_schema()));
    assert_eq!(source.calls(), 0, "no GTS `$ref`, nothing to fetch");
}

#[tokio::test]
async fn inline_gts_refs_to_other_gts_schemas_are_inlined() {
    let (source, resolver) = setup();
    source.insert(
        ADDRESS,
        json!({ "type": "object", "properties": { "zip": { "type": "string" } } }),
    );
    let inline = json!({
        "title": "ship",
        "type": "object",
        "properties": {
            "to": { "$ref": format!("gts://{ADDRESS}") },
            "local": { "$ref": "#/$defs/note" }
        },
        "$defs": { "note": { "type": "string" } }
    });

    let got = resolver
        .resolve(
            &ctx(),
            vec![tool(
                json!({ "type": "inline_gts", "schema": { "json_schema": inline } }),
            )],
        )
        .await
        .expect("inline schema resolves");

    let parameters = function_of(&got[0]).parameters.clone().expect("parameters");
    assert_eq!(
        parameters["properties"]["to"],
        json!({ "type": "object", "properties": { "zip": { "type": "string" } } })
    );
    assert_eq!(
        parameters["properties"]["local"],
        json!({ "$ref": "#/$defs/note" }),
        "non-GTS refs are left untouched"
    );
    assert_eq!(source.requested(), vec![ADDRESS.to_owned()]);
}

#[tokio::test]
async fn refs_inside_referenced_schemas_are_resolved_recursively() {
    let (source, resolver) = setup();
    source.insert(
        WEATHER,
        json!({ "title": "get_weather", "type": "object", "properties": { "at": { "$ref": format!("gts://{GEO}") } } }),
    );
    source.insert(
        GEO,
        json!({ "type": "object", "properties": { "lat": { "type": "number" } } }),
    );

    let got = resolver
        .resolve(
            &ctx(),
            vec![tool(json!({ "type": "reference", "schema_id": WEATHER }))],
        )
        .await
        .expect("nested refs resolve");

    let parameters = function_of(&got[0]).parameters.clone().expect("parameters");
    assert_eq!(
        parameters["properties"]["at"]["properties"]["lat"],
        json!({ "type": "number" })
    );
}

#[tokio::test]
async fn cyclic_gts_refs_are_a_validation_error() {
    let (source, resolver) = setup();
    source.insert(
        GEO,
        json!({ "title": "geo", "properties": { "again": { "$ref": format!("gts://{GEO}") } } }),
    );

    let err = resolver
        .resolve(
            &ctx(),
            vec![tool(json!({ "type": "reference", "schema_id": GEO }))],
        )
        .await
        .expect_err("cycle");

    assert!(matches!(err, LlmGatewayError::Validation { .. }), "{err:?}");
}

#[tokio::test]
async fn unresolvable_reference_is_schema_not_found_with_the_schema_id() {
    let (_source, resolver) = setup();

    let err = resolver
        .resolve(
            &ctx(),
            vec![tool(json!({ "type": "reference", "schema_id": WEATHER }))],
        )
        .await
        .expect_err("unknown schema");

    assert!(
        matches!(&err, LlmGatewayError::SchemaNotFound { schema_id, .. } if schema_id == WEATHER),
        "{err:?}"
    );
}

#[tokio::test]
async fn unresolvable_ref_inside_an_inline_schema_is_schema_not_found() {
    let (_source, resolver) = setup();
    let inline =
        json!({ "title": "t", "properties": { "x": { "$ref": format!("gts://{ADDRESS}") } } });

    let err = resolver
        .resolve(
            &ctx(),
            vec![tool(
                json!({ "type": "inline_gts", "schema": { "json_schema": inline } }),
            )],
        )
        .await
        .expect_err("unknown nested schema");

    assert!(
        matches!(&err, LlmGatewayError::SchemaNotFound { schema_id, .. } if schema_id == ADDRESS),
        "{err:?}"
    );
}

#[tokio::test]
async fn function_gateway_and_provider_tools_pass_through_untouched() {
    let (source, resolver) = setup();
    let tools = vec![
        tool(
            json!({ "type": "function", "name": "f", "parameters": { "type": "object", "$ref": "gts://gts.cf.x.y.z.v1~" } }),
        ),
        tool(json!({ "type": "cf_gears:web_search", "allowed_domains": ["docs.rs"] })),
        tool(json!({ "type": "openai:shell", "env": "default" })),
    ];

    let got = resolver
        .resolve(&ctx(), tools.clone())
        .await
        .expect("pass-through");

    assert_eq!(got, tools);
    assert_eq!(source.calls(), 0, "no Type Registry call without GTS tools");
}

#[tokio::test]
async fn mixed_tools_keep_their_order() {
    let (source, resolver) = setup();
    source.insert(WEATHER, weather_schema());
    let shell = tool(json!({ "type": "openai:shell" }));
    let function = tool(json!({ "type": "function", "name": "f" }));

    let got = resolver
        .resolve(
            &ctx(),
            vec![
                shell.clone(),
                tool(json!({ "type": "reference", "schema_id": WEATHER })),
                function.clone(),
            ],
        )
        .await
        .expect("mixed tools resolve");

    assert_eq!(got.len(), 3);
    assert_eq!(got[0], shell);
    assert_eq!(function_of(&got[1]).name, "get_weather");
    assert_eq!(got[2], function);
}

#[tokio::test]
async fn empty_tool_list_resolves_to_empty() {
    let (source, resolver) = setup();
    let got = resolver.resolve(&ctx(), Vec::new()).await.expect("empty");
    assert!(got.is_empty());
    assert_eq!(source.calls(), 0);
}

#[tokio::test]
async fn schema_without_title_is_named_after_its_sanitized_gts_id() {
    let (source, resolver) = setup();
    source.insert(WEATHER, json!({ "type": "object" }));

    let got = resolver
        .resolve(
            &ctx(),
            vec![tool(json!({ "type": "reference", "schema_id": WEATHER }))],
        )
        .await
        .expect("reference resolves");

    assert_eq!(function_of(&got[0]).name, "gts_cf_test_tools_weather_v1_");
}

#[tokio::test]
async fn sanitized_name_is_cut_to_64_characters() {
    let (source, resolver) = setup();
    let long_id = format!("gts.cf.test.tools.{}.v1~", "x".repeat(80));
    source.insert(&long_id, json!({ "type": "object" }));

    let got = resolver
        .resolve(
            &ctx(),
            vec![tool(json!({ "type": "reference", "schema_id": long_id }))],
        )
        .await
        .expect("reference resolves");

    assert_eq!(function_of(&got[0]).name.len(), 64);
}

#[tokio::test]
async fn inline_schema_without_title_or_id_is_a_validation_error() {
    let (_source, resolver) = setup();

    let err = resolver
        .resolve(
            &ctx(),
            vec![tool(
                json!({ "type": "inline_gts", "schema": { "json_schema": { "type": "object" } } }),
            )],
        )
        .await
        .expect_err("no name");

    assert!(matches!(err, LlmGatewayError::Validation { .. }), "{err:?}");
}

fn assert_validation(err: &LlmGatewayError) {
    assert!(matches!(err, LlmGatewayError::Validation { .. }), "{err:?}");
}

#[tokio::test]
async fn expansion_cap_is_per_request_not_per_tool() {
    let (source, resolver) = setup();
    source.insert(GEO, json!({ "type": "object" }));
    let tools: Vec<Tool> = (0..300)
        .map(|i| {
            tool(json!({ "type": "inline_gts", "schema": { "json_schema": {
                "title": format!("t{i}"),
                "properties": { "at": { "$ref": format!("gts://{GEO}") } }
            } } }))
        })
        .collect();

    let err = resolver
        .resolve(&ctx(), tools)
        .await
        .expect_err("cap exceeded");

    assert_validation(&err);
}

#[tokio::test]
async fn byte_budget_is_enforced_during_expansion() {
    let source = FakeToolSchemas::new(Arc::new(CallLog::default()));
    let resolver = ToolResolver::new(source.clone(), 200);
    source.insert(
        GEO,
        json!({ "type": "object", "description": "x".repeat(300) }),
    );
    let inline =
        json!({ "title": "t", "properties": { "at": { "$ref": format!("gts://{GEO}") } } });

    let err = resolver
        .resolve(
            &ctx(),
            vec![tool(
                json!({ "type": "inline_gts", "schema": { "json_schema": inline } }),
            )],
        )
        .await
        .expect_err("budget exceeded");

    assert_validation(&err);
}

#[tokio::test]
async fn ref_siblings_override_target_keywords() {
    let (source, resolver) = setup();
    source.insert(
        WEATHER,
        json!({ "title": "other_title", "description": "target", "type": "object" }),
    );
    let inline = json!({
        "title": "get_weather",
        "description": "mine",
        "$ref": format!("gts://{WEATHER}")
    });

    let got = resolver
        .resolve(
            &ctx(),
            vec![tool(
                json!({ "type": "inline_gts", "schema": { "json_schema": inline } }),
            )],
        )
        .await
        .expect("resolves");

    let function = function_of(&got[0]);
    assert_eq!(function.name, "get_weather");
    assert_eq!(function.description.as_deref(), Some("mine"));
}

#[tokio::test]
async fn long_gts_ids_keep_their_discriminating_tail() {
    let (source, resolver) = setup();
    let prefix = "a".repeat(70);
    let first = format!("gts.{prefix}.first.v1~");
    let second = format!("gts.{prefix}.second.v1~");
    source.insert(&first, json!({ "type": "object" }));
    source.insert(&second, json!({ "type": "object" }));

    let got = resolver
        .resolve(
            &ctx(),
            vec![
                tool(json!({ "type": "reference", "schema_id": first })),
                tool(json!({ "type": "reference", "schema_id": second })),
            ],
        )
        .await
        .expect("resolves");

    let (a, b) = (
        function_of(&got[0]).name.clone(),
        function_of(&got[1]).name.clone(),
    );
    assert_eq!(a.len(), 64);
    assert_ne!(a, b);
    assert!(a.ends_with("first_v1_"), "{a}");
}
