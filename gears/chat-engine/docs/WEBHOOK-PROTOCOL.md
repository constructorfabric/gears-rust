Created:  2026-03-06 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# Chat Engine Webhook Protocol

Specification for the Webhook protocol — the HTTP calls Chat Engine makes *to* backend services. The client-facing HTTP REST API is in [`openapi.json`](openapi.json).

## Overview

Protocol specification files complement the domain model schemas in `../schemas/` by defining:

- **API operations and flows**: How clients interact with the server
- **Event sequences**: Order and structure of events in request/response cycles
- **Protocol-level constraints**: Timeouts, error handling, streaming patterns
- **Connection configuration**: Authentication, transport details

The Chat Engine API uses **HTTP with chunked streaming**:
- **HTTP REST API**: For CRUD operations, queries, and control operations
- **HTTP Chunked Streaming**: For real-time streaming responses (newline-delimited JSON)
- **Stateless Architecture**: No persistent connections, simpler scaling and deployment

## Files

### HTTP REST API

**Where**: [`openapi.json`](openapi.json) — OpenAPI 3.1, generated from
the live route registrations by `make openapi-chat-engine`. The gear also serves it at
`{prefix}/chat-engine/v1/openapi`, with an API reference page at
`{prefix}/chat-engine/v1/docs`.

This directory used to hold a hand-written `http-protocol.json`. It was written before
the implementation and never matched it — it described `/sessions`, `/messages/send`
and friends, while the gear serves `/chat-engine/v1/...` — so it was deleted rather
than kept as a second, wrong source of truth.

**Contents** of the generated document: 23 operations covering session types, session
lifecycle, messages and SSE streaming, variants, reactions, search, session
intelligence, export and sharing.

**HTTP Configuration**:
- Base path: `{prefix}/chat-engine/v1`, where `{prefix}` is the gateway's `prefix_path`
- Authentication: JWT Bearer token in the Authorization header
- Content-Type: `application/json`
- Standard HTTP status codes; errors are RFC-9457 `application/problem+json`

### SSE streaming (engine → client)

**Format**: Server-Sent Events (`text/event-stream`), typed delta protocol (FR-024)

Do not confuse this with the NDJSON hop below: backend plugins stream **NDJSON** to
the engine, and the engine streams **SSE** to the client.

**Streaming Endpoints**:
- `POST /chat-engine/v1/sessions/{id}/messages` - Send message, stream the response
- `POST /chat-engine/v1/messages/{id}/recreate` - Recreate an assistant variant
- `POST /chat-engine/v1/sessions/{id}/summarize` - Generate a session summary
- `GET /chat-engine/v1/messages/{id}/stream` - Resume a stream via `Last-Event-ID`

**Event types**: `message.start`, `message.part.add`, `message.text.delta`,
`message.complete`, `message.error`. Patches use terse keys — `o` (op), `p` (path),
`v` (value). Each event carries a monotonic `seq` starting at 0, mirrored in the SSE
`id:` field so a dropped connection resumes with `Last-Event-ID`.

**Cancellation**: close the HTTP connection.

### webhook-protocol.json

**Format**: GTS JSON Schema (custom format)

**GTS ID**: `gtx.cf.core.events.event.v1~x.chat_engine.api.webhook_protocol.v1~`

Complete Webhook API specification defining HTTP POST calls from Chat Engine to backend services.

**Contents**:
- **7 Webhook operations**:
  - `session.created` - Session creation notification
  - `message.new` - New user message processing
  - `message.recreate` - Message regeneration request
  - `message.aborted` - Streaming cancellation notification
  - `session.deleted` - Session deletion notification
  - `session.summary` - Session summarization request
  - `session_type.health_check` - Backend health check

**HTTP Configuration**:
  - Method: POST
  - Content-Type: application/json
  - Accept: application/json, text/event-stream

**Streaming Protocol**:
  - HTTP chunked streaming (NDJSON) format
  - Event types: chunk, complete, error
  - Content chunk structure

**Resilience Patterns**:
  - Retry policy (exponential backoff)
  - Circuit breaker (failure threshold, timeout)
  - Timeout handling (abort and notify)

## Protocol Architecture

### HTTP with Chunked Streaming

**HTTP REST API with Streaming** provides:
- ✅ Simple CRUD operations (no persistent connection overhead)
- ✅ Queries and search (standard HTTP caching, CDN-friendly)
- ✅ Standard tooling (curl, Postman, HTTP clients)
- ✅ Easy testing and debugging
- ✅ RESTful patterns and conventions
- ✅ Streaming responses (real-time incremental delivery via chunked transfer)
- ✅ Stateless scaling (no sticky sessions required)
- ✅ Simple cancellation (close connection)
- ✅ Standard load balancing and proxy support

