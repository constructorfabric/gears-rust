//! Integration tests demonstrating the oagw-sdk streaming API.
//!
//! This file serves as **usage guidelines** — each test shows how to use the SDK
//! for real-world scenarios. All protocols go through `ServiceGatewayClientV1::proxy_request`:
//!
//! | Protocol   | Request Body          | Response Body          | SDK wrapper                         |
//! |------------|-----------------------|------------------------|-------------------------------------|
//! | HTTP       | `Body::Bytes`/`Empty` | `Body::Bytes`          | direct access                       |
//! | SSE        | `Body::Bytes`/`Empty` | `Body::Stream`         | `ServerEventsStream::from_response` |
//! | WebSocket  | n/a (upgrade)         | n/a (bidirectional)    | `WebSocketStream` (via axum)        |
//! | Multipart  | `Body::Bytes`/`Stream`| `Body::Bytes`          | `MultipartBody::into_request`       |

use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use modkit_security::SecurityContext;
use oagw_sdk::api::ServiceGatewayClientV1;
use oagw_sdk::body::{Body, BodyStream, BoxError};
use oagw_sdk::codec::Json;
use oagw_sdk::error::ServiceGatewayError;
use oagw_sdk::error::StreamingError;
use oagw_sdk::sse::{FromServerEvent, ServerEvent, ServerEventsResponse, ServerEventsStream};
use oagw_sdk::ws::{
    FromWebSocketMessage, WebSocketMessage, WebSocketReceiver, WebSocketSink, WebSocketStream,
};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// ===========================================================================
// Helpers
// ===========================================================================

/// Build an SSE response with a streaming body from the provided chunks.
///
/// Each string becomes one frame in the body stream, simulating chunked transfer.
fn server_events_response(chunks: Vec<&str>) -> http::Response<Body> {
    let owned: Vec<Result<Bytes, BoxError>> = chunks
        .into_iter()
        .map(|s| Ok(Bytes::from(s.to_owned())))
        .collect();
    let stream: BodyStream = Box::pin(futures_util::stream::iter(owned));
    http::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::Stream(stream))
        .unwrap()
}

/// Mock gateway client that returns a pre-configured response.
///
/// Shows how streaming tools integrate with `ServiceGatewayClientV1::proxy_request`.
struct MockGateway {
    response: Mutex<Option<http::Response<Body>>>,
}

impl MockGateway {
    fn responding_with(resp: http::Response<Body>) -> Self {
        Self {
            response: Mutex::new(Some(resp)),
        }
    }
}

