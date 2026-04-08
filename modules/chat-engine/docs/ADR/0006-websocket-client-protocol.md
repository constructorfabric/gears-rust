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

**StreamingChunkEvent**: Contains type "chunk", message_id, and chunk object with content type, content text, and index.

**StreamingCompleteEvent**: Signals end of streaming, contains type "complete", message_id, and metadata with usage statistics (input_units, output_units).

**StreamingErrorEvent**: Signals streaming error, contains type "error", message_id, and error object with error code and message.

### Cancellation Mechanism

Clients cancel streaming by closing the HTTP connection. In browsers, this is done using AbortController with the fetch API. In other clients (Python, etc.), the HTTP request can be closed/cancelled directly. When the connection is closed, the server detects the disconnection and terminates the streaming process.

## Pros and Cons of the Options

### Option 1: HTTP REST + WebSocket split

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 2: HTTP with chunked streaming (NDJSON)

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 3: HTTP/2 Server-Sent Events (SSE)

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 4: gRPC streaming

See "Considered Options" and "Consequences" above for trade-off analysis.

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
