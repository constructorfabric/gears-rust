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

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-proxy-request`

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
- Body validation fails (400 ValidationError or 413 PayloadTooLarge)
- Upstream returns error response (502 DownstreamError passthrough)
- Upstream connection or request times out (504 ConnectionTimeout / RequestTimeout)
- WebSocket upgrade requested (501 ProtocolError — not supported by unidirectional bridge)
- Pingora-level protocol error (502 ProtocolError — e.g. HTTP/2 downgrade failure)
- X-OAGW-Target-Host missing for multi-endpoint common-suffix upstream (400 MissingTargetHost)
- X-OAGW-Target-Host format invalid (400 InvalidTargetHost)
- X-OAGW-Target-Host does not match any configured endpoint (400 UnknownTargetHost)
- Upstream connection fails at network level (502 DownstreamError)

**Steps**:
1. [ ] - `p1` - Actor sends `{METHOD} /api/oagw/v1/proxy/{alias}[/{path}][?{query}]` - `inst-proxy-1`
2. [ ] - `p1` - API: Extract `SecurityContext` (tenant_id, principal_id, permissions) from Bearer token - `inst-proxy-2`
3. [ ] - `p1` - **IF** token missing or invalid - `inst-proxy-3`
   1. [ ] - `p1` - **RETURN** 401 Unauthorized with `X-OAGW-Error-Source: gateway` - `inst-proxy-3a`
4. [ ] - `p1` - **IF** token lacks `gts.x.core.oagw.proxy.v1~:invoke` permission - `inst-proxy-4`
   1. [ ] - `p1` - **RETURN** 403 Forbidden with `X-OAGW-Error-Source: gateway` - `inst-proxy-4a`
5. [ ] - `p1` - Invoke `DataPlaneService.execute_proxy(alias, path, method, headers, body, security_context)` - `inst-proxy-5`
6. [ ] - `p1` - Resolve upstream by alias via `cpt-cf-oagw-algo-alias-resolution` - `inst-proxy-6`
7. [ ] - `p1` - **IF** upstream not found - `inst-proxy-7`
   1. [ ] - `p1` - **RETURN** 404 RouteNotFound with `X-OAGW-Error-Source: gateway` - `inst-proxy-7a`
8. [ ] - `p1` - **IF** upstream disabled - `inst-proxy-8`
   1. [ ] - `p1` - **RETURN** 503 LinkUnavailable with `X-OAGW-Error-Source: gateway` - `inst-proxy-8a`
9. [ ] - `p1` - Match route via `cpt-cf-oagw-algo-route-matching` - `inst-proxy-9`
10. [ ] - `p1` - **IF** no matching route - `inst-proxy-10`
    1. [ ] - `p1` - **RETURN** 404 RouteNotFound with `X-OAGW-Error-Source: gateway` - `inst-proxy-10a`
11. [ ] - `p1` - Validate request body via `cpt-cf-oagw-algo-body-validation` - `inst-proxy-11`
12. [ ] - `p1` - **IF** body validation fails - `inst-proxy-12`
    1. [ ] - `p1` - **RETURN** 400 ValidationError or 413 PayloadTooLarge with `X-OAGW-Error-Source: gateway` - `inst-proxy-12a`
13. [ ] - `p1` - Compose plugin chain via `cpt-cf-oagw-algo-plugin-chain-execution` - `inst-proxy-13`
14. [ ] - `p1` - Execute auth plugin: inject credentials into outbound request - `inst-proxy-14`
15. [ ] - `p1` - **IF** auth plugin fails (secret not found, credential error) - `inst-proxy-15`
    1. [ ] - `p1` - **RETURN** 401 AuthenticationFailed or 500 SecretNotFound with `X-OAGW-Error-Source: gateway` - `inst-proxy-15a`
16. [ ] - `p1` - Execute guard plugins: validate method, query params, path suffix, CORS - `inst-proxy-16`
17. [ ] - `p1` - **IF** any guard rejects - `inst-proxy-17`
    1. [ ] - `p1` - **RETURN** guard-specific error code with `X-OAGW-Error-Source: gateway` - `inst-proxy-17a`
18. [ ] - `p1` - Execute transform plugins: `on_request` phase — mutate outbound request - `inst-proxy-18`
19. [ ] - `p1` - Apply header transformation via `cpt-cf-oagw-algo-header-transformation` - `inst-proxy-19`
20. [ ] - `p1` - Select target endpoint via `X-OAGW-Target-Host` header or round-robin - `inst-proxy-20`
21. [ ] - `p1` - **IF** multi-endpoint upstream with common-suffix alias AND `X-OAGW-Target-Host` header missing - `inst-proxy-21`
    1. [ ] - `p1` - **RETURN** 400 MissingTargetHost with `X-OAGW-Error-Source: gateway` - `inst-proxy-21a`
22. [ ] - `p1` - **IF** `X-OAGW-Target-Host` present AND format invalid (not hostname or IP; contains port, path, or special chars) - `inst-proxy-22`
    1. [ ] - `p1` - **RETURN** 400 InvalidTargetHost with `X-OAGW-Error-Source: gateway` - `inst-proxy-22a`
23. [ ] - `p1` - **IF** `X-OAGW-Target-Host` present AND value does not match any configured endpoint host - `inst-proxy-23`
    1. [ ] - `p1` - **RETURN** 400 UnknownTargetHost with `X-OAGW-Error-Source: gateway` - `inst-proxy-23a`
24. [ ] - `p1` - **IF** request contains `Upgrade: websocket` header - `inst-proxy-24`
    1. [ ] - `p1` - **RETURN** 501 ProtocolError with `X-OAGW-Error-Source: gateway` (WebSocket requires bidirectional tunnel; current bridge is unidirectional) - `inst-proxy-24a`
25. [ ] - `p1` - Build outbound HTTP request: set target URL (scheme + host + port + path), method, headers, body - `inst-proxy-25`
26. [ ] - `p1` - Serialize request into in-memory duplex stream and forward to Pingora `ProxyHttp` engine via `cpt-cf-oagw-algo-pingora-bridge` - `inst-proxy-26`
27. [ ] - `p1` - **IF** Pingora reports upstream connection failure (refused, DNS, TLS) via `fail_to_proxy` - `inst-proxy-27`
    1. [ ] - `p1` - Map Pingora `ErrorType` to `DomainError` and write RFC 9457 Problem response with `X-OAGW-Error-Source: gateway` - `inst-proxy-27a`
    2. [ ] - `p1` - **RETURN** 502 DownstreamError with `X-OAGW-Error-Source: gateway` - `inst-proxy-27b`
28. [ ] - `p1` - **IF** connection or request timeout (Pingora `ConnectTimedout`, `ReadTimedout`, `WriteTimedout`) - `inst-proxy-28`
    1. [ ] - `p1` - **RETURN** 504 ConnectionTimeout or RequestTimeout via `cpt-cf-oagw-algo-error-source-classification` - `inst-proxy-28a`
29. [ ] - `p1` - **IF** Pingora reports HTTP/2 error (`H2Error`, `H2Downgrade`) - `inst-proxy-29`
    1. [ ] - `p1` - **RETURN** 502 ProtocolError with `X-OAGW-Error-Source: gateway` - `inst-proxy-29a`
30. [ ] - `p1` - Parse upstream response from duplex stream read half - `inst-proxy-30`
31. [ ] - `p1` - **IF** upstream returns error response - `inst-proxy-31`
    1. [ ] - `p1` - Execute transform plugins: `on_error` phase - `inst-proxy-31a`
    2. [ ] - `p1` - **RETURN** upstream response as-is with `X-OAGW-Error-Source: upstream` - `inst-proxy-31b`
32. [ ] - `p1` - Execute transform plugins: `on_response` phase - `inst-proxy-32`
33. [ ] - `p1` - **RETURN** transformed response with `X-OAGW-Error-Source: upstream` - `inst-proxy-33`

## 3. Processes / Business Logic (CDSL)

### Alias Resolution

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-alias-resolution`