This approach follows modern patterns used by:
- OpenAI API (HTTP streaming)
- Anthropic API (HTTP streaming)
- Modern serverless architectures

### Protocol Decision Matrix

| Operation Type | Protocol | Reason |
|---------------|----------|--------|
| Create session | HTTP POST | Simple request/response, no streaming needed |
| Get session | HTTP GET | Standard retrieval, cacheable |
| Delete session | HTTP DELETE | Simple command, idempotent |
| Send message | **HTTP POST (streaming)** | Streaming response via chunked transfer |
| List messages | HTTP GET | Standard query, pagination support |
| Stop streaming | **Close connection** | Stateless cancellation |
| Recreate message | **HTTP POST (streaming)** | Streaming response via chunked transfer |
| Search messages | HTTP GET | Query operation, standard REST patterns |
| Summarize session | **HTTP POST (streaming)** | Streaming response via chunked transfer |

## Relationship to Domain Schemas

Protocol specifications **reference** domain schemas from `../schemas/` using JSON Schema `$ref` or by sharing common types:

```json
{
  "request": {
    "schema": "../schemas/session/SessionCreateRequest.json"
  },
  "response": {
    "schema": "../schemas/session/SessionCreateResponse.json"
  }
}
```

**Domain schemas** (`../schemas/`) define:
- Message structures (requests, responses, events)
- Entity types (Session, Message, SessionType)
- Enums and common types

**Protocol specs** (`./`) define:
- How and when to use those message structures
- Operation flows and sequences
- Protocol-level behavior (timeouts, errors, streaming)

## Usage Examples

`BASE` below is `{host}{prefix}/chat-engine/v1`, where `{prefix}` is the gateway's
`prefix_path` (`/cf` in `config/quickstart.yaml`, empty by default).

### REST

**TypeScript**:
```typescript
const BASE = 'https://example.test/cf/chat-engine/v1';
const auth = { 'Authorization': `Bearer ${jwt}` };

// Create session
const { id: sessionId } = await fetch(`${BASE}/sessions`, {
  method: 'POST',
  headers: { ...auth, 'Content-Type': 'application/json' },
  body: JSON.stringify({ session_type_id: sessionTypeId })
}).then(r => r.json());

// Get session
const session = await fetch(`${BASE}/sessions/${sessionId}`, { headers: auth })
  .then(r => r.json());

// Search within a session (POST, not GET)
const results = await fetch(`${BASE}/sessions/${sessionId}/search`, {
  method: 'POST',
  headers: { ...auth, 'Content-Type': 'application/json' },
  body: JSON.stringify({ query: 'hello', limit: 20 })
}).then(r => r.json());
```

**Python**:
```python
import requests

BASE = 'https://example.test/cf/chat-engine/v1'
headers = {'Authorization': f'Bearer {jwt}'}

session_id = requests.post(
    f'{BASE}/sessions',
    json={'session_type_id': session_type_id},
    headers=headers,
).json()['id']

requests.delete(f'{BASE}/sessions/{session_id}', headers=headers)
```

### SSE streaming

**TypeScript** — `EventSource` cannot POST, so read the body stream directly:
```typescript
async function sendMessage(sessionId: string, text: string) {
  const response = await fetch(`${BASE}/sessions/${sessionId}/messages`, {
    method: 'POST',
    headers: { ...auth, 'Content-Type': 'application/json' },
    body: JSON.stringify({ parts: [{ type: 'text', content: { text } }] })
  });

  const reader = response.body!.getReader();
  const decoder = new TextDecoder();
  let buffer = '';

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });

    // SSE frames are separated by a blank line.
    const frames = buffer.split('\n\n');
    buffer = frames.pop() ?? '';

    for (const frame of frames) {
      const event = frame.match(/^event: (.*)$/m)?.[1];
      const data = JSON.parse(frame.match(/^data: (.*)$/m)![1]);

      switch (event) {
        case 'message.start':      console.log('started', data.message_id); break;
        case 'message.text.delta': process(data.v); break;   // v = appended text
        case 'message.complete':   console.log('done', data.metadata); break;
        case 'message.error':      console.error(data); break;
      }
    }
  }
}
```

**Python** — `httpx` keeps the connection streaming:
```python
import httpx, json

with httpx.stream(
    'POST',
    f'{BASE}/sessions/{session_id}/messages',
    headers=headers,
    json={'parts': [{'type': 'text', 'content': {'text': 'Hello'}}]},
) as response:
    event = None
    for line in response.iter_lines():
        if line.startswith('event: '):
            event = line.removeprefix('event: ')
        elif line.startswith('data: '):
            data = json.loads(line.removeprefix('data: '))
            if event == 'message.text.delta':
                print(data['v'], end='', flush=True)
            elif event == 'message.complete':
                print()
```

