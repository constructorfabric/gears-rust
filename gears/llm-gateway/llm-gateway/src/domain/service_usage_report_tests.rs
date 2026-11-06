//! Slice 5b pipeline tests: the usage report handed to the Usage Reporter
//! carries the `provider_request_id` the plugin captured, on every terminal
//! path of sync, stream and embeddings calls (DESIGN §3.2 Usage Reporter
//! record metadata, `fr-request-traceability`).

use futures_util::StreamExt as _;
use llm_gateway_sdk::models::streaming::ResponseSnapshotEvent;
use llm_gateway_sdk::{
    EmbeddingInput, EmbeddingRequest, EmbeddingResponse, LlmGatewayError, ProviderDetails,
    ProviderErrorCategory, ProviderFailure, ResponseStatus, StreamingEvent, Usage,
};
use rust_decimal::Decimal;

use crate::domain::ports::UsageReport;
use crate::domain::test_support::{
    FakePlugin, Harness, StreamScript, body, ctx, ev_completed, eventually,
    sample_embedding_response, sample_response, set_cost, snapshot,
};

const MODEL: &str = "openai::gpt-4o";
const EMB_MODEL: &str = "openai::text-embedding";

fn usage() -> Usage {
    Usage::reported(100, 50, Decimal::new(4, 4))
}

fn details(request_id: &str) -> ProviderDetails {
    ProviderDetails::default().with_provider_request_id(request_id)
}

fn priced() -> Harness {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
    h
}

fn streaming(events: Vec<StreamingEvent>) -> Harness {
    let mut h = Harness::new();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    h.plugin.set_stream(StreamScript::Events(events));
    h
}

fn embeddings() -> Harness {
    let mut h = Harness::new();
    h.enable_embeddings_with(|v| set_cost(v, Some(4000), None));
    h
}

fn embedding_request() -> EmbeddingRequest {
    EmbeddingRequest::new(EMB_MODEL, EmbeddingInput::Single("hi".to_owned()))
}

/// The single report of the request, once it arrived.
async fn the_report(h: &Harness) -> UsageReport {
    eventually("usage report", || !h.sink.reports().is_empty()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let reports = h.sink.reports();
    assert_eq!(reports.len(), 1, "one report per request");
    reports[0].clone()
}

async fn run_stream(h: &Harness) {
    let stream = h
        .service()
        .create_response_stream(&ctx(), body(MODEL))
        .await
        .expect("stream starts");
    let _ = stream.collect::<Vec<_>>().await;
}

// ── sync ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_sync_response_reports_the_provider_request_id_of_its_provider_details() {
    let mut h = priced();
    h.set_plugin(FakePlugin::with(
        |c| {
            let mut resource = sample_response(&c.request_id, "gpt-4o");
            resource.usage = Some(usage());
            resource.provider_details = Some(details("req_sync"));
            Ok(resource)
        },
        h.log.clone(),
        false,
    ));
    h.service()
        .create_response(&ctx(), body(MODEL))
        .await
        .expect("response");

    let report = the_report(&h).await;
    assert_eq!(report.provider_request_id.as_deref(), Some("req_sync"));
    assert_eq!(report.response_id, h.plugin.contexts()[0].request_id);
}

#[tokio::test]
async fn a_sync_response_without_provider_details_reports_no_provider_request_id() {
    let h = priced();
    h.service()
        .create_response(&ctx(), body(MODEL))
        .await
        .expect("response");
    assert_eq!(the_report(&h).await.provider_request_id, None);
}

#[tokio::test]
async fn a_sync_provider_failure_reports_the_provider_request_id_of_the_error() {
    let mut h = priced();
    h.set_plugin(FakePlugin::with(
        |_| {
            Err(LlmGatewayError::provider(
                ProviderErrorCategory::ProviderUnavailable,
                "upstream down",
                ProviderFailure::default().with_provider_request_id("req_failed"),
            ))
        },
        h.log.clone(),
        false,
    ));
    let err = h
        .service()
        .create_response(&ctx(), body(MODEL))
        .await
        .expect_err("provider failure");
    assert_eq!(err.code(), "provider_unavailable");

    let report = the_report(&h).await;
    assert_eq!(report.provider_request_id.as_deref(), Some("req_failed"));
    assert_eq!(report.quantity_micro_credits, 0);
}

// ── stream ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_completed_stream_reports_the_provider_request_id_of_the_terminal_snapshot() {
    let mut terminal = snapshot("p", ResponseStatus::Completed, Some(usage()));
    terminal.provider_details = Some(details("req_stream"));
    let h = streaming(vec![StreamingEvent::Completed(ResponseSnapshotEvent {
        sequence_number: 0,
        response: terminal,
    })]);
    run_stream(&h).await;

    let report = the_report(&h).await;
    assert_eq!(report.provider_request_id.as_deref(), Some("req_stream"));
    assert_eq!(report.response_id, h.plugin.contexts()[0].request_id);
}

#[tokio::test]
async fn a_failed_stream_reports_the_provider_request_id_of_the_failed_snapshot() {
    let mut failed = snapshot("p", ResponseStatus::Failed, None);
    failed.provider_details = Some(details("req_stream_failed"));
    let h = streaming(vec![StreamingEvent::Failed(ResponseSnapshotEvent {
        sequence_number: 0,
        response: failed,
    })]);
    run_stream(&h).await;

    assert_eq!(
        the_report(&h).await.provider_request_id.as_deref(),
        Some("req_stream_failed")
    );
}

#[tokio::test]
async fn a_stream_whose_terminal_has_no_provider_details_reports_none() {
    let h = streaming(vec![ev_completed(0, "p", Some(usage()))]);
    run_stream(&h).await;
    assert_eq!(the_report(&h).await.provider_request_id, None);
}

// ── embeddings ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_embedding_reports_the_provider_request_id_of_its_provider_details() {
    let h = embeddings();
    h.plugin.set_embedding(|c| {
        let response: EmbeddingResponse =
            sample_embedding_response(&c.request_id, "text-embedding")
                .with_provider_details(details("req_emb"));
        Ok(response)
    });
    h.service()
        .create_embedding(&ctx(), embedding_request())
        .await
        .expect("embedding");

    let report = the_report(&h).await;
    assert_eq!(report.provider_request_id.as_deref(), Some("req_emb"));
    assert!(report.response_id.starts_with("emb_"));
}

#[tokio::test]
async fn an_embedding_provider_failure_reports_the_provider_request_id_of_the_error() {
    let h = embeddings();
    h.plugin.set_embedding(|_| {
        Err(LlmGatewayError::provider(
            ProviderErrorCategory::ProviderUnavailable,
            "upstream down",
            ProviderFailure::default().with_provider_request_id("req_emb_failed"),
        ))
    });
    h.service()
        .create_embedding(&ctx(), embedding_request())
        .await
        .expect_err("provider failure");

    assert_eq!(
        the_report(&h).await.provider_request_id.as_deref(),
        Some("req_emb_failed")
    );
}

#[tokio::test]
async fn an_embedding_without_provider_details_reports_none() {
    let h = embeddings();
    h.service()
        .create_embedding(&ctx(), embedding_request())
        .await
        .expect("embedding");
    assert_eq!(the_report(&h).await.provider_request_id, None);
}
