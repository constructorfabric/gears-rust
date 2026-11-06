//! Capability Checker embeddings contract (DESIGN §3.2, `seq-embeddings`).

use std::collections::HashMap;
use std::sync::Arc;

use llm_gateway_sdk::{
    EmbeddingInput, EmbeddingInputType, EmbeddingRequest, EncodingFormat, LlmGatewayError,
};
use model_registry_sdk::ModelInfoV1;
use serde_json::json;

use super::{CapabilityChecker, EffectiveEmbedding};
use crate::config::LimitsConfig;
use crate::domain::provider_rules::ProviderRulesRegistry;
use crate::domain::test_support::{FakeRules, OPENAI_TYPE, sample_model_info_patched};

fn model(patch: impl FnOnce(&mut serde_json::Value)) -> ModelInfoV1 {
    sample_model_info_patched(OPENAI_TYPE, |v| {
        v["supported_api"] = json!(["embedding"]);
        patch(v);
    })
}

fn request() -> EmbeddingRequest {
    EmbeddingRequest::new("m", EmbeddingInput::Single("hi".to_owned()))
}

fn check_with(
    limits: &LimitsConfig,
    rules: FakeRules,
    model: &ModelInfoV1,
    request: EmbeddingRequest,
) -> Result<EffectiveEmbedding, LlmGatewayError> {
    CapabilityChecker::new(
        limits,
        Arc::new(ProviderRulesRegistry::permissive().with_rules(OPENAI_TYPE, Arc::new(rules))),
    )
    .check_embedding(model, request)
}

fn check(request: EmbeddingRequest) -> Result<EffectiveEmbedding, LlmGatewayError> {
    check_with(
        &LimitsConfig::default(),
        FakeRules::default(),
        &model(|_| {}),
        request,
    )
}

fn param(err: &LlmGatewayError) -> Option<String> {
    err.to_response_error().param
}

#[test]
fn a_plain_request_passes_unchanged_with_nothing_ignored() {
    let effective = check(request()).expect("passes");
    assert_eq!(effective.request, request());
    assert!(effective.ignored_parameters.is_empty());
}

#[test]
fn a_model_without_the_embedding_api_is_not_supported() {
    let completion_only = sample_model_info_patched(OPENAI_TYPE, |_| {});
    let err = check_with(
        &LimitsConfig::default(),
        FakeRules::default(),
        &completion_only,
        request(),
    )
    .expect_err("completion-only model");
    assert_eq!(err.code(), "capability_not_supported");
    assert!(err.to_string().contains("embeddings"), "{err}");
}

#[test]
fn a_model_with_both_apis_is_supported() {
    let both = model(|v| v["supported_api"] = json!(["completion", "embedding"]));
    check_with(
        &LimitsConfig::default(),
        FakeRules::default(),
        &both,
        request(),
    )
    .expect("passes");
}

#[test]
fn empty_inputs_are_validation_errors_naming_input() {
    for input in [
        EmbeddingInput::Single(String::new()),
        EmbeddingInput::Many(Vec::new()),
    ] {
        let err = check(EmbeddingRequest::new("m", input)).expect_err("empty");
        assert_eq!(err.code(), "validation_error");
        assert_eq!(param(&err).as_deref(), Some("input"));
    }
}

#[test]
fn the_input_count_limit_is_inclusive() {
    let limits = LimitsConfig {
        max_embedding_inputs: 2,
        ..LimitsConfig::default()
    };
    let many = |n: usize| {
        EmbeddingRequest::new(
            "m",
            EmbeddingInput::Many((0..n).map(|i| i.to_string()).collect()),
        )
    };
    check_with(&limits, FakeRules::default(), &model(|_| {}), many(2)).expect("at the limit");
    let err = check_with(&limits, FakeRules::default(), &model(|_| {}), many(3))
        .expect_err("over the limit");
    assert_eq!(err.code(), "validation_error");
    assert_eq!(param(&err).as_deref(), Some("input"));
}

#[test]
fn unsupported_dimensions_are_rejected_even_when_dropping_is_allowed() {
    let rules = FakeRules {
        reject_all_dimensions: true,
        ..FakeRules::default()
    };
    for drop in [None, Some(false), Some(true)] {
        let mut req = request().with_dimensions(512);
        req.drop_unsupported_params = drop;
        let err = check_with(&LimitsConfig::default(), rules.clone(), &model(|_| {}), req)
            .expect_err("never dropped");
        assert_eq!(err.code(), "unsupported_parameter");
        assert_eq!(param(&err).as_deref(), Some("dimensions"));
    }
}

#[test]
fn supported_dimensions_pass_and_unset_dimensions_are_not_asked_about() {
    check(request().with_dimensions(512)).expect("permissive rules support dimensions");
    let rules = FakeRules {
        reject_all_dimensions: true,
        ..FakeRules::default()
    };
    check_with(&LimitsConfig::default(), rules, &model(|_| {}), request())
        .expect("no dimensions requested");
}

#[test]
fn unsupported_input_type_is_rejected_or_dropped_and_listed() {
    let rules = FakeRules {
        unsupported_params: ["input_type"].into(),
        ..FakeRules::default()
    };
    let req = request().with_input_type(EmbeddingInputType::Query);

    let err = check_with(
        &LimitsConfig::default(),
        rules.clone(),
        &model(|_| {}),
        req.clone(),
    )
    .expect_err("rejected");
    assert_eq!(err.code(), "unsupported_parameter");
    assert_eq!(param(&err).as_deref(), Some("input_type"));

    let effective = check_with(
        &LimitsConfig::default(),
        rules,
        &model(|_| {}),
        req.with_drop_unsupported_params(true),
    )
    .expect("dropped");
    assert_eq!(effective.request.input_type, None);
    assert_eq!(effective.ignored_parameters, vec!["input_type"]);
}

