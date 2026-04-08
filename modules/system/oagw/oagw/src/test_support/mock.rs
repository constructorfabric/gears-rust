//! Mock upstream server for integration tests.
//!
//! Simulates upstream services: OpenAI-compatible HTTP JSON, SSE streaming,
//! error conditions, WebSocket, WebTransport stub.
//!
//! # Usage
//! ```ignore
//! let mock = MockUpstream::start().await;
//! // Use mock.base_url() to configure upstream endpoints
//! let requests = mock.recorded_requests().await;
//! mock.stop().await;
//! ```

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{OriginalUri, Path, State, WebSocketUpgrade};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use bytes::Bytes;
use dashmap::DashMap;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::sync::oneshot;

// ---------------------------------------------------------------------------
// Dynamic mock response types
// ---------------------------------------------------------------------------

/// Body variant for dynamic mock responses.
#[derive(Clone, Debug)]
pub enum MockBody {
    Json(Value),
    Text(String),
    Sse(Vec<String>),
    /// Body delivery is gated on a channel signal.
    /// When the sender fires, the inner body is delivered.
    /// When the sender is dropped without firing, the handler aborts the connection.
    Channel {
        body: Box<MockBody>,
        gate: Arc<tokio::sync::Mutex<Option<oneshot::Receiver<()>>>>,
    },
}

/// A registered mock response for dynamic routing.
#[derive(Clone, Debug)]
pub struct MockResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: MockBody,
}

/// Key for dynamic route lookup.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub struct RouteKey {
    pub method: String,
    pub path: String,
}

impl MockResponse {
    fn into_axum_response(self) -> axum::response::Response {
        match self.body {
            MockBody::Channel { body, .. } => {
                // Gate handling is done in dynamic_handler before calling this.
                // Here we just unwrap to the inner body.
                MockResponse {
                    status: self.status,
                    headers: self.headers,
                    body: *body,
                }
                .into_axum_response()
            }
            MockBody::Json(value) => {
                let mut builder = axum::response::Response::builder()
                    .status(StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK));
                for (k, v) in &self.headers {
                    builder = builder.header(k.as_str(), v.as_str());
                }
                if !self
                    .headers
                    .iter()
                    .any(|(k, _)| k.to_lowercase() == "content-type")
                {
                    builder = builder.header("content-type", "application/json");
                }
                builder
                    .body(axum::body::Body::from(value.to_string()))
                    .unwrap()
            }
            MockBody::Text(text) => {
                let mut builder = axum::response::Response::builder()
                    .status(StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK));
                for (k, v) in &self.headers {
                    builder = builder.header(k.as_str(), v.as_str());
                }
                builder.body(axum::body::Body::from(text)).unwrap()
            }
            MockBody::Sse(chunks) => {
                let sse_body = chunks
                    .into_iter()
                    .map(|chunk| format!("data: {}\n\n", chunk))
                    .collect::<String>();
                let mut builder = axum::response::Response::builder()
                    .status(StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK));
                for (k, v) in &self.headers {
                    builder = builder.header(k.as_str(), v.as_str());
                }
                if !self
                    .headers
                    .iter()
                    .any(|(k, _)| k.to_lowercase() == "content-type")
                {
                    builder = builder.header("content-type", "text/event-stream");
                }
                builder.body(axum::body::Body::from(sse_body)).unwrap()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Recording types
// ---------------------------------------------------------------------------

/// A captured inbound request for test assertions.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub(crate) struct SharedState {
    recorded: Mutex<VecDeque<RecordedRequest>>,
    max_recorded: usize,
    dynamic_routes: DashMap<RouteKey, MockResponse>,
    /// Sequential responses: pop the front on each request, fall back to
    /// `dynamic_routes` (or 404) once the queue is empty.
    dynamic_sequences: DashMap<RouteKey, Arc<Mutex<VecDeque<MockResponse>>>>,
}

impl SharedState {
    fn new(max_recorded: usize) -> Self {
        Self {
            recorded: Mutex::new(VecDeque::new()),
            max_recorded,
            dynamic_routes: DashMap::new(),
            dynamic_sequences: DashMap::new(),
        }
    }

