#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The remote provider against the published contract, through a mock
//! endpoint — ADR-0004: *"contract tests run all three plugins (ONNX, remote
//! via mock server, fake) against the provider contract"*.
//!
//! The mock speaks the `OpenAI` `/embeddings` protocol and answers with a
//! deterministic hash of each input, at the width the request asks for, so
//! the contract's determinism and alignment clauses are testable without a
//! credential or a network.

use std::time::Duration;

use graph_storage_sdk::models::RemainingBudget;
use graph_storage_sdk::plugin_api::{EmbedRequest, EmbeddingProviderError, EmbeddingProviderV1};
use remote_embedding_plugin::{RemoteEmbeddingProvider, RemoteProviderConfig};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// An endpoint that embeds by hashing: one lane per byte of a rolling sum, at
/// the requested width (or 8 when none is requested).
struct HashingEndpoint {
    /// Return this many vectors for a batch of N, to test short answers.
    drop_last: bool,
    /// Ignore the requested width and answer at this one instead.
    force_width: Option<usize>,
}

impl Respond for HashingEndpoint {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).expect("a JSON body");
        let inputs = body["input"].as_array().expect("an input array");
        let width = self.force_width.unwrap_or_else(|| {
            body["dimensions"]
                .as_u64()
                .and_then(|d| usize::try_from(d).ok())
                .unwrap_or(8)
        });
        let mut data: Vec<Value> = inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let text = input.as_str().expect("string inputs");
                let vector: Vec<f32> = (0..width)
                    .map(|lane| {
                        let lane = u32::try_from(lane).expect("small lane index");
                        let sum: u32 = text
                            .bytes()
                            .enumerate()
                            .map(|(i, b)| {
                                let i = u32::try_from(i).expect("short input");
                                u32::from(b).wrapping_mul(i + lane + 1)
                            })
                            .fold(0, u32::wrapping_add);
                        let bucket = u16::try_from(sum % 1000).expect("under 1000");
                        f32::from(bucket) / 1000.0 + 0.001
                    })
                    .collect();
                json!({ "object": "embedding", "index": index, "embedding": vector })
            })
            .collect();
        if self.drop_last {
            data.pop();
        }
        // Reverse order on purpose: alignment must come from `index`.
        data.reverse();
        ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": data,
            "model": body["model"],
            "usage": { "prompt_tokens": 0, "total_tokens": 0 }
        }))
    }
}

async fn endpoint(responder: HashingEndpoint) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(responder)
        .mount(&server)
        .await;
    server
}

fn provider_for(
    server: &MockServer,
    adjust: impl FnOnce(&mut RemoteProviderConfig),
) -> RemoteEmbeddingProvider {
    let mut config =
        RemoteProviderConfig::new(format!("{}/v1", server.uri()), "test-embedding-model")
            .with_api_key("test-key");
    config.dimension = 8;
    adjust(&mut config);
    RemoteEmbeddingProvider::new(config).expect("a valid configuration")
}