#[test]
fn an_unset_input_type_is_not_listed_even_when_unsupported() {
    let rules = FakeRules {
        unsupported_params: ["input_type"].into(),
        ..FakeRules::default()
    };
    let effective =
        check_with(&LimitsConfig::default(), rules, &model(|_| {}), request()).expect("passes");
    assert!(effective.ignored_parameters.is_empty());
}

#[test]
fn encoding_format_is_always_kept() {
    let rules = FakeRules {
        unsupported_params: ["encoding_format"].into(),
        ..FakeRules::default()
    };
    let effective = check_with(
        &LimitsConfig::default(),
        rules,
        &model(|_| {}),
        request()
            .with_encoding_format(EncodingFormat::Base64)
            .with_drop_unsupported_params(true),
    )
    .expect("passes");
    assert_eq!(
        effective.request.encoding_format,
        Some(EncodingFormat::Base64)
    );
    assert!(effective.ignored_parameters.is_empty());
}

#[test]
fn extra_fields_are_gated_by_allow_extra_params() {
    let extras = || {
        HashMap::from([
            ("truncate".to_owned(), json!("END")),
            ("other".to_owned(), json!(1)),
        ])
    };
    let allowed = model(|v| v["allow_extra_params"] = json!(["truncate", "other"]));
    check_with(
        &LimitsConfig::default(),
        FakeRules::default(),
        &allowed,
        request().with_extra_fields(extras()),
    )
    .expect("all allowed");

    let err = check(request().with_extra_fields(extras())).expect_err("none allowed");
    assert_eq!(err.code(), "unsupported_parameter");
    assert_eq!(
        param(&err).as_deref(),
        Some("other"),
        "first unlisted key, sorted"
    );
}

#[test]
fn unlisted_extra_fields_are_dropped_and_listed_with_the_drop_flag() {
    let allowed = model(|v| v["allow_extra_params"] = json!(["truncate"]));
    let extras = HashMap::from([
        ("truncate".to_owned(), json!("END")),
        ("other".to_owned(), json!(1)),
    ]);
    let effective = check_with(
        &LimitsConfig::default(),
        FakeRules::default(),
        &allowed,
        request()
            .with_extra_fields(extras)
            .with_drop_unsupported_params(true),
    )
    .expect("dropped");

    assert_eq!(effective.ignored_parameters, vec!["other"]);
    let kept = effective.request.extra_fields.expect("allowed key kept");
    assert_eq!(kept.keys().collect::<Vec<_>>(), vec!["truncate"]);
}

#[test]
fn ignored_parameters_are_sorted_and_deduplicated() {
    let rules = FakeRules {
        unsupported_params: ["input_type"].into(),
        ..FakeRules::default()
    };
    let extras = HashMap::from([("b".to_owned(), json!(1)), ("a".to_owned(), json!(1))]);
    let effective = check_with(
        &LimitsConfig::default(),
        rules,
        &model(|_| {}),
        request()
            .with_input_type(EmbeddingInputType::Query)
            .with_extra_fields(extras)
            .with_drop_unsupported_params(true),
    )
    .expect("dropped");
    assert_eq!(effective.ignored_parameters, vec!["a", "b", "input_type"]);
}

#[test]
fn model_default_parameters_do_not_affect_embeddings() {
    let with_defaults = model(|v| {
        v["default_parameters"] = json!({ "temperature": 0.2 });
        v["allow_parameter_override"] = json!(false);
    });
    let effective = check_with(
        &LimitsConfig::default(),
        FakeRules::default(),
        &with_defaults,
        request(),
    )
    .expect("passes");
    assert_eq!(effective.request, request());
    assert!(effective.ignored_parameters.is_empty());
}

#[test]
fn a_batch_holding_an_empty_text_is_empty_input_but_whitespace_is_allowed() {
    for texts in [vec!["a", ""], vec!["", "a"], vec![""]] {
        let input = EmbeddingInput::Many(texts.into_iter().map(str::to_owned).collect());
        let err = check(EmbeddingRequest::new("m", input)).expect_err("empty element");
        assert_eq!(err.code(), "validation_error");
        assert_eq!(param(&err).as_deref(), Some("input"));
    }
    check(EmbeddingRequest::new(
        "m",
        EmbeddingInput::Many(vec![" ".to_owned(), "\n\t".to_owned()]),
    ))
    .expect("whitespace-only texts are allowed");
}

#[test]
fn dimensions_above_the_model_vector_size_are_rejected_even_when_dropping() {
    let sized = model(|v| v["context_window"]["output_vector_size"] = json!(1536));
    for drop in [None, Some(true)] {
        let mut req = request().with_dimensions(1537);
        req.drop_unsupported_params = drop;
        let err = check_with(&LimitsConfig::default(), FakeRules::default(), &sized, req)
            .expect_err("above the vector size");
        assert_eq!(err.code(), "unsupported_parameter");
        assert_eq!(param(&err).as_deref(), Some("dimensions"));
    }
    for ok in [1536, 256] {
        check_with(
            &LimitsConfig::default(),
            FakeRules::default(),
            &sized,
            request().with_dimensions(ok),
        )
        .expect("within the vector size");
    }
}

#[test]
fn zero_dimensions_are_rejected_even_when_dropping() {
    for drop in [None, Some(true)] {
        let mut req = request().with_dimensions(0);
        req.drop_unsupported_params = drop;
        let err = check_with(
            &LimitsConfig::default(),
            FakeRules::default(),
            &model(|_| {}),
            req,
        )
        .expect_err("zero dimensions");
        assert_eq!(err.code(), "unsupported_parameter");
        assert_eq!(param(&err).as_deref(), Some("dimensions"));
    }
}
