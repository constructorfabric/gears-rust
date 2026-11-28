# OAGW SDK

SDK crate for the Outbound API Gateway gear, providing API traits, domain models, error types, and streaming utilities.

## Overview

This crate defines the transport-agnostic interface for the OAGW gear:

- **`ServiceGatewayClientV1`** — Async trait for upstream/route management and request proxying
- **`Upstream` / `Route`** — Core domain models with builder-based construction
- **`ServiceGatewayError`** — Error types for all gateway operations; `request_not_sent()` tells whether a retry is safe for any method
- **`reason`** — Wire `reason` codes with typed enums: `auth`, `permission`, `unavailable` (why a 502/503: `REQUEST_NOT_SENT`, `REQUEST_MAY_HAVE_BEEN_SENT`, `UPSTREAM_DISABLED`, `CREDENTIAL_STORE_UNAVAILABLE`, `IDENTITY_PROVIDER_UNAVAILABLE`) and `timeout` (504)
- **`Body`** — Request/response body abstraction (`Bytes` / `Stream` / `Empty`)
- **`ServerEventsStream`** — SSE response parser with typed event support
- **`WebSocketStream`** — WebSocket abstraction with sender/receiver halves
- **`Json<T>`** — Codec for typed SSE events and WebSocket messages

## Usage

### Getting the Client

```rust
use oagw_sdk::api::ServiceGatewayClientV1;

let gw = hub.get::<dyn ServiceGatewayClientV1>()?;
```

### Proxying an HTTP request

```rust
let req = http::Request::builder()
    .method("POST")
    .uri("/openai/v1/chat/completions")
    .body(Body::from(r#"{"model":"gpt-4"}"#))?;

let resp = gw.proxy_request(ctx, req).await?;
let bytes = resp.into_body().into_bytes().await?;
```

### Streaming a request body

A `Body::Stream` is sent as it is read. Set `Content-Length` when the length
is known: the upstream gets it instead of chunked framing (many upstreams and
signed requests need it), and a stream of another length fails the request
with 400 `INVALID_CONTENT_LENGTH`. End the stream right after its last byte
(drop a channel's sender, for example): without a `Content-Length` the
request waits for the end, and with one, data yielded after the declared
length is not seen.

```rust
let req = http::Request::builder()
    .method("PUT")
    .uri("/storage/v1/objects/report.bin")
    .header(http::header::CONTENT_LENGTH, len)
    .body(Body::Stream(stream))?;
```

### Consuming an SSE stream

```rust
let req = http::Request::get("/openai/v1/chat/completions").body(Body::Empty)?;
let resp = gw.proxy_request(ctx, req).await?;

match ServerEventsStream::from_response::<ServerEvent>(resp) {
    ServerEventsResponse::Events(mut stream) => {
        while let Some(event) = stream.next().await {
            println!("{}", event?.data);
        }
    }
    ServerEventsResponse::Response(resp) => { /* non-SSE fallback */ }
}
```

One event may take up to 16 MiB (`sse::DEFAULT_MAX_EVENT_BYTES`, its lines with
their line endings). A larger event, or a body that never ends an event with a
blank line, ends the stream with `StreamingError::EventTooLarge`; set another
limit with `ServerEventsStream::from_response_with_max_event_bytes`.

An event the body ends before its blank line is dropped, at a clean end too
(W3C EventSource). A body error ends the stream with that error; invalid UTF-8
ends it with `StreamingError::ServerEventsParse`.

## Features

- `axum` — enables `ws::axum_adapter` for bridging axum WebSocket upgrades into `WebSocketStream`

## License

Apache-2.0