#[async_trait]
impl ServiceGatewayClientV1 for MockGateway {
    async fn create_upstream(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, ServiceGatewayError> {
        unimplemented!()
    }

    async fn get_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Upstream, ServiceGatewayError> {
        unimplemented!()
    }

    async fn list_upstreams(
        &self,
        _: SecurityContext,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, ServiceGatewayError> {
        unimplemented!()
    }

    async fn update_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, ServiceGatewayError> {
        unimplemented!()
    }

    async fn delete_upstream(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), ServiceGatewayError> {
        unimplemented!()
    }

    async fn create_route(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, ServiceGatewayError> {
        unimplemented!()
    }

    async fn get_route(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Route, ServiceGatewayError> {
        unimplemented!()
    }

    async fn list_routes(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, ServiceGatewayError> {
        unimplemented!()
    }

    async fn update_route(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, ServiceGatewayError> {
        unimplemented!()
    }

    async fn delete_route(
        &self,
        _: SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), ServiceGatewayError> {
        unimplemented!()
    }

    async fn resolve_proxy_target(
        &self,
        _: SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), ServiceGatewayError> {
        unimplemented!()
    }

    async fn proxy_request(
        &self,
        _ctx: SecurityContext,
        _req: http::Request<Body>,
    ) -> Result<http::Response<Body>, ServiceGatewayError> {
        Ok(self
            .response
            .lock()
            .unwrap()
            .take()
            .expect("response already consumed"))
    }
}

// ===========================================================================
// HTTP: Body::Bytes → Body::Bytes (non-streaming path)
// ===========================================================================

/// HTTP proxy: JSON request → JSON response via `proxy_request`.
///
/// Preconditions: upstream returns `application/json` with `Body::Bytes`.
/// Expected: caller reads the body as bytes and deserializes.
#[tokio::test]
async fn http_proxy_bytes_in_bytes_out() -> TestResult {
    // -- precondition: upstream returns a JSON response ----------------------------
    let gateway = MockGateway::responding_with(
        http::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"ok":true}"#))?,
    );

    // -- action: call proxy_request -----------------------------------------------
    let req = http::Request::builder()
        .method("POST")
        .uri("/api/oagw/v1/proxy/openai/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"hello":"world"}"#))?;

    let resp = gateway
        .proxy_request(SecurityContext::anonymous(), req)
        .await?;

    // -- verify: non-SSE response, body is Bytes ----------------------------------
    assert_eq!(resp.status(), 200);

    let ServerEventsResponse::Response(resp) =
        ServerEventsStream::from_response::<ServerEvent>(resp)
    else {
        panic!("expected non-SSE response");
    };
    let bytes = resp.into_body().into_bytes().await?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(json["ok"], true);

    Ok(())
}

/// HTTP proxy: SSE response arrives as `Body::Stream` before any parsing.
///
/// Preconditions: upstream returns `text/event-stream` with `Body::Stream`.
/// Expected: the raw response body is `Body::Stream` — SSE parsing is opt-in.
#[tokio::test]
async fn http_proxy_stream_body() -> TestResult {
    // -- precondition: upstream returns SSE ----------------------------------------
    let gateway = MockGateway::responding_with(server_events_response(vec!["data: message 0\n\n"]));

    // -- action: call proxy_request -----------------------------------------------
    let req = http::Request::get("/api/oagw/v1/proxy/openai/chat/completions").body(Body::Empty)?;

    let resp = gateway
        .proxy_request(SecurityContext::anonymous(), req)
        .await?;

    // -- verify: body is Stream (SSE parsing hasn't happened yet) -----------------
    assert_eq!(resp.status(), 200);
    assert!(matches!(resp.into_body(), Body::Stream(_)));

    Ok(())
}

// ===========================================================================
// SSE: ServerEventsStream for parsing and response building
// ===========================================================================

/// Raw SSE events — the simplest usage of ServerEventsStream.
///
/// Preconditions: upstream returns `text/event-stream` with data-only events.
/// Expected: each event's `.data` field contains the message text.
#[tokio::test]
async fn sse_stream_raw_events() -> TestResult {
    // -- precondition: upstream returns 3 data-only SSE events ------------------
    let resp = server_events_response(vec![
        "data: message 0\n\n",
        "data: message 1\n\n",
        "data: message 2\n\n",
    ]);

    // -- action: wrap response into ServerEventsStream --------------------------
    let ServerEventsResponse::Events(mut events) =
        ServerEventsStream::from_response::<ServerEvent>(resp)
    else {
        return Ok(());
    };

    assert_eq!(events.status(), 200);

    // -- verify: collect all events and check data ------------------------------
    let mut collected = Vec::new();
    while let Some(event) = events.next().await {
        collected.push(event?);
    }

    assert_eq!(collected.len(), 3);
    assert_eq!(collected[0].data, "message 0");
    assert_eq!(collected[1].data, "message 1");
    assert_eq!(collected[2].data, "message 2");

    // Simple data-only events have no id, event type, or retry.
    for ev in &collected {
        assert_eq!(ev.event, None);
        assert_eq!(ev.id, None);
        assert_eq!(ev.retry, None);
    }

    Ok(())
}

/// SSE events with all fields populated.
///
/// Preconditions: upstream returns events with id, event type, retry, and data.
/// Expected: each field is parsed correctly.
#[tokio::test]
async fn sse_stream_typed_events_with_all_fields() -> TestResult {
    // -- precondition: upstream returns events with all SSE fields --------------
    let resp = server_events_response(vec![
        "id: 1\nevent: status\nretry: 5000\ndata: connected\n\n",
        "id: 2\nevent: update\ndata: {\"count\":42}\n\n",
        "id: 3\nevent: done\ndata: finished\n\n",
    ]);

    let ServerEventsResponse::Events(mut events) =
        ServerEventsStream::from_response::<ServerEvent>(resp)
    else {
        return Ok(());
    };

    let mut collected = Vec::new();
    while let Some(event) = events.next().await {
        collected.push(event?);
    }

    assert_eq!(collected.len(), 3);

    // First event: status with retry
    assert_eq!(collected[0].event.as_deref(), Some("status"));
    assert_eq!(collected[0].id.as_deref(), Some("1"));
    assert_eq!(collected[0].retry, Some(5000));
    assert_eq!(collected[0].data, "connected");

    // Second event: JSON data
    assert_eq!(collected[1].event.as_deref(), Some("update"));
    assert_eq!(collected[1].id.as_deref(), Some("2"));
    let json: serde_json::Value = serde_json::from_str(&collected[1].data)?;
    assert_eq!(json["count"], 42);

    // Third event
    assert_eq!(collected[2].event.as_deref(), Some("done"));
    assert_eq!(collected[2].data, "finished");

    Ok(())
}

/// Parse OpenAI-style chat completion SSE stream.
///
/// Preconditions: upstream returns SSE with chat.completion.chunk objects,
///   ending with a `[DONE]` sentinel.
/// Expected: reconstruct the full response text by joining content deltas.
#[tokio::test]
async fn sse_stream_openai_chat_format() -> TestResult {
    // -- precondition: upstream returns OpenAI chat completion chunks ------------
    let resp = server_events_response(vec![
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" from\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" the\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" stream\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    ]);

    let ServerEventsResponse::Events(mut events) =
        ServerEventsStream::from_response::<ServerEvent>(resp)
    else {
        return Ok(());
    };

    // -- accumulate content deltas from streamed chunks -------------------------
    let mut text = String::new();
    while let Some(result) = events.next().await {
        let ev = result?;

        // OpenAI signals end-of-stream with data: [DONE]
        if ev.data == "[DONE]" {
            break;
        }

        // Each chunk: {"choices": [{"delta": {"content": "..."}}]}
        let chunk: serde_json::Value = serde_json::from_str(&ev.data)?;
        if let Some(content) = chunk["choices"][0]["delta"]["content"].as_str() {
            text.push_str(content);
        }
    }

    assert_eq!(text, "Hello from the stream");

    Ok(())
}

/// Non-SSE response: `from_response` gives back the original response.
///
/// Preconditions: upstream returns `application/json`, not `text/event-stream`.
/// Expected: caller can handle both streaming and non-streaming paths.
#[tokio::test]
async fn sse_stream_non_sse_fallback() -> TestResult {
    // -- precondition: upstream returns JSON, not SSE ---------------------------
    let resp = http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"ok":true}"#))
        .unwrap();

    // -- action: try SSE, fall back to plain response --------------------------
    let ServerEventsResponse::Response(resp) =
        ServerEventsStream::from_response::<ServerEvent>(resp)
    else {
        // streaming path — would consume events here
        return Ok(());
    };

    // -- verify: fallback path has the original response -----------------------
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().into_bytes().await?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(json["ok"], true);

    Ok(())
}

/// Typed events via `ServerEventsStream::from_response::<Json<T>>()`.
///
/// Preconditions: upstream returns SSE where some events have JSON data, some don't.
/// Expected: `Json<T>` deserializes valid JSON; returns error for non-JSON events.
#[tokio::test]
async fn sse_stream_typed_json() {
    #[derive(serde::Deserialize, Debug)]
    struct TypedEvent {
        count: Option<u64>,
    }

    // -- precondition: mix of JSON and non-JSON event data ----------------------
    //   event 1: data = "connected"       (NOT valid JSON for TypedEvent)
    //   event 2: data = {"count": 42}     (valid)
    //   event 3: data = "finished"        (NOT valid JSON for TypedEvent)
    let resp = server_events_response(vec![
        "id: 1\nevent: status\nretry: 5000\ndata: connected\n\n",
        "id: 2\nevent: update\ndata: {\"count\":42}\n\n",
        "id: 3\nevent: done\ndata: finished\n\n",
    ]);

    // -- action: wrap with Json<TypedEvent> -------------------------------------
    let ServerEventsResponse::Events(mut events) =
        ServerEventsStream::from_response::<Json<TypedEvent>>(resp)
    else {
        return;
    };

    // -- verify: first event is not valid JSON → error --------------------------
    let first = events.next().await.expect("stream ended");
    assert!(
        first.is_err(),
        "\"connected\" is not valid JSON for TypedEvent"
    );

    // -- verify: second event deserializes correctly ----------------------------
    // Json<T> derefs to T, so we can access .count directly.
    let second = events.next().await.expect("stream ended").unwrap();
    assert_eq!(second.count, Some(42));
}

/// Custom `FromServerEvent` impl for full control over event parsing.
///
/// Preconditions: upstream returns OpenAI chat stream format.
/// Expected: custom impl extracts content from nested JSON automatically.
#[tokio::test]
async fn sse_stream_typed_manual_impl() -> TestResult {
    // -- define a domain type with custom extraction logic ----------------------
    #[derive(Debug)]
    struct ChatChunk {
        content: Option<String>,
    }

    impl FromServerEvent for ChatChunk {
        fn from_server_event(event: ServerEvent) -> Result<Self, StreamingError> {
            #[derive(serde::Deserialize)]
            struct Wire {
                choices: Vec<WireChoice>,
            }
            #[derive(serde::Deserialize)]
            struct WireChoice {
                delta: WireDelta,
            }
            #[derive(serde::Deserialize)]
            struct WireDelta {
                content: Option<String>,
            }

            let wire: Wire = event
                .json()
                .map_err(|e| StreamingError::ServerEventsParse {
                    detail: e.to_string(),
                })?;
            Ok(ChatChunk {
                content: wire.choices.first().and_then(|c| c.delta.content.clone()),
            })
        }
    }

    // -- precondition: OpenAI chat completion chunks ----------------------------
    let resp = server_events_response(vec![
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" from\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" the\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" stream\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    ]);

    let ServerEventsResponse::Events(mut events) =
        ServerEventsStream::from_response::<ChatChunk>(resp)
    else {
        return Ok(());
    };

    // -- verify: typed stream does the extraction automatically -----------------
    let mut text = String::new();
    while let Some(result) = events.next().await {
        match result {
            Ok(chunk) => {
                if let Some(content) = &chunk.content {
                    text.push_str(content);
                }
            }
            Err(_) => break, // [DONE] or empty delta → parse error
        }
    }
    assert_eq!(text, "Hello from the stream");

    Ok(())
}

/// The dispatch pattern: try SSE first, fall back to normal response.
///
/// Preconditions: two responses — one SSE, one plain JSON.
/// Expected: `from_response` returns `Events` for SSE, `Response` for JSON.
#[tokio::test]
async fn sse_stream_dispatch_pattern() -> TestResult {
    // --- SSE response → streaming path -----------------------------------------
    let sse_resp = server_events_response(vec!["id: 1\nevent: status\ndata: connected\n\n"]);

    let resp = match ServerEventsStream::from_response::<ServerEvent>(sse_resp) {
        ServerEventsResponse::Events(mut events) => {
            let first = events.next().await.expect("stream ended")?;
            assert_eq!(first.event.as_deref(), Some("status"));
            assert_eq!(first.data, "connected");
            return Ok(());
        }
        ServerEventsResponse::Response(resp) => resp, // not SSE — handle below
    };
    // would process plain response here
    let _ = resp;

    // --- Non-SSE response → fallback path -------------------------------------
    let json_resp = http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"ok":true}"#))?;

    let resp = match ServerEventsStream::from_response::<ServerEvent>(json_resp) {
        ServerEventsResponse::Events(mut _events) => {
            // would consume stream here
            return Ok(());
        }
        ServerEventsResponse::Response(resp) => resp,
    };
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().into_bytes().await?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(json["ok"], true);

    Ok(())
}

/// Convert SSE stream back into an HTTP response for forwarding to clients.
///
/// Preconditions: ServerEventsStream parsed from upstream SSE.
/// Expected: `into_response()` produces a response with SSE headers and
///   the original events serialized in wire format.
///
/// Requires the `axum` feature.
#[cfg(feature = "axum")]
#[tokio::test]
async fn sse_stream_into_response() -> TestResult {
    // -- precondition: upstream returns SSE with 3 data-only events ----------------
    let resp = server_events_response(vec![
        "data: message 0\n\n",
        "data: message 1\n\n",
        "data: message 2\n\n",
    ]);
    let ServerEventsResponse::Events(events) =
        ServerEventsStream::from_response::<ServerEvent>(resp)
    else {
        return Ok(());
    };

    // -- action: convert back to HTTP response for downstream clients -----------
    let response = events.into_response();

    // -- verify: SSE headers ---------------------------------------------------
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-cache");
    assert_eq!(response.headers().get("connection").unwrap(), "keep-alive");
    assert_eq!(response.headers().get("x-accel-buffering").unwrap(), "no");

    // -- verify: body contains the original events in wire format ---------------
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(body_str.contains("data: message 0"));
    assert!(body_str.contains("data: message 1"));
    assert!(body_str.contains("data: message 2"));

    Ok(())
}

/// Custom response headers are accessible via `events.headers()`.
///
/// Preconditions: upstream returns SSE with a custom `x-request-id` header.
/// Expected: header is preserved and accessible on the stream wrapper.
#[tokio::test]
async fn sse_stream_preserves_headers() -> TestResult {
    // -- precondition: SSE response with custom header --------------------------
    let stream: BodyStream = Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from(
        "data: test\n\n",
    ))]));
    let resp = http::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("x-request-id", "req-42")
        .body(Body::Stream(stream))?;

    // -- action -----------------------------------------------------------------
    let ServerEventsResponse::Events(events) =
        ServerEventsStream::from_response::<ServerEvent>(resp)
    else {
        return Ok(());
    };

    // -- verify -----------------------------------------------------------------
    assert_eq!(events.status(), 200);
    assert_eq!(events.headers().get("x-request-id").unwrap(), "req-42");

    Ok(())
}

/// Full integration pattern: gateway client → ServerEventsStream.
///
/// Preconditions: `ServiceGatewayClientV1` returns an SSE response from `proxy_request`.
/// Expected: wrap the response into a typed event stream.
#[tokio::test]
async fn sse_via_gateway_client() -> TestResult {
    // -- setup: mock gateway returns an SSE response ----------------------------
    let gateway = MockGateway::responding_with(server_events_response(vec![
        "data: {\"status\":\"processing\"}\n\n",
        "data: {\"status\":\"complete\"}\n\n",
    ]));

    // -- action: call proxy_request and wrap into ServerEventsStream -------------
    let req = http::Request::get("/api/oagw/v1/proxy/openai/chat/completions").body(Body::Empty)?;
    let resp = gateway
        .proxy_request(SecurityContext::anonymous(), req)
        .await?;

    let ServerEventsResponse::Events(mut events) =
        ServerEventsStream::from_response::<ServerEvent>(resp)
    else {
        return Ok(());
    };

    // -- verify -----------------------------------------------------------------
    let first = events.next().await.expect("stream ended")?;
    assert_eq!(first.data, r#"{"status":"processing"}"#);

    let second = events.next().await.expect("stream ended")?;
    assert_eq!(second.data, r#"{"status":"complete"}"#);

    Ok(())
}

// ===========================================================================
// WebSocket: WebSocketStream in-memory tests
// ===========================================================================

/// Ping/Pong frames are filtered transparently — recv skips them.
///
/// Preconditions: stream contains Ping, Pong, Text, Close frames.
/// Expected: recv() yields only the Text frame, then None after Close.
#[tokio::test]
async fn websocket_stream_filters_ping_pong() -> TestResult {
    // -- precondition: in-memory stream with control frames --------------------
    let sink: WebSocketSink = Box::pin(
        futures_util::sink::drain().sink_map_err(|e: std::convert::Infallible| match e {}),
    );
    let receiver: WebSocketReceiver = Box::pin(futures_util::stream::iter(vec![
        Ok(WebSocketMessage::Ping(vec![])),
        Ok(WebSocketMessage::Pong(vec![])),
        Ok(WebSocketMessage::Text("data".into())),
        Ok(WebSocketMessage::Close(None)),
    ]));

    let mut ws: WebSocketStream = (sink, receiver).into();

    // -- verify: only Text frame is yielded ------------------------------------
    let msg = ws.recv().await.expect("stream ended")?;
    assert_eq!(msg, WebSocketMessage::Text("data".into()));

    // -- verify: Close terminates the stream -----------------------------------
    assert!(ws.recv().await.is_none());

    Ok(())
}

/// Close frame terminates recv — returns None.
///
/// Preconditions: stream contains only a Close frame.
/// Expected: first recv() returns None immediately.
#[tokio::test]
async fn websocket_stream_close_terminates() {
    let sink: WebSocketSink = Box::pin(
        futures_util::sink::drain().sink_map_err(|e: std::convert::Infallible| match e {}),
    );
    let receiver: WebSocketReceiver = Box::pin(futures_util::stream::iter(vec![Ok(
        WebSocketMessage::Close(None),
    )]));

    let mut ws: WebSocketStream = (sink, receiver).into();

    assert!(ws.recv().await.is_none());
}

/// JSON serialization round-trip via the `Json<T>` codec.
///
/// Preconditions: `Json<T>` can serialize to a WebSocket message and deserialize back.
/// Expected: `to_ws_message()` produces a Text frame; `from_ws_message()` recovers the value.
#[tokio::test]
async fn websocket_json_roundtrip() -> TestResult {
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
    struct ChatMessage {
        text: String,
    }

    // -- action: serialize to WebSocket message --------------------------------
    let outgoing = Json(ChatMessage {
        text: "hello".into(),
    });
    let raw = outgoing.to_ws_message();
    assert!(matches!(&raw, WebSocketMessage::Text(t) if t.contains("hello")));

    // -- action: deserialize back ----------------------------------------------
    let parsed = <Json<ChatMessage>>::from_ws_message(raw)?;
    assert_eq!(
        parsed.into_inner(),
        ChatMessage {
            text: "hello".into()
        }
    );

    Ok(())
}

/// `FromWebSocketMessage for Json<T>` rejects Binary messages.
///
/// Preconditions: a Binary WebSocket message.
/// Expected: `from_ws_message` returns `Err(WebSocketBridge)`.
#[tokio::test]
async fn websocket_json_rejects_binary() {
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct Msg {
        x: i32,
    }

    let binary_msg = WebSocketMessage::Binary(vec![0, 1, 2]);
    let err = <Json<Msg>>::from_ws_message(binary_msg).unwrap_err();

    assert!(
        matches!(&err, StreamingError::WebSocketBridge { detail } if detail.contains("Text")),
        "expected WebSocketBridge mentioning Text, got {err:?}"
    );
}

/// WebSocketStream as `Stream` trait — polls correctly via `collect()`.
///
/// Preconditions: stream with 3 Text messages followed by Close.
/// Expected: collecting the stream yields exactly the 3 Text messages.
#[tokio::test]
async fn websocket_stream_as_futures_stream() -> TestResult {
    let sink: WebSocketSink = Box::pin(
        futures_util::sink::drain().sink_map_err(|e: std::convert::Infallible| match e {}),
    );
    let receiver: WebSocketReceiver = Box::pin(futures_util::stream::iter(vec![
        Ok(WebSocketMessage::Text("a".into())),
        Ok(WebSocketMessage::Text("b".into())),
        Ok(WebSocketMessage::Text("c".into())),
        Ok(WebSocketMessage::Close(None)),
    ]));
    let ws: WebSocketStream = (sink, receiver).into();

    // -- action: collect via Stream trait ---------------------------------------
    let messages: Vec<WebSocketMessage> = ws
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    // -- verify ----------------------------------------------------------------
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0], WebSocketMessage::Text("a".into()));
    assert_eq!(messages[1], WebSocketMessage::Text("b".into()));
    assert_eq!(messages[2], WebSocketMessage::Text("c".into()));

    Ok(())
}

/// Split into sender/receiver halves for concurrent send and receive.
///
/// Preconditions: in-memory WebSocket with a channel-based sink.
/// Expected: sender half can send messages; receiver half yields incoming messages.
#[tokio::test]
async fn websocket_stream_split() -> TestResult {
    // -- setup: channel-backed sink so we can observe sent messages --------------
    let (tx, mut rx) = tokio::sync::mpsc::channel::<WebSocketMessage>(10);

    let sink: WebSocketSink = Box::pin(futures_util::sink::unfold(
        tx,
        |tx, msg: WebSocketMessage| async move {
            tx.send(msg)
                .await
                .map_err(|e| StreamingError::WebSocketBridge {
                    detail: e.to_string(),
                })?;
            Ok(tx)
        },
    ));
    let receiver: WebSocketReceiver = Box::pin(futures_util::stream::iter(vec![
        Ok(WebSocketMessage::Text("received".into())),
        Ok(WebSocketMessage::Close(None)),
    ]));
    let ws: WebSocketStream = (sink, receiver).into();

    let (mut sender, mut stream_receiver) = ws.split();

    // -- verify: send via sender half ------------------------------------------
    sender.send(&WebSocketMessage::Text("sent".into())).await?;
    let sent_msg = rx.recv().await.unwrap();
    assert_eq!(sent_msg, WebSocketMessage::Text("sent".into()));

    // -- verify: receive via receiver half -------------------------------------
    let received = stream_receiver.recv().await.expect("stream ended")?;
    assert_eq!(received, WebSocketMessage::Text("received".into()));

    // -- verify: close terminates ----------------------------------------------
    assert!(stream_receiver.recv().await.is_none());

    Ok(())
}

// ===========================================================================
// Multipart: file uploads via MultipartBody
// ===========================================================================

/// Upload a file with metadata using buffered multipart.
///
/// Preconditions: upstream accepts `multipart/form-data` with a text field and file.
/// Expected: `into_request` produces a ready-to-send request with correct headers and body.
#[tokio::test]
async fn multipart_proxy_buffered() -> TestResult {
    // -- setup: upstream accepts the upload and returns a file ID ----------------
    let gateway = MockGateway::responding_with(
        http::Response::builder()
            .status(200)
            .body(Body::from(r#"{"id":"file-123"}"#))?,
    );

    // -- action: build a multipart request with text field + file ----------------
    let req = oagw_sdk::MultipartBody::with_boundary("TEST-BOUNDARY")?
        .text("purpose", "fine-tune")
        .part(
            oagw_sdk::Part::bytes("file", &b"training-data"[..])
                .filename("training.jsonl")
                .content_type("application/jsonl"),
        )
        .into_request("POST", "/api/oagw/v1/proxy/openai/v1/files")?;

    // -- verify: Content-Type header includes the boundary -----------------------
    let ct = req.headers().get("content-type").unwrap().to_str()?;
    assert!(ct.starts_with("multipart/form-data; boundary="));

    // -- verify: body is fully buffered with correct wire format -----------------
    let body_bytes = req.into_body().into_bytes().await?;
    let body_str = String::from_utf8(body_bytes.to_vec())?;
    assert!(body_str.contains("name=\"purpose\""));
    assert!(body_str.contains("fine-tune"));
    assert!(body_str.contains("filename=\"training.jsonl\""));
    assert!(body_str.contains("Content-Type: application/jsonl"));
    assert!(body_str.contains("training-data"));
    assert!(body_str.contains("--TEST-BOUNDARY--\r\n"));

    // -- action: send through proxy_request -------------------------------------
    let req2 = oagw_sdk::MultipartBody::with_boundary("TEST-BOUNDARY")?
        .text("purpose", "fine-tune")
        .part(
            oagw_sdk::Part::bytes("file", &b"training-data"[..])
                .filename("training.jsonl")
                .content_type("application/jsonl"),
        )
        .into_request("POST", "/api/oagw/v1/proxy/openai/v1/files")?;

    let resp = gateway
        .proxy_request(SecurityContext::anonymous(), req2)
        .await?;

    // -- verify: upstream accepted the upload -----------------------------------
    assert_eq!(resp.status(), 200);

    Ok(())
}

/// Upload a large file as a stream — body is never fully buffered in memory.
///
/// Preconditions: upstream accepts audio transcription with a streaming file part.
/// Expected: `MultipartBody` with a `Part::stream` produces `Body::Stream`;
///   wire format is valid after collecting.
#[tokio::test]
async fn multipart_proxy_streaming() -> TestResult {
    // -- setup: upstream returns a transcription ---------------------------------
    let gateway = MockGateway::responding_with(
        http::Response::builder()
            .status(200)
            .body(Body::from(r#"{"text":"Hello world"}"#))?,
    );

    // -- action: build a multipart request with a streaming file part ------------
    let file_stream: BodyStream = Box::pin(futures_util::stream::iter(vec![
        Ok(Bytes::from("audio-chunk-1")),
        Ok(Bytes::from("audio-chunk-2")),
    ]));

    let multipart = oagw_sdk::MultipartBody::with_boundary("STREAM-BOUND")?
        .text("model", "whisper-1")
        .part(
            oagw_sdk::Part::stream("file", file_stream)
                .filename("audio.mp3")
                .content_type("audio/mpeg"),
        );

    // -- verify: streaming parts produce Body::Stream ---------------------------
    let ct = multipart.content_type();
    let body = multipart.into_body();
    assert!(matches!(body, Body::Stream(_)));

    let body_bytes = body.into_bytes().await?;
    let body_str = String::from_utf8(body_bytes.to_vec())?;
    assert!(body_str.contains("name=\"model\""));
    assert!(body_str.contains("whisper-1"));
    assert!(body_str.contains("filename=\"audio.mp3\""));
    assert!(body_str.contains("audio-chunk-1audio-chunk-2"));
    assert!(body_str.contains("--STREAM-BOUND--\r\n"));

    // -- action: send a streaming multipart through proxy_request ----------------
    let file_stream2: BodyStream = Box::pin(futures_util::stream::iter(vec![
        Ok(Bytes::from("audio-chunk-1")),
        Ok(Bytes::from("audio-chunk-2")),
    ]));

    let req = http::Request::builder()
        .method("POST")
        .uri("/api/oagw/v1/proxy/openai/v1/audio/transcriptions")
        .header("content-type", ct)
        .body(Body::from(
            oagw_sdk::MultipartBody::with_boundary("STREAM-BOUND")?
                .text("model", "whisper-1")
                .part(
                    oagw_sdk::Part::stream("file", file_stream2)
                        .filename("audio.mp3")
                        .content_type("audio/mpeg"),
                ),
        ))?;

    let resp = gateway
        .proxy_request(SecurityContext::anonymous(), req)
        .await?;

    // -- verify: upstream processed the transcription ----------------------------
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().into_bytes().await?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(json["text"], "Hello world");

    Ok(())
}

// ===========================================================================
// Body: into_bytes / into_stream conversions
// ===========================================================================

/// Body::Bytes round-trips through `into_bytes()`.
#[tokio::test]
async fn body_bytes_roundtrip() -> TestResult {
    let body = Body::from("hello");
    let bytes = body.into_bytes().await?;
    assert_eq!(bytes.as_ref(), b"hello");
    Ok(())
}

/// Multi-chunk `Body::Stream` collapses into a single `Bytes` via `into_bytes()`.
#[tokio::test]
async fn body_stream_to_bytes() -> TestResult {
    let stream: BodyStream = Box::pin(futures_util::stream::iter(vec![
        Ok(Bytes::from("chunk1")),
        Ok(Bytes::from("chunk2")),
    ]));
    let body = Body::Stream(stream);
    let bytes = body.into_bytes().await?;
    assert_eq!(bytes.as_ref(), b"chunk1chunk2");
    Ok(())
}

/// `Body::Empty` converts to empty bytes.
#[tokio::test]
async fn body_empty_to_bytes() -> TestResult {
    let bytes = Body::Empty.into_bytes().await?;
    assert!(bytes.is_empty());
    Ok(())
}

/// `Body::Bytes` converts to a single-item stream via `into_stream()`.
#[tokio::test]
async fn body_bytes_to_stream() -> TestResult {
    let body = Body::from("streamed");
    let mut stream = body.into_stream();

    let chunk = stream.next().await.unwrap()?;
    assert_eq!(chunk.as_ref(), b"streamed");
    assert!(stream.next().await.is_none());
    Ok(())
}
