//! Slice 4 pipeline tests: `create_embedding` through the shared pipeline
//! (DESIGN §3.2 ordering, §4.1 codes, §4.5 limits, `seq-embeddings`).

use std::collections::HashMap;
use std::time::Duration;

use llm_gateway_sdk::{
    EmbeddingInput, EmbeddingInputType, EmbeddingRequest, EncodingFormat, LlmGatewayClientV1,
    LlmGatewayError, ProviderErrorCategory, ProviderFailure,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;

use crate::domain::LlmGatewayLocalClient;
use crate::domain::test_support::{
    FakeBroker, FakeModels, FakePdp, FakePlugin, FakeRules, Harness, PdpMode, SECRET_VALUE, body,
    ctx, sample_embedding_response, sample_model_info_patched,
};

fn request() -> EmbeddingRequest {
    EmbeddingRequest::new(
        "openai::text-embedding",
        EmbeddingInput::Single("hi".to_owned()),
    )
}

fn batch(n: usize) -> EmbeddingInput {
    EmbeddingInput::Many((0..n).map(|i| format!("text {i}")).collect())
}

fn embeddings_harness() -> Harness {
    let mut h = Harness::new();
    h.enable_embeddings();
    h
}

// ── id allocation ───────────────────────────────────────────────────────────

#[tokio::test]
async fn request_id_has_the_emb_prefix_reaches_the_plugin_and_is_returned() {
    let h = embeddings_harness();
    let response = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect("embedding succeeds");

    let contexts = h.plugin.embedding_contexts();
    assert_eq!(contexts.len(), 1);
    assert!(
        contexts[0].request_id.starts_with("emb_"),
        "{}",
        contexts[0].request_id
    );
    assert_eq!(response.id, contexts[0].request_id);
}

#[tokio::test]
async fn gateway_id_overrides_the_id_the_plugin_returned() {
    let h = embeddings_harness();
    h.plugin
        .set_embedding(|_| Ok(sample_embedding_response("provider-side-id", "m")));
    let response = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect("succeeds");

    assert_eq!(response.id, h.plugin.embedding_contexts()[0].request_id);
}

#[tokio::test]
async fn each_embedding_request_gets_a_distinct_id() {
    let h = embeddings_harness();
    let svc = h.service();
    let a = svc.create_embedding(&ctx(), request()).await.expect("a");
    let b = svc.create_embedding(&ctx(), request()).await.expect("b");
    assert_ne!(a.id, b.id);
}

#[tokio::test]
async fn failure_carries_the_allocated_request_id() {
    let mut h = embeddings_harness();
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let failure = h
        .service()
        .create_embedding_traced(&ctx(), request())
        .await
        .expect_err("denied");

    assert!(
        failure.request_id.starts_with("emb_"),
        "{}",
        failure.request_id
    );
    assert!(matches!(failure.error, LlmGatewayError::PermissionDenied));
}

// ── authorization and ordering ──────────────────────────────────────────────

#[tokio::test]
async fn pep_checks_the_create_embedding_action() {
    let h = embeddings_harness();
    h.service()
        .create_embedding(&ctx(), request())
        .await
        .expect("succeeds");

    let asked = h.pdp.last_request().expect("PDP was asked");
    assert_eq!(asked.action.name, "create_embedding");
}

#[tokio::test]
async fn pdp_denial_is_permission_denied_and_no_dependency_is_called() {
    let mut h = embeddings_harness();
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let err = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect_err("denied");

    assert!(matches!(err, LlmGatewayError::PermissionDenied), "{err}");
    assert_eq!(h.log.entries(), vec!["authz"]);
}

#[tokio::test]
async fn pdp_unavailable_fails_closed() {
    let mut h = embeddings_harness();
    h.pdp = FakePdp::new(PdpMode::Unreachable, h.log.clone());
    let err = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect_err("fails closed");

    assert!(matches!(err, LlmGatewayError::GatewayUnavailable), "{err}");
    assert_eq!(h.log.entries(), vec!["authz"]);
}

#[tokio::test]
async fn dependencies_are_called_in_pipeline_order() {
    let h = embeddings_harness();
    h.service()
        .create_embedding(&ctx(), request())
        .await
        .expect("succeeds");

    assert_eq!(
        h.log.entries(),
        vec!["authz", "models", "plugins", "credentials", "plugin"]
    );
}

#[tokio::test]
async fn model_identifier_is_passed_to_the_resolver() {
    let h = embeddings_harness();
    h.service()
        .create_embedding(&ctx(), request())
        .await
        .expect("succeeds");
    assert_eq!(h.models.requested(), vec!["openai::text-embedding"]);
}

#[tokio::test]
async fn unknown_model_is_model_not_found_and_nothing_downstream_runs() {
    let mut h = embeddings_harness();
    h.models = FakeModels::with(|| Err(LlmGatewayError::model_not_found("m")), h.log.clone());
    let err = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect_err("unknown model");

    assert!(
        matches!(err, LlmGatewayError::ModelNotFound { .. }),
        "{err}"
    );
    assert_eq!(h.log.entries(), vec!["authz", "models"]);
}

#[tokio::test]
async fn model_of_an_unserved_provider_is_provider_not_supported() {
    let mut h = embeddings_harness();
    h.models = FakeModels::found(
        sample_model_info_patched("gts.cf.genai.model.info.v1~cf.genai._.other.v1~", |v| {
            v["supported_api"] = json!(["embedding"]);
        }),
        h.log.clone(),
    );
    let err = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect_err("no plugin");

    assert!(
        matches!(err, LlmGatewayError::ProviderNotSupported { .. }),
        "{err}"
    );
    assert_eq!(h.plugin.embedding_calls(), 0);
}

// ── in-flight limit (shared sync pool) ──────────────────────────────────────

#[tokio::test]
async fn inflight_limit_rejects_before_model_resolution_and_slot_is_released() {
    let mut h = embeddings_harness();
    h.limits.max_inflight_sync = 1;
    let gated = FakePlugin::with(
        |_| Err(LlmGatewayError::internal("unused")),
        h.log.clone(),
        true,
    );
    gated.set_embedding(|c| Ok(sample_embedding_response(&c.request_id, "m")));
    h.set_plugin(gated.clone());
    let svc = h.service();

    let first = {
        let svc = svc.clone();
        tokio::spawn(async move { svc.create_embedding(&ctx(), request()).await })
    };
    tokio::time::timeout(Duration::from_secs(5), gated.entered.notified())
        .await
        .expect("the first request reached the plugin");
    assert_eq!(h.models.calls(), 1);

    let err = svc
        .create_embedding(&ctx(), request())
        .await
        .expect_err("limit reached");
    assert!(matches!(err, LlmGatewayError::GatewayUnavailable), "{err}");
    assert_eq!(
        h.models.calls(),
        1,
        "the rejected request must not resolve the model"
    );

    gated.release.notify_one();
    first.await.expect("task joins").expect("first succeeds");

    gated.release.notify_one();
    svc.create_embedding(&ctx(), request())
        .await
        .expect("the slot was released after the first call completed");
}

#[tokio::test]
async fn embeddings_share_the_sync_pool_with_create_response() {
    let mut h = Harness::new();
    h.limits.max_inflight_sync = 1;
    let gated = FakePlugin::with(
        |c| {
            Ok(crate::domain::test_support::sample_response(
                &c.request_id,
                "m",
            ))
        },
        h.log.clone(),
        true,
    );
    h.set_plugin(gated.clone());
    let svc = h.service();

    let first = {
        let svc = svc.clone();
        tokio::spawn(async move { svc.create_response(&ctx(), body("m")).await })
    };
    tokio::time::timeout(Duration::from_secs(5), gated.entered.notified())
        .await
        .expect("the response reached the plugin");

    let err = svc
        .create_embedding(&ctx(), request())
        .await
        .expect_err("the pool is full");
    assert!(matches!(err, LlmGatewayError::GatewayUnavailable), "{err}");

    gated.release.notify_one();
    first.await.expect("task joins").expect("first succeeds");
}

#[tokio::test]
async fn slot_is_released_after_a_failed_embedding() {
    let mut h = embeddings_harness();
    h.limits.max_inflight_sync = 1;
    h.models = FakeModels::with(|| Err(LlmGatewayError::model_not_found("m")), h.log.clone());
    let svc = h.service();

    for _ in 0..3 {
        let err = svc
            .create_embedding(&ctx(), request())
            .await
            .expect_err("model not found");
        assert!(
            matches!(err, LlmGatewayError::ModelNotFound { .. }),
            "{err}"
        );
    }
}

// ── capability gate ─────────────────────────────────────────────────────────

#[tokio::test]
async fn model_without_the_embedding_api_is_capability_not_supported() {
    // The default sample model serves `completion` only.
    let h = Harness::new();
    let err = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect_err("not an embedding model");

    assert_eq!(err.code(), "capability_not_supported");
    assert!(err.to_string().contains("embeddings"), "{err}");
    assert_eq!(h.log.entries(), vec!["authz", "models", "plugins"]);
}

// ── input validation ────────────────────────────────────────────────────────

#[tokio::test]
async fn empty_input_is_a_validation_error_naming_input_before_credentials() {
    for input in [
        EmbeddingInput::Single(String::new()),
        EmbeddingInput::Many(Vec::new()),
    ] {
        let h = embeddings_harness();
        let err = h
            .service()
            .create_embedding(&ctx(), EmbeddingRequest::new("m", input))
            .await
            .expect_err("empty input");

        assert_eq!(err.code(), "validation_error");
        assert_eq!(err.to_response_error().param.as_deref(), Some("input"));
        assert_eq!(h.log.entries(), vec!["authz", "models", "plugins"]);
    }
}

#[tokio::test]
async fn input_beyond_max_embedding_inputs_is_a_validation_error() {
    let mut h = embeddings_harness();
    h.limits.max_embedding_inputs = 2;
    let err = h
        .service()
        .create_embedding(&ctx(), EmbeddingRequest::new("m", batch(3)))
        .await
        .expect_err("too many inputs");

    assert_eq!(err.code(), "validation_error");
    assert_eq!(err.to_response_error().param.as_deref(), Some("input"));
    assert_eq!(h.plugin.embedding_calls(), 0);
    assert_eq!(h.broker.calls(), 0);
}

#[tokio::test]
async fn input_at_max_embedding_inputs_is_accepted() {
    let mut h = embeddings_harness();
    h.limits.max_embedding_inputs = 2;
    h.service()
        .create_embedding(&ctx(), EmbeddingRequest::new("m", batch(2)))
        .await
        .expect("at the limit");
    assert_eq!(h.plugin.embedding_calls(), 1);
}

// ── parameter policy ────────────────────────────────────────────────────────

#[tokio::test]
async fn unsupported_dimensions_is_never_dropped() {
    for drop in [false, true] {
        let mut h = embeddings_harness();
        h.rules = FakeRules {
            reject_all_dimensions: true,
            ..FakeRules::default()
        };
        let err = h
            .service()
            .create_embedding(
                &ctx(),
                request()
                    .with_dimensions(256)
                    .with_drop_unsupported_params(drop),
            )
            .await
            .expect_err("dimensions rejected");

        assert_eq!(err.code(), "unsupported_parameter", "drop={drop}");
        assert_eq!(err.to_response_error().param.as_deref(), Some("dimensions"));
        assert_eq!(h.plugin.embedding_calls(), 0);
        assert_eq!(h.broker.calls(), 0);
    }
}

#[tokio::test]
async fn unsupported_input_type_is_dropped_and_listed_when_dropping_is_allowed() {
    let mut h = embeddings_harness();
    h.rules = FakeRules {
        unsupported_params: ["input_type"].into(),
        ..FakeRules::default()
    };
    let response = h
        .service()
        .create_embedding(
            &ctx(),
            request()
                .with_input_type(EmbeddingInputType::Query)
                .with_drop_unsupported_params(true),
        )
        .await
        .expect("dropped, not rejected");

    assert_eq!(response.ignored_parameters, vec!["input_type"]);
    assert_eq!(h.plugin.embedding_requests()[0].input_type, None);
}

#[tokio::test]
async fn unsupported_input_type_is_rejected_without_the_drop_flag() {
    let mut h = embeddings_harness();
    h.rules = FakeRules {
        unsupported_params: ["input_type"].into(),
        ..FakeRules::default()
    };
    let err = h
        .service()
        .create_embedding(&ctx(), request().with_input_type(EmbeddingInputType::Query))
        .await
        .expect_err("rejected");

    assert_eq!(err.code(), "unsupported_parameter");
    assert_eq!(err.to_response_error().param.as_deref(), Some("input_type"));
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn dimensions_input_type_and_encoding_format_reach_the_plugin_unchanged() {
    let h = embeddings_harness();
    h.service()
        .create_embedding(
            &ctx(),
            request()
                .with_dimensions(256)
                .with_input_type(EmbeddingInputType::Document)
                .with_encoding_format(EncodingFormat::Base64),
        )
        .await
        .expect("succeeds");

    let sent = &h.plugin.embedding_requests()[0];
    assert_eq!(sent.dimensions, Some(256));
    assert_eq!(sent.input_type, Some(EmbeddingInputType::Document));
    assert_eq!(sent.encoding_format, Some(EncodingFormat::Base64));
    assert_eq!(sent.input, EmbeddingInput::Single("hi".to_owned()));
}

#[tokio::test]
async fn extra_fields_outside_allow_extra_params_are_rejected() {
    let h = embeddings_harness();
    let extras = HashMap::from([("truncate".to_owned(), json!("END"))]);
    let err = h
        .service()
        .create_embedding(&ctx(), request().with_extra_fields(extras))
        .await
        .expect_err("not allowed");

    assert_eq!(err.code(), "unsupported_parameter");
    assert_eq!(err.to_response_error().param.as_deref(), Some("truncate"));
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn allowed_extra_fields_reach_the_plugin() {
    let mut h = embeddings_harness();
    h.enable_embeddings_with(|v| v["allow_extra_params"] = json!(["truncate"]));
    let extras = HashMap::from([("truncate".to_owned(), json!("END"))]);
    h.service()
        .create_embedding(&ctx(), request().with_extra_fields(extras))
        .await
        .expect("allowed");

    let sent = &h.plugin.embedding_requests()[0];
    assert_eq!(
        sent.extra_fields.as_ref().and_then(|e| e.get("truncate")),
        Some(&json!("END"))
    );
}

#[tokio::test]
async fn disallowed_extra_fields_are_dropped_and_listed_with_the_drop_flag() {
    let h = embeddings_harness();
    let extras = HashMap::from([("truncate".to_owned(), json!("END"))]);
    let response = h
        .service()
        .create_embedding(
            &ctx(),
            request()
                .with_extra_fields(extras)
                .with_drop_unsupported_params(true),
        )
        .await
        .expect("dropped");

    assert_eq!(response.ignored_parameters, vec!["truncate"]);
    assert_eq!(h.plugin.embedding_requests()[0].extra_fields, None);
}

#[tokio::test]
async fn gateway_and_plugin_ignored_parameters_are_merged_sorted_and_deduplicated() {
    let mut h = embeddings_harness();
    h.rules = FakeRules {
        unsupported_params: ["input_type"].into(),
        ..FakeRules::default()
    };
    h.plugin.set_embedding(|c| {
        let mut response = sample_embedding_response(&c.request_id, "m");
        response.ignored_parameters = vec!["zeta".to_owned(), "input_type".to_owned()];
        Ok(response)
    });
    let response = h
        .service()
        .create_embedding(
            &ctx(),
            request()
                .with_input_type(EmbeddingInputType::Query)
                .with_drop_unsupported_params(true),
        )
        .await
        .expect("succeeds");

    assert_eq!(response.ignored_parameters, vec!["input_type", "zeta"]);
}

// ── credentials and dispatch ────────────────────────────────────────────────

#[tokio::test]
async fn broker_key_reaches_the_plugin_and_never_appears_in_debug_output() {
    let mut h = embeddings_harness();
    h.broker = FakeBroker::with(
        || Ok(Some(SecretString::from(SECRET_VALUE.to_owned()))),
        h.log.clone(),
    );
    h.service()
        .create_embedding(&ctx(), request())
        .await
        .expect("succeeds");

    let contexts = h.plugin.embedding_contexts();
    assert_eq!(
        contexts[0]
            .api_key
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some(SECRET_VALUE)
    );
    assert!(!format!("{:?}", contexts[0]).contains(SECRET_VALUE));
}

#[tokio::test]
async fn broker_failure_is_credentials_unavailable_and_plugin_is_not_called() {
    let mut h = embeddings_harness();
    h.broker = FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    let err = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect_err("no credentials");

    assert!(
        matches!(err, LlmGatewayError::CredentialsUnavailable),
        "{err}"
    );
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn plugin_error_is_returned_unchanged_and_the_call_is_not_retried() {
    let h = embeddings_harness();
    h.plugin.set_embedding(|_| {
        Err(LlmGatewayError::provider(
            ProviderErrorCategory::ProviderUnavailable,
            "down",
            ProviderFailure::default().with_provider_request_id("req_1"),
        ))
    });
    let err = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect_err("provider failure");

    assert_eq!(err.code(), "provider_unavailable");
    assert_eq!(
        err.to_response_error().provider_request_id.as_deref(),
        Some("req_1")
    );
    assert_eq!(h.plugin.embedding_calls(), 1);
}

#[tokio::test]
async fn the_plugin_data_and_usage_are_returned() {
    let h = embeddings_harness();
    let response = h
        .service()
        .create_embedding(&ctx(), request())
        .await
        .expect("succeeds");

    assert_eq!(response.data.len(), 1);
    assert_eq!(response.model, "text-embedding");
}

#[tokio::test]
async fn cancel_token_is_cancelled_after_a_normal_return() {
    let h = embeddings_harness();
    h.service()
        .create_embedding(&ctx(), request())
        .await
        .expect("succeeds");
    assert!(h.plugin.embedding_contexts()[0].cancel.is_cancelled());
}

// ── failure logging ─────────────────────────────────────────────────────────

#[tokio::test]
#[tracing_test::traced_test]
async fn internal_failure_logs_the_source_chain_and_request_id() {
    let h = embeddings_harness();
    h.plugin.set_embedding(|_| {
        Err(LlmGatewayError::internal_from(
            "x",
            std::io::Error::other("disk exploded"),
        ))
    });
    let failure = h
        .service()
        .create_embedding_traced(&ctx(), request())
        .await
        .expect_err("plugin fails");

    assert!(logs_contain("disk exploded"));
    assert!(logs_contain(&failure.request_id));
    assert!(logs_contain("ERROR"));
    assert_eq!(failure.error.to_response_error().message, "internal error");
}

#[tokio::test]
#[tracing_test::traced_test]
async fn caller_side_failure_logs_at_info_without_error_level() {
    let mut h = embeddings_harness();
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let failure = h
        .service()
        .create_embedding_traced(&ctx(), request())
        .await
        .expect_err("denied");

    assert!(logs_contain(&failure.request_id));
    assert!(logs_contain("permission_denied"));
    assert!(!logs_contain("ERROR"));
}

// ── local client ────────────────────────────────────────────────────────────

#[tokio::test]
async fn local_client_delegates_create_embedding_to_the_service() {
    let h = embeddings_harness();
    let client = LlmGatewayLocalClient::new(h.service());
    let response = client
        .create_embedding(&ctx(), request())
        .await
        .expect("delegates");

    assert_eq!(response.id, h.plugin.embedding_contexts()[0].request_id);
    assert_eq!(h.pdp.calls(), 1);
}

#[tokio::test]
async fn local_client_surfaces_service_errors() {
    let mut h = embeddings_harness();
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let client = LlmGatewayLocalClient::new(h.service());
    let err = client
        .create_embedding(&ctx(), request())
        .await
        .expect_err("denied");

    assert!(matches!(err, LlmGatewayError::PermissionDenied), "{err}");
}
