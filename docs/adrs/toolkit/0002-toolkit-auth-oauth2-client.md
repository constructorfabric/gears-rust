# Outbound OAuth2 Client Credentials for ToolKit gears

## Context

ToolKit gears call internal vendor REST services secured with OAuth2 Client Credentials.
Each gear gets:

- `client_id`, `client_secret`
- `token_endpoint` or `issuer_url`
- `scopes` list

The platform HTTP client is `toolkit-http::HttpClient` (hyper + tower), which already provides retry with exponential backoff and jitter, OTel tracing, concurrency limiting, TLS-only transport, and a preconfigured `HttpClientConfig::token_endpoint()` profile.

## Requirements

- token acquisition + in-memory cache
- refresh before expiry
- jitter to avoid stampede
- safe concurrency under load
- automatic `Authorization: Bearer <token>` injection for outbound HTTP
- no `reqwest` dependency (direct or transitive) in `toolkit-auth`

## Decision

Use an in-house `token_watcher` gear (`oauth2/token_watcher.rs`) for token lifecycle management — background refresh with jitter, exponential backoff, and lock-free reads via `ArcSwap`. This replaces the earlier `aliri_tokens` dependency, which was removed to eliminate its transitive `ring` dependency (see [ADR 0005 — FIPS Dependency Policy](../../security/fips/adrs/0005-fips-dependency-policy.md)).

OAuth2 Client Credentials exchange is implemented as a custom token source that uses `toolkit-http::HttpClient` with `HttpClientConfig::token_endpoint()`. Outbound HTTP composition is hyper + tower. Authentication is implemented as a tower layer that composes with the existing `HttpClient` layer stack.

## Consequences

**Good:**

- refresh scheduling, jitter, concurrency control, and backoff are handled by `token_watcher` — no external dependency needed
- input validation rejects pathological token lifetimes (zero, NaN, infinity, zero-window) before caching a token
- `valid_token()` prevents callers from accidentally using expired tokens
- shutdown cancels in-flight refresh requests via `tokio::select!`
- token endpoint HTTP calls reuse `toolkit-http::HttpClient` — retries, timeouts, rate limiting, OTel tracing, and TLS are handled by the existing tower stack; no duplicate implementation needed
- `HttpClientConfig::token_endpoint()` already configures conservative retry (transport errors, timeout, 429 only) appropriate for token acquisition
- outbound auth layer is a standard tower layer, composable with the existing `HttpClient` middleware
- no `reqwest` or `ring` in `toolkit-auth`

**Bad:**

- small amount of glue code is needed: `OAuthTokenSource` implementation wrapping `HttpClient`, optional OIDC discovery, tower auth layer
- `invalidate()` is implemented via watcher rotation after acquisition (pending initial acquisition is shared)

## Implementation Status

All components are implemented in `libs/toolkit-auth/src/oauth2/` and `libs/toolkit-http/src/builder.rs`.

### Gears and public API

| Gear | Path | Public type | Description |
|--------|------|-------------|-------------|
| `config` | `oauth2/config.rs` | `OAuthClientConfig` | Configuration struct with `token_endpoint` / `issuer_url` (mutually exclusive), credentials, scopes, refresh policy, and optional `HttpClientConfig` override. `Debug` redacts `client_secret`. |
| `types` | `oauth2/types.rs` | `ClientAuthMethod`, `SecretString` | Auth method enum (`Basic` / `Form`). `SecretString` re-exported from `toolkit-utils` (backed by `Zeroizing<String>`). |
| `error` | `oauth2/error.rs` | `TokenError` | `#[non_exhaustive]` error enum: `Http`, `HttpStatus`, `InvalidResponse`, `UnsupportedTokenType`, `ConfigError`, `Unavailable`, `InvalidTokenLifetime`. All variants are secret-safe. |
| `token` | `oauth2/token.rs` | `Token` | Handle for obtaining bearer tokens. `Clone + Send + Sync`. Background refresh via in-house `token_watcher::TokenWatcher`. Lock-free reads via `ArcSwap`. `get()` returns `Result<SecretString, TokenError>` (rejects expired tokens), `invalidate()` rotates the watcher without repeating OIDC discovery. |
| `layer` | `oauth2/layer.rs` | `BearerAuthLayer` | Tower `Layer` + `Service` that injects `Authorization: Bearer <token>` (or custom header) into outbound requests. |
| `builder_ext` | `oauth2/builder_ext.rs` | `HttpClientBuilderExt` | Extension trait on `toolkit_http::HttpClientBuilder` providing `.with_bearer_auth(token)` and `.with_bearer_auth_header(token, header_name)`. |
| `discovery` | `oauth2/discovery.rs` | *(crate-internal)* | One-time OIDC discovery: fetches `{issuer_url}/.well-known/openid-configuration` and extracts `token_endpoint`. |
| `token_watcher` | `oauth2/token_watcher.rs` | *(crate-internal)* | Background refresh loop with jitter, exponential backoff, input validation, and shutdown support. Stores the latest `CachedToken` in `ArcSwap` for lock-free reads. |
| `source` | `oauth2/source.rs` | *(crate-internal)* | `OAuthTokenSource` that exchanges client credentials for an access token via `toolkit-http::HttpClient`. |