    async fn record(&self, method: &str, uri: &str, headers: &HeaderMap, body: &[u8]) {
        let hdrs = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let entry = RecordedRequest {
            method: method.to_string(),
            uri: uri.to_string(),
            headers: hdrs,
            body: body.to_vec(),
        };
        let mut queue = self.recorded.lock().await;
        if queue.len() >= self.max_recorded {
            queue.pop_front();
        }
        queue.push_back(entry);
    }
}

// ---------------------------------------------------------------------------
// MockUpstream
// ---------------------------------------------------------------------------

/// A mock upstream HTTP server bound to a random local port.
pub struct MockUpstream {
    addr: SocketAddr,
    state: Arc<SharedState>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for MockUpstream {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

impl MockUpstream {
    /// Start the mock server on `127.0.0.1:0` (random port).
    pub async fn start() -> Self {
        Self::start_on("127.0.0.1:0").await
    }

    /// Start the mock server on a caller-chosen `addr` (e.g. `"127.0.0.2:0"`).
    pub async fn start_on(addr: &str) -> Self {
        let state = Arc::new(SharedState::new(200));
        let app = Self::router(Arc::clone(&state));

        let listener = TcpListener::bind(addr)
            .await
            .expect("failed to bind mock upstream");
        let addr = listener.local_addr().expect("failed to get local addr");

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("mock server error");
        });

        Self {
            addr,
            state,
            shutdown_tx: Some(shutdown_tx),
            handle: Some(handle),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Return a snapshot of all recorded requests (oldest first).
    pub async fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.state.recorded.lock().await.iter().cloned().collect()
    }

    /// Clear all recorded requests.
    pub async fn clear_recorded(&self) {
        self.state.recorded.lock().await.clear();
    }

    /// Gracefully stop the mock server.
    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }

    // -- routing --

    fn router(state: Arc<SharedState>) -> Router {
        Router::new()
            // OpenAI-compatible JSON endpoints
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/models", get(models))
            // Utility
            .route("/echo", post(echo))
            .route("/status/{code}", get(status))
            // Error simulation (delay-free only)
            .route("/error/500", get(error_500))
            // Response header test
            .route("/response-headers", get(response_with_bad_headers))
            // WebSocket (future use)
            .route("/ws/echo", get(ws_echo))
            // WebTransport stub (future use)
            .route("/wt/stub", get(wt_stub))
            // Dynamic route fallback - catches all unmatched paths
            .fallback(dynamic_handler)
            .with_state(state)
    }
}

// ---------------------------------------------------------------------------
// MockHandle — lightweight Send + Sync reference to a running mock server
// ---------------------------------------------------------------------------

/// A lightweight, `Send + Sync` handle to a running mock server.
///
/// Unlike [`MockUpstream`], this type does not own the server lifecycle
/// (no shutdown sender or task handle) and can be stored in a `static`.
pub struct MockHandle {
    addr: SocketAddr,
    state: Arc<SharedState>,
}

impl MockHandle {
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Access the shared state for MockGuard creation.
    pub(crate) fn shared_state(&self) -> Arc<SharedState> {
        Arc::clone(&self.state)
    }
}

// ---------------------------------------------------------------------------
// MockGuard — RAII guard for per-test mock registration
// ---------------------------------------------------------------------------

/// RAII guard that registers mocks with a unique path prefix and cleans them up on drop.
///
/// Each test can create a `MockGuard` to register custom mock responses without
/// interfering with other parallel tests. The guard generates a unique path prefix
/// (e.g., `/t-{uuid}`) and all registered mocks use this prefix.
///
/// # Example
/// ```ignore
/// let mut guard = MockGuard::new();
/// guard.json("POST", "/v1/chat/completions", 200, json!({"id": "test"}));
///
/// // Create routes using guard.path() to get the prefixed path
/// let route_path = guard.path("/v1/chat/completions");
/// // route_path is something like "/t-abc123/v1/chat/completions"
/// ```
pub struct MockGuard {
    test_prefix: String,
    registered_keys: Vec<RouteKey>,
    registered_sequence_keys: Vec<RouteKey>,
    state: Arc<SharedState>,
}