fn request(inputs: &[&str]) -> EmbedRequest {
    EmbedRequest {
        inputs: inputs.iter().map(|t| (*t).to_owned()).collect(),
        budget: RemainingBudget::starting_now(Duration::from_secs(30)),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn the_remote_provider_honours_the_contract() {
    let server = endpoint(HashingEndpoint {
        drop_last: false,
        force_width: None,
    })
    .await;
    let provider = provider_for(&server, |_| {});
    graph_storage_sdk::contract::assert_embedding_provider(&provider).await;
}

#[tokio::test]
async fn a_batch_larger_than_the_request_size_is_split_and_stays_aligned() {
    let server = endpoint(HashingEndpoint {
        drop_last: false,
        force_width: None,
    })
    .await;
    let provider = provider_for(&server, |c| c.batch_size = 2);
    let inputs = ["one", "two", "three", "four", "five"];
    let batched = provider
        .embed(request(&inputs))
        .await
        .expect("embeds")
        .vectors;

    let whole = provider_for(&server, |c| c.batch_size = 100);
    let single = whole.embed(request(&inputs)).await.expect("embeds").vectors;

    assert_eq!(batched.len(), inputs.len());
    assert_eq!(batched, single, "splitting must not move any vector");
    assert_eq!(server.received_requests().await.expect("recorded").len(), 4);
}

#[tokio::test]
async fn a_short_answer_is_an_error_not_a_shifted_vector() {
    let server = endpoint(HashingEndpoint {
        drop_last: true,
        force_width: None,
    })
    .await;
    let provider = provider_for(&server, |_| {});
    let error = provider
        .embed(request(&["a", "b", "c"]))
        .await
        .err()
        .expect("a missing vector must fail the batch");
    assert!(
        matches!(error, EmbeddingProviderError::Internal(_)),
        "{error}"
    );
}

#[tokio::test]
async fn a_vector_of_another_width_is_a_space_mismatch() {
    let server = endpoint(HashingEndpoint {
        drop_last: false,
        force_width: Some(16),
    })
    .await;
    let provider = provider_for(&server, |_| {});
    let error = provider
        .embed(request(&["a"]))
        .await
        .err()
        .expect("must fail");
    assert!(
        matches!(error, EmbeddingProviderError::SpaceMismatch),
        "{error}"
    );
}

#[tokio::test]
async fn a_refused_credential_is_unavailable_and_names_no_secret() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"bad key"}"#))
        .mount(&server)
        .await;
    let provider = provider_for(&server, |c| {
        *c = c.clone().with_api_key("sk-secret-value");
    });
    let error = provider
        .embed(request(&["a"]))
        .await
        .err()
        .expect("must fail");
    let rendered = error.to_string();
    assert!(
        matches!(error, EmbeddingProviderError::Unavailable { .. }),
        "{rendered}"
    );
    assert!(rendered.contains("401"), "{rendered}");
    assert!(!rendered.contains("sk-secret-value"), "{rendered}");

    // ADR-0004: a refused credential is not retried, because no repetition
    // resolves it. `max_retries` defaults to 2, so a loop that treated this
    // like a rate limit would have sent three requests -- and would send three
    // for every chunk of every batch for as long as the key stays wrong, which
    // is how a rotated credential turns into provider-side rate limiting.
    let sent = server
        .received_requests()
        .await
        .expect("the mock server records what it was sent");
    assert_eq!(
        sent.len(),
        1,
        "a refused credential must cost exactly one request, not {}",
        sent.len()
    );
}

#[tokio::test]
async fn an_exhausted_budget_is_a_deadline_before_any_request() {
    let server = endpoint(HashingEndpoint {
        drop_last: false,
        force_width: None,
    })
    .await;
    let provider = provider_for(&server, |_| {});
    let error = provider
        .embed(EmbedRequest {
            inputs: vec!["a".to_owned()],
            budget: RemainingBudget::starting_now(Duration::ZERO),
            cancel: CancellationToken::new(),
        })
        .await
        .err()
        .expect("must fail");
    assert!(matches!(error, EmbeddingProviderError::Deadline), "{error}");
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty()
    );
}

#[tokio::test]
async fn the_dimensions_field_follows_the_configuration() {
    let server = endpoint(HashingEndpoint {
        drop_last: false,
        force_width: Some(8),
    })
    .await;
    let asking = provider_for(&server, |c| c.request_dimensions = true);
    asking.embed(request(&["a"])).await.expect("embeds");
    let silent = provider_for(&server, |c| c.request_dimensions = false);
    silent.embed(request(&["a"])).await.expect("embeds");

    let bodies: Vec<Value> = server
        .received_requests()
        .await
        .expect("recorded")
        .iter()
        .map(|r| serde_json::from_slice(&r.body).expect("json"))
        .collect();
    assert_eq!(bodies[0]["dimensions"], json!(8));
    assert!(bodies[1].get("dimensions").is_none(), "{}", bodies[1]);
    assert_eq!(bodies[0]["model"], json!("test-embedding-model"));
}

#[tokio::test]
async fn an_empty_input_still_gets_its_own_vector() {
    let server = endpoint(HashingEndpoint {
        drop_last: false,
        force_width: None,
    })
    .await;
    let provider = provider_for(&server, |_| {});
    let vectors = provider
        .embed(request(&["", "text", "  "]))
        .await
        .expect("embeds")
        .vectors;
    assert_eq!(vectors.len(), 3);
    assert_eq!(vectors[0], vectors[2], "blank inputs share one placeholder");
    let sent: Value =
        serde_json::from_slice(&server.received_requests().await.expect("recorded")[0].body)
            .expect("json");
    assert_ne!(
        sent["input"][0],
        json!(""),
        "the endpoint never sees an empty string"
    );
}

