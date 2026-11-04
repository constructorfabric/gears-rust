//! Slice 3 pipeline tests: `create_response_stream` pre-stream pipeline,
//! in-flight stream slots, stream relay and cancellation, and the local
//! client (DESIGN §3.2 Request Pipeline / Stream Supervisor, §3.3 wire
//! behavior, `cpt-cf-llm-gateway-seq-streaming-v1`,
//! `cpt-cf-llm-gateway-seq-stream-cancellation-v1`).

use std::time::Duration;

use futures_util::StreamExt as _;
use llm_gateway_sdk::models::core::ResponseStatus;
use llm_gateway_sdk::{
    CreateResponseBody, InputItem, ItemReference, LlmGatewayClientV1, LlmGatewayError,
    ProviderErrorCategory, ProviderFailure, ResponseEventStream, ResponseInput, StreamingEvent,
};
use serde_json::json;

use crate::domain::LlmGatewayLocalClient;
use crate::domain::test_support::{
    FakeBroker, FakeModels, FakePdp, Harness, OPENAI_TYPE, PdpMode, StreamScript, body, ctx,
    ev_completed, ev_error, ev_text_delta, sample_model_info_patched,
};

const MODEL: &str = "openai::gpt-4o";

/// Harness whose model and plugin both support streaming and whose plugin
/// streams `events` then ends.
fn streaming_harness(events: Vec<StreamingEvent>) -> Harness {
    let mut h = Harness::new();
    h.enable_streaming();
    h.plugin.set_stream(StreamScript::Events(events));
    h
}

async fn collect(stream: ResponseEventStream) -> Vec<StreamingEvent> {
    tokio::time::timeout(Duration::from_secs(2), stream.collect::<Vec<_>>())
        .await
        .expect("the stream ends")
}

fn kinds(events: &[StreamingEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            serde_json::to_value(e).expect("event serializes")["type"]
                .as_str()
                .expect("type")
                .to_owned()
        })
        .collect()
}

#[allow(clippy::needless_pass_by_value)] // call sites build the patch inline with `json!`
fn request_with(patch: serde_json::Value) -> CreateResponseBody {
    let mut value = serde_json::to_value(body(MODEL)).expect("body serializes");
    for (key, field) in patch.as_object().expect("patch is an object") {
        value[key] = field.clone();
    }
    serde_json::from_value(value).expect("request deserializes")
}

// ── happy path ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn stream_relays_created_in_progress_deltas_and_the_terminal() {
    let h = streaming_harness(vec![
        ev_text_delta(0, "he"),
        ev_text_delta(1, "llo"),
        ev_completed(2, "provider_id", None),
    ]);

    let stream = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let events = collect(stream).await;

    assert_eq!(
        kinds(&events),
        [
            "response.created",
            "response.in_progress",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.completed"
        ]
    );
}

#[tokio::test]
async fn stream_snapshots_carry_the_response_id_the_plugin_was_given() {
    let h = streaming_harness(vec![ev_completed(0, "provider_id", None)]);

    let stream = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let events = collect(stream).await;

    let request_id = h.plugin.contexts()[0].request_id.clone();
    assert!(request_id.starts_with("resp_"), "{request_id}");
    for event in &events {
        if let StreamingEvent::Created(e)
        | StreamingEvent::InProgress(e)
        | StreamingEvent::Completed(e) = event
        {
            assert_eq!(e.response.id, request_id);
        }
    }
    assert_eq!(h.plugin.stream_calls(), 1);
    assert_eq!(h.plugin.calls(), 0, "the sync entry point is not used");
}

#[tokio::test]
async fn plugin_receives_the_effective_body_and_api_key() {
    let mut h = streaming_harness(vec![ev_completed(0, "p", None)]);
    h.broker = FakeBroker::with(
        || Ok(Some(secrecy::SecretString::from("sk-stream"))),
        h.log.clone(),
    );

    let stream = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let _ = collect(stream).await;

    let contexts = h.plugin.contexts();
    assert!(contexts[0].api_key.is_some());
    assert_eq!(h.plugin.bodies()[0].model, MODEL);
}

#[tokio::test]
async fn pre_stream_stages_run_in_pipeline_order() {
    let h = streaming_harness(vec![ev_completed(0, "p", None)]);

    let stream = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let _ = collect(stream).await;

    assert_eq!(
        h.log.entries(),
        vec!["authz", "models", "plugins", "credentials", "plugin"]
    );
}

#[tokio::test]
async fn provider_error_event_is_an_in_band_failed_terminal_not_an_err() {
    let h = streaming_harness(vec![ev_error(0, "provider_unavailable")]);

    let stream = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("the stream starts; the failure is in-band");
    let events = collect(stream).await;

    assert!(matches!(events.last(), Some(StreamingEvent::Failed(_))));
    assert!(!kinds(&events).iter().any(|k| k == "error"));
}