**Input**: Alias string, tenant_id from SecurityContext

**Output**: Resolved `UpstreamConfig` or error

**Steps**:
1. [ ] - `p1` - DB: SELECT upstream FROM oagw_upstream WHERE alias = :alias AND tenant_id = :tenant_id AND enabled = true - `inst-alias-1`
2. [ ] - `p1` - **IF** upstream found in current tenant - `inst-alias-2`
   1. [ ] - `p1` - **RETURN** resolved UpstreamConfig - `inst-alias-2a`
3. [ ] - `p1` - Walk tenant hierarchy from current tenant toward root (closest ancestor first) - `inst-alias-3`
4. [ ] - `p1` - **FOR EACH** ancestor tenant_id in hierarchy - `inst-alias-4`
   1. [ ] - `p1` - DB: SELECT upstream FROM oagw_upstream WHERE alias = :alias AND tenant_id = :ancestor_id AND sharing != 'private' - `inst-alias-4a`
   2. [ ] - `p1` - **IF** upstream found - `inst-alias-4b`
      1. [ ] - `p1` - **IF** upstream disabled (enabled = false) - `inst-alias-4b1`
         1. [ ] - `p1` - **RETURN** error: upstream disabled (503 LinkUnavailable) - `inst-alias-4b1a`
      2. [ ] - `p1` - **RETURN** resolved UpstreamConfig (closest match wins — shadowing) - `inst-alias-4b2`