impl MockGuard {
    /// Create a new guard with a unique test prefix.
    pub fn new() -> Self {
        let test_prefix = format!("/t-{}", uuid::Uuid::new_v4().as_simple());
        Self {
            test_prefix,
            registered_keys: Vec::new(),
            registered_sequence_keys: Vec::new(),
            state: shared_mock().shared_state(),
        }
    }

    /// Path prefix for this test's routes.
    pub fn prefix(&self) -> &str {
        &self.test_prefix
    }

    /// Get the full prefixed path for route configuration.
    ///
    /// Use this when creating routes to match the registered mock.
    pub fn path(&self, path: &str) -> String {
        format!("{}{}", self.test_prefix, path)
    }

    /// Register a mock response for this test.
    pub fn mock(&mut self, method: &str, path: &str, response: MockResponse) -> &mut Self {
        let full_path = self.path(path);
        let key = RouteKey {
            method: method.to_uppercase(),
            path: full_path,
        };
        self.state.dynamic_routes.insert(key.clone(), response);
        self.registered_keys.push(key);
        self
    }

    /// Register a gated mock response that stalls until the returned sender fires.
    ///
    /// - Send `()` on the sender to release the response.
    /// - Drop the sender without sending to simulate a connection abort.
    pub fn mock_gated(
        &mut self,
        method: &str,
        path: &str,
        response: MockResponse,
    ) -> oneshot::Sender<()> {
        let (tx, rx) = oneshot::channel();
        let gated = MockResponse {
            status: response.status,
            headers: response.headers,
            body: MockBody::Channel {
                body: Box::new(response.body),
                gate: Arc::new(tokio::sync::Mutex::new(Some(rx))),
            },
        };
        self.mock(method, path, gated);
        tx
    }

    /// Register a sequence of mock responses for this test.
    ///
    /// Each request pops the front response from the queue. Once the queue
    /// is exhausted, subsequent requests fall through to `dynamic_routes`
    /// (or 404 if none is registered).
    pub fn mock_sequence(
        &mut self,
        method: &str,
        path: &str,
        responses: Vec<MockResponse>,
    ) -> &mut Self {
        let full_path = self.path(path);
        let key = RouteKey {
            method: method.to_uppercase(),
            path: full_path,
        };
        self.state
            .dynamic_sequences
            .insert(key.clone(), Arc::new(Mutex::new(VecDeque::from(responses))));
        self.registered_sequence_keys.push(key);
        self
    }

    /// Get recorded requests matching this test's prefix.
    pub async fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.state
            .recorded
            .lock()
            .await
            .iter()
            .filter(|r| r.uri.starts_with(&self.test_prefix))
            .cloned()
            .collect()
    }
}

impl Default for MockGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MockGuard {
    fn drop(&mut self) {
        // Clean up all registered routes
        for key in &self.registered_keys {
            self.state.dynamic_routes.remove(key);
        }
        for key in &self.registered_sequence_keys {
            self.state.dynamic_sequences.remove(key);
        }
    }
}

/// Return a reference to the process-global shared mock server.
///
/// The server is started lazily on first call on a dedicated background
/// thread with its own tokio runtime, so it survives individual
/// `#[tokio::test]` runtime teardowns.
pub fn shared_mock() -> &'static MockHandle {
    static SHARED: std::sync::OnceLock<MockHandle> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("failed to create mock runtime");
            let handle = rt.block_on(async {
                let mock = MockUpstream::start().await;
                let h = MockHandle {
                    addr: mock.addr,
                    state: Arc::clone(&mock.state),
                };
                // Prevent Drop from shutting down the server.
                std::mem::forget(mock);
                h
            });
            tx.send(handle).expect("failed to send mock handle");
            // Park this thread forever to keep the runtime (and its spawned
            // tasks, including the mock server) alive for the process lifetime.
            loop {
                std::thread::park();
            }
        });
        rx.recv().expect("failed to receive mock handle")
    })
}

// ---------------------------------------------------------------------------
// OpenAI-compatible HTTP JSON handlers
// ---------------------------------------------------------------------------

