Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0006: HTTP Streaming Protocol for Client Communication


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Protocol Details](#protocol-details)
  - [Authentication](#authentication)
  - [CRUD Operations (HTTP REST)](#crud-operations-http-rest)
  - [Streaming Operations (HTTP Chunked Transfer)](#streaming-operations-http-chunked-transfer)
  - [Streaming Event Types](#streaming-event-types)
  - [Cancellation Mechanism](#cancellation-mechanism)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: HTTP REST + WebSocket split](#option-1-http-rest--websocket-split)
  - [Option 2: HTTP with chunked streaming (NDJSON)](#option-2-http-with-chunked-streaming-ndjson)
  - [Option 3: HTTP/2 Server-Sent Events (SSE)](#option-3-http2-server-sent-events-sse)
  - [Option 4: gRPC streaming](#option-4-grpc-streaming)
- [Related Design Elements](#related-design-elements)
- [References](#references)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**Review**: Revisit if WebSocket support is reconsidered or gRPC demand grows.

**ID**: `cpt-cf-chat-engine-adr-http-client-protocol`

## Context and Problem Statement

Chat Engine needs to support both simple CRUD operations (session management, message retrieval, search) and real-time streaming operations (message streaming with assistant responses). What protocol architecture should be used between client applications and Chat Engine to optimize for both use cases while maintaining operational simplicity?

## Decision Drivers

**For CRUD Operations**:
* Standard RESTful patterns and HTTP semantics
* Easy testing with standard tools (curl, Postman)
* HTTP caching and CDN support
* Standard authentication (Bearer tokens)
* No persistent connection overhead for simple operations

**For Streaming Operations**:
* Real-time streaming of assistant responses (time-to-first-byte < 200ms)
* Efficient connection management
* Simple cancellation mechanism (connection close)
* Support for multiple content types (text, code, images)
* Progress indication for long operations

**Architectural Principles**:
* Prefer stateless over stateful
* Prefer simple over complex
* Prefer standard over custom
* Optimize for modern cloud/serverless environments
* Enable horizontal scaling without session affinity

## Considered Options

* **Option 1: HTTP REST + WebSocket split** - Dual-protocol architecture
* **Option 2: HTTP with chunked streaming (NDJSON)** - Single protocol with streaming
* **Option 3: HTTP/2 Server-Sent Events (SSE)** - HTTP/2 for requests, SSE for streaming
* **Option 4: gRPC streaming** - gRPC unary and streaming

## Decision Outcome

Chosen option: "HTTP with chunked streaming (NDJSON)", because it provides a single protocol for all operations, enables stateless scaling without sticky sessions, simplifies client implementation, uses standard HTTP features (chunked transfer), provides simple cancellation via connection close, improves serverless compatibility, and reduces operational complexity.

### Consequences

**Architectural Benefits**:
* Good, because stateless servers enable true horizontal scaling
* Good, because any request can be handled by any server instance
* Good, because standard HTTP load balancing works without special configuration
* Good, because simpler deployment (no WebSocket proxy configuration)
* Good, because better serverless support (HTTP is universal)

**Operational Benefits**:
* Good, because standard HTTP monitoring and logging tools work
* Good, because easier debugging (curl can test streaming)
* Good, because no persistent connection management overhead
* Good, because graceful shutdown is simpler
* Good, because CDN and proxy compatibility improved

**Development Benefits**:
* Good, because single protocol reduces client complexity
* Good, because no WebSocket library required (standard fetch API)
* Good, because easier testing (standard HTTP tools)
* Good, because NDJSON is simple and human-readable
* Good, because cancellation is intuitive (close connection)

**Trade-offs**:
* Bad, because no server push capability (no persistent connection)
* Bad, because clients must poll for updates if needed
* Bad, because authentication token sent with every request

### Confirmation

Confirmed via design review and alignment with DESIGN.md implementation.

## Protocol Details

### Authentication

All requests use JWT Bearer token authentication.

### CRUD Operations (HTTP REST)

**Session Management**:
* `POST /api/v1/sessions` - Create session
* `GET /api/v1/sessions/{id}` - Get session
* `DELETE /api/v1/sessions/{id}` - Delete session
* `PATCH /api/v1/sessions/{id}/type` - Switch session type
* `POST /api/v1/sessions/{id}/export` - Export session
* `POST /api/v1/sessions/{id}/share` - Share session
* `GET /api/v1/sessions/shared/{token}` - Access shared session

**Message Operations**:
* `GET /api/v1/messages/{id}` - Get message
* `GET /api/v1/sessions/{id}/messages` - List messages
* `GET /api/v1/messages/{id}/variants` - Get message variants
* `POST /api/v1/messages/multi` - Send multiple messages

**Search Operations**:
* `POST /api/v1/sessions/{id}/search` - Search in session
* `POST /api/v1/search` - Search across sessions

### Streaming Operations (HTTP Chunked Transfer)

**Endpoints**:
* `POST /api/v1/messages/send` - Send message with streaming response
* `POST /api/v1/messages/{id}/recreate` - Recreate message with streaming
* `POST /api/v1/sessions/{id}/summarize` - Summarize session with streaming

**Request Format**: HTTP POST with JSON body containing session_id, content, and enabled_capabilities fields. Uses Bearer token authentication and application/json content type.

**Response Format**: NDJSON (newline-delimited JSON) over HTTP chunked transfer encoding. Each line is a separate JSON object representing a streaming event (start, chunk, complete, or error). Content-Type is application/x-ndjson.

### Streaming Event Types

**StreamingStartEvent**: Signals the beginning of streaming, contains type "start" and message_id.

**StreamingChunkEvent**: Contains type "chunk", message_id, and `chunk` — a plain string text fragment to be appended to the assistant message content. Multiple chunks concatenate to the full message text. Wire shape: `{"type": "chunk", "message_id": "<uuid>", "chunk": "<text>"}`.

**StreamingCompleteEvent**: Signals end of streaming, contains type "complete", message_id, and optional `metadata` (plugin-defined object — typically holds `model`, `finish_reason`, and `usage.{input_units,output_units}`). When the plugin produces no metadata the field is omitted from the wire payload (not serialized as `null`). Wire shape: `{"type": "complete", "message_id": "<uuid>", "metadata": {...}?}`.

**StreamingErrorEvent**: Signals a mid-stream failure, contains type "error", message_id, and `error` — a single string describing the failure. After an error event the stream is closed and no further events follow. Wire shape: `{"type": "error", "message_id": "<uuid>", "error": "<text>"}`. When a programmatic discriminator is needed (e.g. `context_overflow`, `stream_interrupted`), plugins prefix it to the human-readable description: `"context_overflow: context window exceeded"`.

### Cancellation Mechanism

Clients cancel streaming by closing the HTTP connection. In browsers, this is done using AbortController with the fetch API. In other clients (Python, etc.), the HTTP request can be closed/cancelled directly. When the connection is closed, the server detects the disconnection and terminates the streaming process.

## Pros and Cons of the Options

### Option 1: HTTP REST + WebSocket split

CRUD operations use standard HTTP REST; streaming operations use a persistent WebSocket connection.

* Good, because WebSocket provides true bidirectional communication, enabling server-initiated push
* Good, because CRUD operations remain standard REST with full HTTP semantics (caching, status codes)
* Good, because WebSocket is a mature, well-supported protocol with broad library availability
* Bad, because two protocols increase client complexity (HTTP client + WebSocket client)
* Bad, because WebSocket connections are stateful, requiring sticky sessions or session affinity for load balancing
* Bad, because WebSocket proxying requires special infrastructure configuration (upgrade handling, timeouts)
* Bad, because serverless and edge environments have limited or no WebSocket support

### Option 2: HTTP with chunked streaming (NDJSON)

Single HTTP protocol for all operations. CRUD uses standard request/response; streaming uses HTTP chunked transfer with NDJSON.

* Good, because single protocol simplifies client implementation (no WebSocket library needed)
* Good, because stateless servers enable true horizontal scaling without sticky sessions
* Good, because standard HTTP load balancing, monitoring, and debugging tools work without modification
* Good, because cancellation is intuitive — close the HTTP connection
* Bad, because no server push capability; clients must poll for asynchronous updates
* Bad, because authentication token must be sent with every request (no persistent authenticated session)
* Bad, because long-running chunked responses may be terminated by intermediate proxies with aggressive timeouts

### Option 3: HTTP/2 Server-Sent Events (SSE)

CRUD operations use HTTP/2 requests; streaming uses Server-Sent Events for server-to-client push.

* Good, because SSE is a W3C standard with native browser support via EventSource API
* Good, because SSE provides automatic reconnection and last-event-id tracking out of the box
* Good, because HTTP/2 multiplexing allows multiple concurrent streams over a single connection
* Bad, because SSE is unidirectional (server-to-client only), requiring a separate channel for client-to-server messages
* Bad, because HTTP/2 is not universally supported by all proxies, CDNs, and legacy infrastructure
* Bad, because SSE uses text-based event format with limited structure compared to NDJSON
* Bad, because EventSource API has limited configurability (no custom headers without polyfill)

### Option 4: gRPC streaming

All operations use gRPC with Protocol Buffers — unary calls for CRUD, server streaming for responses.

* Good, because strongly typed contracts via Protocol Buffers ensure type safety across client and server
* Good, because gRPC streaming is efficient with low overhead and built-in flow control
* Good, because bidirectional streaming enables both server push and client cancellation natively
* Bad, because browser support requires gRPC-Web proxy, adding infrastructure complexity
* Bad, because not human-readable — debugging requires specialized tools (grpcurl, Bloom RPC)
* Bad, because Protocol Buffer schema management adds development overhead and versioning complexity
* Bad, because less familiar to frontend developers compared to REST/JSON patterns

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` - Web/mobile/desktop apps using HTTP REST and HTTP streaming
* Chat Engine instances - HTTP server with chunked streaming support

**Requirements**:
* CRUD operations use HTTP REST for simplicity and standard patterns
* Streaming operations use HTTP chunked transfer for real-time delivery
* `cpt-cf-chat-engine-nfr-streaming` - First byte < 200ms, overhead < 10ms per chunk
* `cpt-cf-chat-engine-nfr-response-time` - HTTP routing < 50ms
* `cpt-cf-chat-engine-fr-stop-streaming` - Cancellation via connection close

**Design Elements**:
* HTTP server - Handles both CRUD and streaming operations
* `cpt-cf-chat-engine-component-response-streaming` - Manages HTTP chunked streaming
* HTTP REST API specification (Section 3.3.1 of DESIGN.md)
* Webhook API specification (Section 3.3.3 of DESIGN.md)

**Related ADRs**:
* ADR-0003 (Streaming Architecture) - HTTP streaming architecture principles
* ADR-0008 (Client-Initiated Streaming Cancellation) - Client cancellation via connection close
* ADR-0009 (Stateless Scaling) - Stateless architecture enabled by HTTP

## References

* OpenAI API uses HTTP streaming: https://platform.openai.com/docs/api-reference/streaming
* Anthropic API uses HTTP streaming: https://docs.anthropic.com/claude/reference/streaming
* HTTP/1.1 Chunked Transfer: RFC 7230 Section 4.1
* NDJSON Format: http://ndjson.org/