5. [ ] - `p1` - **RETURN** error: upstream not found for alias (404 RouteNotFound) - `inst-alias-5`

### Route Matching

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-route-matching`

**Input**: upstream_id, HTTP method, request path

**Output**: Matched `RouteConfig` or error

**Steps**:
1. [ ] - `p1` - DB: SELECT routes FROM oagw_route WHERE upstream_id = :upstream_id AND enabled = true - `inst-route-1`
2. [ ] - `p1` - Filter routes by match type for the request protocol - `inst-route-2`
3. [ ] - `p1` - **FOR EACH** route ordered by path prefix length (longest first), then priority (lowest number first) - `inst-route-3`
   1. [ ] - `p1` - **IF** route.match_type = 'http' - `inst-route-3a`
      1. [ ] - `p1` - DB: Check method in oagw_route_method WHERE route_id = :route_id - `inst-route-3a1`
      2. [ ] - `p1` - DB: Check request path starts with path_prefix from oagw_route_http_match - `inst-route-3a2`
      3. [ ] - `p1` - **IF** method allowed AND path matches prefix - `inst-route-3a3`
         1. [ ] - `p1` - **RETURN** RouteConfig (first match wins — longest prefix + highest priority) - `inst-route-3a3a`
   2. [ ] - `p1` - **IF** route.match_type = 'grpc' - `inst-route-3b`
      1. [ ] - `p1` - DB: Check gRPC service and method from oagw_route_grpc_match - `inst-route-3b1`
      2. [ ] - `p1` - **IF** service and method match - `inst-route-3b2`
         1. [ ] - `p1` - **RETURN** RouteConfig - `inst-route-3b2a`
4. [ ] - `p1` - **RETURN** error: no matching route found (404 RouteNotFound) - `inst-route-4`

### Plugin Chain Execution

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-plugin-chain-execution`

**Input**: Upstream plugin bindings, route plugin bindings, request context, auth plugin reference from upstream

**Output**: Processed request ready for forwarding, or rejection error

**Steps**:
1. [ ] - `p1` - Load upstream plugin bindings ordered by position from oagw_upstream_plugin - `inst-chain-1`
2. [ ] - `p1` - Load route plugin bindings ordered by position from oagw_route_plugin - `inst-chain-2`
3. [ ] - `p1` - Compose ordered chain: `[upstream_plugins...] + [route_plugins...]` (upstream-before-route) - `inst-chain-3`
4. [ ] - `p1` - **FOR EACH** plugin_ref in composed chain - `inst-chain-4`
   1. [ ] - `p1` - Parse GTS identifier: extract instance part (after `~`) - `inst-chain-4a`
   2. [ ] - `p1` - **IF** instance parses as UUID - `inst-chain-4b`
      1. [ ] - `p1` - DB: SELECT plugin FROM oagw_plugin WHERE id = :uuid — must exist and match schema type - `inst-chain-4b1`
   3. [ ] - `p1` - **ELSE** (named plugin) - `inst-chain-4c`
      1. [ ] - `p1` - Resolve via in-process plugin registry - `inst-chain-4c1`
   4. [ ] - `p1` - **IF** plugin not found - `inst-chain-4d`
      1. [ ] - `p1` - **RETURN** 503 PluginNotFound with `X-OAGW-Error-Source: gateway` - `inst-chain-4d1`
