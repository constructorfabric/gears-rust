# cf-gears-toolkit-http-middleware

Server-side HTTP middleware for ToolKit, built on axum and tower.

This crate is the shared home for all of ToolKit's server-side HTTP middleware,
so gears install the same inbound layers from one place rather than each
maintaining their own.

## What it does

- **Two-plane authentication** as axum layers:
  - `security_context_middleware` (tenant plane) — always re-validates the bearer token via
    an injected `BearerAuthenticator` and inserts a `SecurityContext`
  - `internal_auth_middleware` (platform plane) — validates the
    `X-ToolKit-Internal-Token` via an injected `InternalAuthenticator` and inserts
    a `PlatformSecurityContext` plus `PeerAuthenticated`
- Header extractors for `Authorization: Bearer` and `X-ToolKit-Internal-Token`
- `PublicRoute` marker so routes that carry no JWT pass through without `401`
- Renders rejections as canonical RFC 9457 `application/problem+json`

## What it does NOT do

- Run an HTTP server — consumers own the server and router
- Provide the concrete authenticators — they are injected via axum state at the
  gear/bootstrap layer
- Outbound HTTP requests — that is `cf-gears-toolkit-http` (the client crate)

## Usage

```rust
use std::sync::Arc;
use axum::{Router, middleware::from_fn_with_state, routing::get};
use toolkit_http_middleware::{internal_auth_middleware, security_context_middleware};

// `bearer` and `internal` are your concrete `BearerAuthenticator` /
// `InternalAuthenticator` adapters, supplied at the bootstrap layer.
let router = Router::new()
    .route("/widgets", get(list_widgets))
    .route_layer(from_fn_with_state(bearer, security_context_middleware::<MyBearerAuth>))
    .route_layer(from_fn_with_state(internal, internal_auth_middleware::<MyInternalAuth>));
```

## License

Apache-2.0
