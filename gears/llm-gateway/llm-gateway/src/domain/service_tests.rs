//! Sync `create_response` pipeline tests (DESIGN §3.2, §3.6, §4.1, §4.4, §4.5),
//! the default Credential Broker, and the local client.

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use llm_gateway_sdk::{
    InputItem, ItemReference, LlmGatewayClientV1, LlmGatewayError, ProviderErrorCategory,
    ProviderFailure, ResponseInput,
};
use model_registry_sdk::ModelInfoV1;
use secrecy::{ExposeSecret, SecretString};

use crate::domain::credential_broker::secret_ref;
use crate::domain::ports::CredentialBroker;
use crate::domain::test_support::{
    FakeBroker, FakeModels, FakePdp, FakePlugin, Harness, OPENAI_TYPE, PdpMode, SECRET_VALUE, body,
    ctx, sample_model_info, sample_response,
};
use crate::domain::{KeylessCredentialBroker, LlmGatewayLocalClient, Service};

// ── id allocation ───────────────────────────────────────────────────────────

#[tokio::test]
async fn response_id_is_passed_to_plugin_and_returned_on_the_resource() {
    let h = Harness::new();
    let response = h
        .service()
        .create_response(&ctx(), body("openai::gpt-4o"))
        .await
        .expect("sync create_response succeeds");

    let contexts = h.plugin.contexts();
    assert_eq!(contexts.len(), 1);
    assert!(!contexts[0].request_id.is_empty());
    assert_eq!(response.id, contexts[0].request_id);
}

#[tokio::test]
async fn each_request_gets_a_distinct_response_id() {
    let h = Harness::new();
    let svc = h.service();
    let first = svc.create_response(&ctx(), body("m")).await.expect("first");
    let second = svc
        .create_response(&ctx(), body("m"))
        .await
        .expect("second");
    assert_ne!(first.id, second.id);
}

#[tokio::test]
async fn failure_carries_the_allocated_response_id() {
    let mut h = Harness::new();
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let failure = h
        .service()
        .create_response_traced(&ctx(), body("m"))
        .await
        .expect_err("denied");
    assert!(
        !failure.response_id.is_empty(),
        "the id is allocated before authorization, so every failure carries it"
    );
    assert!(matches!(failure.error, LlmGatewayError::PermissionDenied));
}

// ── PEP authorization ───────────────────────────────────────────────────────

#[tokio::test]
async fn pep_runs_before_any_other_dependency_call() {
    let h = Harness::new();
    h.service()
        .create_response(&ctx(), body("m"))
        .await
        .expect("succeeds");

    // The usage hand-off runs in the background after the response; this test
    // is about the pipeline stages.
    let stages: Vec<&str> = h
        .log
        .entries()
        .into_iter()
        .filter(|entry| *entry != "usage_sink")
        .collect();
    assert_eq!(
        stages,
        vec!["authz", "models", "plugins", "credentials", "plugin"]
    );
}

#[tokio::test]
async fn pep_checks_the_create_response_action() {
    let h = Harness::new();
    h.service()
        .create_response(&ctx(), body("m"))
        .await
        .expect("succeeds");

    let request = h.pdp.last_request().expect("PDP was asked");
    assert_eq!(request.action.name, "create_response");
}