5. [ ] - `p1` - Resolve upstream auth plugin from auth_plugin_ref / auth_plugin_uuid columns - `inst-chain-5`
6. [ ] - `p1` - Execute auth plugin: resolve credentials from `cred_store` via secret_ref, inject into request - `inst-chain-6`
7. [ ] - `p1` - **IF** secret not found or credential resolution fails - `inst-chain-7`
   1. [ ] - `p1` - **RETURN** 401 AuthenticationFailed or 500 SecretNotFound - `inst-chain-7a`
8. [ ] - `p1` - **FOR EACH** guard plugin in chain (type = guard) - `inst-chain-8`
   1. [ ] - `p1` - Execute guard: validate request against guard rules (method allowlist, query allowlist, path suffix, timeout) - `inst-chain-8a`
   2. [ ] - `p1` - **IF** guard rejects - `inst-chain-8b`
      1. [ ] - `p1` - **RETURN** guard-specific rejection error - `inst-chain-8b1`
9. [ ] - `p1` - **FOR EACH** transform plugin in chain (type = transform, phase = on_request) - `inst-chain-9`
   1. [ ] - `p1` - Execute transform: mutate request headers, body, query as configured - `inst-chain-9a`
10. [ ] - `p1` - **RETURN** processed request ready for upstream forwarding - `inst-chain-10`

### Header Transformation

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-header-transformation`

**Input**: Inbound request headers, upstream `HeadersConfig`, route `HeadersConfig`

**Output**: Transformed outbound headers

**Steps**:
1. [ ] - `p1` - Strip hop-by-hop headers: Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade - `inst-header-1`
2. [ ] - `p1` - Strip routing header: consume `X-OAGW-Target-Host` value for endpoint selection, then remove - `inst-header-2`
3. [ ] - `p1` - Replace `Host` header with upstream endpoint host (HTTP/1.1) or `:authority` pseudo-header with upstream authority (HTTP/2) - `inst-header-3`
4. [ ] - `p1` - Apply upstream `headers` config: execute set/add/remove operations in declared order - `inst-header-4`
5. [ ] - `p1` - Apply route `headers` config (if any): execute set/add/remove operations in declared order - `inst-header-5`
6. [ ] - `p1` - Validate well-known headers: Content-Length (valid integer), Content-Type (recognized format) — reject with 400 if invalid - `inst-header-6`
7. [ ] - `p1` - **RETURN** transformed header set - `inst-header-7`

### Body Validation

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-body-validation`

**Input**: Request body stream, Content-Length header value, Transfer-Encoding header value

**Output**: Validation pass or rejection error

**Steps**:
1. [ ] - `p1` - **IF** Content-Length header present - `inst-body-1`
   1. [ ] - `p1` - **IF** Content-Length is not a valid non-negative integer - `inst-body-1a`
      1. [ ] - `p1` - **RETURN** 400 ValidationError - `inst-body-1a1`
   2. [ ] - `p1` - **IF** Content-Length exceeds 100MB (104,857,600 bytes) - `inst-body-1b`
      1. [ ] - `p1` - **RETURN** 413 PayloadTooLarge (reject before buffering per `cpt-cf-oagw-constraint-body-limit`) - `inst-body-1b1`
2. [ ] - `p1` - **IF** Transfer-Encoding header present - `inst-body-2`
   1. [ ] - `p1` - **IF** encoding is not `chunked` - `inst-body-2a`
      1. [ ] - `p1` - **RETURN** 400 ValidationError (only `chunked` supported) - `inst-body-2a1`
3. [ ] - `p1` - **IF** actual body size exceeds 100MB during streaming read - `inst-body-3`
   1. [ ] - `p1` - **RETURN** 413 PayloadTooLarge (abort read) - `inst-body-3a`
