---
status: proposed
date: 2026-02-03
decision-makers: OAGW Team
---

# Rust ABI Client Library — Hybrid Deployment Abstraction with SDK Integration


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Core API](#core-api)
  - [Deployment Modes](#deployment-modes)
  - [Response Consumption](#response-consumption)
  - [SDK Integration Strategy](#sdk-integration-strategy)
  - [Build Script Strategy](#build-script-strategy)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Trait-based abstraction with dynamic dispatch](#trait-based-abstraction-with-dynamic-dispatch)
  - [Concrete types with feature flags](#concrete-types-with-feature-flags)
  - [Hybrid approach with deployment abstraction](#hybrid-approach-with-deployment-abstraction)
- [More Information](#more-information)
- [SDK Integration Patterns](#sdk-integration-patterns)
  - [Pattern 1: Drop-In Replacement for reqwest](#pattern-1-drop-in-replacement-for-reqwest)
  - [Pattern 2: HTTP Proxy (Standard Proxy Protocol)](#pattern-2-http-proxy-standard-proxy-protocol)
  - [Pattern 3: Custom HTTP Transport Trait](#pattern-3-custom-http-transport-trait)
  - [Pattern 4: Wrapper Layer (Facade Pattern)](#pattern-4-wrapper-layer-facade-pattern)
  - [Pattern 5: reqwest Middleware/Interceptor](#pattern-5-reqwest-middlewareinterceptor)
  - [Recommended Strategy](#recommended-strategy)
- [HTTP Client Compatibility Analysis](#http-client-compatibility-analysis)
  - [Rust HTTP Client Landscape](#rust-http-client-landscape)
  - [Compatibility Matrix](#compatibility-matrix)
  - [Key Compatibility Issues](#key-compatibility-issues)
  - [Solutions and Mitigations](#solutions-and-mitigations)
  - [Build Script Strategy](#build-script-strategy-1)
- [Detailed Design](#detailed-design)
  - [Usage Example (Deployment-Agnostic)](#usage-example-deployment-agnostic)
  - [Core Types](#core-types)
  - [Client Implementations](#client-implementations)
  - [Out of Scope: OAGW Plugin Development](#out-of-scope-oagw-plugin-development)
  - [WebTransport Support (Future)](#webtransport-support-future)
- [Implementation Plan](#implementation-plan)
  - [Phase 0 (MVP): Core Client Types & RemoteProxyClient](#phase-0-mvp-core-client-types--remoteproxyclient)
  - [Phase 1: SharedProcessClient](#phase-1-sharedprocessclient)
  - [Phase 2: WebSocket Support](#phase-2-websocket-support)
  - [Phase 3: SDK HTTP Proxy, WebTransport & gRPC](#phase-3-sdk-http-proxy-webtransport--grpc)
- [Testing Strategy](#testing-strategy)
  - [Unit Tests](#unit-tests)
  - [Mock Client for Testing](#mock-client-for-testing)
- [Dependencies](#dependencies)
- [Security Considerations](#security-considerations)
- [Performance Considerations](#performance-considerations)
- [Alternatives Considered](#alternatives-considered)
  - [Alternative 1: Use `hyper` Directly](#alternative-1-use-hyper-directly)
  - [Alternative 2: Custom HTTP Client Implementation](#alternative-2-custom-http-client-implementation)
  - [Alternative 3: Single Monolithic Client Type](#alternative-3-single-monolithic-client-type)
- [Related ADRs](#related-adrs)
- [References](#references)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-rust-abi-client-library`

## Context and Problem Statement

Internal CyberFabric modules (workflow engines, agents, background jobs) need to make HTTP requests to external services but are not allowed direct internet access for security and observability reasons. All outbound requests must route through OAGW. A drop-in replacement client library is needed that: (1) routes requests through OAGW's `/proxy/{alias}/*` endpoint, (2) supports shared-process mode (direct function calls) and remote mode (HTTP requests), (3) handles multiple response types (plain HTTP, SSE, streaming), (4) supports multiple protocols (HTTP/1.1, HTTP/2, WebSocket, WebTransport), (5) works as HTTP backend for third-party Rust SDKs, and (6) uses explicit alias routing where the caller specifies the OAGW upstream alias in the API.

**Current gaps**:

- No client library for internal modules to use
- No abstraction for shared-process vs remote-OAGW modes
- No streaming-aware API design
- No SDK integration strategy

**Scope**: This ADR covers HTTP/HTTPS/WS/SSE protocols. WebTransport (WT) and gRPC are future work (Phase 3).

## Decision Drivers

* Ergonomics: simple, intuitive API for internal module developers
* SDK compatibility: works as HTTP backend for third-party Rust SDKs (OpenAI, Anthropic)
* Performance: zero-copy where possible, minimal allocations
* Safety: strong typing, compile-time guarantees
* Flexibility: support plain, streaming, bidirectional protocols
* Deployment transparency: same API works in shared-process and remote modes
* Observability: request tracing, metrics collection routed through OAGW
* Testability: easy to mock for unit tests
* Security: no direct internet access from internal modules

## Considered Options

* Trait-based abstraction with dynamic dispatch
* Concrete types with feature flags
* Hybrid approach with deployment abstraction (single `OagwClient` type)

## Decision Outcome

Chosen option: "Hybrid approach with deployment abstraction", because it provides a single concrete type (`OagwClient`) that internally dispatches to the appropriate implementation based on configuration, keeping application code deployment-agnostic.

### Core API

```rust
pub struct OagwClient { inner: OagwClientImpl }

impl OagwClient {
    pub fn from_config(config: OagwClientConfig) -> Result<Self, ClientError>;
    pub async fn execute(&self, alias: &str, req: Request) -> Result<Response, ClientError>;
    pub async fn websocket(&self, alias: &str, req: Request) -> Result<WebSocketConn, ClientError>;
    pub fn execute_blocking(&self, alias: &str, req: Request) -> Result<Response, ClientError>;
}
```

### Deployment Modes

- **SharedProcess**: Direct function calls to OAGW Data Plane (same process, zero serialization)
- **RemoteProxy**: HTTP requests to OAGW `/proxy/{alias}/*` endpoint (separate process)

### Response Consumption

`Response` supports flexible consumption: `.bytes()` (buffer), `.json::<T>()` (parse), `.text()` (string), `.into_stream()` (byte stream for SSE/chunked), `.into_sse_stream()` (parsed SSE events). Error source (`gateway` vs `upstream`) extracted from `X-OAGW-Error-Source` header.

### SDK Integration Strategy

- **Phase 0 (MVP)**: Pattern 4 (Wrapper Layer) for OpenAI + Pattern 1 (Drop-In Replacement) for internal modules + Blocking API
- **Phase 1**: Pattern 2 (HTTP Proxy) for unmodified third-party SDKs
- **Phase 2**: Pattern 3 (Custom Transport Trait) as standardized pattern for new SDKs
- **Phase 3**: WebTransport & gRPC (future)

### Build Script Strategy

Build scripts continue using `ureq` directly (no OAGW routing needed at compile time). Runtime code uses `OagwClient`. Future enhancement: blocking API wrapper for build scripts that need OAGW routing.

### Consequences

* Good, because deployment-agnostic — application code never changes
* Good, because zero-cost abstraction — enum dispatch optimized by compiler
* Good, because easy to test — mock can be added as third variant
* Good, because type-safe — single concrete type, no trait objects
* Good, because configuration-driven — mode selected from config, not code
* Good, because blocking API supports sync contexts (build scripts, CLI, FFI)
* Bad, because slightly more boilerplate in enum dispatch methods
* Bad, because `RemoteProxyClient` has hard dependency on reqwest
* Bad, because external plugins require Rust implementation (no scripting)

### Confirmation

Integration tests verify: async usage, blocking usage, build-script pattern (no tokio runtime), SSE streaming consumption, WebSocket connection lifecycle, error source extraction from response headers.

## Pros and Cons of the Options

### Trait-based abstraction with dynamic dispatch

* Good, because clean separation, easy to mock
* Good, because pluggable backends
* Bad, because dynamic dispatch overhead (negligible for network I/O)

### Concrete types with feature flags

* Good, because zero-cost abstraction (static dispatch)
* Bad, because cannot use both modes in same binary
* Bad, because harder to test (feature-gated test code)

### Hybrid approach with deployment abstraction

* Good, because single concrete type, deployment-agnostic
* Good, because enum dispatch optimized by compiler
* Good, because configuration-driven mode selection
* Bad, because boilerplate in dispatch methods

## More Information

HTTP client compatibility: reqwest (full), hyper (adapter needed), ureq (sync only — blocking API or proxy mode), surf (proxy mode recommended).

Five SDK integration patterns analyzed: (1) Drop-in replacement for reqwest, (2) HTTP Proxy (standard protocol), (3) Custom HTTP Transport Trait, (4) Wrapper Layer (Facade), (5) reqwest Middleware/Interceptor. Patterns 1+4 recommended for MVP, Pattern 2 for Phase 1 universal compatibility.

## SDK Integration Patterns

Third-party Rust SDKs (OpenAI, Anthropic, AWS, etc.) need to route requests through OAGW. This section analyzes all integration approaches.

### Pattern 1: Drop-In Replacement for reqwest

Provide a `reqwest`-compatible API so SDKs can use our client as a custom HTTP backend.

```rust
pub struct OagwClient { /* ... */ }

impl OagwClient {
    pub fn get(&self, alias: &str, path: &str) -> RequestBuilder {
        RequestBuilder::new(self.clone(), Method::GET, alias, path)
    }

    pub fn post(&self, alias: &str, path: &str) -> RequestBuilder {
        RequestBuilder::new(self.clone(), Method::POST, alias, path)
    }
}

// Usage in SDK code (minimal changes)
// Before: let client = reqwest::Client::new();
// After:  let client = OagwClient::new(config);
//
// Before: client.post("https://api.openai.com/v1/chat/completions")
// After:  client.post("openai", "/v1/chat/completions")
```

* Good, because minimal SDK code changes
* Good, because familiar API for Rust developers
* Good, because type-safe at compile time
* Bad, because requires SDK source modifications (can't use unmodified crates)
* Bad, because every SDK needs manual integration
* Bad, because explicit alias parameter changes SDK API surface

**Use case**: Internal SDKs we control (custom wrappers around OpenAI/Anthropic)

### Pattern 2: HTTP Proxy (Standard Proxy Protocol)

Act as an HTTP proxy that SDKs configure via standard `HTTP_PROXY` environment variable or `reqwest::Proxy`.

```rust
// Start OAGW proxy server on localhost:8080
// Internal modules set: HTTP_PROXY=http://localhost:8080

// OAGW proxy server intercepts requests and routes based on destination host
// Request: GET https://api.openai.com/v1/chat/completions
// OAGW maps api.openai.com → "openai" alias
// OAGW proxies to /proxy/openai/v1/chat/completions
```

* Good, because works with **unmodified** third-party SDKs
* Good, because standard HTTP proxy protocol (RFC 7230)
* Good, because no SDK code changes required
* Bad, because requires host-to-alias mapping configuration (api.openai.com → "openai")
* Bad, because DNS resolution needed (or hardcoded host mapping)
* Bad, because adds local proxy server overhead
* Bad, because HTTPS CONNECT tunneling complexity for TLS

**Use case**: Drop-in compatibility with unmodified third-party SDKs

### Pattern 3: Custom HTTP Transport Trait

Define a trait that SDK maintainers implement to route through OAGW.

```rust
#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn request(&self, req: http::Request<Bytes>) -> Result<http::Response<Bytes>, Error>;
}

pub struct OpenAiClient<T: HttpTransport> {
    transport: Arc<T>,
}

pub struct OagwTransport {
    client: OagwClient,
    alias: String,
}

#[async_trait]
impl HttpTransport for OagwTransport {
    async fn request(&self, req: http::Request<Bytes>) -> Result<http::Response<Bytes>, Error> {
        let oagw_req = Request::from_http(req);
        let response = self.client.execute(&self.alias, oagw_req).await?;
        Ok(response.into_http())
    }
}
```

* Good, because clean abstraction
* Good, because testable with mock transports
* Good, because no explicit alias in SDK API
* Bad, because requires SDKs to be designed with transport abstraction
* Bad, because most existing SDKs don't support this pattern
* Bad, because cannot use unmodified third-party crates

**Use case**: Future SDK design pattern (if we influence SDK maintainers)

### Pattern 4: Wrapper Layer (Facade Pattern)

Wrap third-party SDKs with our own API that internally routes through OAGW.

```rust
pub struct CfOpenAiClient {
    oagw_client: OagwClient,
    alias: String,
}

impl CfOpenAiClient {
    pub async fn chat_completion(&self, req: ChatRequest) -> Result<ChatResponse> {
        let oagw_req = Request::builder()
            .method(Method::POST)
            .path("/v1/chat/completions")
            .json(&req)?
            .build()?;

        let response = self.oagw_client.execute(&self.alias, oagw_req).await?;
        Ok(serde_json::from_slice(&response.body)?)
    }
}
```

* Good, because complete control over API surface
* Good, because can add CF-specific features (retry policies, circuit breakers)
* Good, because no dependency on third-party SDK internals
* Bad, because must manually implement entire SDK API surface
* Bad, because maintenance burden (keep in sync with upstream SDK)
* Bad, because duplicates SDK functionality

**Use case**: High-value SDKs we want to fully control (OpenAI, Anthropic)

### Pattern 5: reqwest Middleware/Interceptor

Extend `reqwest::Client` with middleware that intercepts requests and routes through OAGW.

* Good, because transparent to SDK code
* Good, because works with unmodified SDKs (if middleware is injected at runtime)
* Bad, because `reqwest` doesn't support middleware natively
* Bad, because would require forking `reqwest` or complex runtime injection
* Bad, because fragile (depends on `reqwest` internals)

**Use case**: Not recommended (too complex)

### Recommended Strategy

**Phase 0 (MVP)**: Pattern 4 (Wrapper Layer) for OpenAI + Pattern 1 (Drop-In Replacement) for internal modules

- Immediate value: Works for critical use cases (OpenAI)
- Clean API: Internal modules get ergonomic client
- Testable: Both patterns support mocking

**Phase 1**: Pattern 2 (HTTP Proxy) for unmodified third-party SDKs

- Unlocks broader ecosystem
- No SDK modifications required
- Adds complexity (proxy server + host mapping)

**Phase 2**: Pattern 3 (Custom Transport Trait) as standardized pattern for new SDKs

**Phase 3**: WebTransport & gRPC (future)

- Protocol support for WebTransport and gRPC
- Extends client library beyond HTTP-based patterns

## HTTP Client Compatibility Analysis

### Rust HTTP Client Landscape

Popular HTTP clients in the Rust ecosystem:

1. **reqwest** - High-level, async (most popular, ~35M downloads/month)
2. **hyper** - Low-level, async (foundation for reqwest and others)
3. **ureq** - Lightweight, sync-only (used in our codebase for build scripts)
4. **surf** - Async, runtime-agnostic
5. **isahc** - Built on libcurl, supports sync and async
6. **actix-web client (awc)** - Part of actix ecosystem

### Compatibility Matrix

| HTTP Client | Pattern 1 (Drop-In) | Pattern 2 (Proxy) | Pattern 4 (Wrapper) | Direct `OagwClient` Use | Notes |
|-------------|---------------------|-------------------|---------------------|------------------------|-------|
| **reqwest** | Yes | Yes | Yes | Yes | Full compatibility - OagwClient uses reqwest internally |
| **hyper** | No | Yes | Yes | Partial | Can build custom adapter; reqwest is built on hyper |
| **ureq** | No | Yes | Yes | No | Sync-only; needs blocking wrapper or proxy mode |
| **surf** | No | Yes | Yes | Partial | Different async API; proxy mode recommended |
| **isahc** | No | Yes | Yes | Partial | Sync API needs blocking wrapper |
| **awc** | No | Yes | Yes | Partial | Actix runtime; proxy mode recommended |

**Legend**: Yes = full compatibility; Partial = works with additional adapter layer; No = incompatible with this pattern.

### Key Compatibility Issues

#### Issue 1: Synchronous vs Asynchronous APIs

The proposed `OagwClient` is fully async (requires tokio runtime), but some popular clients and use cases are synchronous:

```rust
// Proposed OagwClient: async-only
pub async fn execute(&self, alias: &str, request: Request) -> Result<Response, ClientError>;

// Current codebase usage: ureq (synchronous)
let resp = ureq::get(url).call()?;
```

**Impact**:
- Cannot use `OagwClient` in build scripts without blocking wrapper
- Cannot use in sync contexts (non-async functions, FFI, etc.)
- Existing `ureq` code cannot migrate to `OagwClient` without becoming async
- Libraries like `ureq` and `isahc` (when used in sync mode) require workarounds

**Affected scenarios**: Build scripts (`build.rs`), CLI tools that prefer sync APIs, FFI boundaries, legacy sync codebases.

#### Issue 2: Hard Dependency on reqwest

`RemoteProxyClient` directly depends on `reqwest::Client`:

```rust
struct RemoteProxyClient {
    http_client: reqwest::Client,  // Hard dependency on reqwest
    auth_token: String,
}
```

**Impact**:
- Modules using different HTTP clients must include both dependencies
- Binary size increase if module already uses another client
- Pattern 1 (Drop-In Replacement) only works for reqwest-based SDKs

**Trade-off**: reqwest provides ergonomic API and is battle-tested, but adds dependency weight and limits universal compatibility.

#### Issue 3: Build-Time HTTP Requests

The ADR doesn't address build-time usage where async runtime is unavailable. Current build scripts use `ureq` for direct synchronous downloads.

### Solutions and Mitigations

#### Solution 1: Add Blocking API Wrapper

Provide synchronous API for build scripts and sync contexts:

```rust
impl OagwClient {
    pub fn execute_blocking(
        &self,
        alias: &str,
        request: Request,
    ) -> Result<Response, ClientError> {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on(self.execute(alias, request)),
            Err(_) => tokio::runtime::Runtime::new()?.block_on(self.execute(alias, request)),
        }
    }

    pub fn blocking(&self) -> BlockingClient {
        BlockingClient { inner: self }
    }
}

pub struct BlockingClient<'a> {
    inner: &'a OagwClient,
}

impl BlockingClient<'_> {
    pub fn execute(&self, alias: &str, request: Request) -> Result<Response, ClientError> {
        self.inner.execute_blocking(alias, request)
    }

    pub fn get(&self, alias: &str, path: &str) -> BlockingRequestBuilder {
        BlockingRequestBuilder::new(self.inner, Method::GET, alias, path)
    }

    pub fn post(&self, alias: &str, path: &str) -> BlockingRequestBuilder {
        BlockingRequestBuilder::new(self.inner, Method::POST, alias, path)
    }
}
```

#### Solution 2: HTTP Proxy for Universal Compatibility

Pattern 2 (HTTP Proxy) provides universal compatibility — any HTTP client can use OAGW via standard `HTTP_PROXY` / `HTTPS_PROXY` environment variables. All requests route through OAGW transparently regardless of the HTTP client used (reqwest, ureq, surf, etc.).

#### Solution 3: Feature-Gated HTTP Client Backend

Allow selecting HTTP client backend via Cargo features (`backend-reqwest`, `backend-hyper`, `backend-ureq`). Not recommended for MVP due to complexity and testing burden.

### Build Script Strategy

**Option A (Recommended for MVP)**: Keep `ureq` for build scripts. Build-time asset downloads don't need OAGW routing; observability and policy enforcement only matter for runtime requests.

**Option B (Future Enhancement)**: OAGW routing for build scripts via blocking API wrapper, for environments requiring all network access through proxy (corporate environments, air-gapped builds).

## Detailed Design

### Usage Example (Deployment-Agnostic)

Internal module code **never changes** regardless of deployment mode:

```rust
use oagw_client::{OagwClient, OagwClientConfig, Request, Method};

pub struct MyInternalService {
    oagw_client: OagwClient,
}

impl MyInternalService {
    pub fn new() -> Result<Self, Error> {
        let config = OagwClientConfig::from_env()?;
        let oagw_client = OagwClient::from_config(config)?;
        Ok(Self { oagw_client })
    }

    pub async fn call_openai(&self, prompt: &str) -> Result<String, Error> {
        let request = Request::builder()
            .method(Method::POST)
            .path("/v1/chat/completions")
            .json(&json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": prompt}]
            }))?
            .build()?;

        let response = self.oagw_client.execute("openai", request).await?;

        if response.error_source() == ErrorSource::Gateway {
            warn!("OAGW gateway error");
        }

        let data: serde_json::Value = response.json().await?;
        Ok(data["choices"][0]["message"]["content"].as_str().unwrap().to_string())
    }

    pub async fn call_openai_streaming(&self, prompt: &str) -> Result<impl Stream<Item = String>, Error> {
        let request = Request::builder()
            .method(Method::POST)
            .path("/v1/chat/completions")
            .json(&json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": prompt}],
                "stream": true
            }))?
            .build()?;

        let response = self.oagw_client.execute("openai", request).await?;
        let mut sse = response.into_sse_stream();

        Ok(stream::unfold(sse, |mut sse| async move {
            match sse.next_event().await {
                Ok(Some(event)) if !event.data.contains("[DONE]") => {
                    let data: serde_json::Value = serde_json::from_str(&event.data).ok()?;
                    let delta = data["choices"][0]["delta"]["content"].as_str()?;
                    Some((delta.to_string(), sse))
                }
                _ => None,
            }
        }))
    }
}
```

**Configuration (environment determines mode)**:

```bash
# Development (shared-process mode)
export OAGW_MODE=shared
# No additional config needed - control plane injected by modkit

# Production (remote mode)
export OAGW_MODE=remote
export OAGW_BASE_URL=https://oagw.internal.cf
export OAGW_AUTH_TOKEN=<token>
```

### Core Types

#### Request Builder

```rust
pub struct Request {
    method: Method,
    path: String,              // Relative path: "/v1/chat/completions"
    headers: HeaderMap,
    body: Body,
    timeout: Option<Duration>,
    extensions: Extensions,
}

impl Request {
    pub fn builder() -> RequestBuilder { ... }
    pub fn method(&self) -> &Method { &self.method }
    pub fn path(&self) -> &str { &self.path }
    pub fn headers(&self) -> &HeaderMap { &self.headers }
    pub fn headers_mut(&mut self) -> &mut HeaderMap { &mut self.headers }
    pub fn body(&self) -> &Body { &self.body }
    pub fn into_body(self) -> Body { self.body }
}

pub struct RequestBuilder {
    method: Method,
    path: Option<String>,
    headers: HeaderMap,
    body: Option<Body>,
    timeout: Option<Duration>,
}

impl RequestBuilder {
    pub fn method(mut self, method: Method) -> Self { ... }
    pub fn path(mut self, path: impl Into<String>) -> Self { ... }
    pub fn header<K, V>(mut self, key: K, value: V) -> Self { ... }
    pub fn body<B: Into<Body>>(mut self, body: B) -> Self { ... }
    pub fn json<T: Serialize>(mut self, value: &T) -> Result<Self, serde_json::Error> { ... }
    pub fn timeout(mut self, duration: Duration) -> Self { ... }
    pub fn build(self) -> Result<Request, BuildError> { ... }
}
```

#### Response Types

```rust
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: ResponseBody,
    error_source: ErrorSource,  // Parsed from X-OAGW-Error-Source header
    extensions: Extensions,
}

enum ResponseBody {
    Buffered(Bytes),
    Streaming(BoxStream<'static, Result<Bytes, ClientError>>),
}

impl Response {
    pub fn status(&self) -> StatusCode { self.status }
    pub fn headers(&self) -> &HeaderMap { &self.headers }
    pub fn error_source(&self) -> ErrorSource { self.error_source }

    /// Buffer entire response body
    pub async fn bytes(self) -> Result<Bytes, ClientError> {
        match self.body {
            ResponseBody::Buffered(bytes) => Ok(bytes),
            ResponseBody::Streaming(mut stream) => {
                let mut buf = BytesMut::new();
                while let Some(chunk) = stream.next().await {
                    buf.extend_from_slice(&chunk?);
                }
                Ok(buf.freeze())
            }
        }
    }

    /// Parse response body as JSON
    pub async fn json<T: DeserializeOwned>(self) -> Result<T, ClientError> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| ClientError::InvalidResponse(e.to_string()))
    }

    /// Parse response body as text
    pub async fn text(self) -> Result<String, ClientError> {
        let bytes = self.bytes().await?;
        String::from_utf8(bytes.to_vec()).map_err(|e| ClientError::InvalidResponse(e.to_string()))
    }

    /// Consume response as byte stream (for SSE, chunked responses)
    pub fn into_stream(self) -> BoxStream<'static, Result<Bytes, ClientError>> {
        match self.body {
            ResponseBody::Buffered(bytes) => Box::pin(stream::once(async move { Ok(bytes) })),
            ResponseBody::Streaming(stream) => stream,
        }
    }

    /// Convenience: parse as Server-Sent Events stream
    pub fn into_sse_stream(self) -> SseEventStream {
        SseEventStream::new(self.into_stream())
    }
}

pub struct SseEventStream {
    inner: BoxStream<'static, Result<Bytes, ClientError>>,
    buffer: Vec<u8>,
}

impl SseEventStream {
    pub fn new(stream: BoxStream<'static, Result<Bytes, ClientError>>) -> Self {
        Self { inner: stream, buffer: Vec::new() }
    }

    pub async fn next_event(&mut self) -> Result<Option<SseEvent>, ClientError> {
        loop {
            if let Some(event) = self.parse_buffered_event()? {
                return Ok(Some(event));
            }
            match self.inner.next().await {
                Some(Ok(chunk)) => self.buffer.extend_from_slice(&chunk),
                Some(Err(e)) => return Err(e),
                None => {
                    if self.buffer.is_empty() {
                        return Ok(None);
                    } else {
                        return self.parse_buffered_event();
                    }
                }
            }
        }
    }

    fn parse_buffered_event(&mut self) -> Result<Option<SseEvent>, ClientError> {
        if let Some(pos) = self.buffer.windows(2).position(|w| w == b"\n\n") {
            let event_bytes = self.buffer.drain(..pos + 2).collect::<Vec<u8>>();
            return Ok(Some(Self::parse_sse_event(&event_bytes)?));
        }
        Ok(None)
    }

    fn parse_sse_event(data: &[u8]) -> Result<SseEvent, ClientError> {
        let mut id = None;
        let mut event = None;
        let mut data_lines = Vec::new();
        let mut retry = None;

        for line in data.split(|&b| b == b'\n') {
            if line.is_empty() { continue; }
            if let Some(colon_pos) = line.iter().position(|&b| b == b':') {
                let field = &line[..colon_pos];
                let value = &line[colon_pos + 1..];
                let value = if value.first() == Some(&b' ') { &value[1..] } else { value };
                match field {
                    b"id" => id = Some(String::from_utf8_lossy(value).to_string()),
                    b"event" => event = Some(String::from_utf8_lossy(value).to_string()),
                    b"data" => data_lines.push(String::from_utf8_lossy(value).to_string()),
                    b"retry" => { retry = String::from_utf8_lossy(value).parse().ok(); }
                    _ => {}
                }
            }
        }
        Ok(SseEvent { id, event, data: data_lines.join("\n"), retry })
    }
}

pub struct SseEvent {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: String,
    pub retry: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSource {
    Gateway,
    Upstream,
    Unknown,
}

pub struct WebSocketConn {
    send: mpsc::Sender<WsMessage>,
    recv: mpsc::Receiver<Result<WsMessage, ClientError>>,
}

impl WebSocketConn {
    pub async fn send(&mut self, msg: WsMessage) -> Result<(), ClientError> {
        self.send.send(msg).await.map_err(|_| ClientError::ConnectionClosed)
    }

    pub async fn recv(&mut self) -> Result<Option<WsMessage>, ClientError> {
        self.recv.recv().await.transpose()
    }

    pub async fn close(self) -> Result<(), ClientError> { ... }
}

pub enum WsMessage {
    Text(String),
    Binary(Bytes),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(Option<CloseFrame>),
}
```

#### Body Abstraction

```rust
pub enum Body {
    Empty,
    Bytes(Bytes),
    Stream(BoxStream<'static, Result<Bytes, std::io::Error>>),
}

impl Body {
    pub fn empty() -> Self { Body::Empty }

    pub fn from_bytes(bytes: impl Into<Bytes>) -> Self { Body::Bytes(bytes.into()) }

    pub fn from_json<T: Serialize>(value: &T) -> Result<Self, serde_json::Error> {
        Ok(Body::Bytes(serde_json::to_vec(value)?.into()))
    }

    pub fn from_stream<S>(stream: S) -> Self
    where S: Stream<Item=Result<Bytes, std::io::Error>> + Send + 'static,
    {
        Body::Stream(Box::pin(stream))
    }

    pub async fn into_bytes(self) -> Result<Bytes, ClientError> {
        match self {
            Body::Empty => Ok(Bytes::new()),
            Body::Bytes(b) => Ok(b),
            Body::Stream(mut s) => {
                let mut buf = BytesMut::new();
                while let Some(chunk) = s.next().await {
                    buf.extend_from_slice(&chunk?);
                }
                Ok(buf.freeze())
            }
        }
    }
}

impl From<()> for Body { fn from(_: ()) -> Self { Body::Empty } }
impl From<Bytes> for Body { fn from(b: Bytes) -> Self { Body::Bytes(b) } }
impl From<Vec<u8>> for Body { fn from(v: Vec<u8>) -> Self { Body::Bytes(v.into()) } }
impl From<String> for Body { fn from(s: String) -> Self { Body::Bytes(s.into()) } }
```

#### Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Request build error: {0}")]
    BuildError(String),

    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {status}")]
    Http { status: StatusCode, body: Bytes },
}
```

### Client Implementations

#### Public API (Deployment-Agnostic)

```rust
pub struct OagwClient {
    inner: OagwClientImpl,
}

enum OagwClientImpl {
    SharedProcess(SharedProcessClient),
    RemoteProxy(RemoteProxyClient),
}

impl OagwClient {
    pub fn from_config(config: OagwClientConfig) -> Result<Self, ClientError> {
        let inner = match config.mode {
            ClientMode::SharedProcess { control_plane } => {
                OagwClientImpl::SharedProcess(SharedProcessClient::new(control_plane)?)
            }
            ClientMode::RemoteProxy { base_url, auth_token, timeout } => {
                OagwClientImpl::RemoteProxy(RemoteProxyClient::new(base_url, auth_token, timeout)?)
            }
        };
        Ok(Self { inner })
    }

    pub async fn execute(&self, alias: &str, request: Request) -> Result<Response, ClientError> {
        match &self.inner {
            OagwClientImpl::SharedProcess(c) => c.execute(alias, request).await,
            OagwClientImpl::RemoteProxy(c) => c.execute(alias, request).await,
        }
    }

    pub async fn websocket(&self, alias: &str, request: Request) -> Result<WebSocketConn, ClientError> {
        match &self.inner {
            OagwClientImpl::SharedProcess(c) => c.websocket(alias, request).await,
            OagwClientImpl::RemoteProxy(c) => c.websocket(alias, request).await,
        }
    }
}

pub struct OagwClientConfig {
    pub mode: ClientMode,
    pub default_timeout: Duration,
}

pub enum ClientMode {
    SharedProcess { control_plane: Arc<dyn ControlPlaneService> },
    RemoteProxy { base_url: String, auth_token: String, timeout: Duration },
}

impl OagwClientConfig {
    pub fn from_env() -> Result<Self, ClientError> {
        match std::env::var("OAGW_MODE").as_deref() {
            Ok("shared") => {
                let control_plane = get_control_plane_from_di()?;
                Ok(Self {
                    mode: ClientMode::SharedProcess { control_plane },
                    default_timeout: Duration::from_secs(30),
                })
            }
            Ok("remote") | Err(_) => {
                let base_url = std::env::var("OAGW_BASE_URL")
                    .unwrap_or_else(|_| "https://oagw.internal.cf".to_string());
                let auth_token = std::env::var("OAGW_AUTH_TOKEN")
                    .map_err(|e| ClientError::BuildError(format!("OAGW_AUTH_TOKEN not set: {e}")))?;
                let default_timeout = Duration::from_secs(30);
                Ok(Self {
                    mode: ClientMode::RemoteProxy {
                        base_url, auth_token, timeout: default_timeout,
                    },
                    default_timeout,
                })
            }
            Ok(other) => Err(ClientError::BuildError(format!(
                "unknown OAGW_MODE '{other}': allowed values are \"shared\", \"remote\"",
            ))),
        }
    }
}
```

##### Metrics RAII Guard

Both client implementations use an RAII guard to ensure `requests_in_flight` is always decremented and `request_duration` is always observed, even on early error returns:

```rust
struct InFlightGuard<'a> {
    metrics: &'a Metrics,
    start: Instant,
}

impl<'a> InFlightGuard<'a> {
    fn new(metrics: &'a Metrics) -> Self {
        metrics.requests_in_flight.inc();
        Self { metrics, start: Instant::now() }
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.metrics.requests_in_flight.dec();
        self.metrics.request_duration.observe(self.start.elapsed().as_secs_f64());
    }
}
```

#### Shared-Process Client (Internal Implementation)

Used when internal module and OAGW run in the same process (development, single-executable deployment).

```rust
struct SharedProcessClient {
    control_plane: Arc<dyn ControlPlaneService>,
    metrics: Arc<Metrics>,
}

impl SharedProcessClient {
    fn new(control_plane: Arc<dyn ControlPlaneService>) -> Result<Self, ClientError> {
        Ok(Self { control_plane, metrics: Arc::new(Metrics::default()) })
    }

    async fn execute(&self, alias: &str, request: Request) -> Result<Response, ClientError> {
        let _guard = InFlightGuard::new(&self.metrics);

        let proxy_request = ProxyRequest {
            alias: alias.to_string(),
            method: request.method().clone(),
            path: request.path().to_string(),
            headers: request.headers().clone(),
            body: request.into_body(),
        };

        let proxy_response = self.control_plane.proxy_request(proxy_request).await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        let error_source = proxy_response.headers
            .get("x-oagw-error-source")
            .and_then(|v| v.to_str().ok())
            .map(|s| match s {
                "gateway" => ErrorSource::Gateway,
                "upstream" => ErrorSource::Upstream,
                _ => ErrorSource::Unknown,
            })
            .unwrap_or(ErrorSource::Unknown);

        let body = if proxy_response.is_streaming {
            ResponseBody::Streaming(proxy_response.body_stream)
        } else {
            ResponseBody::Buffered(proxy_response.body)
        };

        Ok(Response { status: proxy_response.status, headers: proxy_response.headers, body, error_source, extensions: Extensions::default() })
    }

    async fn websocket(&self, alias: &str, request: Request) -> Result<WebSocketConn, ClientError> {
        let proxy_request = ProxyRequest {
            alias: alias.to_string(),
            method: request.method().clone(),
            path: request.path().to_string(),
            headers: request.headers().clone(),
            body: request.into_body(),
        };

        let ws_conn = self.control_plane.proxy_websocket(proxy_request).await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        Ok(ws_conn)
    }
}
```

#### Remote OAGW Client (Internal Implementation)

Used when internal module and OAGW run in separate processes (production, microservice deployment).

```rust
struct RemoteProxyClient {
    oagw_base_url: String,
    http_client: reqwest::Client,
    auth_token: String,
    metrics: Arc<Metrics>,
}

impl RemoteProxyClient {
    fn new(base_url: String, auth_token: String, timeout: Duration) -> Result<Self, ClientError> {
        let http_client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| ClientError::BuildError(e.to_string()))?;

        Ok(Self { oagw_base_url: base_url, http_client, auth_token, metrics: Arc::new(Metrics::default()) })
    }

    async fn execute(&self, alias: &str, request: Request) -> Result<Response, ClientError> {
        let _guard = InFlightGuard::new(&self.metrics);

        // Build URL: https://oagw.internal.cf/api/oagw/v1/proxy/{alias}{path}
        let url = format!("{}/api/oagw/v1/proxy/{}{}",
            self.oagw_base_url, alias, request.path());

        let mut req_builder = self.http_client.request(request.method().clone(), &url)
            .header("Authorization", format!("Bearer {}", self.auth_token));

        for (name, value) in request.headers() {
            req_builder = req_builder.header(name, value);
        }

        match request.into_body() {
            Body::Empty => {}
            Body::Bytes(b) => { req_builder = req_builder.body(b.to_vec()); }
            Body::Stream(_) => {
                return Err(ClientError::BuildError(
                    "Streaming body not supported for plain requests".into()
                ));
            }
        }

        let resp = req_builder.send().await.map_err(|e| self.map_reqwest_error(e))?;

        let status = resp.status();
        let headers = resp.headers().clone();

        let error_source = headers
            .get("x-oagw-error-source")
            .and_then(|v| v.to_str().ok())
            .map(|s| match s {
                "gateway" => ErrorSource::Gateway,
                "upstream" => ErrorSource::Upstream,
                _ => ErrorSource::Unknown,
            })
            .unwrap_or(ErrorSource::Unknown);

        let stream = resp.bytes_stream()
            .map_err(|e| ClientError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)));

        Ok(Response {
            status, headers,
            body: ResponseBody::Streaming(Box::pin(stream)),
            error_source, extensions: Extensions::default(),
        })
    }

    async fn websocket(&self, alias: &str, request: Request) -> Result<WebSocketConn, ClientError> {
        let ws_url = self.oagw_base_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        let url = format!("{}/api/oagw/v1/proxy/{}{}", ws_url, alias, request.path());

        let req = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", generate_ws_key())
            .body(())
            .map_err(|e| ClientError::BuildError(e.to_string()))?;

        let (ws_stream, _) = tokio_tungstenite::connect_async(req).await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        let (write, read) = ws_stream.split();
        let (send_tx, mut send_rx) = mpsc::channel(16);
        let (recv_tx, recv_rx) = mpsc::channel(16);

        // Send task
        tokio::spawn(async move {
            let mut write = write;
            while let Some(msg) = send_rx.recv().await {
                let ws_msg = match msg {
                    WsMessage::Text(s) => tokio_tungstenite::tungstenite::Message::Text(s),
                    WsMessage::Binary(b) => tokio_tungstenite::tungstenite::Message::Binary(b.to_vec()),
                    WsMessage::Ping(p) => tokio_tungstenite::tungstenite::Message::Ping(p),
                    WsMessage::Pong(p) => tokio_tungstenite::tungstenite::Message::Pong(p),
                    WsMessage::Close(f) => tokio_tungstenite::tungstenite::Message::Close(f),
                };
                if write.send(ws_msg).await.is_err() { break; }
            }
        });

        // Receive task
        tokio::spawn(async move {
            let mut read = read;
            while let Some(msg) = read.next().await {
                let result = match msg {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(s)) => Ok(WsMessage::Text(s)),
                    Ok(tokio_tungstenite::tungstenite::Message::Binary(b)) => Ok(WsMessage::Binary(b.into())),
                    Ok(tokio_tungstenite::tungstenite::Message::Ping(p)) => Ok(WsMessage::Ping(p)),
                    Ok(tokio_tungstenite::tungstenite::Message::Pong(p)) => Ok(WsMessage::Pong(p)),
                    Ok(tokio_tungstenite::tungstenite::Message::Close(f)) => Ok(WsMessage::Close(f)),
                    Err(e) => Err(ClientError::Protocol(e.to_string())),
                    _ => continue,
                };
                if recv_tx.send(result).await.is_err() { break; }
            }
        });

        Ok(WebSocketConn { send: send_tx, recv: recv_rx })
    }

    fn map_reqwest_error(&self, error: reqwest::Error) -> ClientError {
        if error.is_timeout() {
            ClientError::Timeout(error.to_string())
        } else if error.is_connect() {
            ClientError::Connection(error.to_string())
        } else {
            ClientError::Protocol(error.to_string())
        }
    }
}
```

### Out of Scope: OAGW Plugin Development

Plugin development APIs (PluginContext, Starlark integration) are **not part of this client library**. They belong in OAGW's plugin system (see [ADR: Plugin System](./0003-plugin-system.md)).

This client library is solely for **internal modules** to make HTTP requests **through** OAGW, not for developing plugins that **run inside** OAGW.

### WebTransport Support (Future)

```rust
pub struct WebTransportConn {
    session: webtransport::Session,
}

impl WebTransportConn {
    pub async fn open_stream(&mut self) -> Result<WebTransportStream, ClientError> {
        todo!("WebTransport implementation")
    }

    pub async fn accept_stream(&mut self) -> Result<WebTransportStream, ClientError> {
        todo!("WebTransport implementation")
    }

    pub async fn send_datagram(&mut self, data: Bytes) -> Result<(), ClientError> {
        todo!("WebTransport implementation")
    }

    pub async fn recv_datagram(&mut self) -> Result<Bytes, ClientError> {
        todo!("WebTransport implementation")
    }
}

pub struct WebTransportStream {
    send: BoxSink<'static, Bytes>,
    recv: BoxStream<'static, Result<Bytes, ClientError>>,
}
```

## Implementation Plan

### Phase 0 (MVP): Core Client Types & RemoteProxyClient

- `Request`, `Response`, `Body`, `ErrorSource` types
- `ResponseBody` enum (Buffered/Streaming) - streaming built-in from day 1
- `OagwClient` type with deployment abstraction
- `RemoteProxyClient` implementation (HTTP to OAGW `/proxy/{alias}/*`)
- Response consumption patterns: `.bytes()`, `.json()`, `.into_stream()`, `.into_sse_stream()`
- Error source distinction (`X-OAGW-Error-Source` header parsing)
- SSE event stream parsing (`SseEventStream`, `SseEvent`)
- Error handling and metrics
- SDK Integration: Pattern 4 (Wrapper Layer) for OpenAI

**Deliverable**: Internal modules can make HTTP requests (buffered and streaming) through OAGW in production

### Phase 1: SharedProcessClient

- `SharedProcessClient` implementation (direct function calls to Data Plane)
- Integration with `ControlPlaneService` trait
- Configuration abstraction via `OagwClientConfig::from_env()`
- Automatic mode selection (shared vs remote)

**Deliverable**: Development mode with zero serialization overhead, deployment-agnostic code

### Phase 2: WebSocket Support

- `WebSocketConn` type
- WebSocket upgrade handling
- Bidirectional message passing
- Connection lifecycle management
- Support in both client implementations

**Deliverable**: WebSocket connections work through OAGW

### Phase 3: SDK HTTP Proxy, WebTransport & gRPC

- Implement Pattern 2 (HTTP Proxy)
- Local proxy server on `localhost:8080`
- Host-to-alias mapping configuration
- Transparent support for unmodified third-party SDKs
- QUIC transport layer (future)
- `WebTransportConn` implementation (future)
- Stream multiplexing (future)
- Datagram support (future)
- gRPC transport support via tonic (future)

**Deliverable**: Any Rust SDK can use OAGW via `HTTP_PROXY` env var; WebTransport and gRPC protocol support (future)

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_client() -> OagwClient {
        let config = OagwClientConfig {
            mode: ClientMode::RemoteProxy {
                base_url: "https://oagw.internal.cf".to_string(),
                auth_token: "test-token".to_string(),
                timeout: Duration::from_secs(30),
            },
            default_timeout: Duration::from_secs(30),
        };
        OagwClient::from_config(config).unwrap()
    }

    #[tokio::test]
    async fn test_buffered_response() {
        let client = create_test_client();
        let request = Request::builder()
            .method(Method::POST)
            .path("/v1/chat/completions")
            .json(&json!({"model": "gpt-4", "messages": []}))
            .unwrap()
            .build()
            .unwrap();

        let response = client.execute("openai", request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.error_source(), ErrorSource::Upstream);
        let data = response.json::<ChatResponse>().await.unwrap();
        assert!(!data.choices.is_empty());
    }

    #[tokio::test]
    async fn test_streaming_response() {
        let client = create_test_client();
        let request = Request::builder()
            .method(Method::POST)
            .path("/v1/chat/completions")
            .json(&json!({"model": "gpt-4", "messages": [], "stream": true}))
            .unwrap()
            .build()
            .unwrap();

        let response = client.execute("openai", request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.error_source(), ErrorSource::Upstream);

        let mut stream = response.into_stream();
        let mut chunks = 0;
        while let Some(chunk) = stream.next().await {
            chunk.unwrap();
            chunks += 1;
        }
        assert!(chunks > 0);
    }

    #[tokio::test]
    async fn test_sse_streaming() {
        let client = create_test_client();
        let request = Request::builder()
            .method(Method::POST)
            .path("/v1/chat/completions")
            .json(&json!({"model": "gpt-4", "messages": [], "stream": true}))
            .unwrap()
            .build()
            .unwrap();

        let response = client.execute("openai", request).await.unwrap();
        let mut sse = response.into_sse_stream();
        while let Some(event) = sse.next_event().await.unwrap() {
            println!("SSE event: {}", event.data);
            if event.data.contains("[DONE]") { break; }
        }
    }

    #[tokio::test]
    async fn test_websocket() {
        let client = create_test_client();
        let request = Request::builder()
            .method(Method::GET)
            .path("/ws")
            .build()
            .unwrap();

        let mut conn = client.websocket("echo-service", request).await.unwrap();
        conn.send(WsMessage::Text("hello".into())).await.unwrap();
        let msg = conn.recv().await.unwrap().unwrap();
        match msg {
            WsMessage::Text(s) => assert_eq!(s, "hello"),
            _ => panic!("Expected text message"),
        }
    }

    #[tokio::test]
    async fn test_error_source_gateway() {
        let client = create_test_client();
        let request = Request::builder()
            .method(Method::GET)
            .path("/nonexistent")
            .build()
            .unwrap();

        let response = client.execute("invalid-alias", request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.error_source(), ErrorSource::Gateway);
    }

    #[tokio::test]
    async fn test_deployment_agnostic_code() {
        let shared_config = OagwClientConfig {
            mode: ClientMode::SharedProcess {
                control_plane: get_mock_control_plane(),
            },
            default_timeout: Duration::from_secs(30),
        };
        let shared_client = OagwClient::from_config(shared_config).unwrap();

        let remote_config = OagwClientConfig {
            mode: ClientMode::RemoteProxy {
                base_url: "https://oagw.internal.cf".to_string(),
                auth_token: "test-token".to_string(),
                timeout: Duration::from_secs(30),
            },
            default_timeout: Duration::from_secs(30),
        };
        let remote_client = OagwClient::from_config(remote_config).unwrap();

        let request1 = Request::builder()
            .method(Method::POST)
            .path("/v1/chat/completions")
            .json(&json!({"model": "gpt-4", "messages": []}))
            .unwrap()
            .build()
            .unwrap();

        let request2 = Request::builder()
            .method(Method::POST)
            .path("/v1/chat/completions")
            .json(&json!({"model": "gpt-4", "messages": []}))
            .unwrap()
            .build()
            .unwrap();

        let response1 = shared_client.execute("openai", request1).await;
        let response2 = remote_client.execute("openai", request2).await;
        assert!(response1.is_ok() || response2.is_ok());
    }
}
```

### Mock Client for Testing

Mock can be added as a third variant in the client enum:

```rust
enum OagwClientImpl {
    SharedProcess(SharedProcessClient),
    RemoteProxy(RemoteProxyClient),
    #[cfg(test)]
    Mock(Arc<MockClient>),
}

#[cfg(test)]
struct MockClient {
    responses: Arc<Mutex<HashMap<String, VecDeque<Result<Response, ClientError>>>>>,
}

#[cfg(test)]
impl MockClient {
    fn new() -> Self {
        Self { responses: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn push_response(&self, alias: &str, response: Response) {
        self.responses.lock().unwrap()
            .entry(alias.to_string()).or_default()
            .push_back(Ok(response));
    }

    fn push_error(&self, alias: &str, error: ClientError) {
        self.responses.lock().unwrap()
            .entry(alias.to_string()).or_default()
            .push_back(Err(error));
    }

    async fn execute(&self, alias: &str, _request: Request) -> Result<Response, ClientError> {
        self.responses.lock().unwrap()
            .get_mut(alias)
            .and_then(|queue| queue.pop_front())
            .unwrap_or(Err(ClientError::Connection(
                format!("No mock response configured for alias: {}", alias)
            )))
    }

    async fn websocket(&self, _alias: &str, _request: Request) -> Result<WebSocketConn, ClientError> {
        Err(ClientError::BuildError("Mock WebSocket not implemented".into()))
    }
}

#[cfg(test)]
impl OagwClient {
    pub fn mock() -> (Self, Arc<MockClient>) {
        let mock = Arc::new(MockClient::new());
        let client = Self { inner: OagwClientImpl::Mock(Arc::clone(&mock)) };
        (client, mock)
    }
}

// Usage in tests
#[tokio::test]
async fn test_with_mock() {
    let (client, mock) = OagwClient::mock();

    mock.push_response("openai", Response {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: ResponseBody::Buffered(Bytes::from(r#"{"choices": []}"#)),
        error_source: ErrorSource::Upstream,
        extensions: Extensions::default(),
    });

    let request = Request::builder()
        .method(Method::POST)
        .path("/v1/chat/completions")
        .build()
        .unwrap();

    let response = client.execute("openai", request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

## Dependencies

```toml
[dependencies]
# Core HTTP client (for RemoteProxyClient)
reqwest = { version = "0.11", features = ["json", "stream"] }
hyper = "1.5"  # Updated to match workspace version
http = "1.3"   # Updated to match workspace version
bytes = "1.11" # Updated to match workspace version

# Async runtime
# Note: Blocking API uses tokio::runtime::Runtime internally
tokio = { version = "1.47", features = ["full", "rt-multi-thread"] }
futures = "0.3"
async-trait = "0.1"

# WebSocket
tokio-tungstenite = "0.21"

# WebTransport (future)
# webtransport = "0.1"

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Error handling
thiserror = "2.0"  # Updated to match workspace version
anyhow = "1.0"

# Metrics
prometheus = "0.13"

# Tracing
tracing = "0.1"

# Out-of-process RPC (future)
# tonic = "0.10"
# prost = "0.12"

[dev-dependencies]
# HTTP server for testing proxy mode
axum = "0.8"
tower = "0.5"

# Testing utilities
tokio-test = "0.4"
httpmock = "0.8"

# For compatibility tests
ureq = "2.12"  # Test sync client compatibility
```

**Notes on dependencies**:

1. **reqwest**: Required for `RemoteProxyClient`. Version aligned with workspace.
2. **tokio runtime**: Blocking API (`execute_blocking()`) requires `rt-multi-thread` feature to create temporary runtimes.
3. **Version alignment**: Updated versions to match workspace `Cargo.toml` (hyper 1.5, http 1.3, bytes 1.11, thiserror 2.0).
4. **Dev dependencies**: Added `ureq` for testing sync client compatibility via HTTP proxy mode.

## Security Considerations

1. **Timeout enforcement**: All requests have mandatory timeouts to prevent resource exhaustion
2. **Body size limits**: Maximum body size (100MB) enforced before buffering
3. **TLS verification**: Certificate validation always enabled (no insecure mode in production)
4. **Connection pooling**: Limits on idle connections per host to prevent resource leaks
5. **Sandbox isolation**: Starlark plugins cannot make network requests directly
6. **Header validation**: Well-known hop-by-hop headers stripped automatically
7. **WebSocket security**: Proper origin validation and frame size limits

## Performance Considerations

1. **Zero-copy streaming**: SSE and chunked responses use stream-based processing
2. **Connection reuse**: HTTP/1.1 and HTTP/2 connection pooling enabled
3. **Adaptive window sizing**: HTTP/2 flow control optimized for throughput
4. **Minimal allocations**: `Bytes` type uses reference counting for zero-copy operations
5. **Async I/O**: Non-blocking operations throughout the stack
6. **Metrics overhead**: Negligible (<0.1ms per request)

## Alternatives Considered

### Alternative 1: Use `hyper` Directly

* Good, because lower-level control and slightly better performance
* Bad, because more complex API, more boilerplate code, missing high-level features (redirects, cookies, etc.)

**Decision**: Use `reqwest` (built on `hyper`) for better ergonomics.

### Alternative 2: Custom HTTP Client Implementation

* Good, because full control over implementation, optimized for OAGW use cases
* Bad, because significant development effort, maintenance burden, likely inferior to battle-tested libraries

**Decision**: Use existing libraries (`reqwest`, `tokio-tungstenite`).

### Alternative 3: Single Monolithic Client Type

* Good, because simpler API surface
* Bad, because cannot support multiple backends, harder to test, less flexible for future extensions

**Decision**: Use hybrid approach with enum-based deployment abstraction.

## Related ADRs

- [ADR: Component Architecture](./0001-component-architecture.md) - OAGW deployment modes (shared-process vs microservice)
- [ADR: Error Source Distinction](./0013-error-source-distinction.md) - `X-OAGW-Error-Source` header for gateway vs upstream errors
- [ADR: Plugin System](./0003-plugin-system.md) - OAGW plugin development (separate from this client library)
- [ADR: Request Routing](./0002-request-routing.md) - How OAGW routes requests to upstreams

## References

- [reqwest documentation](https://docs.rs/reqwest)
- [hyper documentation](https://docs.rs/hyper)
- [tokio-tungstenite documentation](https://docs.rs/tokio-tungstenite)
- [WebTransport specification](https://w3c.github.io/webtransport/)
- [Server-Sent Events specification](https://html.spec.whatwg.org/multipage/server-sent-events.html)
- [WebSocket protocol RFC 6455](https://datatracker.ietf.org/doc/html/rfc6455)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-interface-sdk-client` — Public Rust trait for inter-module communication
* `cpt-cf-oagw-fr-request-proxy` — Client library routes all requests through OAGW proxy
* `cpt-cf-oagw-fr-streaming` — SSE and WebSocket support in client API
* `cpt-cf-oagw-nfr-credential-isolation` — No direct internet access; all credentials handled by OAGW
* `cpt-cf-oagw-nfr-observability` — All requests routed through OAGW for centralized logging
