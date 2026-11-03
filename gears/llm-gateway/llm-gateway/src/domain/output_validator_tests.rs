//! Output Validator (DESIGN §3.2, `cpt-cf-llm-gateway-seq-structured-output-v1`).

use llm_gateway_sdk::LlmGatewayError;
use serde_json::json;

use super::OutputValidator;
use crate::domain::test_support::{sample_response, sample_response_with_text};

const SENTINEL: &str = "SENTINEL-VALUE-91c7";

fn schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": { "name": { "type": "string" }, "age": { "type": "integer" } },
        "required": ["name"]
    })
}

fn output_validation_message(err: &LlmGatewayError) -> &str {
    let LlmGatewayError::OutputValidation { message, .. } = err else {
        panic!("expected output_validation_error, got {err:?}");
    };
    message
}

#[test]
fn output_matching_the_schema_is_valid() {
    let resource = sample_response_with_text("resp_1", "m", r#"{"name":"Ada","age":36}"#);
    OutputValidator
        .validate(&schema(), &resource)
        .expect("valid output");
}

#[test]
fn text_split_over_output_parts_is_assembled_before_validation() {
    let mut resource = sample_response_with_text("resp_1", "m", r#"{"name":"#);
    let second = sample_response_with_text("resp_1", "m", r#""Ada"}"#);
    let llm_gateway_sdk::models::items::OutputItem::Message(first_message) =
        &mut resource.output[0]
    else {
        panic!("message output");
    };
    let llm_gateway_sdk::models::items::OutputItem::Message(second_message) = &second.output[0]
    else {
        panic!("message output");
    };
    first_message.content.extend(second_message.content.clone());

    OutputValidator
        .validate(&schema(), &resource)
        .expect("assembled text is valid");
}

#[test]
fn schema_mismatch_lists_paths_and_keywords_but_no_content_values() {
    let text = json!({ "age": SENTINEL }).to_string();
    let resource = sample_response_with_text("resp_1", "m", &text);

    let err = OutputValidator
        .validate(&schema(), &resource)
        .expect_err("invalid output");

    let message = output_validation_message(&err);
    assert!(message.contains("/age"), "instance path listed: {message}");
    assert!(message.contains("type"), "failed keyword listed: {message}");
    assert!(
        message.contains("required"),
        "failed keyword listed: {message}"
    );
    assert!(
        !message.contains(SENTINEL),
        "content value leaked: {message}"
    );
}

#[test]
fn non_json_output_is_an_output_validation_error_without_the_text() {
    let resource = sample_response_with_text("resp_1", "m", &format!("not json {SENTINEL}"));

    let err = OutputValidator
        .validate(&schema(), &resource)
        .expect_err("not JSON");

    let message = output_validation_message(&err);
    assert!(!message.contains(SENTINEL), "content leaked: {message}");
}

#[test]
fn response_without_output_text_is_an_output_validation_error() {
    let resource = sample_response("resp_1", "m");
    let err = OutputValidator
        .validate(&schema(), &resource)
        .expect_err("no text");
    assert!(
        matches!(err, LlmGatewayError::OutputValidation { .. }),
        "{err:?}"
    );
}

#[test]
fn failure_listing_is_bounded_and_says_and_more() {
    let schema = json!({
        "type": "object",
        "properties": { "items": { "type": "array", "items": { "type": "string" } } }
    });
    let text = json!({ "items": vec![1; 40] }).to_string();
    let resource = sample_response_with_text("resp_1", "m", &text);

    let err = OutputValidator
        .validate(&schema, &resource)
        .expect_err("invalid output");

    let message = output_validation_message(&err);
    assert!(message.ends_with("; and more)"), "{message}");
    assert_eq!(message.matches("type").count(), 16, "{message}");
}