4. [ ] - `p1` - **RETURN** validation passed - `inst-body-4`

### Pingora In-Memory Bridge

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-pingora-bridge`

**Input**: Serialized HTTP/1.1 request (headers + body), selected endpoint

**Output**: Upstream HTTP response or Pingora error

**Steps**:
1. [ ] - `p1` - Inject internal metadata headers (`x-oagw-internal-*`) for Pingora endpoint selection into outbound headers - `inst-bridge-1`
2. [ ] - `p1` - Create in-memory `tokio::io::duplex` stream pair (write half, read half) - `inst-bridge-2`
3. [ ] - `p1` - Serialize HTTP/1.1 request headers (including internal metadata) into write half - `inst-bridge-3`
4. [ ] - `p1` - **IF** request has body AND Content-Length known - `inst-bridge-4`
   1. [ ] - `p1` - Write full body with Content-Length framing - `inst-bridge-4a`
5. [ ] - `p1` - **ELSE IF** request has streaming body - `inst-bridge-5`
   1. [ ] - `p1` - Write headers only, then stream body chunks; body boundary is `Connection: close` (EOF on write-half shutdown) - `inst-bridge-5a`
6. [ ] - `p1` - Feed duplex read half to Pingora `http_proxy_service_with_name` as client session - `inst-bridge-6`
7. [ ] - `p1` - Pingora `ProxyHttp` implementation resolves `HttpPeer` from internal headers, connects to upstream, forwards request, streams response back into duplex write half - `inst-bridge-7`
8. [ ] - `p1` - **IF** Pingora encounters upstream error - `inst-bridge-8`
   1. [ ] - `p1` - `fail_to_proxy` converts `pingora_core::ErrorType` → `DomainError` → RFC 9457 `Problem` and writes directly to session - `inst-bridge-8a`
9. [ ] - `p1` - Parse HTTP/1.1 response from duplex read half (status, headers, body) - `inst-bridge-9`
10. [ ] - `p1` - **RETURN** parsed `http::Response<Body>` - `inst-bridge-10`

### Error Source Classification

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-error-source-classification`

**Input**: Error origin context (gateway processing step or upstream HTTP response)

**Output**: Error response with `X-OAGW-Error-Source` header and appropriate body format

**Steps**:
1. [ ] - `p1` - **IF** error originated during gateway processing (alias resolution, route matching, auth, guard, body validation, plugin resolution, endpoint selection, connection failure, timeout) - `inst-errsrc-1`
   1. [ ] - `p1` - Set response header `X-OAGW-Error-Source: gateway` - `inst-errsrc-1a`
   2. [ ] - `p1` - Format response body as RFC 9457 Problem Details (`application/problem+json`) with GTS type identifier per `cpt-cf-oagw-principle-rfc9457` - `inst-errsrc-1b`
   3. [ ] - `p1` - Include standard fields: `type` (GTS ID), `title`, `status`, `detail`, `instance` - `inst-errsrc-1c`
   4. [ ] - `p1` - Include extension fields where applicable: `upstream_id`, `host`, `path`, `retry_after_seconds`, `trace_id` - `inst-errsrc-1d`
2. [ ] - `p1` - **IF** error originated from upstream service HTTP response - `inst-errsrc-2`
   1. [ ] - `p1` - Set response header `X-OAGW-Error-Source: upstream` - `inst-errsrc-2a`
   2. [ ] - `p1` - Pass through upstream response body as-is (no rewriting, preserve original content-type) - `inst-errsrc-2b`
3. [ ] - `p1` - **RETURN** classified error response - `inst-errsrc-3`

## 4. States (CDSL)

Not applicable. The proxy engine is a stateless request-response flow — each proxy request is independent and does not persist gateway-side state between invocations. Circuit breaker state management is out of scope for this feature (see `cpt-cf-oagw-feature-rate-limiting`).

## 5. Definitions of Done