async fn chat_completions(
    State(state): State<Arc<SharedState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    state
        .record("POST", &uri.to_string(), &headers, &body)
        .await;

    let resp = json!({
        "id": "chatcmpl-mock-123",
        "object": "chat.completion",
        "created": 1_234_567_890_u64,
        "model": "gpt-4-mock",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Hello from mock server"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 20,
            "total_tokens": 30
        }
    });
    (StatusCode::OK, axum::Json(resp))
}

async fn models(
    State(state): State<Arc<SharedState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> impl IntoResponse {
    state.record("GET", &uri.to_string(), &headers, &[]).await;

    let resp = json!({
        "object": "list",
        "data": [
            {"id": "gpt-4", "object": "model", "created": 1_234_567_890_u64, "owned_by": "openai"},
            {"id": "gpt-3.5-turbo", "object": "model", "created": 1_234_567_890_u64, "owned_by": "openai"}
        ]
    });
    (StatusCode::OK, axum::Json(resp))
}

// ---------------------------------------------------------------------------
// Utility handlers
// ---------------------------------------------------------------------------

async fn echo(
    State(state): State<Arc<SharedState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    state
        .record("POST", &uri.to_string(), &headers, &body)
        .await;

    let hdrs: serde_json::Map<String, Value> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                Value::String(v.to_str().unwrap_or("").to_string()),
            )
        })
        .collect();
    let body_str = String::from_utf8_lossy(&body);

    let resp = json!({
        "headers": hdrs,
        "body": body_str,
    });
    (StatusCode::OK, axum::Json(resp))
}

async fn status(
    State(state): State<Arc<SharedState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Path(code): Path<u16>,
) -> impl IntoResponse {
    state.record("GET", &uri.to_string(), &headers, &[]).await;

    let sc = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let resp = json!({
        "status": code,
        "description": sc.canonical_reason().unwrap_or("Unknown"),
    });
    (sc, axum::Json(resp))
}

// ---------------------------------------------------------------------------
// Error simulation handlers (delay-free only)
// ---------------------------------------------------------------------------

