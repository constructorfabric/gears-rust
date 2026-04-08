# Feature: Streaming & Protocol Support


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [SSE Proxy Flow](#sse-proxy-flow)
  - [WebSocket Proxy Flow](#websocket-proxy-flow)
  - [WebTransport Proxy Flow](#webtransport-proxy-flow)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [HTTP Version Negotiation](#http-version-negotiation)
  - [Streaming Connection Lifecycle](#streaming-connection-lifecycle)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement SSE Streaming Proxy](#implement-sse-streaming-proxy)
  - [Implement WebSocket Streaming Proxy](#implement-websocket-streaming-proxy)
  - [Implement WebTransport Streaming Proxy](#implement-webtransport-streaming-proxy)
  - [Implement HTTP Version Negotiation and Protocol Cache](#implement-http-version-negotiation-and-protocol-cache)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Additional Context](#7-additional-context)
  - [Performance Considerations](#performance-considerations)
  - [Security Considerations](#security-considerations)
  - [Configuration Parameters](#configuration-parameters)
  - [Deliberate Omissions](#deliberate-omissions)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-oagw-featstatus-streaming-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p2` - `cpt-cf-oagw-feature-streaming`

## 1. Feature Context

### 1.1 Overview

Extend the base HTTP proxy engine with SSE (Server-Sent Events), WebSocket, and WebTransport streaming support, including proper connection lifecycle management and adaptive HTTP version negotiation with protocol version caching.

### 1.2 Purpose

Many external APIs use SSE for streaming responses (e.g., OpenAI chat completions); WebSocket and WebTransport are needed for bidirectional real-time protocols. This feature extends the proxy flow defined in `cpt-cf-oagw-feature-proxy-engine` with streaming-aware connection handling. Covers `cpt-cf-oagw-fr-streaming`.

Design component: `cpt-cf-oagw-interface-api` (streaming variant of the proxy endpoint).

Design principles enforced (inherited from `cpt-cf-oagw-feature-proxy-engine`): `cpt-cf-oagw-principle-error-source`, `cpt-cf-oagw-principle-no-retry`, `cpt-cf-oagw-principle-cred-isolation`.

Design constraints enforced: `cpt-cf-oagw-constraint-https-only`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-oagw-actor-app-developer` | Sends proxy requests to SSE/WebSocket/WebTransport endpoints |
| `cpt-cf-oagw-actor-upstream-service` | Provides streaming responses via SSE/WebSocket/WebTransport |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — `cpt-cf-oagw-fr-streaming`
- **Design**: [DESIGN.md](../DESIGN.md) — `cpt-cf-oagw-interface-api`, `cpt-cf-oagw-seq-proxy-flow`
- **ADR**: [ADR/0014-grpc-support.md](../ADR/0014-grpc-support.md) (out-of-scope reference)
- **Dependencies**: `cpt-cf-oagw-feature-proxy-engine`

## 2. Actor Flows (CDSL)

### SSE Proxy Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-streaming-sse`

**Actor**: `cpt-cf-oagw-actor-app-developer`

**Success Scenarios**:
- SSE connection is established to upstream and events are forwarded to the caller in real time
- Connection closes cleanly when upstream sends final event or client disconnects

**Error Scenarios**:
- Upstream does not respond with `Content-Type: text/event-stream` (502 ProtocolError)
- Upstream connection drops mid-stream (502 StreamAborted)
- Idle timeout exceeded with no events received (504 IdleTimeout)
- Auth/guard/body validation failures before streaming begins (same as base proxy flow)

**Steps**:
1. [ ] - `p1` - Actor sends `{METHOD} /api/oagw/v1/proxy/{alias}[/{path}][?{query}]` with `Accept: text/event-stream` header - `inst-sse-1`
2. [ ] - `p1` - Execute base proxy flow steps (auth, alias resolution, route matching, plugin chain) via `cpt-cf-oagw-flow-proxy-request` - `inst-sse-2`
3. [ ] - `p1` - Negotiate HTTP version with upstream via `cpt-cf-oagw-algo-protocol-version-negotiation` - `inst-sse-3`
4. [ ] - `p1` - Open connection to upstream endpoint (HTTPS-only per `cpt-cf-oagw-constraint-https-only`) - `inst-sse-4`
5. [ ] - `p1` - Forward request to upstream with original headers (after plugin chain transformation) - `inst-sse-5`
6. [ ] - `p1` - **IF** upstream responds with `Content-Type: text/event-stream` and status 200 - `inst-sse-6`
   1. [ ] - `p1` - Set response `Content-Type: text/event-stream` and begin streaming to caller - `inst-sse-6a`
   2. [ ] - `p1` - Set response header `X-OAGW-Error-Source: upstream` (data originates from upstream) - `inst-sse-6b`
7. [ ] - `p1` - **ELSE** (non-SSE response) - `inst-sse-7`
   1. [ ] - `p1` - **IF** upstream returns non-2xx status - `inst-sse-7a`
      1. [ ] - `p1` - **RETURN** upstream error response as-is with `X-OAGW-Error-Source: upstream` - `inst-sse-7a1`
   2. [ ] - `p1` - **ELSE** (2xx but not `text/event-stream`) - `inst-sse-7b`
      1. [ ] - `p1` - **RETURN** 502 ProtocolError with `X-OAGW-Error-Source: gateway` — expected SSE but received different content type - `inst-sse-7b1`
8. [ ] - `p1` - **FOR EACH** SSE event received from upstream - `inst-sse-8`
   1. [ ] - `p1` - Forward event data to caller as-is (preserve `data:`, `event:`, `id:`, `retry:` fields) - `inst-sse-8a`
9. [ ] - `p1` - **IF** upstream closes connection (EOF) - `inst-sse-9`
   1. [ ] - `p1` - Close caller connection gracefully - `inst-sse-9a`
10. [ ] - `p1` - **IF** caller disconnects before upstream completes - `inst-sse-10`
    1. [ ] - `p1` - Abort upstream connection and release resources - `inst-sse-10a`
11. [ ] - `p1` - **IF** upstream connection drops unexpectedly (TCP reset, TLS error) - `inst-sse-11`
    1. [ ] - `p1` - **RETURN** 502 StreamAborted with `X-OAGW-Error-Source: gateway` - `inst-sse-11a`
12. [ ] - `p1` - **IF** idle timeout exceeded (no events received within configured timeout) - `inst-sse-12`
    1. [ ] - `p1` - **RETURN** 504 IdleTimeout with `X-OAGW-Error-Source: gateway` - `inst-sse-12a`

### WebSocket Proxy Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-streaming-websocket`

**Actor**: `cpt-cf-oagw-actor-app-developer`

**Success Scenarios**:
- WebSocket upgrade negotiated with both caller and upstream
- Bidirectional messages forwarded transparently
- Close frames propagated cleanly in both directions

**Error Scenarios**:
- Upstream rejects WebSocket upgrade (502 ProtocolError)
- Upstream connection drops during session (502 StreamAborted)
- Idle timeout on WebSocket session (504 IdleTimeout)
- Auth/guard failures before upgrade (same as base proxy flow)

**Steps**:
1. [ ] - `p1` - Actor sends `GET /api/oagw/v1/proxy/{alias}[/{path}]` with `Upgrade: websocket` and `Connection: Upgrade` headers - `inst-ws-1`
2. [ ] - `p1` - Execute base proxy flow steps (auth, alias resolution, route matching, guard plugins) via `cpt-cf-oagw-flow-proxy-request` — transform plugins are NOT executed on WebSocket frames - `inst-ws-2`
3. [ ] - `p1` - Initiate WebSocket upgrade handshake with upstream endpoint (HTTPS/WSS-only) - `inst-ws-3`
4. [ ] - `p1` - **IF** upstream accepts upgrade (101 Switching Protocols) - `inst-ws-4`
   1. [ ] - `p1` - Complete upgrade with caller (101 Switching Protocols) - `inst-ws-4a`
5. [ ] - `p1` - **ELSE** (upstream rejects upgrade) - `inst-ws-5`
   1. [ ] - `p1` - **RETURN** 502 ProtocolError with `X-OAGW-Error-Source: gateway` — upstream rejected WebSocket upgrade - `inst-ws-5a`
6. [ ] - `p1` - **FOR EACH** message from caller - `inst-ws-6`
   1. [ ] - `p1` - **IF** message exceeds configured max frame size (default: no limit; pass-through) - `inst-ws-6a`
      1. [ ] - `p1` - Send Close frame (1009 Message Too Big) to caller and abort upstream - `inst-ws-6a1`
   2. [ ] - `p1` - Forward message to upstream (text or binary frame, preserve opcode) - `inst-ws-6b`
7. [ ] - `p1` - **FOR EACH** message from upstream - `inst-ws-7`
   1. [ ] - `p1` - **IF** message exceeds configured max frame size (default: no limit; pass-through) - `inst-ws-7a`
      1. [ ] - `p1` - Send Close frame (1009 Message Too Big) to upstream and close caller - `inst-ws-7a1`
   2. [ ] - `p1` - Forward message to caller (text or binary frame, preserve opcode) - `inst-ws-7b`
8. [ ] - `p1` - **IF** either side sends Close frame - `inst-ws-8`
   1. [ ] - `p1` - Forward Close frame to the other side with status code and reason - `inst-ws-8a`
   2. [ ] - `p1` - Wait for Close frame response (up to close timeout) - `inst-ws-8b`
   3. [ ] - `p1` - Release connection resources - `inst-ws-8c`
9. [ ] - `p1` - **IF** upstream connection drops unexpectedly - `inst-ws-9`
   1. [ ] - `p1` - Send Close frame (1006 Abnormal Closure) to caller - `inst-ws-9a`
   2. [ ] - `p1` - Release connection resources - `inst-ws-9b`
10. [ ] - `p1` - **IF** caller disconnects unexpectedly - `inst-ws-10`
    1. [ ] - `p1` - Send Close frame to upstream - `inst-ws-10a`
    2. [ ] - `p1` - Release connection resources - `inst-ws-10b`
11. [ ] - `p1` - **IF** idle timeout exceeded (no messages in either direction within configured timeout) - `inst-ws-11`
    1. [ ] - `p1` - Send Close frame (1001 Going Away) to both sides - `inst-ws-11a`
    2. [ ] - `p1` - Release connection resources - `inst-ws-11b`

### WebTransport Proxy Flow

- [ ] `p2` - **ID**: `cpt-cf-oagw-flow-streaming-webtransport`

**Actor**: `cpt-cf-oagw-actor-app-developer`

**Success Scenarios**:
- WebTransport session established with upstream over HTTP/2 (HTTP/3 future work)
- Multiplexed streams forwarded bidirectionally
- Session closes cleanly

**Error Scenarios**:
- Upstream does not support WebTransport (502 ProtocolError)
- HTTP/2 negotiation fails (502 ProtocolError with fallback guidance)
- Session drops mid-stream (502 StreamAborted)

**Steps**:
1. [ ] - `p2` - Actor initiates WebTransport session via `CONNECT` to `/api/oagw/v1/proxy/{alias}[/{path}]` with `:protocol` pseudo-header set to `webtransport` - `inst-wt-1`
2. [ ] - `p2` - Execute base proxy flow steps (auth, alias resolution, route matching, guard plugins) via `cpt-cf-oagw-flow-proxy-request` - `inst-wt-2`
3. [ ] - `p2` - Negotiate HTTP/2 with upstream via `cpt-cf-oagw-algo-protocol-version-negotiation` - `inst-wt-3`
4. [ ] - `p2` - **IF** HTTP/2 negotiation fails - `inst-wt-4`
   1. [ ] - `p2` - **RETURN** 502 ProtocolError with `X-OAGW-Error-Source: gateway` — WebTransport requires HTTP/2 minimum - `inst-wt-4a`
5. [ ] - `p2` - Initiate WebTransport session with upstream via extended CONNECT - `inst-wt-5`
6. [ ] - `p2` - **IF** upstream accepts session (200 OK) - `inst-wt-6`
   1. [ ] - `p2` - Confirm session establishment to caller - `inst-wt-6a`
7. [ ] - `p2` - **ELSE** (upstream rejects session) - `inst-wt-7`
   1. [ ] - `p2` - **RETURN** 502 ProtocolError with `X-OAGW-Error-Source: gateway` — upstream rejected WebTransport session - `inst-wt-7a`
8. [ ] - `p2` - **FOR EACH** stream opened by caller or upstream - `inst-wt-8`
   1. [ ] - `p2` - Create corresponding stream on the other side (unidirectional or bidirectional) - `inst-wt-8a`
   2. [ ] - `p2` - Forward stream data bidirectionally - `inst-wt-8b`
9. [ ] - `p2` - **IF** either side closes session - `inst-wt-9`
   1. [ ] - `p2` - Propagate session close to the other side with error code - `inst-wt-9a`
   2. [ ] - `p2` - Close all open streams - `inst-wt-9b`
   3. [ ] - `p2` - Release session resources - `inst-wt-9c`
10. [ ] - `p2` - **IF** session drops unexpectedly - `inst-wt-10`
    1. [ ] - `p2` - Close all open streams - `inst-wt-10a`
    2. [ ] - `p2` - Release session resources - `inst-wt-10b`

## 3. Processes / Business Logic (CDSL)

### HTTP Version Negotiation

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-protocol-version-negotiation`

**Input**: Upstream endpoint host/IP, requested protocol (SSE/WebSocket/WebTransport)

**Output**: Negotiated HTTP version (HTTP/1.1 or HTTP/2) or error

**Steps**:
1. [ ] - `p1` - Compute cache key: `{scheme}://{host}:{port}` - `inst-proto-1`
2. [ ] - `p1` - **IF** protocol version cache contains entry for this key AND entry is not expired (TTL: 1 hour) - `inst-proto-2`
   1. [ ] - `p1` - **RETURN** cached HTTP version - `inst-proto-2a`
3. [ ] - `p1` - Attempt TLS handshake with ALPN extension offering `h2` (HTTP/2) and `http/1.1` - `inst-proto-3`
4. [ ] - `p1` - **IF** ALPN negotiation selects `h2` - `inst-proto-4`
   1. [ ] - `p1` - Cache entry: `{key} → HTTP/2, expires_at = now + 1h` - `inst-proto-4a`
   2. [ ] - `p1` - **RETURN** HTTP/2 - `inst-proto-4b`
5. [ ] - `p1` - **ELSE** (ALPN selects `http/1.1` or ALPN not supported) - `inst-proto-5`
   1. [ ] - `p1` - Cache entry: `{key} → HTTP/1.1, expires_at = now + 1h` - `inst-proto-5a`
   2. [ ] - `p1` - **RETURN** HTTP/1.1 - `inst-proto-5b`
6. [ ] - `p1` - **IF** TLS handshake fails entirely - `inst-proto-6`
   1. [ ] - `p1` - **RETURN** error: connection failed (502 DownstreamError) - `inst-proto-6a`
7. [ ] - `p1` - **IF** cached version fails at runtime (e.g., HTTP/2 connection error on a host cached as HTTP/2) - `inst-proto-7`
   1. [ ] - `p1` - Evict cache entry for this key - `inst-proto-7a`
   2. [ ] - `p1` - Re-negotiate from step 3 on next request (not current request — no retry per `cpt-cf-oagw-principle-no-retry`) - `inst-proto-7b`

### Streaming Connection Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-streaming-connection-lifecycle`

**Input**: Established streaming connection (SSE, WebSocket, or WebTransport), idle timeout configuration

**Output**: Connection managed through open/active/closing/closed phases with proper resource cleanup

**Steps**:
1. [ ] - `p1` - Initialize connection state: `OPEN` - `inst-lifecycle-1`
2. [ ] - `p1` - Start idle timer with configured timeout (from upstream/route `timeout` guard plugin config or system default) - `inst-lifecycle-2`
3. [ ] - `p1` - **FOR EACH** data event (SSE event, WebSocket message, WebTransport stream data) - `inst-lifecycle-3`
   1. [ ] - `p1` - Reset idle timer - `inst-lifecycle-3a`
   2. [ ] - `p1` - **IF** destination is not ready to receive (backpressure) - `inst-lifecycle-3b`
      1. [ ] - `p1` - Apply async backpressure: pause reading from source until destination is ready (TCP flow control) - `inst-lifecycle-3b1`
   3. [ ] - `p1` - Forward data to the appropriate destination - `inst-lifecycle-3c`
4. [ ] - `p1` - **IF** idle timer expires (no data in either direction) - `inst-lifecycle-4`
   1. [ ] - `p1` - Transition to `CLOSING` state - `inst-lifecycle-4a`
   2. [ ] - `p1` - Initiate protocol-appropriate close (SSE: close response stream; WebSocket: send Close frame 1001; WebTransport: close session) - `inst-lifecycle-4b`
5. [ ] - `p1` - **IF** upstream signals close (EOF, Close frame, session close) - `inst-lifecycle-5`
   1. [ ] - `p1` - Transition to `CLOSING` state - `inst-lifecycle-5a`
   2. [ ] - `p1` - Propagate close to caller - `inst-lifecycle-5b`
6. [ ] - `p1` - **IF** caller disconnects - `inst-lifecycle-6`
   1. [ ] - `p1` - Transition to `CLOSING` state - `inst-lifecycle-6a`
   2. [ ] - `p1` - Abort upstream connection - `inst-lifecycle-6b`
7. [ ] - `p1` - **IF** connection error (TCP reset, TLS error, protocol violation) - `inst-lifecycle-7`
   1. [ ] - `p1` - Transition to `CLOSED` state immediately - `inst-lifecycle-7a`
   2. [ ] - `p1` - Notify the non-errored side (if still connected) - `inst-lifecycle-7b`
8. [ ] - `p1` - Release all connection resources (sockets, buffers, timers) - `inst-lifecycle-8`
9. [ ] - `p1` - **RETURN** final connection state - `inst-lifecycle-9`

## 4. States (CDSL)

Not applicable. Streaming connections are transient request-scoped sessions — each connection is independent and does not persist gateway-side state between sessions. Runtime connection states (OPEN, CLOSING, CLOSED) are managed within `cpt-cf-oagw-algo-streaming-connection-lifecycle` and do not represent a persistent entity lifecycle.

## 5. Definitions of Done

### Implement SSE Streaming Proxy

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-sse-streaming`

The system **MUST** detect SSE responses (`Content-Type: text/event-stream`) from upstream and forward events to the caller in real time without buffering the full response. SSE fields (`data:`, `event:`, `id:`, `retry:`) **MUST** be preserved as-is. Individual SSE events are forwarded without size validation (pass-through; upstream controls event granularity). The system **MUST** handle connection lifecycle: upstream close (clean EOF), caller disconnect (abort upstream), unexpected drop (502 StreamAborted), and idle timeout (504 IdleTimeout). Non-SSE responses from an upstream expected to return SSE **MUST** return 502 ProtocolError with `X-OAGW-Error-Source: gateway`. Error source distinction per `cpt-cf-oagw-principle-error-source` applies to all streaming error scenarios.

**Implements**:
- `cpt-cf-oagw-flow-streaming-sse`
- `cpt-cf-oagw-algo-streaming-connection-lifecycle`

**Touches**:
- API: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}` (streaming variant)

### Implement WebSocket Streaming Proxy

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-websocket-streaming`

The system **MUST** handle HTTP Upgrade (`Upgrade: websocket`) by negotiating the WebSocket handshake with both the caller and upstream. Messages (text and binary frames) **MUST** be forwarded bidirectionally with opcode preservation. **IF** a configurable max frame size is set, messages exceeding the limit **MUST** trigger Close frame 1009 (Message Too Big); by default, no per-message size limit is enforced (pass-through). Close frames **MUST** be propagated with status code and reason; Close frame reason strings **MUST NOT** include internal gateway details (use standard WebSocket status codes only). The system **MUST** handle unexpected disconnects: upstream drop (send 1006 Abnormal Closure to caller), caller drop (send Close to upstream), idle timeout (send 1001 Going Away to both sides). Auth and guard plugins **MUST** execute before the upgrade handshake. Transform plugins are NOT executed on individual WebSocket frames. Upstream rejection of the upgrade **MUST** return 502 ProtocolError with `X-OAGW-Error-Source: gateway`.

**Implements**:
- `cpt-cf-oagw-flow-streaming-websocket`
- `cpt-cf-oagw-algo-streaming-connection-lifecycle`

**Touches**:
- API: `GET /api/oagw/v1/proxy/{alias}/{path}` (WebSocket upgrade)

### Implement WebTransport Streaming Proxy

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-webtransport-streaming`

The system **MUST** handle WebTransport session establishment via extended CONNECT with `:protocol` pseudo-header. Session setup **MUST** require HTTP/2 minimum (verified via `cpt-cf-oagw-algo-protocol-version-negotiation`); failure to negotiate HTTP/2 **MUST** return 502 ProtocolError. Multiplexed streams (unidirectional and bidirectional) **MUST** be forwarded between caller and upstream. Session close **MUST** propagate error codes and close all open streams. Auth and guard plugins **MUST** execute before session establishment.

**Implements**:
- `cpt-cf-oagw-flow-streaming-webtransport`
- `cpt-cf-oagw-algo-streaming-connection-lifecycle`

**Touches**:
- API: `CONNECT /api/oagw/v1/proxy/{alias}/{path}` (WebTransport session)

### Implement HTTP Version Negotiation and Protocol Cache

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-protocol-version-cache`

The system **MUST** implement adaptive per-host HTTP version detection using ALPN during TLS handshake. On first connection to an upstream host, the system **MUST** offer both `h2` and `http/1.1` via ALPN. The negotiated version **MUST** be cached per `{scheme}://{host}:{port}` with a 1-hour TTL. Subsequent requests to the same host **MUST** use the cached version. On TLS handshake failure, the system **MUST** return 502 DownstreamError. Cache eviction **MUST** occur after TTL expiry. Additionally, if a request fails due to a protocol-level error on a cached version (e.g., HTTP/2 connection error on a host cached as HTTP/2-capable), the cache entry **MUST** be evicted so the next request re-negotiates via ALPN (current request is not retried per `cpt-cf-oagw-principle-no-retry`).

**Implements**:
- `cpt-cf-oagw-algo-protocol-version-negotiation`

**Touches**:
- Entities: `Upstream` (protocol field), `ServerConfig`, `Endpoint`

## 6. Acceptance Criteria

- [ ] SSE responses (`Content-Type: text/event-stream`) are forwarded event-by-event to the caller without full-response buffering
- [ ] SSE fields (`data:`, `event:`, `id:`, `retry:`) are preserved as-is during forwarding
- [ ] Upstream close (EOF) during SSE streaming results in clean caller connection closure
- [ ] Caller disconnect during SSE streaming aborts the upstream connection and releases resources
- [ ] Unexpected upstream drop during SSE streaming returns 502 StreamAborted with `X-OAGW-Error-Source: gateway`
- [ ] Idle timeout during SSE streaming (no events within configured timeout) returns 504 IdleTimeout with `X-OAGW-Error-Source: gateway`
- [ ] Non-SSE upstream response when SSE was expected returns 502 ProtocolError with `X-OAGW-Error-Source: gateway`
- [ ] WebSocket upgrade requests (`Upgrade: websocket`) are negotiated with both caller and upstream
- [ ] WebSocket text and binary messages are forwarded bidirectionally with opcode preservation
- [ ] WebSocket Close frames are propagated with status code and reason to the other side
- [ ] Upstream WebSocket drop sends 1006 Abnormal Closure to caller
- [ ] Caller WebSocket disconnect sends Close frame to upstream
- [ ] Idle timeout on WebSocket session sends 1001 Going Away to both sides
- [ ] Upstream rejection of WebSocket upgrade returns 502 ProtocolError with `X-OAGW-Error-Source: gateway`
- [ ] Transform plugins are NOT executed on individual WebSocket frames
- [ ] Auth and guard plugins execute before any streaming upgrade/session establishment
- [ ] WebTransport session establishment requires HTTP/2 (verified via ALPN); failure returns 502 ProtocolError
- [ ] WebTransport multiplexed streams (unidirectional and bidirectional) are forwarded between caller and upstream
- [ ] WebTransport session close propagates error codes and closes all open streams
- [ ] HTTP version negotiation uses ALPN during TLS handshake, offering `h2` and `http/1.1`
- [ ] Negotiated HTTP version is cached per `{scheme}://{host}:{port}` with 1-hour TTL
- [ ] Cached HTTP version is used for subsequent requests to the same host
- [ ] TLS handshake failure during version negotiation returns 502 DownstreamError
- [ ] All upstream connections use HTTPS-only per `cpt-cf-oagw-constraint-https-only`
- [ ] `X-OAGW-Error-Source` header is set correctly for all streaming error scenarios (gateway vs upstream)
- [ ] No credentials appear in logs or error messages during streaming sessions
- [ ] WebSocket Close frame reason strings do not leak internal gateway details
- [ ] When a configurable max WebSocket frame size is set, oversized messages trigger Close frame 1009 (Message Too Big)
- [ ] Backpressure is applied via TCP flow control when one side of a streaming connection is slower than the other
- [ ] Protocol version cache entries are evicted on protocol-level errors (re-negotiation on next request)

## 7. Additional Context

### Performance Considerations

Streaming connections are forwarded at the TCP/TLS level with no per-event processing overhead beyond frame forwarding. SSE events, WebSocket messages, and WebTransport stream data are passed through as-is without buffering the full response body. The protocol version cache eliminates redundant ALPN negotiation for subsequent requests to the same host (1h TTL). Connection resources (sockets, buffers, timers) are released immediately on session end to prevent resource leaks under high concurrency.

**Backpressure**: When one side of a streaming connection produces data faster than the other side can consume, OAGW relies on TCP flow control (receive window backpressure) to pause the fast sender. No application-level buffering beyond OS socket buffers is performed. This prevents unbounded memory growth under asymmetric throughput conditions.

### Security Considerations

All streaming connections are subject to the same auth and guard plugin chain as regular proxy requests — authentication and authorization are enforced before any upgrade or session establishment. HTTPS-only constraint (`cpt-cf-oagw-constraint-https-only`) applies to all streaming upstream connections. Credential isolation per `cpt-cf-oagw-principle-cred-isolation` is maintained — secrets are resolved from `cred_store` at connection time and never logged or stored. WebSocket and WebTransport sessions do not re-authenticate after the initial handshake (session-scoped auth).

**Input validation**: SSE events and WebSocket/WebTransport frames are forwarded as-is (pass-through proxy). Per-message size limits are not enforced by default — the upstream controls payload granularity. An optional max WebSocket frame size can be configured per upstream/route; oversized messages result in Close frame 1009 (Message Too Big). WebSocket Close frame reason strings **MUST NOT** include internal gateway details to prevent information leakage.

### Configuration Parameters

| Parameter | Scope | Default | Description |
|-----------|-------|---------|-------------|
| `streaming_idle_timeout_seconds` | upstream / route | Inherited from `timeout` guard plugin or system default | Idle timeout for streaming connections (no data in either direction) |
| `websocket_max_frame_size_bytes` | upstream / route | None (pass-through) | Optional max WebSocket message size; exceeding triggers Close 1009 |
| `websocket_close_timeout_seconds` | system | 5 | Timeout for WebSocket Close frame handshake |
| `protocol_version_cache_ttl_seconds` | system | 3600 (1 hour) | TTL for cached ALPN negotiation results per host |

All parameters are read from upstream/route configuration at connection time. System-level defaults are set via module configuration (`OagwConfig`).

### Deliberate Omissions

- **States section**: Not applicable — streaming connections are transient request-scoped sessions with no persistent entity lifecycle. Runtime connection states are managed within the connection lifecycle algorithm.
- **gRPC streaming**: Out of scope — gRPC support (server, client, bidirectional streaming) is future work. See `cpt-cf-oagw-adr-grpc-support`.
- **HTTP/3 (QUIC)**: Out of scope — QUIC-native WebTransport is future work. Current WebTransport support uses HTTP/2 extended CONNECT.
- **Rate limiting during streaming**: Out of scope — rate limiting on streaming connections (e.g., per-event or per-message throttling) belongs to `cpt-cf-oagw-feature-rate-limiting`.
- **Metrics and audit logging for streaming**: Out of scope — streaming-specific metrics (connection duration, message counts, bytes transferred) belong to `cpt-cf-oagw-feature-observability`.
- **UX/Accessibility**: Not applicable — OAGW is a backend API module with no user interface.
- **Compliance/Privacy**: OAGW does not handle PII directly. Credential isolation via `cred_store` references covers data protection. No additional regulatory compliance beyond standard platform requirements.