### Implement DataPlaneService Proxy Execution

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-proxy-execution`

The system **MUST** implement `DataPlaneService::proxy_request(...)` that orchestrates the full proxy flow: alias resolution, route matching, body validation, plugin chain execution, HTTP forwarding via the Pingora in-memory bridge, and response transformation. No upstream response caching per `cpt-cf-oagw-principle-no-cache`. Pingora's stale pooled-connection reconnect (re-establishing a connection that was closed server-side before request bytes are sent) is permitted; all other failures **MUST** be returned immediately without additional application-layer retries per `cpt-cf-oagw-principle-no-retry`.

**Implements**:
- `cpt-cf-oagw-flow-proxy-request`
- `cpt-cf-oagw-algo-error-source-classification`

**Touches**:
- API: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}`
- Entities: `Upstream`, `Route`, `Plugin`, `ServerConfig`, `Endpoint`

### Implement Alias Resolution

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-alias-resolution`

The system **MUST** resolve upstreams by `(tenant_id, alias)` with tenant hierarchy walk from descendant to root. The closest match wins (shadowing). Disabled upstreams **MUST** return 503 LinkUnavailable. Private upstreams (sharing = `private`) **MUST NOT** be visible to descendant tenants.

**Implements**:
- `cpt-cf-oagw-algo-alias-resolution`

**Touches**:
- DB: `oagw_upstream`
- Entities: `Upstream`

### Implement Route Matching

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-route-matching`

The system **MUST** match inbound requests to routes by HTTP method allowlist and longest path prefix, ordered by priority (lowest number wins). Only enabled routes are considered. gRPC routes match by `(service, method)` via `oagw_route_grpc_match`. No two enabled routes under the same upstream may share `(path_prefix, priority)` for the same method.

**Implements**:
- `cpt-cf-oagw-algo-route-matching`

**Touches**:
- DB: `oagw_route`, `oagw_route_http_match`, `oagw_route_grpc_match`, `oagw_route_method`
- Entities: `Route`

### Implement Plugin Chain Execution

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-plugin-chain`

The system **MUST** execute the plugin chain in deterministic order: Auth → Guards → Transform(on_request) → upstream call → Transform(on_response/on_error). Upstream plugins **MUST** execute before route plugins (`[U1, U2] + [R1, R2]`). Plugins **MUST** be resolved via GTS identifier: UUID-backed plugins from `oagw_plugin`, named plugins from in-process registry. Auth plugin credentials **MUST** be resolved from `cred_store` via `secret_ref` at request time per `cpt-cf-oagw-principle-cred-isolation`.

**Implements**:
- `cpt-cf-oagw-algo-plugin-chain-execution`

**Touches**:
- DB: `oagw_upstream_plugin`, `oagw_route_plugin`, `oagw_plugin`
- Entities: `Plugin`

### Implement Header Transformation

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-header-transformation`

The system **MUST** strip hop-by-hop headers (Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade), strip `X-OAGW-Target-Host` after consuming for routing, replace `Host` (HTTP/1.1) or `:authority` (HTTP/2) with upstream host, and apply upstream/route header configs with set/add/remove operations. Well-known headers (Content-Length, Content-Type) **MUST** be validated; invalid headers **MUST** result in 400 Bad Request.

**Implements**:
- `cpt-cf-oagw-algo-header-transformation`

**Touches**:
- Entities: `Upstream` (headers config), `Route` (headers config)

### Implement Body Validation

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-body-validation`

The system **MUST** validate Content-Length (valid non-negative integer), enforce 100MB hard limit (reject before buffering per `cpt-cf-oagw-constraint-body-limit`), and reject unsupported Transfer-Encoding (only `chunked` supported). Invalid requests **MUST** return 400 ValidationError or 413 PayloadTooLarge.

**Implements**:
- `cpt-cf-oagw-algo-body-validation`

**Touches**:
- API: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}`

### Implement Error Source Distinction

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-error-source-distinction`

The system **MUST** set `X-OAGW-Error-Source: gateway` on all gateway-originated errors and `X-OAGW-Error-Source: upstream` on upstream passthrough errors per `cpt-cf-oagw-principle-error-source`. Gateway errors **MUST** use RFC 9457 Problem Details format (`application/problem+json`) with GTS type identifiers per `cpt-cf-oagw-principle-rfc9457`. Upstream error responses **MUST** be passed through as-is without body rewriting.

**Implements**:
- `cpt-cf-oagw-algo-error-source-classification`

**Touches**:
- API: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}`