// ── authorization ───────────────────────────────────────────────────────────

#[tokio::test]
async fn pep_checks_create_response_before_any_other_dependency() {
    let h = streaming_harness(vec![ev_completed(0, "p", None)]);
    let stream = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let _ = collect(stream).await;

    let request = h.pdp.last_request().expect("PDP asked");
    assert_eq!(request.action.name, "create_response");
    assert_eq!(h.log.entries()[0], "authz");
}

#[tokio::test]
async fn denied_stream_request_is_permission_denied_and_calls_nothing_else() {
    let mut h = streaming_harness(Vec::new());
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());

    let err = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("denied");

    assert!(matches!(err, LlmGatewayError::PermissionDenied), "{err:?}");
    assert_eq!(h.models.calls(), 0);
    assert_eq!(h.plugin.stream_calls(), 0);
}

#[tokio::test]
async fn stream_true_requests_are_authorized_even_when_streaming_is_unsupported() {
    // Backlog F-006: streaming used to be rejected before the PEP check.
    let h = Harness::new(); // model without `streaming`
    let _ = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("unsupported");

    assert_eq!(h.pdp.calls(), 1);
}

// ── pre-stream failures are Err ─────────────────────────────────────────────

#[tokio::test]
async fn pre_stream_failure_carries_the_allocated_response_id() {
    let mut h = streaming_harness(Vec::new());
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());

    let failure = h
        .service()
        .create_response_stream_traced(&ctx(), body(MODEL))
        .await
        .err()
        .expect("denied");

    assert!(failure.response_id.starts_with("resp_"));
    assert!(matches!(failure.error, LlmGatewayError::PermissionDenied));
}

#[tokio::test]
async fn item_reference_is_rejected_before_model_resolution() {
    let h = streaming_harness(Vec::new());
    let mut request = body(MODEL);
    request.input = Some(ResponseInput::Items(vec![InputItem::ItemReference(
        ItemReference {
            id: "item_1".to_owned(),
        },
    )]));

    let err = h
        .service()
        .create_response_stream(&ctx(), request)
        .await
        .err()
        .expect("rejected");

    assert!(
        matches!(err, LlmGatewayError::CapabilityNotSupported { .. }),
        "{err:?}"
    );
    assert_eq!(h.models.calls(), 0);
}

#[tokio::test]
async fn unknown_model_is_an_err() {
    let mut h = streaming_harness(Vec::new());
    h.models = FakeModels::with(
        || Err(LlmGatewayError::model_not_found("nope")),
        h.log.clone(),
    );

    let err = h
        .service()
        .create_response_stream(&ctx(), body("nope"))
        .await
        .err()
        .expect("unknown model");

    assert!(
        matches!(err, LlmGatewayError::ModelNotFound { .. }),
        "{err:?}"
    );
    assert_eq!(h.plugin.stream_calls(), 0);
}

#[tokio::test]
async fn model_without_streaming_capability_is_capability_not_supported() {
    let mut h = Harness::new();
    h.plugin.set_streaming_transport(true);
    h.models = FakeModels::found(
        sample_model_info_patched(OPENAI_TYPE, |v| {
            v["capabilities"]["streaming"] = json!(false);
        }),
        h.log.clone(),
    );

    let err = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("unsupported");

    assert!(
        matches!(err, LlmGatewayError::CapabilityNotSupported { .. }),
        "{err:?}"
    );
    assert_eq!(h.plugin.stream_calls(), 0);
    assert_eq!(h.broker.calls(), 0);
}

#[tokio::test]
async fn plugin_without_streaming_transport_is_capability_not_supported() {
    let mut h = Harness::new();
    h.enable_streaming();
    h.plugin.set_streaming_transport(false);

    let err = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("unsupported");

    assert!(
        matches!(err, LlmGatewayError::CapabilityNotSupported { .. }),
        "{err:?}"
    );
    assert_eq!(h.plugin.stream_calls(), 0);
    assert_eq!(h.broker.calls(), 0);
}

#[tokio::test]
async fn capability_checks_of_the_sync_path_apply_to_streams() {
    let mut h = streaming_harness(Vec::new());
    h.rules = crate::domain::test_support::FakeRules {
        unsupported_params: ["temperature"].into(),
        ..crate::domain::test_support::FakeRules::default()
    };

    let err = h
        .service()
        .create_response_stream(&ctx(), request_with(json!({ "temperature": 0.5 })))
        .await
        .err()
        .expect("rejected parameter");

    assert!(
        matches!(err, LlmGatewayError::UnsupportedParameter { .. }),
        "{err:?}"
    );
    assert_eq!(h.plugin.stream_calls(), 0);
    assert_eq!(h.broker.calls(), 0);
}

