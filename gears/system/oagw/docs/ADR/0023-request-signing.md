---
description: "Auth plugins get an optional signing phase that runs on the final outbound request, right before it is sent."
status: proposed
date: 2026-11-19
---

# Late Request Signing for Auth Plugins

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Position in the pipeline](#position-in-the-pipeline)
  - [Interface](#interface)
  - [What the plugin sees](#what-the-plugin-sees)
  - [What the plugin returns](#what-the-plugin-returns)
  - [Repeated headers](#repeated-headers)
  - [Failures](#failures)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. Sign in `authenticate`](#a-sign-in-authenticate)
  - [B. Move the auth step to the end](#b-move-the-auth-step-to-the-end)
  - [C. Optional signing phase on `AuthPlugin`](#c-optional-signing-phase-on-authplugin)
  - [D. A separate signer plugin type](#d-a-separate-signer-plugin-type)
  - [E. Signing in the upstream engine](#e-signing-in-the-upstream-engine)
- [Open Questions](#open-questions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-request-signing`

## Context and Problem Statement

Some upstreams accept only signed requests. AWS services, for example, require [Signature Version 4](https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv.html): the signature covers the method, path, query, selected headers, a hash of the body and the time. Any change to these after signing makes the upstream reject the request ([#5842](https://github.com/constructorfabric/gears-rust/issues/5842)).

[ADR-0003](./0003-plugin-system.md#execution-order) runs the auth plugin once, before guards. In the [Data Plane](../../oagw/src/infra/proxy/service.rs), several steps change the request after that: header rules, transform plugins (headers and query), endpoint selection, the `Host` header, `request_ref` key injection ([ADR-0022](./0022-request-secret-refs.md)) and the URL build, which form-encodes the query again when a transform changed it or it has a literal `;`. `AuthPlugin::authenticate` also does not see the request: it only lists headers to set. A plugin therefore has no place where it can see and sign the request as it is sent.

Separately, the `allowlist` passthrough mode forwarded only the first line of a repeated header, so the outbound request lost values the client sent.

## Decision Drivers

- Sign exactly what is sent: method, URL, headers and body, byte for byte.
- Existing auth plugins compile and behave unchanged.
- Fail closed: a request that cannot be signed is not sent.
- Keep the plugin interface in domain types (`#[domain_model]`), without `http` types.
- Signatures and the credentials behind them never reach logs or error messages.

## Considered Options

1. Sign in `authenticate`, at its current position.
2. Move the whole auth step to the end of the request pipeline.
3. Add an optional signing phase to `AuthPlugin` that runs last.
4. Add a separate signer plugin type.
5. Sign in the upstream engine (`toolkit-http-proxy`).

## Decision Outcome

We propose **C, an optional signing phase on `AuthPlugin`**. `AuthPlugin` gains `signs`, which defaults to `false`, and `sign`, which defaults to a no-op. For a plugin whose `signs` returns `true`, the Data Plane calls `sign` once per request, after every other change to the outbound request and right before the request is sent, on the HTTP and WebSocket paths. This amends the execution order of ADR-0003: `authenticate` keeps its position, and `sign` is added at the end of the request phase.

### Position in the pipeline

```text
Incoming request
  → resolve upstream and route; CORS, query allowlist, path suffix, HTTPS-only checks
  → outbound headers: passthrough, strip
  → auth plugin: authenticate (credential headers)
  → guard plugins
  → header rules, transform plugins (on_request)
  → endpoint selection, Host header; request_ref key injection (ADR-0022)
  → rate limits; URL build; upstream address resolution
  → WebSocket only: Connection: upgrade
  → signing plugins only: engine header sanitization; auth plugin: sign (this ADR)
  → WebSocket handshake or HTTP request to the upstream
  → transform plugins (on_response / on_error)
```

Signing runs after the rate limit check, so a rejected request is not signed. It runs under the request deadline that already covers endpoint selection, address resolution and the exchange; a signing phase that does not finish in time fails the request with 504 `RequestTimeout`. Signing time is the gateway's, not the upstream's: the circuit breaker takes it off the upstream's budget ([ADR-0005](./0005-circuit-breaker.md), *Full budget*). The `sign` future is then dropped at whatever await it reached, so a plugin must not leave shared state (a nonce, a cached credential) half-updated across an await. In `request_ref` mode the proxy injects the key itself and runs no auth plugin, so there is no signing phase.

### Interface

```rust
// oagw/src/domain/plugin/mod.rs
#[async_trait]
pub trait AuthPlugin: Send + Sync {
    async fn authenticate(&self, ctx: &mut AuthContext) -> Result<(), PluginError>;

    fn signs(&self) -> bool {
        false
    }

    async fn sign(&self, _ctx: &SigningContext) -> Result<SigningOutput, PluginError> {
        Ok(SigningOutput::default())
    }
}

pub struct SigningContext {
    pub method: String,
    pub scheme: String,            // http, https or wss
    pub host: String,
    pub port: u16,                 // also when the URL omits the default port
    pub path: String,              // as sent, percent-encoded
    pub path_rewritten: bool,      // the gateway removed `//` or `.`/`..` from the client's path, or removed or added a trailing `/`
    pub query: Option<String>,     // as sent, without `?`
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: SigningBody,
    pub config: HashMap<String, String>,
    pub security_context: SecurityContext,
}

pub enum SigningBody {
    Buffered(Bytes),               // the whole body; empty for no body and for a WebSocket handshake
    Streamed,                      // streamed to the upstream; bytes not known yet
}

#[derive(Default)]
pub struct SigningOutput {         // Debug prints header names only
    pub set_headers: Vec<(String, Vec<u8>)>,
    pub query: QueryChange,
}

#[derive(Default)]
pub enum QueryChange {             // Debug hides the replacement
    #[default]
    Keep,
    Remove,
    Replace(String),               // without `?`
}
```

The plugin gets the same `config` and security context as `authenticate`. Header values are bytes, so a value that is not UTF-8 is visible and can be signed. `host` is the host as in the URL, so an IPv6 address keeps its brackets. A plugin that overrides `sign` must also return `true` from `signs`; otherwise the gateway never calls `sign` and the request goes out unsigned.

### What the plugin sees

- **URL**: scheme, endpoint host and port, the path and the query exactly as they go on the request line. The query is the client's, byte for byte, or the form-encoded pairs when a transform changed them or the query has a literal `;`.
- **Headers**: every line the request is sent with, names in lowercase. A repeated name keeps all its lines, in the order they are sent; order across names has no meaning in HTTP and is not kept. The view includes `Host`, with the port unless it is the scheme's default, the headers `authenticate` set, and, for a WebSocket handshake, `Connection`, `Upgrade` and `Sec-WebSocket-*`.
- **Body**: the buffered bytes, or `Streamed` for a body streamed from an in-process SDK caller. REST callers are always buffered. A WebSocket handshake has an empty body.

Before building the view, the Data Plane applies the upstream engine's own request header sanitization (`toolkit_http_proxy::sanitize::sanitize_request_headers`, after the engine's handshake validation for a WebSocket upgrade), so the view does not show fields the engine drops (`Content-Length`, `Expect`, `Proxy-Connection`, `HTTP2-Settings`, hop-by-hop other than `TE: trailers`) or rewrites (`Upgrade` and `Connection` of a handshake, as `websocket` and `upgrade`). The engine sanitizes again when it sends the request; that pass then changes nothing. After signing, the engine still derives `Content-Length` and `Transfer-Encoding` from the body, joins `Cookie` lines into one on HTTP/1.1, and on HTTP/2 sends `Host` as `:authority` with the same value. A signer must not cover these fields. The path and query are sent as given.

### What the plugin returns

- `set_headers`: each header replaces the lines of that name and is marked sensitive, as the headers from `authenticate` are. A name or value that is not a valid header, a name set twice (a later value would replace a signed one), a header the gateway sets or strips itself (`Host`, `Content-Length`, hop-by-hop, forwarding, `X-OAGW-*`), or one the engine drops from every request (`toolkit_http_proxy::sanitize::is_dropped_request_header`: also `Expect`, `Proxy-Connection`, `HTTP2-Settings`) fails the request: the gateway or engine would change or drop it after the signature was computed.
- `query`: `Keep` (the default) sends the query as built; `Remove` sends the URL without a query; `Replace(q)` sends `q` byte for byte, without re-encoding. A replacement the URI parser would change (for example, a `#` that would start a fragment) or reject fails the request. A signer that needs a canonical query encoding, such as SigV4, rewrites the query and the upstream receives exactly that form.

Errors name the header, never its value. The query is not logged, and the `Debug` output of `SigningOutput` shows only the header names and the kind of query change.

### Repeated headers

The `allowlist` passthrough mode now forwards every line of an allowlisted header, in order, instead of the first one. The other steps already keep repeated lines: `passthrough: all` copies the request, `authenticate` replaces only the names it sets ([#4982](https://github.com/constructorfabric/gears-rust/issues/4982)), and header rules and transforms rewrite only the names they change. After signing, no step changes the header lines, apart from the engine fields listed above.

### Failures

A signing failure stops the request before anything is sent to the upstream. It maps to a `DomainError` as an `authenticate` failure does, and `on_error` transforms do not run, as for other errors raised before forwarding.

| Signing outcome | Error | Status |
|---|---|---|
| `PluginError::AuthFailed`, `PluginError::Rejected` | `AuthenticationFailed`, reason `AUTH_PLUGIN_FAILED` | 401 |
| `PluginError::Internal` | `AuthenticationFailed`, reason `AUTH_PLUGIN_INTERNAL` | 401 |
| `PluginError::SecretNotFound` | `SecretNotFound`, reason `AUTH_CREDENTIAL_NOT_FOUND` | 401 |
| `PluginError::CredentialStoreUnavailable` | `CredentialStoreUnavailable`, reason `CREDENTIAL_STORE_UNAVAILABLE` | 503 |
| `PluginError::IdpUnavailable` (identity provider token endpoint) | `IdpUnavailable`, reason `IDENTITY_PROVIDER_UNAVAILABLE` | 503 |
| `PluginError::IdpInvalidResponse` | `IdpInvalidResponse`, reason `REQUEST_NOT_SENT` | 502 |
| `PluginError::InvalidConfig`, or an output the gateway cannot send | `Internal` (plugin misconfiguration) | 500 |
| `PluginError::InvalidRequest`: a valid config, but the plugin cannot handle the request as the caller sent it | `Validation`, the field the plugin names (a header name, `body`, `path`), reason `VALIDATION` | 400 |
| Request deadline passed | `RequestTimeout`, reason `REQUEST_NOT_SENT` | 504 |

Amended by [#4985](https://github.com/constructorfabric/gears-rust/issues/4985): this is the one credential-failure mapping every auth plugin uses, for `authenticate` and `sign`. Before it, a missing secret was 500 `SecretNotFound`, and `InvalidConfig`, `Rejected` and an unsendable output were 400 `INVALID_PLUGIN_CONFIG`.

Failures of the gateway's own steps before `sign` keep their usual mapping: a built URL that does not parse is a `DownstreamError` ("invalid upstream request"), as when the request is built for the engine, and a handshake the engine would refuse fails with the engine's error.

### Consequences

- Good, because a plugin can sign the request exactly as the upstream receives it, on both paths.
- Good, because existing plugins compile unchanged and keep their behavior: the default phase changes nothing.
- Good, because a failing signer cannot send an unsigned or half-signed request.
- Good, because `allowlist` passthrough no longer drops repeated header lines, with or without a signer.
- Neutral, because only a plugin whose `signs` returns `true` pays for the signing view (a copy of the outbound headers, the auth config and the security context, and a URL parse); other plugins skip the phase.
- Bad, because the signer must know which fields the engine still rewrites and leave them out of the signature.
- A resend by the engine ([ADR-0019](./0019-upstream-proxy-engine.md)) reuses the signed request; signatures valid for minutes, such as SigV4's, are not affected.
- Signing time is recorded as `oagw_request_duration_seconds{phase="sign"}`, only for a plugin that signs, on failures and timeouts too, as `phase="auth"` is for `authenticate`. It also counts toward `total`.

### Confirmation

- `infra/proxy/signing.rs` unit tests: the view carries every header line in order, non-UTF-8 values, the port, the auth config and the caller's security context; the default phase does not sign and changes nothing; returned headers replace their name and are sensitive; a replacement or removed query is sent verbatim, and a property test checks that any replacement is refused or sent byte for byte on the same path; invalid, gateway-owned, engine-dropped or repeated header names and an unusable query fail the request without echoing the value; `Debug` hides values.
- `infra/proxy/signing_tests.rs`, through the pipeline against the mock upstream with a test auth plugin and a test transform:
  - the phase runs once and sees the query after the transform changed it, `Host` with the non-default port, both lines of an allowlisted repeated header, the `authenticate` header, the buffered body, the upstream's auth config and the caller;
  - on the scheme's default port it gets the port while the URL and `Host` leave it out;
  - with `passthrough: all`, a client's `Expect`, a `Content-Length` on a streamed body and a mixed-case `Upgrade` do not reach the view, and the view plus the output is exactly the header set the upstream receives, on both paths;
  - a plugin that does not sign is not called;
  - the phase is timed as `phase="sign"` once per signed request, on a failure too, and not for a plugin that does not sign;
  - a `sign` that does not return fails the request with 504 at the request deadline, and nothing is sent;
  - its headers and its replacement query reach the upstream unchanged;
  - a streamed body is seen as `Streamed` and still reaches the upstream;
  - a WebSocket handshake is signed with an empty body and its handshake headers;
  - a signing failure or an unusable output returns 401 or 500 and the upstream receives nothing, on both paths.
- `infra/proxy/headers.rs`: `allowlist` passthrough keeps every line of a repeated header, in order.
- Existing auth plugin and pipeline tests pass unchanged.

## Pros and Cons of the Options

### A. Sign in `authenticate`

- Good, because no interface change.
- Bad, because header rules, transforms, `Host` and the URL build change the request afterwards and break the signature.
- Bad, because `authenticate` does not see the request.

### B. Move the auth step to the end

- Good, because one auth call per request, with the final request.
- Bad, because it changes what existing configurations see: header rules and transforms run after the credential is set today, and an auth failure stops the request before guards run.
- Bad, because a credential fetch (OAuth2 token, CredStore read) would then run after rate limiting and endpoint selection for every plugin, not just signers.

### C. Optional signing phase on `AuthPlugin`

- Good, because the signer uses the same plugin instance, config and credentials as `authenticate`.
- Good, because the default no-op leaves existing plugins unchanged.
- Good, because one place covers both the HTTP and WebSocket paths.
- Bad, because `AuthPlugin` grows a second method, and its position differs from `authenticate`.

### D. A separate signer plugin type

- Good, because signing is a distinct concern.
- Bad, because the signer needs the auth plugin's credentials, so its config would repeat the auth config or reference it.
- Bad, because it adds a fourth plugin type with its own registry, binding rules, GTS schema and write-time checks.

### E. Signing in the upstream engine

- Good, because the engine sees the bytes it writes, framing included.
- Bad, because `toolkit-http-proxy` is a generic engine with no plugin, CredStore or tenant concepts.

## Open Questions

- Whether `SigningOutput` should be able to remove a header. The AWS SigV4 plugin replaces a client's `Authorization`, `X-Amz-Date` and `X-Amz-Content-Sha256`, but cannot drop a client's `X-Amz-Security-Token` when it has no session token, so it refuses that request (`InvalidRequest`, field `x-amz-security-token`) and the operator strips the header with `headers.request.remove`.

## Traceability

- **PRD**: [PRD.md](../PRD.md) — `cpt-cf-oagw-fr-auth-injection`, `cpt-cf-oagw-fr-plugin-system`
- **DESIGN**: [DESIGN.md](../DESIGN.md) — `cpt-cf-oagw-seq-proxy-flow`
- **Amends**: [ADR-0003 Plugin System](./0003-plugin-system.md#execution-order)
- **Related ADR**: [ADR-0022 Per-Request Secret References](./0022-request-secret-refs.md)
- **Features**: [Plugin System](../features/0003-cpt-cf-oagw-feature-plugin-system.md) (including the [AWS SigV4 auth plugin](../features/0003-cpt-cf-oagw-feature-plugin-system.md#aws-sigv4-auth-plugin), the first built-in signer), [HTTP Proxy Engine](../features/0004-cpt-cf-oagw-feature-proxy-engine.md)