All public types are re-exported from `toolkit_auth::oauth2` and from the crate root (`toolkit_auth::{...}`).

### `toolkit-http` integration

`HttpClientBuilder::with_auth_layer(wrap)` accepts a generic `FnOnce(BoxCloneService) -> BoxCloneService` transform inserted between retry and timeout in the tower stack. This avoids a circular dependency (`toolkit-auth` depends on `toolkit-http`, not vice versa). The `HttpClientBuilderExt` extension trait in `toolkit-auth` wraps `BearerAuthLayer` into this hook.

### Architecture notes

- **No circular dependency:** `toolkit-http` has zero awareness of `toolkit-auth`. The auth layer is injected via a generic hook (`with_auth_layer`) and an extension trait pattern.
- **OIDC discovery is one-time:** Runs on the first token fetch. The resolved `token_endpoint` is shared by source clones through `OnceCell`, so `invalidate()` rebuilds the watcher without re-running successful discovery. Failed discovery can be retried.
- **Secret safety:** `SecretString` uses `Zeroizing<String>` for secure memory cleanup. `Debug` and `Display` impls are redacted. Access tokens are only exposed transiently in `format!("Bearer {}", secret.expose())` within the auth layer.
- **Stack order:** `Buffer -> OTel -> LoadShed -> Retry -> **Auth** -> Timeout -> UserAgent -> Decompress -> Redirect -> hyper`

### Lazy startup

`Token::new_lazy()` validates the runtime and configuration and builds the HTTP
client without network I/O. Its watcher waits for the first `get()`,
`wait_ready()` or `invalidate()` call. Initial acquisition retries use 1–30 second
exponential backoff independent of the refresh policy; after success the watcher
uses the configured refresh schedule and backoff. These are delays between
attempts, not a total acquisition deadline. HTTP timeouts and transport retries
still belong to `toolkit-http`.

The cache atomically publishes a token and the last failure. Empty-cache reads
return `Unavailable` before the first result, then the actual acquisition error.
Successful discovery is retained across retries and watcher rotation. Shutdown
interrupts discovery, token exchange and retry sleeps. The eager constructor
remains available. Construction outside Tokio returns `ConfigError`.

`wait_ready()` subscribes to a per-attempt result channel before triggering lazy
acquisition, avoiding missed wakeups. A caller without a usable cached token
waits for the selected current or next scheduled attempt and receives its token
or error. Later attempts do not overwrite results retained by earlier waiters.
Cancelling one waiter does not cancel the shared worker. Callers own their wait
timeout. Synchronous reads retain `ArcSwap::load` and notify only an empty cache.

### HTTP error compatibility

HTTP response failures now use the non-exhaustive `TokenError::HttpStatus`
variant with `HttpOperation::{Discovery, TokenRequest}` and a typed status,
excluding response bodies and headers. `TokenError::status()` exposes that status.
`Http(String)` remains for transport failures. Existing matches on `Http(..)` no
longer catch HTTP response failures: consumers must handle the new variant or
use `status()` to preserve their intended classification.
