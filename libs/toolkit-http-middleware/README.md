# cf-gears-toolkit-http-middleware

Server-side HTTP middleware for ToolKit, built on axum and tower.

This crate is the shared home for all of ToolKit's server-side HTTP middleware,
so gears install the same inbound layers from one place rather than each
maintaining their own.

## What it does

- **Two-plane authentication** as axum layers:
  - `security_context_middleware` (tenant plane) — resolves each route's
    `AuthRequirement` via an injected `RouteAuthPolicy`, then, for authenticated
    routes, always re-validates the bearer token via an injected
    `BearerAuthenticator` and inserts a `SecurityContext`
  - `internal_auth_middleware` (platform plane) — validates the
    `X-ToolKit-Internal-Token` via an injected `InternalAuthenticator` and inserts
    a `PlatformSecurityContext` plus `PeerAuthenticated`
- Header extractors for `Authorization: Bearer` and `X-ToolKit-Internal-Token`
- `RouteAuthPolicy` decides, per `(method, path)`, whether a route requires a
  JWT; public routes receive an anonymous `SecurityContext` and pass through.
  The crate ships a ready-made `MatchitRouteAuthPolicy` (per-method `matchit`
  matching of explicit authenticated/public route sets, with a
  `require_auth_by_default` fallback) that most gears can use directly
- Renders rejections as canonical RFC 9457 `application/problem+json` with RFC
  6750 `WWW-Authenticate` challenges
- **CORS-preflight bypass is opt-in** (`SecurityContextLayerState::with_cors_preflight_bypass`)
  and off by default — CORS is an edge concern, so normal OoP gears stay lightweight and 
  only the gateway enables it
- **Generic HTTP hygiene** layers any gear serving HTTP can install:
  - `request_id` — `X-Request-Id` generation/propagation into request extensions
  - `access_log` — one structured `tracing` event per request, with accurate
    `bytes_sent` (byte-counting body wrapper for chunked/SSE responses)
  - `http_metrics` — OpenTelemetry HTTP server metrics, a baseline capability for
    every gear serving HTTP. A no-op meter is used until the consumer installs a
    meter provider, so it is near-zero cost when metrics aren't exported
- **Per-route request policy** middleware, driven by lookup tables/rules the gear
  builds from its own operation specs and config (the crate stays free of gear
  spec/config/error types; rejections render under a caller-supplied GTS scope):
  - `mime_validation` — per-route `Content-Type` allow-listing
  - `license_validation` — per-route feature-entitlement gating
  - `rate_limit` — per-route token-bucket + in-flight limiting
  - `scope_enforcement` — coarse-grained, pre-PDP token-scope checks

## What it does NOT do

- Run an HTTP server — consumers own the server and router
- Provide the concrete authenticators — they are injected via axum state at the
  gear/bootstrap layer
- Outbound HTTP requests — that is `cf-gears-toolkit-http` (the client crate)

## Usage

```rust
use std::sync::Arc;
use axum::{Router, middleware::from_fn_with_state, routing::get};
use toolkit_http_middleware::{
    MatchitRouteAuthPolicy, SecurityContextLayerState, internal_auth_middleware,
    security_context_middleware,
};

// `bearer` and `internal` are your concrete `BearerAuthenticator` /
// `InternalAuthenticator` adapters, supplied at the bootstrap layer.
//
// Build the route policy from your authenticated/public route sets (or
// implement `RouteAuthPolicy` yourself for bespoke matching):
let policy = Arc::new(MatchitRouteAuthPolicy::from_route_sets(
    authenticated_routes, // HashSet<(Method, String)>
    public_routes,        // HashSet<(Method, String)>
    /* require_auth_by_default */ true,
)?);
let bearer_state = SecurityContextLayerState::new(bearer, policy);
// At an edge that installs a CORS layer (the gateway), also call
// `.with_cors_preflight_bypass()`.

let router = Router::new()
    .route("/widgets", get(list_widgets))
    .route_layer(from_fn_with_state(bearer_state, security_context_middleware::<MyBearerAuth>))
    .route_layer(from_fn_with_state(internal, internal_auth_middleware::<MyInternalAuth>));
```

## License

Apache-2.0
