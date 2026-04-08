# OAGW SDK

SDK crate for the Outbound API Gateway module, providing API traits, domain models, error types, and streaming utilities.

## Overview

This crate defines the transport-agnostic interface for the OAGW module:

- **`ServiceGatewayClientV1`** — Async trait for upstream/route management and request proxying
- **`Upstream` / `Route`** — Core domain models with builder-based construction
- **`ServiceGatewayError`** — Error types for all gateway operations
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

## Features

- `axum` — enables `ws::axum_adapter` for bridging axum WebSocket upgrades into `WebSocketStream`

## License

Apache-2.0