/// A gateway having a bad second does not cost an ingest batch its vectors.
///
/// One 503 used to drop the whole chunk, and the nodes in it stayed
/// unembedded until something touched them again — a quiet loss of recall
/// rather than a visible failure. The endpoint here refuses twice and then
/// answers; the provider must come back with vectors.
#[tokio::test]
async fn a_transient_refusal_is_retried_within_the_budget() {
    let server = MockServer::start().await;
    // Two refusals, then the real answer. wiremock serves mounts in order and
    // `up_to_n_times` retires one after its quota, so this is a script rather
    // than a race.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(HashingEndpoint {
            drop_last: false,
            force_width: None,
        })
        .mount(&server)
        .await;

    let provider = provider_for(&server, |_| {});
    let response = provider
        .embed(request(&["a", "b"]))
        .await
        .expect("the third attempt answers");
    assert_eq!(response.vectors.len(), 2);
}

/// A timeout is not one condition, and the two it can be want opposite
/// answers.
///
/// The per-attempt timeout is `min(caller's remaining budget, our configured
/// timeout)`. When ours is the smaller one -- the ordinary case, since the
/// default is a minute and an ingest batch is given far more -- a timeout
/// says the endpoint is slow, not that the caller is out of time, and the
/// caller may still have minutes of budget to retry into. Treating it as the
/// caller's deadline abandoned the whole batch on the first slow response,
/// which is the quiet loss of recall the retry loop was added to prevent.
#[tokio::test]
async fn a_slow_endpoint_is_retried_while_the_caller_still_has_budget() {
    let server = MockServer::start().await;
    // Answers correctly, but later than our own timeout allows.
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{}")
                .set_delay(Duration::from_millis(400)),
        )
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(HashingEndpoint {
            drop_last: false,
            force_width: None,
        })
        .mount(&server)
        .await;

    // Ours binds: 50ms against the request helper's 30-second budget.
    let provider = provider_for(&server, |config| {
        config.timeout = Duration::from_millis(50);
    });
    let response = provider
        .embed(request(&["a"]))
        .await
        .expect("a slow endpoint is transient, and the third attempt is quick");
    assert_eq!(response.vectors.len(), 1);
    assert_eq!(
        server.received_requests().await.map(|r| r.len()),
        Some(3),
        "the two slow attempts must have been retried, not given up on"
    );
}

/// The other half of the same rule: once the caller's budget is what ran out,
/// there is nothing to retry into and the answer is its deadline.
#[tokio::test]
async fn a_timeout_on_the_callers_own_budget_is_a_deadline() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{}")
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;

    // The caller's budget binds: 150ms against a ten-second configuration.
    let provider = provider_for(&server, |config| {
        config.timeout = Duration::from_secs(10);
    });
    let error = provider
        .embed(EmbedRequest {
            inputs: vec!["a".to_owned()],
            budget: RemainingBudget::starting_now(Duration::from_millis(150)),
            cancel: CancellationToken::new(),
        })
        .await
        .err()
        .expect("the caller's deadline is not something a retry can fix");
    assert!(matches!(error, EmbeddingProviderError::Deadline), "{error}");
    assert_eq!(
        server.received_requests().await.map(|r| r.len()),
        Some(1),
        "an exhausted caller budget must not be retried into"
    );
}

/// Retries are bounded, and a refusal that outlives them is reported.
#[tokio::test]
async fn a_persistent_refusal_gives_up_and_says_so() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
        .mount(&server)
        .await;

    let provider = provider_for(&server, |config| config.max_retries = 1);
    let error = provider
        .embed(request(&["a"]))
        .await
        .err()
        .expect("a persistently unwell endpoint fails the batch");
    assert!(
        matches!(error, EmbeddingProviderError::Unavailable { .. }),
        "{error}"
    );
    // Two requests: the attempt and its one retry.
    assert_eq!(server.received_requests().await.map(|r| r.len()), Some(2));
}
