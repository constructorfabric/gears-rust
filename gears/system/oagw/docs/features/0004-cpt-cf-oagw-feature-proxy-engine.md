# Feature: HTTP Proxy Engine


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Proxy HTTP Request](#proxy-http-request)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Alias Resolution](#alias-resolution)
  - [Route Matching](#route-matching)
  - [Plugin Chain Execution](#plugin-chain-execution)
  - [Header Transformation](#header-transformation)
  - [Body Validation](#body-validation)
  - [Pingora In-Memory Bridge](#pingora-in-memory-bridge)
  - [Error Source Classification](#error-source-classification)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement DataPlaneService Proxy Execution](#implement-dataplaneservice-proxy-execution)
  - [Implement Alias Resolution](#implement-alias-resolution)
  - [Implement Route Matching](#implement-route-matching)
  - [Implement Plugin Chain Execution](#implement-plugin-chain-execution)
  - [Implement Header Transformation](#implement-header-transformation)
  - [Implement Body Validation](#implement-body-validation)
  - [Implement Error Source Distinction](#implement-error-source-distinction)
  - [Implement Pingora Proxy Engine](#implement-pingora-proxy-engine)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Additional Context](#7-additional-context)
  - [Performance Considerations](#performance-considerations)
  - [Security Considerations](#security-considerations)
  - [Deliberate Omissions](#deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-oagw-featstatus-proxy-engine-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-oagw-feature-proxy-engine`

## 1. Feature Context

### 1.1 Overview

Implement the Data Plane proxy execution flow: resolve upstream by alias, match route, execute plugin chain, forward HTTP request via Pingora in-memory bridge, transform response, and return with error source distinction.

### 1.2 Purpose

Core value proposition of OAGW — a unified proxy endpoint that handles credential injection, transformation, and forwarding transparently. Covers `cpt-cf-oagw-fr-request-proxy`, `cpt-cf-oagw-fr-header-transform`, `cpt-cf-oagw-fr-error-codes`, `cpt-cf-oagw-nfr-low-latency`, `cpt-cf-oagw-nfr-input-validation`, `cpt-cf-oagw-nfr-ssrf-protection`, `cpt-cf-oagw-nfr-credential-isolation`.

Design principles enforced: `cpt-cf-oagw-principle-no-retry`, `cpt-cf-oagw-principle-no-cache`, `cpt-cf-oagw-principle-error-source`, `cpt-cf-oagw-principle-rfc9457`, `cpt-cf-oagw-principle-cred-isolation`.

Design constraints enforced: `cpt-cf-oagw-constraint-body-limit`, `cpt-cf-oagw-constraint-https-only`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-oagw-actor-app-developer` | Sends proxy requests via `/api/oagw/v1/proxy/{alias}/{path}` |
| `cpt-cf-oagw-actor-upstream-service` | Receives forwarded HTTP requests from OAGW |
| `cpt-cf-oagw-actor-cred-store` | Provides credentials for auth plugin injection during proxy flow |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Sequence**: `cpt-cf-oagw-seq-proxy-flow`
- **Dependencies**: `cpt-cf-oagw-feature-management-api`, `cpt-cf-oagw-feature-plugin-system`

## 2. Actor Flows (CDSL)

### Proxy HTTP Request

- [x] `p1` - **ID**: `cpt-cf-oagw-flow-proxy-request`

**Actor**: `cpt-cf-oagw-actor-app-developer`

**Success Scenarios**:
- Request is forwarded to upstream and response is returned to caller
- Auth credentials are injected transparently via auth plugin
- Headers are transformed per upstream/route configuration

**Error Scenarios**:
- Upstream not found by alias (404 RouteNotFound)
- Upstream disabled (503 LinkUnavailable)
- Route not matched for method/path (404 RouteNotFound)
- Auth plugin fails credential injection (401 AuthenticationFailed)
- Guard plugin rejects request (4xx per guard rule)
- Body validation fails (400 ValidationError or 400 PayloadTooLarge)
- Upstream returns error response (passthrough as-is with `X-OAGW-Error-Source: upstream` — see `inst-proxy-29b`; not a gateway-fabricated `DownstreamError`)
- Upstream connection or request times out (504 ConnectionTimeout / RequestTimeout)
- WebSocket upgrade requested — bridged bidirectionally; succeeds with `101 Switching Protocols` when the upstream also upgrades, otherwise OAGW returns a gateway-fabricated `503 Service Unavailable` (`ProtocolError`), discarding the upstream's real non-101 status and body (see [positive-14.1](../../scenarios/protocols/websocket/positive-14.1-websocket-upgrade-proxied.md) / [negative-14.8](../../scenarios/protocols/websocket/negative-14.8-websocket-upgrade-rejected-non-ws-upstream.md))
- Pingora-level protocol error (503 ProtocolError, canonical `service_unavailable` — e.g. HTTP/2 downgrade failure)
- X-OAGW-Target-Host missing for multi-endpoint common-suffix upstream (400 MissingTargetHost)
- X-OAGW-Target-Host format invalid (400 InvalidTargetHost)
- X-OAGW-Target-Host does not match any configured endpoint (400 UnknownTargetHost)
- Upstream connection fails at network level (503 DownstreamError, canonical `service_unavailable`)

**Steps**:
1. [x] - `p1` - Actor sends `{METHOD} /api/oagw/v1/proxy/{alias}[/{path}][?{query}]` - `inst-proxy-1`
2. [x] - `p1` - API: Extract `SecurityContext` (tenant_id, principal_id, permissions) from Bearer token - `inst-proxy-2`
3. [x] - `p1` - Invoke `DataPlaneService.execute_proxy(alias, path, method, headers, body, security_context)` - `inst-proxy-3`
4. [x] - `p1` - Resolve upstream by alias via `cpt-cf-oagw-algo-alias-resolution` - `inst-proxy-4`
5. [x] - `p1` - **IF** upstream not found - `inst-proxy-5`
   1. [x] - `p1` - **RETURN** 404 RouteNotFound with `X-OAGW-Error-Source: gateway` - `inst-proxy-5a`
6. [x] - `p1` - **IF** upstream disabled - `inst-proxy-6`
   1. [x] - `p1` - **RETURN** 503 LinkUnavailable with `X-OAGW-Error-Source: gateway` - `inst-proxy-6a`
7. [x] - `p1` - Match route via `cpt-cf-oagw-algo-route-matching` - `inst-proxy-7`
8. [x] - `p1` - **IF** no matching route - `inst-proxy-8`
    1. [x] - `p1` - **RETURN** 404 RouteNotFound with `X-OAGW-Error-Source: gateway` - `inst-proxy-8a`
9. [x] - `p1` - Validate request body via `cpt-cf-oagw-algo-body-validation` - `inst-proxy-9`
10. [x] - `p1` - **IF** body validation fails - `inst-proxy-10`
    1. [x] - `p1` - **RETURN** 400 ValidationError or 400 PayloadTooLarge with `X-OAGW-Error-Source: gateway` - `inst-proxy-10a`
11. [x] - `p1` - Compose plugin chain via `cpt-cf-oagw-algo-plugin-chain-execution` - `inst-proxy-11`
12. [x] - `p1` - Execute auth plugin: inject credentials into outbound request - `inst-proxy-12`
13. [x] - `p1` - **IF** auth plugin fails (secret not found, credential error) - `inst-proxy-13`
    1. [x] - `p1` - **RETURN** 401 AuthenticationFailed or 500 SecretNotFound with `X-OAGW-Error-Source: gateway` - `inst-proxy-13a`
14. [x] - `p1` - Execute guard plugins: validate method, query params, path suffix, CORS origin (actual requests only; preflight returns permissive 204 at handler level — see [ADR: CORS](../ADR/0006-cors.md)) - `inst-proxy-14`
15. [x] - `p1` - **IF** any guard rejects - `inst-proxy-15`
    1. [x] - `p1` - **RETURN** guard-specific error code with `X-OAGW-Error-Source: gateway` - `inst-proxy-15a`
16. [x] - `p1` - Execute transform plugins: `on_request` phase — mutate outbound request - `inst-proxy-16`
17. [x] - `p1` - Apply header transformation via `cpt-cf-oagw-algo-header-transformation` - `inst-proxy-17`
18. [x] - `p1` - Select target endpoint via `X-OAGW-Target-Host` header or round-robin - `inst-proxy-18`
19. [x] - `p1` - **IF** multi-endpoint upstream with common-suffix alias AND `X-OAGW-Target-Host` header missing - `inst-proxy-19`
    1. [x] - `p1` - **RETURN** 400 MissingTargetHost with `X-OAGW-Error-Source: gateway` - `inst-proxy-19a`
20. [x] - `p1` - **IF** `X-OAGW-Target-Host` present AND format invalid (not hostname or IP; contains port, path, or special chars) - `inst-proxy-20`
    1. [x] - `p1` - **RETURN** 400 InvalidTargetHost with `X-OAGW-Error-Source: gateway` - `inst-proxy-20a`
21. [x] - `p1` - **IF** `X-OAGW-Target-Host` present AND value does not match any configured endpoint host - `inst-proxy-21`
    1. [x] - `p1` - **RETURN** 400 UnknownTargetHost with `X-OAGW-Error-Source: gateway` - `inst-proxy-21a`
22. [x] - `p1` - **IF** request contains `Upgrade: websocket` header - `inst-proxy-22`
    1. [x] - `p1` - Forward the upgrade handshake to the upstream via a bidirectional bridge - `inst-proxy-22a`
    2. [x] - `p1` - **IF** upstream responds `101 Switching Protocols` - `inst-proxy-22b`
       1. [x] - `p1` - Complete the upgrade and relay frames bidirectionally until either side closes — terminal; steps 23+ do not run for this request - `inst-proxy-22b1`
    3. [x] - `p1` - **ELSE** (upstream does not upgrade) - `inst-proxy-22c`
       1. [x] - `p1` - Return a gateway-fabricated `503 Service Unavailable` (`ProtocolError`) — the upstream's real non-101 status and body are discarded, not propagated; terminal, steps 23+ do not run for this request - `inst-proxy-22c1`
23. [x] - `p1` - Build outbound HTTP request: set target URL (scheme + host + port + path), method, headers, body — only reached for non-WebSocket requests, since step 22 is terminal for `Upgrade: websocket` requests - `inst-proxy-23`
24. [x] - `p1` - Serialize request into in-memory duplex stream and forward to Pingora `ProxyHttp` engine via `cpt-cf-oagw-algo-pingora-bridge` - `inst-proxy-24`
25. [x] - `p1` - **IF** Pingora reports upstream connection failure (refused, DNS, TLS) via `fail_to_proxy` - `inst-proxy-25`
    1. [x] - `p1` - Map Pingora `ErrorType` to `DomainError` and write RFC 9457 Problem response with `X-OAGW-Error-Source: gateway` - `inst-proxy-25a`
    2. [x] - `p1` - **RETURN** 503 DownstreamError (canonical `service_unavailable`) with `X-OAGW-Error-Source: gateway` - `inst-proxy-25b`
26. [x] - `p1` - **IF** connection or request timeout (Pingora `ConnectTimedout`, `ReadTimedout`, `WriteTimedout`) - `inst-proxy-26`
    1. [x] - `p1` - **RETURN** 504 ConnectionTimeout or RequestTimeout via `cpt-cf-oagw-algo-error-source-classification` - `inst-proxy-26a`
27. [x] - `p1` - **IF** Pingora reports HTTP/2 error (`H2Error`, `H2Downgrade`) - `inst-proxy-27`
    1. [x] - `p1` - **RETURN** 503 ProtocolError (canonical `service_unavailable`) with `X-OAGW-Error-Source: gateway` - `inst-proxy-27a`
28. [x] - `p1` - Parse upstream response from duplex stream read half - `inst-proxy-28`
29. [x] - `p1` - **IF** upstream returns error response - `inst-proxy-29`
    1. [x] - `p1` - Execute transform plugins: `on_error` phase - `inst-proxy-29a`
    2. [x] - `p1` - **RETURN** upstream response as-is with `X-OAGW-Error-Source: upstream` - `inst-proxy-29b`
30. [x] - `p1` - Execute transform plugins: `on_response` phase - `inst-proxy-30`
31. [x] - `p1` - **RETURN** transformed response with `X-OAGW-Error-Source: upstream` - `inst-proxy-31`

## 3. Processes / Business Logic (CDSL)

### Alias Resolution

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-alias-resolution`

**Input**: Alias string, tenant_id from SecurityContext

**Output**: Resolved `UpstreamConfig` or error

**Steps**:
1. [x] - `p1` - DB: SELECT upstream FROM oagw_upstream WHERE alias = :alias AND tenant_id = :tenant_id AND enabled = true - `inst-alias-1`
2. [x] - `p1` - **IF** upstream found in current tenant - `inst-alias-2`
   1. [x] - `p1` - **RETURN** resolved UpstreamConfig - `inst-alias-2a`
3. [x] - `p1` - Walk tenant hierarchy from current tenant toward root (closest ancestor first) - `inst-alias-3`
4. [x] - `p1` - **FOR EACH** ancestor tenant_id in hierarchy - `inst-alias-4`
   1. [x] - `p1` - DB: SELECT upstream FROM oagw_upstream WHERE alias = :alias AND tenant_id = :ancestor_id AND sharing != 'private' - `inst-alias-4a`
   2. [x] - `p1` - **IF** upstream found - `inst-alias-4b`
      1. [x] - `p1` - **IF** upstream disabled (enabled = false) - `inst-alias-4b1`
         1. [x] - `p1` - **RETURN** error: upstream disabled (503 LinkUnavailable) - `inst-alias-4b1a`
      2. [x] - `p1` - **RETURN** resolved UpstreamConfig (closest match wins — shadowing) - `inst-alias-4b2`
5. [x] - `p1` - **RETURN** error: upstream not found for alias (404 RouteNotFound) - `inst-alias-5`

### Route Matching

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-route-matching`

**Input**: upstream_id, HTTP method, request path

**Output**: Matched `RouteConfig` or error

**Steps**:
1. [x] - `p1` - DB: SELECT routes FROM oagw_route WHERE upstream_id = :upstream_id AND enabled = true - `inst-route-1`
2. [x] - `p1` - Filter routes by match type for the request protocol - `inst-route-2`
3. [x] - `p1` - **FOR EACH** route ordered by path prefix length (longest first), then priority (lowest number first) - `inst-route-3`
   1. [x] - `p1` - **IF** route.match_type = 'http' - `inst-route-3a`
      1. [x] - `p1` - DB: Check method in oagw_route_method WHERE route_id = :route_id - `inst-route-3a1`
      2. [x] - `p1` - DB: Check request path starts with path_prefix from oagw_route_http_match - `inst-route-3a2`
      3. [x] - `p1` - **IF** method allowed AND path matches prefix - `inst-route-3a3`
         1. [x] - `p1` - **RETURN** RouteConfig (first match wins — longest prefix + highest priority) - `inst-route-3a3a`
   2. [x] - `p1` - **IF** route.match_type = 'grpc' - `inst-route-3b`
      1. [x] - `p1` - DB: Check gRPC service and method from oagw_route_grpc_match - `inst-route-3b1`
      2. [x] - `p1` - **IF** service and method match - `inst-route-3b2`
         1. [x] - `p1` - **RETURN** RouteConfig - `inst-route-3b2a`
4. [x] - `p1` - **RETURN** error: no matching route found (404 RouteNotFound) - `inst-route-4`

### Plugin Chain Execution

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-plugin-chain-execution`

**Input**: Upstream plugin bindings, route plugin bindings, request context, auth plugin reference from upstream

**Output**: Processed request ready for forwarding, or rejection error

**Steps**:
1. [x] - `p1` - Load upstream plugin bindings ordered by position from oagw_upstream_plugin - `inst-chain-1`
2. [x] - `p1` - Load route plugin bindings ordered by position from oagw_route_plugin - `inst-chain-2`
3. [x] - `p1` - Compose ordered chain: `[upstream_plugins...] + [route_plugins...]` (upstream-before-route) - `inst-chain-3`
4. [x] - `p1` - **FOR EACH** plugin_ref in composed chain - `inst-chain-4`
   1. [x] - `p1` - Parse GTS identifier: extract instance part (after `~`) - `inst-chain-4a`
   2. [x] - `p1` - **IF** instance parses as UUID - `inst-chain-4b`
      1. [x] - `p1` - DB: SELECT plugin FROM oagw_plugin WHERE id = :uuid — must exist and match schema type - `inst-chain-4b1`
   3. [x] - `p1` - **ELSE** (named plugin) - `inst-chain-4c`
      1. [x] - `p1` - Resolve via in-process plugin registry - `inst-chain-4c1`
   4. [x] - `p1` - **IF** plugin not found, handling is type-specific (resolution failure is not a single shared step — each plugin type resolves and handles its own failure at execution time; see steps 6, 8, 9) - `inst-chain-4d`
5. [x] - `p1` - Resolve upstream auth plugin from auth_plugin_ref / auth_plugin_uuid columns - `inst-chain-5`
6. [x] - `p1` - Execute auth plugin: resolve credentials from `cred_store` via secret_ref, inject into request - `inst-chain-6`
   1. [x] - `p1` - **IF** auth plugin resolution fails - `inst-chain-6a`
      1. [x] - `p1` - **RETURN** 401 AuthenticationFailed - `inst-chain-6a1`
7. [x] - `p1` - **IF** secret not found or credential resolution fails - `inst-chain-7`
   1. [x] - `p1` - **RETURN** 401 AuthenticationFailed or 500 SecretNotFound - `inst-chain-7a`
8. [x] - `p1` - **FOR EACH** guard plugin in chain (type = guard) - `inst-chain-8`
   1. [x] - `p1` - **IF** guard plugin resolution fails - `inst-chain-8x`
      1. [x] - `p1` - **RETURN** 500 Internal — guards use fail-hard semantics; the first resolution failure or rejection terminates the pipeline immediately - `inst-chain-8x1`
   2. [x] - `p1` - Execute guard: validate request against guard rules (method allowlist, query allowlist, path suffix, timeout) - `inst-chain-8a`
   3. [x] - `p1` - **IF** guard rejects - `inst-chain-8b`
      1. [x] - `p1` - **RETURN** guard-specific rejection error - `inst-chain-8b1`
9. [x] - `p1` - **FOR EACH** transform plugin in chain (type = transform, phase = on_request) - `inst-chain-9`
   1. [x] - `p1` - **IF** transform plugin resolution fails, or the transform returns an error - `inst-chain-9x`
      1. [x] - `p1` - Log a warning and **SKIP** this plugin, continuing the chain — transforms use log-and-continue semantics so a single misbehaving or unresolvable transform cannot block the request - `inst-chain-9x1`
   2. [x] - `p1` - Execute transform: mutate request headers, body, query as configured - `inst-chain-9a`
10. [x] - `p1` - **RETURN** processed request ready for upstream forwarding - `inst-chain-10`

### Header Transformation

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-header-transformation`

**Input**: Inbound request headers, upstream `HeadersConfig`, route `HeadersConfig`

**Output**: Transformed outbound headers

**Steps**:
1. [x] - `p1` - Strip hop-by-hop headers: Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade - `inst-header-1`
2. [x] - `p1` - Strip routing header: consume `X-OAGW-Target-Host` value for endpoint selection, then remove - `inst-header-2`
3. [x] - `p1` - Replace `Host` header with upstream endpoint host (HTTP/1.1) or `:authority` pseudo-header with upstream authority (HTTP/2) - `inst-header-3`
4. [x] - `p1` - Apply upstream `headers` config: execute set/add/remove operations in declared order - `inst-header-4`
5. [x] - `p1` - Apply route `headers` config (if any): execute set/add/remove operations in declared order - `inst-header-5`
6. [x] - `p1` - Validate well-known headers: Content-Length (valid integer), Content-Type (recognized format) — reject with 400 if invalid - `inst-header-6`
7. [x] - `p1` - **RETURN** transformed header set - `inst-header-7`

### Body Validation

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-body-validation`

**Input**: Request body stream, Content-Length header value, Transfer-Encoding header value

**Output**: Validation pass or rejection error

**Steps**:
1. [x] - `p1` - **IF** Content-Length header present - `inst-body-1`
   1. [x] - `p1` - **IF** Content-Length is not a valid non-negative integer - `inst-body-1a`
      1. [x] - `p1` - **RETURN** 400 ValidationError - `inst-body-1a1`
   2. [x] - `p1` - **IF** Content-Length exceeds 100MB (104,857,600 bytes) - `inst-body-1b`
      1. [x] - `p1` - **RETURN** 400 PayloadTooLarge (canonical `OutOfRange`; reject before buffering per `cpt-cf-oagw-constraint-body-limit`) - `inst-body-1b1`
2. [x] - `p1` - **IF** Transfer-Encoding header present - `inst-body-2`
   1. [x] - `p1` - **IF** encoding is not `chunked` - `inst-body-2a`
      1. [x] - `p1` - **RETURN** 400 ValidationError (only `chunked` supported) - `inst-body-2a1`
3. [x] - `p1` - **IF** actual body size exceeds 100MB during streaming read - `inst-body-3`
   1. [x] - `p1` - **RETURN** 400 PayloadTooLarge (canonical `OutOfRange`; abort read) - `inst-body-3a`
4. [x] - `p1` - **RETURN** validation passed - `inst-body-4`

### Pingora In-Memory Bridge

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-pingora-bridge`

**Input**: Serialized HTTP/1.1 request (headers + body), selected endpoint

**Output**: Upstream HTTP response or Pingora error

**Steps**:
1. [x] - `p1` - Inject internal metadata headers (`x-oagw-internal-*`) for Pingora endpoint selection into outbound headers - `inst-bridge-1`
2. [x] - `p1` - Create in-memory `tokio::io::duplex` stream pair (write half, read half) - `inst-bridge-2`
3. [x] - `p1` - Serialize HTTP/1.1 request headers (including internal metadata) into write half - `inst-bridge-3`
4. [x] - `p1` - **IF** request has body AND Content-Length known - `inst-bridge-4`
   1. [x] - `p1` - Write full body with Content-Length framing - `inst-bridge-4a`
5. [x] - `p1` - **ELSE IF** request has streaming body - `inst-bridge-5`
   1. [x] - `p1` - Write headers only, then stream body chunks; body boundary is `Connection: close` (EOF on write-half shutdown) - `inst-bridge-5a`
6. [x] - `p1` - Feed duplex read half to Pingora `http_proxy_service_with_name` as client session - `inst-bridge-6`
7. [x] - `p1` - Pingora `ProxyHttp` implementation resolves `HttpPeer` from internal headers, connects to upstream, forwards request, streams response back into duplex write half - `inst-bridge-7`
8. [x] - `p1` - **IF** Pingora encounters upstream error - `inst-bridge-8`
   1. [x] - `p1` - `fail_to_proxy` converts `pingora_core::ErrorType` → `DomainError` → RFC 9457 `Problem` and writes directly to session - `inst-bridge-8a`
9. [x] - `p1` - Parse HTTP/1.1 response from duplex read half (status, headers, body) - `inst-bridge-9`
10. [x] - `p1` - **RETURN** parsed `http::Response<Body>` - `inst-bridge-10`

### Error Source Classification

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-error-source-classification`

**Input**: Error origin context (gateway processing step or upstream HTTP response)

**Output**: Error response with `X-OAGW-Error-Source` header and appropriate body format

**Steps**:
1. [x] - `p1` - **IF** error originated during gateway processing (alias resolution, route matching, auth, guard, body validation, plugin resolution, endpoint selection, connection failure, timeout) - `inst-errsrc-1`
   1. [x] - `p1` - Set response header `X-OAGW-Error-Source: gateway` - `inst-errsrc-1a`
   2. [x] - `p1` - Format response body as RFC 9457 Problem Details (`application/problem+json`) with GTS type identifier per `cpt-cf-oagw-principle-rfc9457` - `inst-errsrc-1b`
   3. [x] - `p1` - Include standard fields: `type` (GTS ID), `title`, `status`, `detail`, `instance` - `inst-errsrc-1c`
   4. [x] - `p1` - Include extension fields where applicable: `upstream_id`, `host`, `path`, `retry_after_seconds`, `trace_id` - `inst-errsrc-1d`
2. [x] - `p1` - **IF** error originated from upstream service HTTP response - `inst-errsrc-2`
   1. [x] - `p1` - Set response header `X-OAGW-Error-Source: upstream` - `inst-errsrc-2a`
   2. [x] - `p1` - Pass through upstream response body as-is (no rewriting, preserve original content-type) - `inst-errsrc-2b`
3. [x] - `p1` - **RETURN** classified error response - `inst-errsrc-3`

## 4. States (CDSL)

Not applicable. The proxy engine is a stateless request-response flow — each proxy request is independent and does not persist gateway-side state between invocations. Circuit breaker state management is out of scope for this feature (see `cpt-cf-oagw-feature-rate-limiting`).

## 5. Definitions of Done

### Implement DataPlaneService Proxy Execution

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-proxy-execution`

The system **MUST** implement `DataPlaneService::proxy_request(...)` that orchestrates the full proxy flow: alias resolution, route matching, body validation, plugin chain execution, HTTP forwarding via the Pingora in-memory bridge, and response transformation. No upstream response caching per `cpt-cf-oagw-principle-no-cache`. Pingora's stale pooled-connection reconnect (re-establishing a connection that was closed server-side before request bytes are sent) is permitted; all other failures **MUST** be returned immediately without additional application-layer retries per `cpt-cf-oagw-principle-no-retry`.

**Implements**:
- `cpt-cf-oagw-flow-proxy-request`
- `cpt-cf-oagw-algo-error-source-classification`

**Touches**:
- API: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}`
- Entities: `Upstream`, `Route`, `Plugin`, `ServerConfig`, `Endpoint`

### Implement Alias Resolution

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-alias-resolution`

The system **MUST** resolve upstreams by `(tenant_id, alias)` with tenant hierarchy walk from descendant to root. The closest match wins (shadowing). Disabled upstreams **MUST** return 503 LinkUnavailable. Private upstreams (sharing = `private`) **MUST NOT** be visible to descendant tenants.

**Implements**:
- `cpt-cf-oagw-algo-alias-resolution`

**Touches**:
- DB: `oagw_upstream`
- Entities: `Upstream`

### Implement Route Matching

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-route-matching`

The system **MUST** match inbound requests to routes by HTTP method allowlist and longest path prefix, ordered by priority (lowest number wins). Only enabled routes are considered. gRPC routes match by `(service, method)` via `oagw_route_grpc_match`. No two enabled routes under the same upstream may share `(path_prefix, priority)` for the same method.

**Implements**:
- `cpt-cf-oagw-algo-route-matching`

**Touches**:
- DB: `oagw_route`, `oagw_route_http_match`, `oagw_route_grpc_match`, `oagw_route_method`
- Entities: `Route`

### Implement Plugin Chain Execution

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-plugin-chain`

The system **MUST** execute the plugin chain in deterministic order: Auth → Guards → Transform(on_request) → upstream call → Transform(on_response/on_error). Upstream plugins **MUST** execute before route plugins (`[U1, U2] + [R1, R2]`). Plugins **MUST** be resolved via GTS identifier: UUID-backed plugins from `oagw_plugin`, named plugins from in-process registry. Auth plugin credentials **MUST** be resolved from `cred_store` via `secret_ref` at request time per `cpt-cf-oagw-principle-cred-isolation`.

**Implements**:
- `cpt-cf-oagw-algo-plugin-chain-execution`

**Touches**:
- DB: `oagw_upstream_plugin`, `oagw_route_plugin`, `oagw_plugin`
- Entities: `Plugin`

### Implement Header Transformation

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-header-transformation`

The system **MUST** strip hop-by-hop headers (Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade), strip `X-OAGW-Target-Host` after consuming for routing, replace `Host` (HTTP/1.1) or `:authority` (HTTP/2) with upstream host, and apply upstream/route header configs with set/add/remove operations. Well-known headers (Content-Length, Content-Type) **MUST** be validated; invalid headers **MUST** result in 400 Bad Request.

**Implements**:
- `cpt-cf-oagw-algo-header-transformation`

**Touches**:
- Entities: `Upstream` (headers config), `Route` (headers config)

### Implement Body Validation

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-body-validation`

The system **MUST** validate Content-Length (valid non-negative integer), enforce 100MB hard limit (reject before buffering per `cpt-cf-oagw-constraint-body-limit`), and reject unsupported Transfer-Encoding (only `chunked` supported). Invalid requests **MUST** return 400 ValidationError or 400 PayloadTooLarge.

**Implements**:
- `cpt-cf-oagw-algo-body-validation`

**Touches**:
- API: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}`

### Implement Error Source Distinction

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-error-source-distinction`

The system **MUST** set `X-OAGW-Error-Source: gateway` on all gateway-originated errors and `X-OAGW-Error-Source: upstream` on upstream passthrough errors per `cpt-cf-oagw-principle-error-source`. Gateway errors **MUST** use RFC 9457 Problem Details format (`application/problem+json`) with GTS type identifiers per `cpt-cf-oagw-principle-rfc9457`. Upstream error responses **MUST** be passed through as-is without body rewriting.

**Implements**:
- `cpt-cf-oagw-algo-error-source-classification`

**Touches**:
- API: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}`

### Implement Pingora Proxy Engine

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-pingora-proxy`

The system **MUST** use Pingora (`pingora-proxy`, `pingora-load-balancing`) as the upstream HTTP engine, connected via an in-memory `tokio::io::duplex` bridge (`cpt-cf-oagw-algo-pingora-bridge`). Pingora manages connection pooling, TLS termination, and health checks internally. Multi-endpoint upstreams **MUST** distribute requests via `LoadBalancer<RoundRobin>` with `TcpHealthCheck` (10s interval). When `X-OAGW-Target-Host` header is present, the system **MUST** select the matching endpoint explicitly (no round-robin). All endpoints in a pool **MUST** have identical protocol, scheme, and port. `X-OAGW-Target-Host` **MUST** be validated: required for multi-endpoint common-suffix upstreams (400 MissingTargetHost); format must be hostname or IP without port/path/special chars (400 InvalidTargetHost); value must match a configured endpoint (400 UnknownTargetHost). Non-timeout upstream connection failures (refused, DNS, TLS) **MUST** return 503 DownstreamError (canonical `service_unavailable`). WebSocket `Upgrade` requests **MUST** be bridged bidirectionally through the duplex bridge, succeeding with `101 Switching Protocols` when the upstream itself upgrades.

Pingora-level errors are handled by the `fail_to_proxy` callback, which **MUST** convert `pingora_core::ErrorType` variants into `DomainError`, then use the canonical `DomainError` → RFC 9457 `Problem` pipeline. The response **MUST** include `X-OAGW-Error-Source: gateway` and `Content-Type: application/problem+json`.

**Implements**:
- `cpt-cf-oagw-flow-proxy-request`
- `cpt-cf-oagw-algo-pingora-bridge`

**Touches**:
- Entities: `ServerConfig`, `Endpoint`

## 6. Acceptance Criteria

- [x] Proxy requests via `{METHOD} /api/oagw/v1/proxy/{alias}/{path}` are forwarded to the correct upstream and response is returned
- [x] Requests without valid Bearer token return 401; requests without `gts.cf.core.oagw.proxy.v1~:invoke` permission return 403
- [x] Alias resolution walks tenant hierarchy from descendant to root and returns closest match (shadowing)
- [x] Disabled upstreams return 503 LinkUnavailable with `X-OAGW-Error-Source: gateway`
- [x] Private upstreams are not visible to descendant tenants during alias resolution
- [x] Route matching selects the longest path prefix match ordered by priority (lowest number first)
- [x] Unmatched routes return 404 RouteNotFound with `X-OAGW-Error-Source: gateway`
- [x] Plugin chain executes in deterministic order: Auth → Guards → Transform(on_request) → upstream → Transform(on_response/on_error)
- [x] Upstream plugins execute before route plugins in the composed chain
- [x] Auth plugin resolves credentials from `cred_store` via `secret_ref` at request time; failure returns 401 or 500
- [x] Guard plugins reject invalid requests (method, query, path suffix) before upstream forwarding
- [x] Hop-by-hop headers (Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade) are stripped from outbound requests
- [x] `X-OAGW-Target-Host` is consumed for endpoint selection and stripped before forwarding
- [x] `Host` (HTTP/1.1) or `:authority` (HTTP/2) is replaced with upstream endpoint host
- [x] Header set/add/remove operations from upstream/route config are applied in declared order
- [x] Content-Length validation rejects non-integer or mismatched values with 400
- [x] 100MB body limit is enforced before buffering (400 PayloadTooLarge)
- [x] Only `chunked` Transfer-Encoding is accepted; others return 400
- [x] Gateway errors use RFC 9457 Problem Details (`application/problem+json`) with GTS type identifiers
- [x] `X-OAGW-Error-Source: gateway` is set on all gateway-originated errors
- [x] `X-OAGW-Error-Source: upstream` is set on upstream passthrough responses (body passed as-is)
- [x] Multi-endpoint upstreams distribute requests via round-robin
- [x] `X-OAGW-Target-Host` selects specific endpoint in multi-endpoint upstreams
- [x] HTTPS-only constraint is enforced for all upstream connections
- [x] Multi-endpoint upstream with common-suffix alias returns 400 MissingTargetHost when `X-OAGW-Target-Host` is absent
- [x] Invalid `X-OAGW-Target-Host` format (not hostname or IP) returns 400 InvalidTargetHost
- [x] `X-OAGW-Target-Host` value not matching any configured endpoint returns 400 UnknownTargetHost
- [x] Upstream connection failures (refused, DNS, TLS) return 503 DownstreamError (canonical `service_unavailable`) with `X-OAGW-Error-Source: gateway`
- [x] No credentials appear in logs, error messages, or API responses
- [x] Application layer does not add retries; only Pingora's built-in connection-level retry (up to 1 retry on reusable connections) is permitted
- [x] WebSocket upgrade requests (`Upgrade: websocket`) are bridged bidirectionally: `101 Switching Protocols` when the upstream upgrades, otherwise a gateway-fabricated `503 Service Unavailable` (`ProtocolError`) is returned and the upstream's real non-101 status/body are discarded
- [x] Pingora `fail_to_proxy` errors produce RFC 9457 Problem Details body with GTS type identifiers and `X-OAGW-Error-Source: gateway`
- [x] When `X-OAGW-Error-Source` is absent (normal upstream response after `upstream_response_filter` strips `x-oagw-*` headers), `ErrorSource` defaults to `Upstream`; Pingora-generated error responses (`fail_to_proxy`) always set `X-OAGW-Error-Source: gateway` explicitly

## 7. Additional Context

### Performance Considerations

The proxy engine is on the critical path for every outbound API call — less than 10ms added latency at p95 per `cpt-cf-oagw-nfr-low-latency`. Plugin chain execution and header transformation are in-memory operations. Pingora manages upstream connection pooling and TLS session reuse internally, with `TcpHealthCheck` at 10s intervals to avoid routing to unhealthy backends. The in-memory duplex bridge (`tokio::io::duplex`) avoids network overhead between the application layer and Pingora — serialization/deserialization is the only cost. Body validation checks Content-Length before buffering to reject oversized payloads early. For streaming bodies (e.g. SSE uploads), `Connection: close` framing avoids buffering the full body; Pingora reads until the write half shuts down (EOF).

### Security Considerations

SSRF protection is partially addressed in this feature through scheme enforcement (HTTPS-only per `cpt-cf-oagw-constraint-https-only`) and hop-by-hop / routing header stripping. Full IP pinning and DNS validation hardening are addressed in `cpt-cf-oagw-feature-observability`. HTTP smuggling is prevented by strict header parsing: CR/LF rejection, Content-Length / Transfer-Encoding combination validation.

Credential isolation is enforced by resolving secrets from `cred_store` at request time — OAGW never stores or logs secret material per `cpt-cf-oagw-principle-cred-isolation`.

### Deliberate Omissions

<!-- out-of-scope feature reference -->
- [x] `p2` - `cpt-cf-oagw-feature-tenant-hierarchy`

- **States section**: Not applicable — proxy engine is a stateless request-response flow. Circuit breaker state management belongs to `cpt-cf-oagw-feature-rate-limiting`.
- **Multi-tenant hierarchy merge**: Out of scope — hierarchical config override and sharing mode merge strategies belong to cpt-cf-oagw-feature-tenant-hierarchy.
- **Rate limiting enforcement**: Out of scope — rate limiting and circuit breaker during proxy flow belong to `cpt-cf-oagw-feature-rate-limiting`.
- **SSE/WebTransport streaming**: Out of scope — streaming protocol support belongs to `cpt-cf-oagw-feature-streaming`. **WebSocket upgrade is in scope for this feature and is bridged bidirectionally** (see step `inst-proxy-22` above) — it is not rejected; it succeeds end-to-end whenever the upstream itself completes the handshake.
- **Metrics and audit logging**: Out of scope — Prometheus metrics, structured logging, and CORS handling belong to `cpt-cf-oagw-feature-observability`.
- **UX/Accessibility**: Not applicable — OAGW is a backend API gear with no user interface.
- **Compliance/Privacy**: OAGW does not handle PII directly. Credential isolation via `cred_store` references covers data protection. No additional regulatory compliance beyond standard platform requirements.