### Validating Protocol Compliance

```python
import json
from openapi_spec_validator import validate

with open('docs/openapi.json') as f:
    validate(json.load(f))
```

## Protocol Versioning

Protocol specifications use semantic versioning:

**HTTP REST API** (`openapi.json`):
- **Current version**: the `cf-chat-engine` crate version, emitted into OpenAPI `info.version`
- **URL versioning**: `/chat-engine/v1/` prefix
- **Breaking changes**: Increment major version, update URL prefix to `/chat-engine/v2/`

**Webhook API** (`webhook-protocol.json`):
- **Current version**: `1.0`
- **GTS identifier**: `v1~`
- **Breaking changes**: Increment major version, notify backends

**Version compatibility rules**:
- Clients must support protocol version from server handshake
- New operations can be added without version bump (optional features)
- Changing existing operation signatures requires version bump
- Event sequence changes require version bump

## Validation and Testing

### JSON Syntax Validation

```bash
# Validate JSON syntax
python3 -m json.tool docs/openapi.json > /dev/null
python3 -m json.tool docs/webhook-protocol.json > /dev/null
```

### OpenAPI Validation

```bash
# Validate HTTP REST API spec
npx @redocly/cli lint docs/openapi.json
```

### Protocol Completeness Check

Not needed for the HTTP surface: `docs/openapi.json` is generated from the same
`OperationBuilder` chains that mount the routes, so it cannot drift from the code.
Regenerate with `make openapi-chat-engine` and diff.

## Tools and Libraries

### HTTP REST API

- **OpenAPI Tools**:
  - Redoc: Interactive documentation
  - Swagger UI: API explorer
  - OpenAPI Generator: Client/server code generation

- **Testing**:
  - Postman: Manual testing and collections
  - curl: Command-line testing
  - pytest with requests: Automated testing

### Webhook API

- **JSON Schema Validation**:
  - Python: `jsonschema` library
  - TypeScript: `ajv` library
  - Rust: `jsonschema` crate

### Documentation Generation

- **HTTP**: Redoc, Swagger UI, or the reference page the gear serves at `{prefix}/chat-engine/v1/docs`
- **Webhook**: Custom documentation from JSON Schema

## See Also

- [`../schemas/README.md`](../schemas/README.md) - Domain model schema documentation
- [`DESIGN.md`](DESIGN.md) - Complete architecture and design (section 3.3: API Contracts)
- [`PRD.md`](PRD.md) - Product requirements
- [`ADR/`](ADR/) - Architecture decision records

## Examples

### Complete Request Flow Example

**Create a session, send a message, read the history**:

1. Create session
   ```http
   POST /cf/chat-engine/v1/sessions
   Authorization: Bearer <token>
   Content-Type: application/json

   {"session_type_id": "3fa85f64-5717-4562-b3fc-2c963f66afa6"}
   ```

2. Send message
   ```http
   POST /cf/chat-engine/v1/sessions/{session_id}/messages
   Authorization: Bearer <token>
   Content-Type: application/json

   {"parts": [{"type": "text", "content": {"text": "Hello"}}]}
   ```

3. Receive the response (SSE, `text/event-stream`)
   ```
   id: 0
   event: message.start
   data: {"type":"message.start","message_id":"987fcdeb-51a2-43c1-b789-012345678abc","seq":0}

   id: 1
   event: message.part.add
   data: {"o":"add","p":"parts/0","v":{"type":"text","content":{"text":""},"number":0}}

   id: 2
   event: message.text.delta
   data: {"o":"append","p":"parts/0/content/text","v":"Hi"}

   id: 3
   event: message.text.delta
   data: {"o":"append","p":"parts/0/content/text","v":" there"}

   id: 4
   event: message.complete
   data: {"o":"stop","metadata":{"model":"...","finish_reason":"stop","usage":{}}}
   ```

   A dropped connection resumes from the last `id:` seen:
   ```http
   GET /cf/chat-engine/v1/messages/{message_id}/stream
   Last-Event-ID: 3
   ```

4. Retrieve message history
   ```http
   GET /cf/chat-engine/v1/sessions/{session_id}/messages
   Authorization: Bearer <token>
   ```

---

**Protocol Version**: HTTP REST API — see `info.version` in `openapi.json`; Webhook API 1.0
**Maintainers**: Chat Engine Team