#[tokio::test]
async fn credential_failure_is_an_err_and_the_plugin_is_not_called() {
    let mut h = streaming_harness(Vec::new());
    h.broker = FakeBroker::with(
        || Err(LlmGatewayError::CredentialsUnavailable),
        h.log.clone(),
    );

    let err = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("credentials unavailable");

    assert!(
        matches!(err, LlmGatewayError::CredentialsUnavailable),
        "{err:?}"
    );
    assert_eq!(h.plugin.stream_calls(), 0);
}

#[tokio::test]
async fn plugin_start_failure_is_an_err_with_the_provider_category() {
    let h = streaming_harness(Vec::new());
    h.plugin.set_stream(StreamScript::StartError(|| {
        LlmGatewayError::provider(
            ProviderErrorCategory::RateLimited,
            "slow down",
            ProviderFailure::default(),
        )
    }));

    let err = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("plugin failed before the stream started");

    assert!(
        matches!(
            err,
            LlmGatewayError::Provider {
                category: ProviderErrorCategory::RateLimited,
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn ignored_parameters_are_attached_to_the_terminal_snapshot() {
    let mut h = streaming_harness(vec![ev_completed(0, "p", None)]);
    h.rules = crate::domain::test_support::FakeRules {
        unsupported_params: ["top_p"].into(),
        ..crate::domain::test_support::FakeRules::default()
    };

    let stream = h
        .service()
        .create_response_stream(
            &ctx(),
            request_with(json!({ "top_p": 0.4, "drop_unsupported_params": true })),
        )
        .await
        .expect("stream starts");
    let events = collect(stream).await;

    let Some(StreamingEvent::Completed(terminal)) = events.last() else {
        panic!("expected completed, got {events:?}");
    };
    assert_eq!(terminal.response.ignored_parameters, ["top_p"]);
    assert_eq!(h.plugin.bodies()[0].top_p, None);
}

// ── structured output ───────────────────────────────────────────────────────

fn json_schema_request() -> CreateResponseBody {
    request_with(json!({ "text": { "format": {
        "type": "json_schema", "name": "person", "strict": true,
        "schema": { "type": "object", "properties": { "age": { "type": "integer" } }, "required": ["age"] }
    }}}))
}

#[tokio::test]
async fn structured_output_is_validated_against_the_original_schema() {
    let mut h = streaming_harness(vec![
        ev_text_delta(0, "{\"age\":\"old\"}"),
        ev_completed(1, "p", None),
    ]);
    h.enable_streaming_with(|v| v["capabilities"]["response_schema"] = json!(true));

    let stream = h
        .service()
        .create_response_stream(&ctx(), json_schema_request())
        .await
        .expect("stream starts");
    let events = collect(stream).await;

    let Some(StreamingEvent::Failed(terminal)) = events.last() else {
        panic!("expected failed, got {:?}", kinds(&events));
    };
    assert_eq!(terminal.response.status, ResponseStatus::Failed);
    assert_eq!(
        terminal.response.error.as_ref().map(|e| e.code.as_str()),
        Some("output_validation_error")
    );
}

#[tokio::test]
async fn valid_structured_output_completes() {
    let mut h = streaming_harness(vec![
        ev_text_delta(0, "{\"age\":36}"),
        ev_completed(1, "p", None),
    ]);
    h.enable_streaming_with(|v| v["capabilities"]["response_schema"] = json!(true));

    let stream = h
        .service()
        .create_response_stream(&ctx(), json_schema_request())
        .await
        .expect("stream starts");
    let events = collect(stream).await;

    assert!(matches!(events.last(), Some(StreamingEvent::Completed(_))));
}

// ── in-flight stream slots ──────────────────────────────────────────────────

#[tokio::test]
async fn stream_slot_is_held_until_the_stream_is_dropped() {
    let mut h = streaming_harness(Vec::new());
    h.plugin
        .set_stream(StreamScript::EventsThenHang(vec![ev_text_delta(0, "x")]));
    h.limits.max_inflight_streams = 1;
    let svc = h.service();

    let first = svc
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("first stream");
    let err = svc
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("second stream over the limit");
    assert!(
        matches!(err, LlmGatewayError::GatewayUnavailable),
        "{err:?}"
    );

    drop(first);

    // The slot comes back once the supervisor has wound down.
    let mut second = None;
    for _ in 0..200 {
        if let Ok(stream) = svc.create_response_stream(&ctx(), body(MODEL)).await {
            second = Some(stream);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(second.is_some(), "the slot was not released after the drop");
}

#[tokio::test]
async fn stream_slot_is_released_when_the_stream_ends() {
    let mut h = streaming_harness(vec![ev_completed(0, "p", None)]);
    h.limits.max_inflight_streams = 1;
    let svc = h.service();

    let first = svc
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("first stream");
    let _ = collect(first).await;

    let mut second = None;
    for _ in 0..200 {
        if let Ok(stream) = svc.create_response_stream(&ctx(), body(MODEL)).await {
            second = Some(stream);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(second.is_some(), "the slot was not released at stream end");
}

#[tokio::test]
async fn stream_limit_breach_fails_before_model_resolution() {
    let mut h = streaming_harness(Vec::new());
    h.limits.max_inflight_streams = 0;

    let err = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("over the limit");

    assert!(
        matches!(err, LlmGatewayError::GatewayUnavailable),
        "{err:?}"
    );
    assert_eq!(h.models.calls(), 0);
}

#[tokio::test]
async fn stream_and_sync_limits_are_independent() {
    let mut h = streaming_harness(vec![ev_completed(0, "p", None)]);
    h.limits.max_inflight_sync = 0;
    // Streams are not limited by the sync limit.
    let stream = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts although no sync slot exists");
    let _ = collect(stream).await;

    let mut h = Harness::new();
    h.limits.max_inflight_streams = 0;
    // And sync calls are not limited by the stream limit.
    h.service()
        .create_response(&ctx(), body(MODEL))
        .await
        .expect("sync call succeeds although no stream slot exists");
}

#[tokio::test]
async fn pre_stream_failure_releases_the_stream_slot() {
    let mut h = streaming_harness(Vec::new());
    h.limits.max_inflight_streams = 1;
    h.plugin.set_stream(StreamScript::StartError(|| {
        LlmGatewayError::internal("boom")
    }));
    let svc = h.service();

    let _ = svc
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("plugin failed");
    h.plugin
        .set_stream(StreamScript::Events(vec![ev_completed(0, "p", None)]));

    let stream = svc
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("the slot of the failed attempt was released");
    let _ = collect(stream).await;
}

// ── cancellation ────────────────────────────────────────────────────────────

#[tokio::test]
async fn dropping_the_stream_cancels_the_request_and_the_plugin_stream() {
    let h = streaming_harness(Vec::new());
    h.plugin
        .set_stream(StreamScript::EventsThenHang(vec![ev_text_delta(0, "x")]));

    let mut stream = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let _ = stream.next().await.expect("created");
    let cancel = h.plugin.contexts()[0].cancel.clone();
    assert!(!cancel.is_cancelled());

    drop(stream);

    tokio::time::timeout(Duration::from_secs(2), cancel.cancelled())
        .await
        .expect("the plugin observes the cancellation");
    h.plugin.wait_stream_dropped().await;
}

#[tokio::test]
async fn pre_stream_failure_cancels_the_request_token() {
    let h = streaming_harness(Vec::new());
    h.plugin.set_stream(StreamScript::StartError(|| {
        LlmGatewayError::internal("boom")
    }));

    let _ = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("plugin failed");

    // The call context the plugin saw is cancelled on the failure exit path,
    // like on the sync path.
    assert!(h.plugin.contexts()[0].cancel.is_cancelled());
}

// ── local client ────────────────────────────────────────────────────────────

#[tokio::test]
async fn local_client_streams_through_the_service() {
    let h = streaming_harness(vec![ev_text_delta(0, "hi"), ev_completed(1, "p", None)]);
    let client = LlmGatewayLocalClient::new(h.service());

    let stream = client
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let events = collect(stream).await;

    assert_eq!(
        kinds(&events),
        [
            "response.created",
            "response.in_progress",
            "response.output_text.delta",
            "response.completed"
        ]
    );
}

#[tokio::test]
async fn local_client_returns_pre_stream_failures_as_err() {
    let mut h = streaming_harness(Vec::new());
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let client = LlmGatewayLocalClient::new(h.service());

    let err = client
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .err()
        .expect("denied");

    assert!(matches!(err, LlmGatewayError::PermissionDenied), "{err:?}");
}

#[tokio::test]
async fn local_client_drop_cancels_the_request() {
    let h = streaming_harness(Vec::new());
    h.plugin
        .set_stream(StreamScript::EventsThenHang(Vec::new()));
    let client = LlmGatewayLocalClient::new(h.service());

    let mut stream = client
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let _ = stream.next().await.expect("created");
    let cancel = h.plugin.contexts()[0].cancel.clone();

    drop(stream);

    tokio::time::timeout(Duration::from_secs(2), cancel.cancelled())
        .await
        .expect("cancelled");
}