async fn error_500(
    State(state): State<Arc<SharedState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> impl IntoResponse {
    state.record("GET", &uri.to_string(), &headers, &[]).await;

    let resp = json!({
        "error": {
            "message": "Internal server error",
            "type": "server_error",
            "code": "internal_error"
        }
    });
    (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(resp))
}

// ---------------------------------------------------------------------------
// WebSocket handlers (future use — OAGW WS proxy not in this phase)
// ---------------------------------------------------------------------------

async fn ws_echo(
    State(state): State<Arc<SharedState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    state.record("GET", &uri.to_string(), &headers, &[]).await;
    ws.on_upgrade(handle_ws_echo)
}

async fn handle_ws_echo(mut socket: WebSocket) {
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(text) => {
                if socket.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
            Message::Binary(data) => {
                if socket.send(Message::Binary(data)).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Response header test handler
// ---------------------------------------------------------------------------

async fn response_with_bad_headers(
    State(state): State<Arc<SharedState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> axum::response::Response {
    state.record("GET", &uri.to_string(), &headers, &[]).await;

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .header("x-custom-safe", "keep-me")
        // Hop-by-hop headers that should be stripped
        // (avoid transfer-encoding — it confuses the HTTP transport layer)
        .header("proxy-authenticate", "Basic realm=mock")
        .header("trailer", "X-Checksum")
        .header("upgrade", "websocket")
        // Internal x-oagw-* headers that should be stripped
        .header("x-oagw-debug", "true")
        .header("x-oagw-trace-id", "mock-trace-123")
        .body(axum::body::Body::from(r#"{"ok":true}"#))
        .expect("response builder should not fail")
}

// ---------------------------------------------------------------------------
// WebTransport stub (future use)
// ---------------------------------------------------------------------------

async fn wt_stub(
    State(state): State<Arc<SharedState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> impl IntoResponse {
    state.record("GET", &uri.to_string(), &headers, &[]).await;

    let resp = json!({
        "error": "WebTransport is not implemented",
        "description": "Placeholder for future WebTransport support"
    });
    (StatusCode::NOT_IMPLEMENTED, axum::Json(resp))
}

// ---------------------------------------------------------------------------
// Dynamic route handler (catch-all for MockGuard-registered routes)
// ---------------------------------------------------------------------------

/// Return an immediate 502 response when the gate sender is dropped.
///
/// Uses an empty non-streaming body so hyper can write it instantly and
/// release the connection task — avoiding leaked tasks that prevent
/// the test process from exiting.
fn abort_response() -> axum::response::Response {
    axum::response::Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            r#"{"error":"mock: gate sender dropped — simulated connection abort"}"#,
        ))
        .expect("response builder should not fail")
}

/// Wait for a `MockBody::Channel` gate (if present) and convert to an axum response.
///
/// Shared by both the `dynamic_sequences` and `dynamic_routes` code paths so
/// that gated responses are honoured regardless of registration method.
async fn wait_gate_and_respond(response: MockResponse) -> axum::response::Response {
    if let MockBody::Channel { ref gate, .. } = response.body {
        let receiver = gate.lock().await.take();
        if let Some(rx) = receiver {
            match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
                Ok(Ok(())) => {} // Gate opened — deliver inner body
                Ok(Err(_)) | Err(_) => return abort_response(),
            }
        }
    }
    response.into_axum_response()
}

async fn dynamic_handler(
    State(state): State<Arc<SharedState>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    state
        .record(method.as_ref(), &uri.to_string(), &headers, &body)
        .await;

    let path = uri.path().to_string();
    let key = RouteKey {
        method: method.to_string(),
        path: path.clone(),
    };

    // Check sequential responses first — pop the front of the queue.
    // Clone the Arc out and drop the DashMap ref so the shard lock is not
    // held across the `.lock().await` (mirrors the MockBody::Channel pattern).
    if let Some(seq_entry) = state.dynamic_sequences.get(&key) {
        let seq_arc = seq_entry.value().clone();
        drop(seq_entry);
        let mut queue = seq_arc.lock().await;
        if let Some(response) = queue.pop_front() {
            drop(queue);
            return wait_gate_and_respond(response).await;
        }
    }

    // Check dynamic registry for exact match
    if let Some(entry) = state.dynamic_routes.get(&key) {
        let response = entry.value().clone();
        drop(entry); // Release the lock before async operations
        return wait_gate_and_respond(response).await;
    }

    // No match in dynamic registry - return 404
    axum::response::Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({"error": "not found", "path": path}).to_string(),
        ))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Unit tests for MockGuard
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_guard_generates_unique_prefix() {
        let guard1 = MockGuard::new();
        let guard2 = MockGuard::new();

        assert!(guard1.prefix().starts_with("/t-"));
        assert!(guard2.prefix().starts_with("/t-"));
        assert_ne!(guard1.prefix(), guard2.prefix());
    }

    #[test]
    fn mock_guard_path_helper_prepends_prefix() {
        let guard = MockGuard::new();
        let full_path = guard.path("/v1/chat/completions");

        assert!(full_path.starts_with(guard.prefix()));
        assert!(full_path.ends_with("/v1/chat/completions"));
    }

    #[test]
    fn mock_guard_registers_and_cleans_up_routes() {
        // Create a guard and track its specific routes
        let key1;
        let key2;

        {
            let mut guard = MockGuard::new();
            guard.mock(
                "POST",
                "/test",
                MockResponse {
                    status: 200,
                    headers: vec![],
                    body: MockBody::Json(json!({"ok": true})),
                },
            );
            guard.mock(
                "GET",
                "/test2",
                MockResponse {
                    status: 201,
                    headers: vec![],
                    body: MockBody::Json(json!({"created": true})),
                },
            );

            key1 = RouteKey {
                method: "POST".into(),
                path: guard.path("/test"),
            };
            key2 = RouteKey {
                method: "GET".into(),
                path: guard.path("/test2"),
            };

            // Routes should be registered
            assert!(guard.state.dynamic_routes.contains_key(&key1));
            assert!(guard.state.dynamic_routes.contains_key(&key2));
        }

        // After guard is dropped, routes should be cleaned up
        let state = shared_mock().shared_state();
        assert!(!state.dynamic_routes.contains_key(&key1));
        assert!(!state.dynamic_routes.contains_key(&key2));
    }
}
