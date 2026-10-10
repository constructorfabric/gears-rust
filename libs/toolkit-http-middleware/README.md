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
- `AnonymousRoute` marker so routes that carry no JWT pass through without `401`
- Per-route policy for gear listeners: `RouteAuthPolicy` + `route_auth_middleware`
  select auth before both planes (anonymous routes get an anonymous `SecurityContext`);
  `platform_route_middleware` requires a validated internal token and refuses any
  unvalidated presented credential. `layer_route_auth` installs all four layers in the
  required order — prefer it over wiring them by hand
- Renders rejections as canonical RFC 9457 `application/problem+json`

## What it does NOT do

- Run an HTTP server — consumers own the server and router
- Provide the concrete authenticators — they are injected via axum state at the
  gear/bootstrap layer
- Outbound HTTP requests — that is `cf-gears-toolkit-http` (the client crate)

## Usage

```rust
use axum::{Router, routing::get};
use toolkit_http_middleware::{RouteAuth, RouteAuthPolicy, layer_route_auth};

// `bearer` and `internal` are `Option<DynBearerAuthenticator>` /
// `Option<DynInternalAuthenticator>` adapters, supplied at the bootstrap layer.
let policy: RouteAuthPolicy = [(http::Method::GET, "/widgets".to_owned(), RouteAuth::Authenticated)]
    .into_iter()
    .collect();
let router = layer_route_auth(
    Router::new().route("/widgets", get(list_widgets)),
    policy,
    bearer,
    internal,
);
```

## License

Apache-2.0