### Implement Pingora Proxy Engine

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-pingora-proxy`

The system **MUST** use Pingora (`pingora-proxy`, `pingora-load-balancing`) as the upstream HTTP engine, connected via an in-memory `tokio::io::duplex` bridge (`cpt-cf-oagw-algo-pingora-bridge`). Pingora manages connection pooling, TLS termination, and health checks internally. Multi-endpoint upstreams **MUST** distribute requests via `LoadBalancer<RoundRobin>` with `TcpHealthCheck` (10s interval). When `X-OAGW-Target-Host` header is present, the system **MUST** select the matching endpoint explicitly (no round-robin). All endpoints in a pool **MUST** have identical protocol, scheme, and port. `X-OAGW-Target-Host` **MUST** be validated: required for multi-endpoint common-suffix upstreams (400 MissingTargetHost); format must be hostname or IP without port/path/special chars (400 InvalidTargetHost); value must match a configured endpoint (400 UnknownTargetHost). Non-timeout upstream connection failures (refused, DNS, TLS) **MUST** return 502 DownstreamError. WebSocket `Upgrade` requests **MUST** be rejected with 501 ProtocolError before reaching the bridge (the duplex bridge is unidirectional and cannot support the bidirectional tunnel WebSocket requires).

Pingora-level errors are handled by the `fail_to_proxy` callback, which **MUST** convert `pingora_core::ErrorType` variants into `DomainError`, then use the canonical `DomainError` → RFC 9457 `Problem` pipeline. The response **MUST** include `X-OAGW-Error-Source: gateway` and `Content-Type: application/problem+json`.

**Implements**:
- `cpt-cf-oagw-flow-proxy-request`
- `cpt-cf-oagw-algo-pingora-bridge`

**Touches**:
- Entities: `ServerConfig`, `Endpoint`

## 6. Acceptance Criteria

- [ ] Proxy requests via `{METHOD} /api/oagw/v1/proxy/{alias}/{path}` are forwarded to the correct upstream and response is returned
- [ ] Requests without valid Bearer token return 401; requests without `gts.x.core.oagw.proxy.v1~:invoke` permission return 403
- [ ] Alias resolution walks tenant hierarchy from descendant to root and returns closest match (shadowing)
- [ ] Disabled upstreams return 503 LinkUnavailable with `X-OAGW-Error-Source: gateway`
- [ ] Private upstreams are not visible to descendant tenants during alias resolution
- [ ] Route matching selects the longest path prefix match ordered by priority (lowest number first)
- [ ] Unmatched routes return 404 RouteNotFound with `X-OAGW-Error-Source: gateway`
- [ ] Plugin chain executes in deterministic order: Auth → Guards → Transform(on_request) → upstream → Transform(on_response/on_error)
- [ ] Upstream plugins execute before route plugins in the composed chain
- [ ] Auth plugin resolves credentials from `cred_store` via `secret_ref` at request time; failure returns 401 or 500
- [ ] Guard plugins reject invalid requests (method, query, path suffix) before upstream forwarding
- [ ] Hop-by-hop headers (Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade) are stripped from outbound requests
- [ ] `X-OAGW-Target-Host` is consumed for endpoint selection and stripped before forwarding
- [ ] `Host` (HTTP/1.1) or `:authority` (HTTP/2) is replaced with upstream endpoint host
- [ ] Header set/add/remove operations from upstream/route config are applied in declared order
- [ ] Content-Length validation rejects non-integer or mismatched values with 400
- [ ] 100MB body limit is enforced before buffering (413 PayloadTooLarge)
- [ ] Only `chunked` Transfer-Encoding is accepted; others return 400
- [ ] Gateway errors use RFC 9457 Problem Details (`application/problem+json`) with GTS type identifiers
- [ ] `X-OAGW-Error-Source: gateway` is set on all gateway-originated errors
- [ ] `X-OAGW-Error-Source: upstream` is set on upstream passthrough responses (body passed as-is)
- [ ] Multi-endpoint upstreams distribute requests via round-robin
- [ ] `X-OAGW-Target-Host` selects specific endpoint in multi-endpoint upstreams
- [ ] HTTPS-only constraint is enforced for all upstream connections
- [ ] Multi-endpoint upstream with common-suffix alias returns 400 MissingTargetHost when `X-OAGW-Target-Host` is absent
- [ ] Invalid `X-OAGW-Target-Host` format (not hostname or IP) returns 400 InvalidTargetHost
- [ ] `X-OAGW-Target-Host` value not matching any configured endpoint returns 400 UnknownTargetHost
- [ ] Upstream connection failures (refused, DNS, TLS) return 502 DownstreamError with `X-OAGW-Error-Source: gateway`
- [ ] No credentials appear in logs, error messages, or API responses
- [ ] Application layer does not add retries; only Pingora's built-in connection-level retry (up to 1 retry on reusable connections) is permitted
- [ ] WebSocket upgrade requests (`Upgrade: websocket`) are rejected with 501 ProtocolError and `X-OAGW-Error-Source: gateway`
- [ ] Pingora `fail_to_proxy` errors produce RFC 9457 Problem Details body with GTS type identifiers and `X-OAGW-Error-Source: gateway`
- [ ] When `X-OAGW-Error-Source` is absent (normal upstream response after `upstream_response_filter` strips `x-oagw-*` headers), `ErrorSource` defaults to `Upstream`; Pingora-generated error responses (`fail_to_proxy`) always set `X-OAGW-Error-Source: gateway` explicitly

## 7. Additional Context

### Performance Considerations

The proxy engine is on the critical path for every outbound API call — less than 10ms added latency at p95 per `cpt-cf-oagw-nfr-low-latency`. Plugin chain execution and header transformation are in-memory operations. Pingora manages upstream connection pooling and TLS session reuse internally, with `TcpHealthCheck` at 10s intervals to avoid routing to unhealthy backends. The in-memory duplex bridge (`tokio::io::duplex`) avoids network overhead between the application layer and Pingora — serialization/deserialization is the only cost. Body validation checks Content-Length before buffering to reject oversized payloads early. For streaming bodies (e.g. SSE uploads), `Connection: close` framing avoids buffering the full body; Pingora reads until the write half shuts down (EOF).

### Security Considerations

SSRF protection is partially addressed in this feature through scheme enforcement (HTTPS-only per `cpt-cf-oagw-constraint-https-only`) and hop-by-hop / routing header stripping. Full IP pinning and DNS validation hardening are addressed in `cpt-cf-oagw-feature-observability`. HTTP smuggling is prevented by strict header parsing: CR/LF rejection, Content-Length / Transfer-Encoding combination validation.

Credential isolation is enforced by resolving secrets from `cred_store` at request time — OAGW never stores or logs secret material per `cpt-cf-oagw-principle-cred-isolation`.

### Deliberate Omissions

- **States section**: Not applicable — proxy engine is a stateless request-response flow. Circuit breaker state management belongs to `cpt-cf-oagw-feature-rate-limiting`.
- **Multi-tenant hierarchy merge**: Out of scope — hierarchical config override and sharing mode merge strategies belong to `cpt-cf-oagw-feature-tenant-hierarchy`.
- **Rate limiting enforcement**: Out of scope — rate limiting and circuit breaker during proxy flow belong to `cpt-cf-oagw-feature-rate-limiting`.
- **SSE/WebTransport streaming**: Out of scope — streaming protocol support belongs to `cpt-cf-oagw-feature-streaming`. **WebSocket upgrade requests are explicitly rejected** with 501 ProtocolError in this feature because the unidirectional duplex bridge cannot support the bidirectional tunnel WebSocket requires.
- **Metrics and audit logging**: Out of scope — Prometheus metrics, structured logging, and CORS handling belong to `cpt-cf-oagw-feature-observability`.
- **UX/Accessibility**: Not applicable — OAGW is a backend API module with no user interface.
- **Compliance/Privacy**: OAGW does not handle PII directly. Credential isolation via `cred_store` references covers data protection. No additional regulatory compliance beyond standard platform requirements.