#[tokio::test]
async fn pdp_denial_is_permission_denied_and_no_dependency_is_called() {
    let mut h = Harness::new();
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let err = h
        .service()
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("denied");

    assert!(matches!(err, LlmGatewayError::PermissionDenied), "{err}");
    assert_eq!(h.models.calls(), 0);
    assert_eq!(h.plugins.calls(), 0);
    assert_eq!(h.broker.calls(), 0);
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn pdp_unavailable_fails_closed_without_calling_dependencies() {
    let mut h = Harness::new();
    h.pdp = FakePdp::new(PdpMode::Unreachable, h.log.clone());
    let result = h.service().create_response(&ctx(), body("m")).await;

    assert!(result.is_err(), "an unreachable PDP must never permit");
    assert_eq!(h.pdp.calls(), 1, "the PDP was consulted");
    assert_eq!(h.models.calls(), 0);
    assert_eq!(h.plugin.calls(), 0);
}

// ── in-flight limit ─────────────────────────────────────────────────────────

#[tokio::test]
async fn inflight_limit_rejects_before_model_resolution_and_slot_is_released() {
    let mut h = Harness::new();
    h.limits.max_inflight_sync = 1;
    let gated = FakePlugin::with(
        |c| Ok(sample_response(&c.request_id, "gpt-4o")),
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
        .expect("the first request reached the plugin");
    assert_eq!(h.models.calls(), 1);

    let err = svc
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("limit reached");
    assert!(matches!(err, LlmGatewayError::GatewayUnavailable), "{err}");
    assert_eq!(
        h.models.calls(),
        1,
        "the rejected request must not reach model resolution"
    );

    gated.release.notify_one();
    first.await.expect("task joins").expect("first succeeds");

    gated.release.notify_one();
    svc.create_response(&ctx(), body("m"))
        .await
        .expect("the slot was released after the first call completed");
}

#[tokio::test]
async fn slot_is_released_after_a_failed_call() {
    let mut h = Harness::new();
    h.limits.max_inflight_sync = 1;
    h.models = FakeModels::with(|| Err(LlmGatewayError::model_not_found("m")), h.log.clone());
    let svc = h.service();

    for _ in 0..3 {
        let err = svc
            .create_response(&ctx(), body("m"))
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
async fn item_reference_input_is_capability_not_supported_without_provider_call() {
    let h = Harness::new();
    let mut request = body("m");
    request.input = Some(ResponseInput::Items(vec![InputItem::ItemReference(
        ItemReference {
            id: "item_1".to_owned(),
        },
    )]));
    let err = h
        .service()
        .create_response(&ctx(), request)
        .await
        .expect_err("rejected");

    assert!(
        matches!(err, LlmGatewayError::CapabilityNotSupported { .. }),
        "{err}"
    );
    assert_eq!(h.plugin.calls(), 0);
}

// ── model resolution ────────────────────────────────────────────────────────

#[tokio::test]
async fn model_identifier_is_passed_to_the_resolver() {
    let h = Harness::new();
    h.service()
        .create_response(&ctx(), body("openai::gpt-4o"))
        .await
        .expect("succeeds");
    assert_eq!(h.models.requested(), vec!["openai::gpt-4o".to_owned()]);
}

#[tokio::test]
async fn unknown_model_is_model_not_found_and_nothing_downstream_runs() {
    let mut h = Harness::new();
    h.models = FakeModels::with(|| Err(LlmGatewayError::model_not_found("m")), h.log.clone());
    let err = h
        .service()
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("not found");

    assert!(
        matches!(err, LlmGatewayError::ModelNotFound { .. }),
        "{err}"
    );
    assert_eq!(h.plugins.calls(), 0);
    assert_eq!(h.broker.calls(), 0);
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn model_registry_outage_is_model_resolution_unavailable() {
    let mut h = Harness::new();
    h.models = FakeModels::with(
        || Err(LlmGatewayError::ModelResolutionUnavailable),
        h.log.clone(),
    );
    let err = h
        .service()
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("unavailable");

    assert!(
        matches!(err, LlmGatewayError::ModelResolutionUnavailable),
        "{err}"
    );
    assert_eq!(h.plugin.calls(), 0);
}

// ── plugin selection ────────────────────────────────────────────────────────

#[tokio::test]
async fn plugin_is_selected_by_the_model_gts_type() {
    let h = Harness::new();
    h.service()
        .create_response(&ctx(), body("m"))
        .await
        .expect("succeeds");
    assert_eq!(h.plugins.requested(), vec![OPENAI_TYPE.to_owned()]);
    assert_eq!(
        h.plugin.contexts()[0].model_info.gts_type.as_ref(),
        OPENAI_TYPE
    );
}

#[tokio::test]
async fn model_of_an_unserved_provider_is_provider_not_supported() {
    let mut h = Harness::new();
    let other = "gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~";
    h.models = FakeModels::found(sample_model_info(other), h.log.clone());
    let err = h
        .service()
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("no plugin");

    assert!(
        matches!(err, LlmGatewayError::ProviderNotSupported { .. }),
        "{err}"
    );
    assert_eq!(h.broker.calls(), 0);
    assert_eq!(h.plugin.calls(), 0);
}

// ── credentials ─────────────────────────────────────────────────────────────

#[test]
fn secret_ref_is_absent_for_every_model() {
    let info: ModelInfoV1 = sample_model_info(OPENAI_TYPE);
    assert_eq!(secret_ref(&info), None);
}

#[tokio::test]
async fn default_broker_returns_no_key_for_any_model() {
    let key = KeylessCredentialBroker
        .resolve(&ctx(), &sample_model_info(OPENAI_TYPE))
        .await
        .expect("keyless broker never fails");
    assert!(key.is_none());
}

#[tokio::test]
async fn default_broker_gives_the_plugin_no_api_key() {
    let h = Harness::new();
    let svc = Service::new(
        PolicyEnforcer::new(h.pdp.clone()),
        h.models.clone(),
        h.plugins.clone(),
        Arc::new(KeylessCredentialBroker),
        h.tool_schemas.clone(),
        Arc::new(crate::domain::provider_rules::ProviderRulesRegistry::permissive()),
        h.quota_guard(),
        &h.limits,
        &h.stream,
        &h.timeouts,
    );
    svc.create_response(&ctx(), body("m"))
        .await
        .expect("succeeds");

    let contexts = h.plugin.contexts();
    assert_eq!(contexts.len(), 1);
    assert!(contexts[0].api_key.is_none());
}

#[tokio::test]
async fn broker_key_reaches_the_plugin_and_never_appears_in_debug_output() {
    let mut h = Harness::new();
    h.broker = FakeBroker::with(
        || Ok(Some(SecretString::from(SECRET_VALUE.to_owned()))),
        h.log.clone(),
    );
    h.service()
        .create_response(&ctx(), body("m"))
        .await
        .expect("succeeds");

    let contexts = h.plugin.contexts();
    let call = &contexts[0];
    assert_eq!(
        call.api_key.as_ref().map(ExposeSecret::expose_secret),
        Some(SECRET_VALUE)
    );
    assert!(!format!("{call:?}").contains(SECRET_VALUE));
}

#[tokio::test]
async fn errors_never_contain_the_key_even_when_the_plugin_fails() {
    let mut h = Harness::new();
    h.broker = FakeBroker::with(
        || Ok(Some(SecretString::from(SECRET_VALUE.to_owned()))),
        h.log.clone(),
    );
    h.set_plugin(FakePlugin::with(
        |_| {
            Err(LlmGatewayError::provider(
                ProviderErrorCategory::AuthenticationFailed,
                "provider rejected the key",
                ProviderFailure::default(),
            ))
        },
        h.log.clone(),
        false,
    ));
    let err = h
        .service()
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("plugin fails");

    assert_eq!(err.code(), "authentication_failed");
    assert_eq!(
        h.plugin.calls(),
        1,
        "the plugin ran with the key in its context"
    );
    assert!(!format!("{err:?}").contains(SECRET_VALUE));
    assert!(!err.to_string().contains(SECRET_VALUE));
}

#[tokio::test]
async fn broker_failure_is_credentials_unavailable_and_plugin_is_not_called() {
    let mut h = Harness::new();
    h.broker = FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );
    let err = h
        .service()
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("credentials fail");

    assert!(
        matches!(err, LlmGatewayError::CredentialsUnavailable),
        "{err}"
    );
    assert_eq!(h.broker.calls(), 1);
    assert_eq!(h.plugin.calls(), 0);
}

// ── plugin errors ───────────────────────────────────────────────────────────

#[tokio::test]
async fn plugin_error_is_returned_unchanged_and_the_call_is_not_retried() {
    let mut h = Harness::new();
    h.set_plugin(FakePlugin::with(
        |_| {
            Err(LlmGatewayError::provider(
                ProviderErrorCategory::RateLimited,
                "slow down",
                ProviderFailure::default()
                    .with_provider_code("rate_limit_exceeded")
                    .with_retry_after(7),
            ))
        },
        h.log.clone(),
        false,
    ));
    let err = h
        .service()
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("plugin fails");

    match err {
        LlmGatewayError::Provider {
            category,
            message,
            details,
            ..
        } => {
            assert_eq!(category, ProviderErrorCategory::RateLimited);
            assert_eq!(message, "slow down");
            assert_eq!(
                details.provider_code.as_deref(),
                Some("rate_limit_exceeded")
            );
            assert_eq!(details.retry_after, Some(7));
        }
        other => panic!("expected the provider error unchanged, got {other}"),
    }
    assert_eq!(h.plugin.calls(), 1, "the service never retries the plugin");
}

#[tokio::test]
async fn retryable_provider_failure_is_still_a_single_plugin_call() {
    let mut h = Harness::new();
    h.set_plugin(FakePlugin::with(
        |_| {
            Err(LlmGatewayError::provider(
                ProviderErrorCategory::ProviderUnavailable,
                "overloaded",
                ProviderFailure::default(),
            ))
        },
        h.log.clone(),
        false,
    ));
    let err = h
        .service()
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("plugin fails");

    assert_eq!(err.code(), "provider_unavailable");
    assert_eq!(h.plugin.calls(), 1);
}

// ── local client ────────────────────────────────────────────────────────────

#[tokio::test]
async fn local_client_delegates_create_response_to_the_service() {
    let h = Harness::new();
    let client = LlmGatewayLocalClient::new(h.service());
    let response = client
        .create_response(&ctx(), body("m"))
        .await
        .expect("delegates");

    assert_eq!(response.id, h.plugin.contexts()[0].request_id);
    assert_eq!(h.pdp.calls(), 1);
}

#[tokio::test]
async fn local_client_surfaces_service_errors() {
    let mut h = Harness::new();
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let client = LlmGatewayLocalClient::new(h.service());
    let err = client
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("denied");

    assert!(matches!(err, LlmGatewayError::PermissionDenied), "{err}");
}

#[tokio::test]
async fn local_client_streaming_without_model_support_is_capability_not_supported() {
    let h = Harness::new();
    let client = LlmGatewayLocalClient::new(h.service());
    let err = client
        .create_response_stream(&ctx(), body("m"))
        .await
        .err()
        .expect("the default model has no streaming capability");

    assert_eq!(err.code(), "capability_not_supported");
    assert_eq!(
        h.pdp.calls(),
        1,
        "streaming requests are authorized first (F-006)"
    );
}

// ── cancellation ────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_token_is_cancelled_after_a_normal_return() {
    let h = Harness::new();
    h.service()
        .create_response(&ctx(), body("m"))
        .await
        .expect("sync create_response succeeds");

    let contexts = h.plugin.contexts();
    assert!(
        contexts[0].cancel.is_cancelled(),
        "the per-request token is cancelled when the call ends"
    );
}

#[tokio::test]
async fn dropping_the_request_cancels_the_token_the_plugin_holds() {
    let mut h = Harness::new();
    let gated = FakePlugin::with(
        |c| Ok(sample_response(&c.request_id, "gpt-4o")),
        h.log.clone(),
        true,
    );
    h.set_plugin(gated.clone());
    let svc = h.service();

    let task = tokio::spawn(async move { svc.create_response(&ctx(), body("m")).await });
    tokio::time::timeout(Duration::from_secs(5), gated.entered.notified())
        .await
        .expect("the request reached the plugin");
    let token = gated.contexts()[0].cancel.clone();
    assert!(!token.is_cancelled(), "not cancelled while in flight");

    task.abort();
    task.await.expect_err("the aborted task was cancelled");

    assert!(
        token.is_cancelled(),
        "dropping the request future cancels the token"
    );
}

#[tokio::test]
async fn cancel_token_is_cancelled_after_a_failed_call() {
    let mut h = Harness::new();
    h.set_plugin(FakePlugin::with(
        |_| Err(LlmGatewayError::internal("boom")),
        h.log.clone(),
        false,
    ));
    h.service()
        .create_response(&ctx(), body("m"))
        .await
        .expect_err("plugin fails");
    assert!(h.plugin.contexts()[0].cancel.is_cancelled());
}

// ── failure logging ─────────────────────────────────────────────────────────

#[tokio::test]
#[tracing_test::traced_test]
async fn internal_failure_logs_the_source_chain_and_response_id() {
    let mut h = Harness::new();
    h.set_plugin(FakePlugin::with(
        |_| {
            Err(LlmGatewayError::internal_from(
                "x",
                std::io::Error::other("disk exploded"),
            ))
        },
        h.log.clone(),
        false,
    ));
    let failure = h
        .service()
        .create_response_traced(&ctx(), body("m"))
        .await
        .expect_err("plugin fails");

    assert!(logs_contain("disk exploded"), "source text is logged");
    assert!(logs_contain(&failure.response_id), "response id is logged");
    assert!(logs_contain("ERROR"), "internal failures log at error");
    assert_eq!(failure.error.to_response_error().message, "internal error");
}

#[tokio::test]
#[tracing_test::traced_test]
async fn caller_side_failure_logs_at_info_without_error_level() {
    let mut h = Harness::new();
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let failure = h
        .service()
        .create_response_traced(&ctx(), body("m"))
        .await
        .expect_err("denied");
    assert!(logs_contain(&failure.response_id));
    assert!(logs_contain("permission_denied"));
    assert!(!logs_contain("ERROR"));
    assert!(!logs_contain("WARN"));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn provider_failure_logs_truncated_escaped_message_and_identifiers() {
    let mut h = Harness::new();
    let message = format!("{}\nFORGED log line", "S".repeat(5000));
    h.set_plugin(FakePlugin::with(
        move |_| {
            Err(LlmGatewayError::provider(
                ProviderErrorCategory::ContextLengthExceeded,
                message.clone(),
                ProviderFailure::default()
                    .with_provider_code("ctx_len")
                    .with_provider_request_id("req-abc-123"),
            ))
        },
        h.log.clone(),
        false,
    ));
    let failure = h
        .service()
        .create_response_traced(&ctx(), body("m"))
        .await
        .expect_err("plugin fails");

    assert!(logs_contain(&failure.response_id));
    assert!(logs_contain("req-abc-123"));
    assert!(logs_contain("ctx_len"));
    assert!(!logs_contain(&"S".repeat(1025)), "message is truncated");
    assert!(!logs_contain("\nFORGED"), "newline is escaped");
}
