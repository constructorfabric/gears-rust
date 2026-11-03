# ToolKit Auth

Authentication infrastructure for Gears / ToolKit.

## Overview

The `cf-gears-toolkit-auth` crate provides:

- **JWT / JWKS** — `KeyProvider` trait, `JwksKeyProvider` with background key refresh, `ValidationConfig`, standard claim constants
- **Token validation** — `TokenValidator` trait, `ClaimsError` / `AuthError` error types
- **Auth configuration** — `AuthConfig` (issuers, audiences, leeway, JWKS endpoint)
- **Outbound OAuth2 client credentials** — `TokenProvider` handle with proactive background refresh and on-demand invalidation, `OAuthClientConfig`, `BearerAuthLayer` (tower), `BearerAuthAutoRefreshLayer` (reactive refresh on 401, port of go-appkit's `AuthBearerRoundTripper`), `HttpClientBuilderExt` for `toolkit-http` integration
- **Auth metrics** — `AuthMetrics` trait with `LoggingMetrics` and `NoOpMetrics` implementations

## Outbound OAuth2 quick start

```rust
use toolkit_auth::{HttpClientBuilderExt, OAuthClientConfig, SecretString, TokenProvider};
use toolkit_http::HttpClientBuilder;

let token_provider = TokenProvider::new(OAuthClientConfig {
    token_endpoint: Some("https://idp.example.com/oauth/token".parse()?),
    client_id: "my-service".into(),
    client_secret: SecretString::new("my-secret"),
    scopes: vec!["api.read".into()],
    ..Default::default()
})?;

let client = HttpClientBuilder::new()
    .with_bearer_auth(token_provider)
    .build()?;

// Every request gets Authorization: Bearer <token> automatically
let resp = client.get("https://api.example.com/resource").send().await?;
```

## Reactive refresh on 401

`with_bearer_auth` covers the proactive case: the background `TokenWatcher`
refreshes the credential before TTL expires. It does **not** detect tokens
revoked out-of-band by the issuer — those surface only as a 401 from the
upstream.

`with_bearer_auth_auto_refresh` adds the reactive half: on a 401 response it
calls `TokenProvider::invalidate()`, re-reads the cached token, and replays the
original request once with the refreshed credential. Behavior is a port of
go-appkit's `AuthBearerRoundTripper`:

- requests that already carry the configured auth header pass through
  untouched (no refresh, no retry);
- `TokenProvider::invalidate()` is throttled per layer instance — default 15 minutes
  via `toolkit_auth::oauth2::DEFAULT_MIN_INVALIDATION_INTERVAL` — so a burst
  of 401s will not hammer the token endpoint;
- if the refreshed token equals the previous value, or if the invalidate
  fetch fails, the original 401 is surfaced as-is (no retry loop);
- exactly one retry per call, no backoff. For multi-step retry strategies,
  compose with `toolkit_http::RetryLayer`.

```rust
use toolkit_auth::{
    BearerAuthAutoRefreshOpts, HttpClientBuilderExt, OAuthClientConfig, SecretString, TokenProvider,
};
use toolkit_http::HttpClientBuilder;
use std::sync::Arc;
use std::time::Duration;

let token_provider = TokenProvider::new(OAuthClientConfig { /* … */ ..Default::default() })?;

// Defaults: Authorization header, retry on 401 only, 15-min throttle.
let client = HttpClientBuilder::new()
    .with_bearer_auth_auto_refresh(token_provider.clone())
    .build()?;

// Custom predicate / header / throttle:
let opts = BearerAuthAutoRefreshOpts {
    min_invalidation_interval: Duration::from_secs(60),
    should_refresh: Arc::new(|s| s.as_u16() == 401 || s.as_u16() == 419),
    header_name: http::header::AUTHORIZATION,
};
let custom = HttpClientBuilder::new()
    .with_bearer_auth_auto_refresh_opts(token_provider, opts)
    .build()?;
```

Cost: auto-refresh keeps a clone of the request body so it can be replayed
on retry. With `toolkit-http`'s default body (`Full<Bytes>`) this is a
reference-counted bump; with custom `B`, the type must be `Clone`. Pick the
plain `with_bearer_auth` when the upstream is known not to revoke tokens
out-of-band.

See `examples/` for more patterns (OIDC discovery, token invalidation, shared token, form auth).

## Acquisition and lifecycle

`TokenProvider::new(config)?` validates configuration and creates an idle provider
without network requests. It requires an active Tokio runtime. The first
`get().await` or `invalidate().await` starts acquisition. Both HTTP auth layers
await `get()` before sending a request, so first use waits for a token.

`get().await` immediately returns a usable cached token, or waits for the current
or next scheduled attempt and returns its result. Concurrent callers share one
worker, including through clones. A failed attempt is retained for its waiters;
later retries cannot overwrite their error. A success invalidated or expired
before delivery waits for replacement. Cancelling one caller's future stops only
its wait. Use `tokio::time::timeout` for a caller-owned deadline.

Initial discovery/token failures retry after 1, 2, 4, 8, 16, then at most 30
seconds. These delays are independent of `min_refresh_period`; request duration
and transport retries are additional. Once acquisition starts, retries continue
in the background. After success, the configured refresh schedule and backoff
apply while a usable token exists. A failed proactive refresh leaves the cached
token usable until expiry; once expired, retries use the bounded 1–30 second
acquisition schedule.
Successful discovery is reused across retries and invalidation.

`invalidate().await?` discards the cached credential immediately and waits for
replacement. Concurrent invalidations and gets share the pending attempt.
Failure never restores the invalidated token. If the cache is already empty,
invalidation respects the pending attempt's backoff.

`shutdown().await` permanently stops the shared provider, cancels pending network
work and retry sleeps, wakes waiters with `TokenError::Shutdown`, and joins the
worker. It is safe to call repeatedly or concurrently. Dropping a shutdown future
does not undo cancellation; another shutdown call can still join. Dropping the
last provider handle cancels work as a fallback.

To require a token during application startup, call `get().await?` explicitly:

```rust
let token_provider = TokenProvider::new(config)?;
token_provider.get().await?; // startup check, including OIDC discovery
// Pass token_provider.clone() to HTTP clients.
token_provider.shutdown().await; // at application shutdown
```

## HTTP errors

Rejected HTTP responses return the non-exhaustive
`TokenError::HttpStatus { operation, status, target_url, .. }` variant.
Use `TokenError::status()` to inspect the response status.
`HttpOperation::{Discovery, TokenRequest}` identifies the failed operation.
`target_url` identifies the requested endpoint with userinfo, query and fragment
removed. Response bodies and headers are discarded. Transport failures use
`TokenError::Http` and have no HTTP response status.

See the [changelog](../../CHANGELOG.md#toolkit-auth-080-migration) for migration instructions.

## License

Licensed under Apache-2.0.
