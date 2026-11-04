//! The public structs and error variants are `#[non_exhaustive]`, so code
//! outside this crate (plugins, the Gateway core, consumers) cannot build them
//! with struct literals. These tests build each of them the way such code
//! has to: through constructors and `with_*` setters only.

use llm_gateway_sdk::{
    EmbeddingInput, EmbeddingInputType, EmbeddingRequest, EmbeddingResponse, EncodingFormat,
    InputTokensDetails, ItemStatus, LlmGatewayError, MediaInputMode, OutputTokensDetails,
    ProviderDetails, ProviderErrorCategory, ProviderFailure, ProviderPluginCapabilities,
    ResponseError, Usage, WebSearchCallOutput, WebSearchResult, WebSearchTool,
};
use rust_decimal::Decimal;

#[test]
fn usage_constructors_set_completeness_flags() {
    let reported = Usage::reported(10, 5, Decimal::new(125, 4));
    assert_eq!(reported.total_tokens, 15);
    assert!(reported.complete);
    assert!(!reported.estimated);

    let estimated = Usage::estimated(10, 3, Decimal::ONE);
    assert!(!estimated.complete);
    assert!(estimated.estimated);

    let unknown = Usage::unknown();
    assert_eq!(unknown.cost, Decimal::ZERO);
    assert!(!unknown.complete);
    assert!(!unknown.estimated);
}

#[test]
fn usage_with_details_and_provider_raw() {
    let usage = Usage::reported(10, 5, Decimal::ZERO)
        .with_details(
            InputTokensDetails { cached_tokens: 4 },
            OutputTokensDetails {
                reasoning_tokens: 2,
            },
        )
        .with_provider_raw(serde_json::json!({ "prompt_tokens": 10 }));
    assert_eq!(usage.input_tokens_details.cached_tokens, 4);
    assert_eq!(usage.output_tokens_details.reasoning_tokens, 2);
    assert!(usage.provider_raw.is_some());
}

#[test]
fn plugin_capabilities_builder() {
    let caps = ProviderPluginCapabilities::new(MediaInputMode::UrlForward)
        .with_streaming_transport()
        .with_web_search();
    assert!(caps.streaming_transport);
    assert!(caps.web_search);
    assert_eq!(caps.media_input, MediaInputMode::UrlForward);

    let bare = ProviderPluginCapabilities::new(MediaInputMode::PrefetchRequired);
    assert!(!bare.streaming_transport);
    assert!(!bare.web_search);
}

#[test]
fn embedding_request_and_response_builders() {
    let req = EmbeddingRequest::new("embed", EmbeddingInput::Many(vec!["a".into()]))
        .with_dimensions(256)
        .with_input_type(EmbeddingInputType::Document)
        .with_encoding_format(EncodingFormat::Base64)
        .with_drop_unsupported_params(true);
    assert_eq!(req.dimensions, Some(256));
    assert_eq!(req.input_type, Some(EmbeddingInputType::Document));

    let resp = EmbeddingResponse::new("resp_1", "embed", Vec::new(), Usage::unknown())
        .with_provider_details(ProviderDetails::default().with_provider_request_id("req_1"));
    assert!(resp.ignored_parameters.is_empty());
    assert_eq!(
        resp.provider_details
            .and_then(|details| details.provider_request_id)
            .as_deref(),
        Some("req_1")
    );
}

#[test]
fn web_search_builders() {
    let tool = WebSearchTool::default()
        .with_allowed_domains(vec!["docs.rs".into()])
        .with_option("search_context_size", serde_json::json!("low"));
    assert_eq!(
        tool.allowed_domains.as_deref(),
        Some(&["docs.rs".to_owned()][..])
    );
    assert!(tool.options.contains_key("search_context_size"));

    let call = WebSearchCallOutput::new("ws_1", ItemStatus::Completed)
        .with_queries(vec!["rust".into()])
        .with_results(vec![
            WebSearchResult::new("https://docs.rs").with_title("Docs.rs"),
        ]);
    assert_eq!(call.results[0].title.as_deref(), Some("Docs.rs"));
}

#[test]
fn error_constructors_cover_struct_variants() {
    let failure = ProviderFailure::default()
        .with_provider_code("overloaded_error")
        .with_provider_message("Overloaded")
        .with_provider_request_id("req_1")
        .with_retry_after(5);
    let cases = [
        (LlmGatewayError::validation("bad"), "validation_error"),
        (
            LlmGatewayError::output_validation("bad"),
            "output_validation_error",
        ),
        (LlmGatewayError::model_not_found("m"), "model_not_found"),
        (
            LlmGatewayError::provider_not_supported("t"),
            "provider_not_supported",
        ),
        (
            LlmGatewayError::capability_not_supported("c"),
            "capability_not_supported",
        ),
        (
            LlmGatewayError::unsupported_parameter("p"),
            "unsupported_parameter",
        ),
        (
            LlmGatewayError::unsupported_schema("s"),
            "unsupported_schema",
        ),
        (LlmGatewayError::schema_not_found("id"), "schema_not_found"),
        (LlmGatewayError::media_not_found(), "media_not_found"),
        (
            LlmGatewayError::provider(ProviderErrorCategory::ProviderUnavailable, "down", failure),
            "provider_unavailable",
        ),
        (LlmGatewayError::internal("x"), "internal"),
    ];
    for (err, code) in cases {
        assert_eq!(err.code(), code);
    }

    let wire = LlmGatewayError::media_not_found_param("input[0].content[1]").to_response_error();
    assert_eq!(wire.param.as_deref(), Some("input[0].content[1]"));
}

#[test]
fn matching_struct_variants_needs_rest_pattern() {
    let err = LlmGatewayError::unsupported_parameter("temperature");
    let param = match &err {
        LlmGatewayError::UnsupportedParameter { param, .. } => Some(param.as_str()),
        _ => None,
    };
    assert_eq!(param, Some("temperature"));
}

#[test]
fn response_error_and_provider_details_builders() {
    let error = ResponseError::new("internal", "internal error");
    assert_eq!(error.code, "internal");
    assert!(error.provider_code.is_none());

    let details = ProviderDetails::default()
        .with_stop_reason("end_turn")
        .with_provider_request_id("req_1");
    assert_eq!(details.stop_reason.as_deref(), Some("end_turn"));
}
