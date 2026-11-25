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
  - [Credential Resolution](#credential-resolution)
  - [Route Matching](#route-matching)
  - [Plugin Chain Execution](#plugin-chain-execution)
  - [Header Transformation](#header-transformation)
  - [Body Validation](#body-validation)
  - [Upstream Forwarding](#upstream-forwarding)
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
  - [Implement Upstream Proxy Engine](#implement-upstream-proxy-engine)
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

Implement the Data Plane proxy execution flow: resolve upstream by alias, match route, execute plugin chain, forward HTTP request through the `toolkit-http-proxy` upstream engine, transform response, and return with error source distinction.

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
- **ADR**: [ADR/0020-proxy-config-cache.md](../ADR/0020-proxy-config-cache.md) — proxy configuration cache with a database; [ADR/0023-request-signing.md](../ADR/0023-request-signing.md) — auth plugin signing phase on the final request
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
- Upstream disabled (503 UpstreamDisabled, reason `UPSTREAM_DISABLED`, `Retry-After: 300`)
- Route not matched for method/path (404 RouteNotFound)
- Auth plugin fails credential injection (one mapping for all plugins: 401 `AUTH_CREDENTIAL_NOT_FOUND` for a missing or inaccessible secret, 503 `CREDENTIAL_STORE_UNAVAILABLE` or `IDENTITY_PROVIDER_UNAVAILABLE`, 502 for an invalid identity provider response, 500 for a misconfiguration, 401 AuthenticationFailed otherwise)
- Guard plugin rejects request (4xx per guard rule)
- Body validation fails (400 ValidationError or 413 PayloadTooLarge)
- Upstream returns error response (passed through with its status, body and headers, `Retry-After` and `WWW-Authenticate` included; `X-OAGW-Error-Source: upstream`)
- Upstream connection or request times out (504 ConnectionTimeout / RequestTimeout)
- Upstream rejects a WebSocket upgrade (502 ProtocolError — no `101 Switching Protocols`)
- Upstream HTTP/2 protocol error (502 ProtocolError)
- X-OAGW-Target-Host missing for multi-endpoint common-suffix upstream (400 MissingTargetHost)
- X-OAGW-Target-Host format invalid (400 InvalidTargetHost)
- X-OAGW-Target-Host does not match any configured endpoint (400 UnknownTargetHost)
- Upstream connection fails at network level (502 DownstreamError or 503 LinkUnavailable, reason `REQUEST_NOT_SENT`; a connection lost after sending is 502 with `REQUEST_MAY_HAVE_BEEN_SENT`)

**Steps**:
1. [x] - `p1` - Actor sends `{METHOD} /api/oagw/v1/proxy/{alias}[/{path}][?{query}]` - `inst-proxy-1`
2. [x] - `p1` - API: Extract `SecurityContext` (tenant_id, principal_id, permissions) from Bearer token - `inst-proxy-2`
3. [x] - `p1` - **IF** token missing or invalid - `inst-proxy-3`
   1. [x] - `p1` - **RETURN** 401 Unauthorized with `X-OAGW-Error-Source: gateway` - `inst-proxy-3a`
4. [x] - `p1` - **IF** token lacks `gts.cf.core.oagw.proxy.v1~:invoke` permission - `inst-proxy-4`
   1. [x] - `p1` - **RETURN** 403 Forbidden with `X-OAGW-Error-Source: gateway` - `inst-proxy-4a`
5. [x] - `p1` - Invoke `DataPlaneService.execute_proxy(alias, path, method, headers, body, security_context)` - `inst-proxy-5`
6. [x] - `p1` - Resolve upstream by alias via `cpt-cf-oagw-algo-alias-resolution` - `inst-proxy-6`
7. [x] - `p1` - **IF** upstream not found - `inst-proxy-7`
   1. [x] - `p1` - **RETURN** 404 RouteNotFound with `X-OAGW-Error-Source: gateway` - `inst-proxy-7a`
8. [x] - `p1` - **IF** upstream disabled - `inst-proxy-8`
   1. [x] - `p1` - **RETURN** 503 `service_unavailable` with reason `UPSTREAM_DISABLED`, `retry_after_seconds: 300` and `Retry-After: 300` (only an operator can enable it again), with `X-OAGW-Error-Source: gateway` - `inst-proxy-8a`
9. [x] - `p1` - Match route via `cpt-cf-oagw-algo-route-matching` - `inst-proxy-9`
10. [x] - `p1` - **IF** no matching route - `inst-proxy-10`
    1. [x] - `p1` - **RETURN** 404 RouteNotFound with `X-OAGW-Error-Source: gateway` - `inst-proxy-10a`
11. [x] - `p1` - Validate request body via `cpt-cf-oagw-algo-body-validation` - `inst-proxy-11`
12. [x] - `p1` - **IF** body validation fails - `inst-proxy-12`
    1. [x] - `p1` - **RETURN** 400 ValidationError or 413 PayloadTooLarge with `X-OAGW-Error-Source: gateway` - `inst-proxy-12a`
13. [x] - `p1` - Compose plugin chain via `cpt-cf-oagw-algo-plugin-chain-execution` - `inst-proxy-13`
14. [x] - `p1` - **IF** an endpoint of the upstream is `http` and the gear option `allow_http_upstream` is off (a row stored before the write-time check) - `inst-proxy-13b`
    1. [x] - `p1` - **RETURN** 400 `invalid_argument`, field violation on `server.endpoints.scheme` with reason `HTTP_UPSTREAM_FORBIDDEN` and `X-OAGW-Error-Source: gateway`, before any auth plugin runs, so no credential is prepared for a plaintext upstream - `inst-proxy-13b1`
15. [x] - `p1` - Execute auth plugin: inject credentials into outbound request - `inst-proxy-14`
16. [x] - `p1` - **IF** auth plugin fails (secret not found, credential error) - `inst-proxy-15`
    1. [x] - `p1` - **RETURN** the credential-failure mapping (DESIGN "Credential Failures"): 401 `AUTH_CREDENTIAL_NOT_FOUND` when the secret is missing or not accessible, 503 `CREDENTIAL_STORE_UNAVAILABLE` with the store's retry hint when the credential store is unavailable, 503 `IDENTITY_PROVIDER_UNAVAILABLE` when the identity provider's token endpoint is unreachable or answers 408, 429 or 5xx, 502 when the identity provider answers with an invalid response, 500 for a plugin misconfiguration, 401 `AUTH_PLUGIN_FAILED` / `AUTH_PLUGIN_INTERNAL` otherwise, with `X-OAGW-Error-Source: gateway`. The error and the server log name the auth config key (`auth.config.secret_ref`, `auth.config.client_id_ref`, `auth.config.client_secret_ref`), never the reference or the credential store's own text, which may quote it - `inst-proxy-15a`
17. [x] - `p1` - Execute guard plugins: validate method, query params (names decoded as `application/x-www-form-urlencoded`), path suffix, CORS origin (actual requests only; a preflight, `OPTIONS` with `Origin` and `Access-Control-Request-Method`, returns a permissive 204 at handler level, and any other `OPTIONS` is routed; see [ADR: CORS](../ADR/0006-cors.md)) - `inst-proxy-16`
18. [x] - `p1` - **IF** any guard rejects - `inst-proxy-17`
    1. [x] - `p1` - **RETURN** guard-specific error code with `X-OAGW-Error-Source: gateway` - `inst-proxy-17a`
19. [x] - `p1` - Execute transform plugins: `on_request` phase: mutate outbound request. They see the decoded query pairs; the query is rebuilt from the pairs, form-encoded, only when a transform changed them or the query has a literal `;` (step 28) - `inst-proxy-18`
20. [x] - `p1` - Apply header transformation via `cpt-cf-oagw-algo-header-transformation` - `inst-proxy-19`
21. [x] - `p1` - Select target endpoint via `X-OAGW-Target-Host` header or round-robin - `inst-proxy-20`
22. [x] - `p1` - **IF** multi-endpoint upstream with common-suffix alias AND `X-OAGW-Target-Host` header missing - `inst-proxy-21`
    1. [x] - `p1` - **RETURN** 400 MissingTargetHost with `X-OAGW-Error-Source: gateway` - `inst-proxy-21a`
23. [x] - `p1` - **IF** `X-OAGW-Target-Host` present AND format invalid (not hostname or IP; contains port, path, or special chars) - `inst-proxy-22`
    1. [x] - `p1` - **RETURN** 400 InvalidTargetHost with `X-OAGW-Error-Source: gateway` - `inst-proxy-22a`
24. [x] - `p1` - **IF** `X-OAGW-Target-Host` present AND value does not match any configured endpoint host - `inst-proxy-23`
    1. [x] - `p1` - **RETURN** 400 UnknownTargetHost with `X-OAGW-Error-Source: gateway` - `inst-proxy-23a`
25. [x] - `p1` - Run the auth plugin's signing phase ([ADR-0023](../ADR/0023-request-signing.md)) on the request as it will be sent: apply the upstream engine's request header sanitization (after its handshake validation for a WebSocket upgrade), then pass the method, URL (scheme, host, port, path, query after transforms), every header line (`Host` and, for a WebSocket handshake, `Connection: upgrade` and `Upgrade: websocket` included) and the body (buffered bytes, streamed marker, or empty for a WebSocket handshake). Set the headers it returns, marked sensitive, and keep, remove or replace the query as it returns, a replacement sent as returned. Only with a configured auth plugin whose `signs` flag is `true`, outside `request_ref` mode - `inst-proxy-24s`
26. [x] - `p1` - **IF** signing fails, its output cannot be sent as given, or the request deadline passes (a built URL that does not parse is a `DownstreamError`, as for the engine request) - `inst-proxy-24s1`
    1. [x] - `p1` - **RETURN** the `authenticate` error mapping (an output the gateway cannot send is a misconfiguration, 500) or 504 RequestTimeout with reason `REQUEST_NOT_SENT` with `X-OAGW-Error-Source: gateway`; nothing is sent - `inst-proxy-24s1a`
27. [x] - `p1` - **IF** request contains `Upgrade: websocket` header - `inst-proxy-24`
    1. [x] - `p1` - Perform the handshake on a dedicated upstream connection via the engine's upgrade; tunnel only after the upstream answers `101`, otherwise **RETURN** 502 ProtocolError with reason `REQUEST_MAY_HAVE_BEEN_SENT` and `X-OAGW-Error-Source: gateway` - `inst-proxy-24a`
28. [x] - `p1` - Build outbound HTTP request: set target URL (scheme + host + port + path + query), method, headers, body. The query is the client's, byte for byte, unless a transform changed it (step 19) or it has a literal `;`: the allowlist splits pairs on `&` only, and an upstream that also splits on `;` (Rack 2, older Python `parse_qs`, older Go `net/url`) would read parameters the allowlist never checked, so such a query goes out form-encoded (`model=x;admin=1` as `model=x%3Badmin%3D1`). The method is the one the route matched, in upper case - `inst-proxy-25`
29. [x] - `p1` - Forward the request through the upstream engine via `cpt-cf-oagw-algo-upstream-forwarding` - `inst-proxy-26`
30. [x] - `p1` - **IF** the engine reports an upstream connection failure (refused, DNS, TLS) - `inst-proxy-27`
    1. [x] - `p1` - Map the engine error kind to `DomainError` and to its dispatch stage (DESIGN "Dispatch Reasons"); the REST layer writes the RFC 9457 Problem response with the stage as `context.reason` and `X-OAGW-Error-Source: gateway` - `inst-proxy-27a`
    2. [x] - `p1` - **RETURN** 502 DownstreamError (503 LinkUnavailable for DNS failure, SSRF refusal or no route) with reason `REQUEST_NOT_SENT`, or 502 with `REQUEST_MAY_HAVE_BEEN_SENT` when the connection was lost after sending, with `X-OAGW-Error-Source: gateway` - `inst-proxy-27b`
31. [x] - `p1` - **IF** connection or request timeout (connect timeout, response-head or body read timeout) - `inst-proxy-28`
    1. [x] - `p1` - **RETURN** 504 ConnectionTimeout (reason `REQUEST_NOT_SENT`) or RequestTimeout (reason `REQUEST_MAY_HAVE_BEEN_SENT`) via `cpt-cf-oagw-algo-error-source-classification` - `inst-proxy-28a`
32. [x] - `p1` - **IF** the engine reports an HTTP/2 protocol error - `inst-proxy-29`
    1. [x] - `p1` - **RETURN** 502 ProtocolError with reason `REQUEST_MAY_HAVE_BEEN_SENT` and `X-OAGW-Error-Source: gateway` - `inst-proxy-29a`
33. [x] - `p1` - Receive the upstream response head; the body streams - `inst-proxy-30`
34. [x] - `p1` - **IF** the exchange fails on the gateway side (connection failure, timeout, protocol error, request body limit, response guard rejection) - `inst-proxy-31`
    1. [x] - `p1` - Execute transform plugins: `on_error` phase, with the status the client gets; `on_error` runs only for these gateway-originated errors (errors raised before forwarding, such as validation, auth, guard or rate-limit rejections, return without it, and so does a failed WebSocket upgrade) - `inst-proxy-31a`
    2. [x] - `p1` - **RETURN** the gateway error with `X-OAGW-Error-Source: gateway` - `inst-proxy-31b`
35. [x] - `p1` - Execute transform plugins: `on_response` phase, for every upstream response; an upstream 4xx/5xx goes through `on_response`, not `on_error`, and is returned with its status, headers and body and `X-OAGW-Error-Source: upstream` - `inst-proxy-32`
36. [x] - `p1` - **RETURN** transformed response with `X-OAGW-Error-Source: upstream` - `inst-proxy-33`

## 3. Processes / Business Logic (CDSL)

### Alias Resolution

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-alias-resolution`

**Input**: Alias string, tenant chain (requesting tenant first, then ancestors to root)

**Output**: Resolved `UpstreamConfig` or error

**Planned.** Named requests use the ADR-0020 cache for alias, route and parent policies, then read the selected local credential live. Usual visibility and route checks still apply. Pass the resolved parent policies to `cpt-cf-oagw-algo-credential-resolution`. See [ADR-0021](../ADR/0021-upstream-credentials.md).

**With the proxy configuration cache** (a database and `proxy_cache.enabled: true`; `cpt-cf-oagw-algo-obs-proxy-config-cache`): step 1 reads the alias section, keyed by the alias and the tenant chain. A hit runs no query; a miss runs step 1's query and stores its rows as they are (disabled and not-visible upstreams included), or an empty list. Steps 2–6 run on every request, unchanged. An alias that fails validation answers *not found* without a read and without a cache entry.

**Steps**:
1. [x] - `p1` - Repo: `list_by_alias_for_tenants(alias, tenant_chain, Tags::Skip)` — one query `WHERE alias = :alias AND tenant_id IN (:chain)` with plugin rows joined in and no tags, replacing one lookup per tenant - `inst-alias-1`
2. [x] - `p1` - Order the returned upstreams by their tenant's position in the chain (closest first) - `inst-alias-3`
3. [x] - `p1` - **FOR EACH** upstream in chain order - `inst-alias-4`
   1. [x] - `p1` - **IF** upstream belongs to an ancestor AND none of its sharing modes is visible (all `private`), skip it - `inst-alias-4a`
   2. [x] - `p1` - **IF** upstream disabled (enabled = false), record the disabled alias and continue - `inst-alias-4b1`
   3. [x] - `p1` - **ELSE** keep it as a visible enabled match - `inst-alias-4b`
4. [x] - `p1` - **IF** at least one visible enabled match - `inst-alias-2`
   1. [x] - `p1` - **RETURN** the closest one as resolved UpstreamConfig (shadowing); visible ancestors form the merge chain - `inst-alias-4b2`
5. [x] - `p1` - **IF** a disabled match was recorded - `inst-alias-4b1-ret`
   1. [x] - `p1` - **RETURN** error: upstream disabled (503 UpstreamDisabled, reason `UPSTREAM_DISABLED`) - `inst-alias-4b1a`
6. [x] - `p1` - **RETURN** error: upstream not found for alias (404 RouteNotFound) - `inst-alias-5`

### Credential Resolution

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-credential-resolution`

**Input**: Authorized invoke context, single validated selector, selected upstream and route, applicable ancestor policies and server from the routing cache.

**Output**: Authorized credential UUID, API-key configuration and optional bucket policy, or an error before forwarding. SDK preview returns only safe identifiers.

1. [ ] - `p1` - Check use at credential type/owner scope, then read the current row by caller tenant, selected local upstream UUID and name. Check its UUID before disclosing state. Refuse unsupported scopes. Do not cache rows or misses - `inst-cred-resolve-1`
2. [ ] - `p1` - For proxy and preview, check enabled state, then suspension (503), then cached ancestor constraints, auth compatibility and every destination against the owner's approvals (403). Runtime use requires no ancestor management grant - `inst-cred-resolve-2`
3. [ ] - `p1` - Preview returns safe IDs without secrets, debits or forwarding. Proxy handles preflight without a debit, then checks upstream, credential and route limits before CredStore. Pass the pin to auth and the generation to the limiter. Keep attempt debits on later failure and check final outbound headers - `inst-cred-resolve-3`

[ADR-0021](../ADR/0021-upstream-credentials.md) defines the request header and when a key revocation takes effect.

### Route Matching

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-route-matching`

**Input**: tenant chain, ordered upstream IDs (selected upstream first, then merge-chain ancestor upstreams closest-first), HTTP method, request path

**Output**: Matched `RouteConfig` or error

**With the proxy configuration cache** (a database and `proxy_cache.enabled: true`): steps 1 and 5 are replaced by a read of the route section, which holds each upstream's enabled HTTP routes in full, for each upstream in `upstream_ids`. Hits run no query; the upstreams that miss are loaded with one `list_proxy_routes` query. Steps 2–4 (`select_route`) run on every request. No re-check is needed: every route was loaded in full by one query.

**Steps**:
1. [x] - `p1` - Repo: `find_matching_in_tenants(tenant_chain, upstream_ids, method, path, Tags::Skip)` with `upstream_ids` ordered selected upstream first, then ancestor upstreams closest-first — one candidate query `WHERE tenant_id IN (:chain) AND upstream_id IN (:upstream_ids) AND enabled = true AND match_type = 'http'` joined with `oagw_route_http_match` and `oagw_route_method` and filtered on the request method when a database is configured, replacing one `find_matching` call per tenant and per upstream - `inst-route-1`
2. [x] - `p1` - Evaluate candidate routes by upstream preference (list order: selected upstream first, then ancestor upstreams closest-first), then by tenant chain position (closest first); the first upstream and tenant with a matching route wins, so routes on the selected upstream and descendant routes take priority - `inst-route-1b`
3. [x] - `p1` - Within a tenant, filter routes by match type for the request protocol - `inst-route-2`
4. [x] - `p1` - **FOR EACH** route ordered by path prefix length (longest first), then priority (higher number first, per `cpt-cf-oagw-adr-storage-schema`), then lowest route ID - `inst-route-3`
   1. [x] - `p1` - **IF** route.match_type = 'http' - `inst-route-3a`
      1. [x] - `p1` - Check request method is in the route's HTTP method list - `inst-route-3a1`
      2. [x] - `p1` - Check request path starts with the route's path prefix on a segment boundary: the prefix is not followed by a character that continues a segment name (ASCII letter or digit, `-`, `_`, `~`), so `/v1/chat` covers `/v1/chat`, `/v1/chat/…`, `/v1/chat.json` and `/v1/chat:verb`, not `/v1/chatty`; a suffix that does not start with `/` is forwarded as sent; a prefix ending in `/` covers everything below it, and route `/` also covers the bare alias - `inst-route-3a2`
      3. [x] - `p1` - **IF** method allowed AND path matches prefix - `inst-route-3a3`
         1. [x] - `p1` - **RETURN** RouteConfig (first match wins in the order defined by `inst-route-1b` and `inst-route-3`) - `inst-route-3a3a`
   2. [ ] - `p1` - **IF** route.match_type = 'grpc' (planned; gRPC routes are stored but never matched today) - `inst-route-3b`
      1. [ ] - `p1` - Check gRPC service and method against the route's gRPC match - `inst-route-3b1`
      2. [ ] - `p1` - **IF** service and method match - `inst-route-3b2`
         1. [ ] - `p1` - **RETURN** RouteConfig - `inst-route-3b2a`
5. [x] - `p1` - Repo: load the winning route in full with its path, method and plugin binding rows (tag rows only for `Tags::Load`); **IF** the loaded route no longer matches the request (a concurrent write changed, removed or moved it), select once more from step 1, then fall through to step 6 - `inst-route-5`
6. [x] - `p1` - **RETURN** error: no matching route found in any tenant (404 RouteNotFound) - `inst-route-4`

### Plugin Chain Execution

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-plugin-chain-execution`

**Input**: Upstream plugin bindings, route plugin bindings, request context, auth plugin reference from upstream

**Output**: Processed request ready for forwarding, or rejection error

**Steps**:
1. [x] - `p1` - Load upstream plugin bindings ordered by position (`oagw_upstream_plugin` when a database is configured) - `inst-chain-1`
2. [x] - `p1` - Load route plugin bindings ordered by position (`oagw_route_plugin` when a database is configured) - `inst-chain-2`
3. [x] - `p1` - Compose ordered chain: `[upstream_plugins...] + [route_plugins...]` (upstream-before-route) - `inst-chain-3`
4. [x] - `p1` - **FOR EACH** plugin_ref in composed chain - `inst-chain-4`
   1. [x] - `p1` - Parse GTS identifier: extract instance part (after `~`) - `inst-chain-4a`
   2. [x] - `p1` - **IF** instance parses as UUID - `inst-chain-4b`
      1. [ ] - `p1` - DB: SELECT plugin FROM oagw_plugin WHERE id = :uuid — must exist and match schema type (with custom plugin support; `oagw_plugin` is not created yet) - `inst-chain-4b1`
   3. [x] - `p1` - **ELSE** (named plugin) - `inst-chain-4c`
      1. [x] - `p1` - Resolve via in-process plugin registry - `inst-chain-4c1`
   4. [x] - `p1` - **IF** plugin not found - `inst-chain-4d`
      1. [x] - `p1` - **RETURN** 503 PluginNotFound with `X-OAGW-Error-Source: gateway` - `inst-chain-4d1`
5. [x] - `p1` - Resolve upstream auth plugin from the upstream's auth configuration (`auth_plugin_ref` / `auth_plugin_uuid` when a database is configured) - `inst-chain-5`
6. [x] - `p1` - Execute auth plugin: resolve credentials from `cred_store` via secret_ref; the plugin lists the headers to set, and each replaces the request's lines of that name, marked sensitive. Other headers, repeated or non-UTF-8 ones included, pass unchanged. A header name or value that is not valid fails the request with 500 (plugin misconfiguration) instead of sending it without credentials - `inst-chain-6`
7. [x] - `p1` - **IF** secret not found or credential resolution fails - `inst-chain-7`
   1. [x] - `p1` - **RETURN** the credential-failure mapping (401 `AUTH_CREDENTIAL_NOT_FOUND`, 503 `CREDENTIAL_STORE_UNAVAILABLE`, 503 `IDENTITY_PROVIDER_UNAVAILABLE`, 502 invalid identity provider response, 500 misconfiguration, 401 AuthenticationFailed otherwise), naming the auth config key, not the reference - `inst-chain-7a`
8. [x] - `p1` - **FOR EACH** guard plugin in chain (type = guard) - `inst-chain-8`
   1. [x] - `p1` - Execute guard: validate request against guard rules (method allowlist, query allowlist, path suffix, timeout) - `inst-chain-8a`
   2. [x] - `p1` - **IF** guard rejects - `inst-chain-8b`
      1. [x] - `p1` - **RETURN** guard-specific rejection error - `inst-chain-8b1`
9. [x] - `p1` - **FOR EACH** transform plugin in chain (type = transform, phase = on_request) - `inst-chain-9`
   1. [x] - `p1` - Execute transform: mutate request headers, body, query as configured. The transform sees the headers whose values are visible ASCII; lines that are not (obs-text, non-UTF-8) pass through unseen. Only the names whose lines the transform changed are rewritten: all their lines, unseen ones included, are replaced by the transform's (none: removed), keeping the sensitive flag. Every other line, the credential set by the auth plugin included, keeps its bytes, order and sensitive flag. The same applies to response headers in `on_response` - `inst-chain-9a`
10. [x] - `p1` - **RETURN** processed request ready for upstream forwarding; the auth plugin's signing phase runs later, once the URL is built (`inst-proxy-24s`) - `inst-chain-10`

### Header Transformation

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-header-transformation`

**Input**: Inbound request headers, upstream `HeadersConfig`, route `HeadersConfig`

**Output**: Transformed outbound headers

**Steps**:
1. [x] - `p1` - Strip hop-by-hop headers: Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade - `inst-header-1`
2. [x] - `p1` - Strip routing header: consume `X-OAGW-Target-Host` value for endpoint selection, then remove - `inst-header-2`
3. [x] - `p1` - Replace `Host` header with upstream endpoint host (HTTP/1.1) or `:authority` pseudo-header with upstream authority (HTTP/2). The port is appended unless it is the default for the endpoint's scheme (80 for `http`, 443 for the others), so `http` on 443 sends `host:443` and `https` on 80 sends `host:80` - `inst-header-3`
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
   2. [x] - `p1` - **IF** Content-Length exceeds `max_body_size_bytes` (at most and by default 100 MiB, 104,857,600 bytes; the api-gateway's `body_limit_bytes`, 16 MiB by default, rejects larger bodies before OAGW sees them) - `inst-body-1b`
      1. [x] - `p1` - **RETURN** 413 PayloadTooLarge (reject before reading the body per `cpt-cf-oagw-constraint-body-limit`) - `inst-body-1b1`
2. [x] - `p1` - **IF** Transfer-Encoding header present - `inst-body-2`
   1. [x] - `p1` - **IF** encoding is not `chunked` - `inst-body-2a`
      1. [x] - `p1` - **RETURN** 400 ValidationError (only `chunked` supported) - `inst-body-2a1`
3. [x] - `p1` - **IF** actual body size exceeds `max_body_size_bytes` during streaming read - `inst-body-3`
   1. [x] - `p1` - **RETURN** 413 PayloadTooLarge (abort read) - `inst-body-3a`
4. [x] - `p1` - **IF** a streamed body yields more or fewer bytes than its Content-Length - `inst-body-5`
   1. [x] - `p1` - **RETURN** 400 `INVALID_CONTENT_LENGTH` (abort read) - `inst-body-5a`
5. [x] - `p1` - **IF** reading the body fails otherwise (client abort, SDK stream error) - `inst-body-6`
   1. [x] - `p1` - **RETURN** 400 `BODY_READ_FAILED`; a REST client gone mid-upload is counted as `Cancelled` (an SDK stream error never is) - `inst-body-6a`
6. [x] - `p1` - **RETURN** validation passed - `inst-body-4`

Steps 3 to 5 run while the body streams to the upstream (see `cpt-cf-oagw-algo-upstream-forwarding`). If the upstream's response head arrived before the failure, that response is returned and the failure is logged; otherwise the upstream may have received the head and part of the body, never a complete request. Over REST, hyper parses the framing first: a `Content-Length` that is not an integer or a `Transfer-Encoding` other than `chunked` gets hyper's bare `400` (no `problem+json`, no `X-OAGW-Error-Source`); steps 1 and 2 answer with `problem+json` for in-process SDK requests. On the SDK path a `Body::Bytes` is sent with its own length whatever its `Content-Length` header says.

### Upstream Forwarding

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-upstream-forwarding`

**Input**: Outbound request (method, URL, headers, body), selected endpoint

**Output**: Upstream HTTP response (head + streaming body) or engine error

**Steps**:
1. [x] - `p1` - Build the engine target: the load balancer's pre-resolved address, or resolve the endpoint host (retries 100/500/2500 ms) and take the first address the SSRF policy allows - `inst-fwd-1`
2. [x] - `p1` - Choose transport and protocol policy: TLS for `https`/`wss`/`wt`, HTTP/1.1 only for `wss`, cleartext HTTP/1.1 for `http` - `inst-fwd-2`
3. [x] - `p1` - **IF** request body is buffered (an SDK `Body::Bytes`, or a stream read whole because the auth plugin's signing phase hashes it: `signs_body`, `aws_sigv4` without `unsigned_payload`; or read whole because the upstream's circuit breaker is not closed) - `inst-fwd-3`
   1. [x] - `p1` - Send it with `Content-Length` framing derived by the engine - `inst-fwd-3a`
4. [x] - `p1` - **ELSE IF** request body is streaming (every REST body, an SDK `Body::Stream`) - `inst-fwd-4`
   1. [x] - `p1` - Forward chunks as they arrive, with the client's `Content-Length` when it sent one and chunked framing otherwise, enforcing `max_body_size` and the declared length; on breach, length mismatch or stream error abort the upstream request without completing its framing and **RETURN** 413 PayloadTooLarge, 400 `INVALID_CONTENT_LENGTH` or 400 `BODY_READ_FAILED` - `inst-fwd-4a`
   2. [x] - `p1` - An upstream response that arrives before the body is fully sent (an early `401` or `413`) is returned as the response - `inst-fwd-4b`
5. [x] - `p1` - Engine reuses a pooled connection or connects (ALPN `h2`/`http/1.1` for TLS), rewrites framing and hop-by-hop headers, and sends the request; the engine sends it again at most once, on another connection, only in the cases of ADR-0019 (a request that never left, `REFUSED_STREAM`, `HTTP_1_1_REQUIRED`, and for idempotent methods a stale keep-alive connection or a graceful `GOAWAY`). A buffered body is resent from its bytes; a streamed body from the copy of its first `request_retry_buffer_bytes` (default 1 MiB) kept while it is sent, followed by the rest of the stream, with its `Content-Length` still checked. A streamed body longer than that is not resent: its copy is freed once it passes the limit - `inst-fwd-5`
6. [x] - `p1` - **IF** the engine fails - `inst-fwd-6`
   1. [x] - `p1` - Map the engine error kind to `DomainError` (see [ADR-0019](../ADR/0019-upstream-proxy-engine.md)) and to its dispatch stage: only the connect and TLS kinds are `REQUEST_NOT_SENT`, unless the engine resent the request after a first attempt the upstream may have processed; every other kind, a future one included, is `REQUEST_MAY_HAVE_BEEN_SENT` - `inst-fwd-6a`
7. [x] - `p1` - Strip every `x-oagw-*` header from the upstream response head - `inst-fwd-7`
8. [x] - `p1` - **RETURN** response head and streaming body - `inst-fwd-8`

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

The system **MUST** implement `DataPlaneService::proxy_request(...)` that orchestrates the full proxy flow: alias resolution, route matching, body validation, plugin chain execution, HTTP forwarding through the upstream proxy engine, and response transformation. No upstream response caching per `cpt-cf-oagw-principle-no-cache`. The engine's single connection-level resend in the cases listed in ADR-0019 is permitted; all other failures **MUST** be returned immediately without additional application-layer retries per `cpt-cf-oagw-principle-no-retry`.

**Implements**:
- `cpt-cf-oagw-flow-proxy-request`
- `cpt-cf-oagw-algo-error-source-classification`

**Touches**:
- API: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}`
- Entities: `Upstream`, `Route`, `Plugin`, `ServerConfig`, `Endpoint`

### Implement Alias Resolution

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-alias-resolution`

The system **MUST** resolve upstreams by `(tenant_id, alias)` with tenant hierarchy walk from descendant to root. The closest enabled match wins (shadowing); a disabled upstream is skipped, and when only disabled upstreams match the alias resolution **MUST** return 503 with reason `UPSTREAM_DISABLED`. Private upstreams (sharing = `private`) **MUST NOT** be visible to descendant tenants. The upstreams for the whole tenant chain **MUST** be fetched with one `list_by_alias_for_tenants` call; the walk then runs in memory, so the number of queries does not grow with hierarchy depth. With the proxy configuration cache, that call's result is cached per alias and tenant chain and the walk still runs on every request, so a cached lookup returns what an uncached one does.

**Implements**:
- `cpt-cf-oagw-algo-alias-resolution`

**Touches**:
- DB: `oagw_upstream`
- Entities: `Upstream`

### Implement Route Matching

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-route-matching`

The system **MUST** match inbound requests to routes by HTTP method allowlist and longest path prefix, ordered by priority (higher number wins, per `cpt-cf-oagw-adr-storage-schema`). Only enabled routes are considered. No two enabled routes under the same upstream may share `(path_prefix, priority)` for the same method. Matching runs in application code over the upstream's enabled routes, identically for in-memory and database storage. Candidate routes for the whole tenant chain **MUST** be fetched with one `find_matching_in_tenants` call that loads only the columns matching reads. The call takes the ordered upstream IDs (selected upstream first, then ancestor upstreams closest-first), so the fallback to routes on ancestor upstreams needs no extra call. Routes on the selected upstream take priority over routes on ancestor upstreams, and descendant routes take priority over ancestor routes. The lowest route ID breaks any remaining tie, and only the winning route is loaded in full, with its path, method and plugin rows (tag rows only for `Tags::Load`, which the proxy never passes). The loaded route **MUST** be re-checked against the request; a route that a concurrent write changed, removed or moved is selected once more, and a second miss is no match. With the proxy configuration cache, each upstream's enabled HTTP routes are cached in full, the upstreams that miss are loaded with one `list_proxy_routes` query, and matching runs over them with the same code and no re-check.

**Implements**:
- `cpt-cf-oagw-algo-route-matching`

**Touches**:
- DB: `oagw_route`, `oagw_route_http_match`, `oagw_route_method`, `oagw_route_plugin` (when a database is configured)
- Entities: `Route`

### Implement Plugin Chain Execution

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-plugin-chain`

The system **MUST** execute the plugin chain in deterministic order: Auth → Guards → Transform(on_request) → upstream call → Transform(on_response/on_error). `on_response` **MUST** run for every upstream response, 4xx/5xx included; `on_error` runs only for gateway-originated errors of the upstream exchange. Upstream plugins **MUST** execute before route plugins (`[U1, U2] + [R1, R2]`). Plugins **MUST** be resolved via GTS identifier: UUID-backed plugins from `oagw_plugin`, named plugins from in-process registry. Auth plugin credentials **MUST** be resolved from `cred_store` via `secret_ref` at request time per `cpt-cf-oagw-principle-cred-isolation`.

**Implements**:
- `cpt-cf-oagw-algo-plugin-chain-execution`

**Touches**:
- DB: `oagw_upstream_plugin`, `oagw_route_plugin`; `oagw_plugin` once custom plugins are supported
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

The system **MUST** validate Content-Length (valid non-negative integer), enforce `max_body_size_bytes` (at most 100 MiB; reject before buffering per `cpt-cf-oagw-constraint-body-limit`), and reject unsupported Transfer-Encoding (only `chunked` supported). Invalid requests **MUST** return 400 ValidationError or 413 PayloadTooLarge.

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

### Implement Upstream Proxy Engine

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-upstream-proxy-engine`

The system **MUST** forward upstream traffic through the `toolkit-http-proxy` engine (`cpt-cf-oagw-algo-upstream-forwarding`, [ADR-0019](../ADR/0019-upstream-proxy-engine.md)). The engine manages connection pooling, ALPN, FIPS-hardened TLS and outbound framing. Multi-endpoint upstreams **MUST** distribute requests round-robin over healthy backends, with TCP health checks every 10 s and DNS refresh every 30 s; every resolved address **MUST** pass the SSRF policy. When `X-OAGW-Target-Host` header is present, the system **MUST** select the matching endpoint explicitly (no round-robin). All endpoints in a pool **MUST** have identical protocol, scheme, and port. `X-OAGW-Target-Host` **MUST** be validated: required for multi-endpoint common-suffix upstreams (400 MissingTargetHost); format must be hostname or IP without port/path/special chars (400 InvalidTargetHost); value must match a configured endpoint (400 UnknownTargetHost). Non-timeout upstream connection failures (refused, DNS, TLS) **MUST** return 502 DownstreamError. WebSocket upgrades **MUST** be tunnelled on a dedicated connection only after the upstream answered `101 Switching Protocols`; any other answer **MUST** return 502 ProtocolError.

The endpoint selector caches each upstream's backend pool by upstream ID. It **MUST** rebuild a cached pool whenever the endpoints passed for that upstream differ from the cached list, so a replica serves updated endpoints after another replica writes the change to the shared database. Explicit invalidation by the local management handlers and the SDK remains as a fast path; correctness **MUST NOT** depend on it. A pool not selected for 10 minutes **MUST** be dropped, so pools of upstreams deleted through other replicas do not keep their DNS and health-check task running; dropping a pool **MUST** stop that task.

Engine errors **MUST** be converted into `DomainError` and returned through the canonical `DomainError` → RFC 9457 `Problem` pipeline. The response **MUST** include `X-OAGW-Error-Source: gateway` and `Content-Type: application/problem+json`. Upstream responses **MUST** lose every `x-oagw-*` header before error-source classification, so an upstream cannot claim to be the gateway.

**Implements**:
- `cpt-cf-oagw-flow-proxy-request`
- `cpt-cf-oagw-algo-upstream-forwarding`

**Touches**:
- Entities: `ServerConfig`, `Endpoint`

## 6. Acceptance Criteria

- [x] Proxy requests via `{METHOD} /api/oagw/v1/proxy/{alias}/{path}` are forwarded to the correct upstream and response is returned
- [x] Requests without valid Bearer token return 401; requests without `gts.cf.core.oagw.proxy.v1~:invoke` permission return 403
- [x] Alias resolution walks tenant hierarchy from descendant to root and returns closest match (shadowing)
- [x] Disabled upstreams return 503 `service_unavailable` with reason `UPSTREAM_DISABLED`, `Retry-After: 300` and `X-OAGW-Error-Source: gateway`
- [x] Private upstreams are not visible to descendant tenants during alias resolution
- [x] Route matching selects the longest path prefix match, then the higher priority number
- [x] A route path prefix ends on a segment boundary (`/v1/chat` does not cover `/v1/chatty` or `/v1/chat-x`, but covers `/v1/chat.json`, `/v1/chat:verb` and `/v1/chat%2Fx`; the rule is for ASCII names, so an escaped non-ASCII character also ends the segment and `/v1/chat%C3%A9` is covered), and route `/` matches the bare `/oagw/v1/proxy/{alias}`; a request that would select another route with `%2F`, `%5C`, `\` and the fullwidth and small solidi `%EF%BC%8F`, `%EF%BC%BC` and `%EF%B9%A8`, which NFKC folds to `/` and `\` read as `/` (routes `/v1` and `/v1/admin`, request `/v1%2Fadmin`), or the reverse (routes `/v3` and `/v3/a%2Fb`, request `/v3/a/b`), is rejected with 400 `INVALID_PROXY_PATH`
- [x] A path segment that is a dot-segment once percent-decoded (`%2e%2e`, `.%2e`, `..%2f`, `..\`, `..;param`, `%252e%252e`) or whose escapes are not UTF-8, a path with a `%` not followed by two hex digits, and a segment with a `;` path parameter (raw or `%3B`), are rejected with 400 `INVALID_PROXY_PATH`; literal `.` and `..` are resolved before matching, and an encoded `/` inside a segment is forwarded encoded
- [x] Route matching and the upstream URL use the normalized path: percent-encoded unreserved characters are decoded (`/v1/ch%61t` is `/v1/chat`) and other escapes are uppercased (RFC 3986 §6.2.2)
- [x] Route matching evaluates candidates by upstream preference (selected upstream first, then ancestor upstreams closest-first) and tenant chain position before path prefix, and breaks remaining ties by the lowest route ID
- [x] Unmatched routes return 404 RouteNotFound with `X-OAGW-Error-Source: gateway`
- [x] Plugin chain executes in deterministic order: Auth → Guards → Transform(on_request) → upstream → Transform(on_response/on_error)
- [x] Upstream plugins execute before route plugins in the composed chain
- [x] Auth plugin resolves credentials from `cred_store` via `secret_ref` at request time; a missing or inaccessible secret returns 401 `AUTH_CREDENTIAL_NOT_FOUND`, a credential store outage 503 `CREDENTIAL_STORE_UNAVAILABLE`, an identity provider outage 503 `IDENTITY_PROVIDER_UNAVAILABLE`, an invalid identity provider response 502, a plugin misconfiguration 500
- [x] Guard plugins reject invalid requests (method, query, path suffix) before upstream forwarding
- [x] Routes match `GET`, `POST`, `PUT`, `DELETE`, `PATCH`, `HEAD` and `OPTIONS`, each only when listed (`GET` does not imply `HEAD`); a `HEAD` response carries the upstream's headers, `Content-Length` included, and no body; an `OPTIONS` that is not a CORS preflight is proxied
- [x] The upstream receives the client's query byte for byte (escapes, `+` vs `%2B`, repeated keys, empty values, keys without `=`, order); the allowlist checks the decoded names; a transform that changes the parameters gets them form-encoded
- [x] A query with a literal `;` reaches the upstream form-encoded (`;` as `%3B`), so an upstream that splits on `;` sees the parameters the allowlist checked
- [x] A method sent in lower case (`head`) reaches the upstream as the method the route matched (`HEAD`)
- [x] Hop-by-hop headers (Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade) are stripped from outbound requests
- [x] `X-OAGW-Target-Host` is consumed for endpoint selection and stripped before forwarding
- [x] `Host` (HTTP/1.1) or `:authority` (HTTP/2) is replaced with upstream endpoint host
- [x] Header set/add/remove operations from upstream/route config are applied in declared order
- [x] Content-Length validation rejects non-integer or mismatched values with 400
- [x] `max_body_size_bytes` (at most 100 MiB) is enforced before buffering (413 PayloadTooLarge); the effective limit is the smaller of it and the api-gateway's `body_limit_bytes` (16 MiB by default); a declared length over the api-gateway limit gets its `413`, an `about:blank` problem without `X-OAGW-Error-Source`, while a body without one is cut as it streams and gets OAGW's `413`
- [x] Only `chunked` Transfer-Encoding is accepted; others return 400
- [x] Gateway errors use RFC 9457 Problem Details (`application/problem+json`) with GTS type identifiers
- [x] `X-OAGW-Error-Source: gateway` is set on all gateway-originated errors
- [x] `X-OAGW-Error-Source: upstream` is set on upstream passthrough responses (body and headers passed as-is, the upstream's `Retry-After` and `WWW-Authenticate` included)
- [x] Multi-endpoint upstreams distribute requests via round-robin
- [x] `X-OAGW-Target-Host` selects specific endpoint in multi-endpoint upstreams
- [x] Changing an upstream's endpoints rebuilds its cached pool on every replica without explicit invalidation
- [x] A pool idle past its TTL is dropped and its background task stops, as on invalidation
- [x] With a database configured, alias resolution and route matching issue the same number of queries for tenant hierarchies of depth 1 and depth 5
- [x] With a database and the proxy configuration cache enabled, a repeated lookup runs no query, a lookup that misses both sections runs two, and the result equals an uncached lookup's (`cpt-cf-oagw-adr-proxy-config-cache`)
- [x] With the cache, each configuration write is visible to the next lookup on the same replica, and on other replicas once the change is delivered through the cluster change log; a lost change shows within `proxy_cache.ttl`
- [x] The SSRF check runs on every request, so a cached upstream whose host is later denied is rejected on the next lookup
- [x] HTTPS-only constraint is enforced for all upstream connections
- [x] Multi-endpoint upstream with common-suffix alias returns 400 MissingTargetHost when `X-OAGW-Target-Host` is absent
- [x] Invalid `X-OAGW-Target-Host` format (not hostname or IP) returns 400 InvalidTargetHost
- [x] `X-OAGW-Target-Host` value not matching any configured endpoint returns 400 UnknownTargetHost
- [x] Upstream connection failures return gateway errors with `X-OAGW-Error-Source: gateway` and reason `REQUEST_NOT_SENT`: refused and TLS 502 DownstreamError, DNS, SSRF refusal and no route 503 LinkUnavailable, connect timeout 504
- [x] Failures after the request may have been sent (connection closed, read or write error, HTTP/2 or protocol error, read or write timeout) carry reason `REQUEST_MAY_HAVE_BEEN_SENT`
- [x] No credentials appear in logs, error messages, or API responses
- [x] Application layer does not add retries; only the engine's single connection-level resend (ADR-0019) is permitted
- [x] WebSocket upgrade requests (`Upgrade: websocket`) are tunnelled only after the upstream answers `101`; a rejected upgrade returns 502 ProtocolError with reason `REQUEST_MAY_HAVE_BEEN_SENT` and `X-OAGW-Error-Source: gateway`
- [x] Upstream engine errors produce RFC 9457 Problem Details body with GTS type identifiers and `X-OAGW-Error-Source: gateway`
- [x] Upstream responses lose every `x-oagw-*` header, so `ErrorSource` defaults to `Upstream` for them; gateway-generated error responses always set `X-OAGW-Error-Source: gateway` explicitly

## 7. Additional Context

### Performance Considerations

The proxy engine is on the critical path for every outbound API call — less than 10ms added latency at p95 per `cpt-cf-oagw-nfr-low-latency`. Plugin chain execution and header transformation are in-memory operations. With a database configured, configuration lookup costs three queries per request whatever the tenant depth (the upstreams with their plugin rows, the candidate routes, and the winning route with its path, method and plugin rows; two more only for a request that raced a configuration write); with the proxy configuration cache (on by default with a database; DESIGN §4.1), a hit runs no query and a miss one per section that missed. The upstream engine pools connections (HTTP/2 multiplexed, HTTP/1.1 keep-alive) and reuses TLS sessions, with TCP health checks at 10s intervals to avoid routing to unhealthy backends. Requests go straight from the pipeline to the engine — no serialization, pipe or spawned task per request. Body validation checks Content-Length before buffering to reject oversized payloads early. Streaming bodies (e.g. SSE uploads) are forwarded chunk by chunk without buffering the full body.

### Security Considerations

SSRF protection is partially addressed in this feature through scheme enforcement (HTTPS-only per `cpt-cf-oagw-constraint-https-only`) and hop-by-hop / routing header stripping. Full IP pinning and DNS validation hardening are addressed in `cpt-cf-oagw-feature-observability`. HTTP smuggling is prevented by strict header parsing: CR/LF rejection, Content-Length / Transfer-Encoding combination validation.

Credential isolation is enforced by resolving secrets from `cred_store` at request time — OAGW never stores or logs secret material per `cpt-cf-oagw-principle-cred-isolation`.

### Deliberate Omissions

<!-- out-of-scope feature reference -->
- [x] `p2` - `cpt-cf-oagw-feature-tenant-hierarchy`

- **States section**: Not applicable — proxy engine is a stateless request-response flow. Circuit breaker state management belongs to `cpt-cf-oagw-feature-rate-limiting`.
- **Multi-tenant hierarchy merge**: Out of scope — hierarchical config override and sharing mode merge strategies belong to cpt-cf-oagw-feature-tenant-hierarchy.
- **Rate limiting enforcement**: Out of scope — rate limiting and circuit breaker during proxy flow belong to `cpt-cf-oagw-feature-rate-limiting`.
- **SSE/WebTransport streaming**: Out of scope — streaming protocol support belongs to `cpt-cf-oagw-feature-streaming`. WebSocket upgrades are tunnelled through the engine's upgrade path (step `inst-proxy-24`); the WebSocket relay itself belongs to the streaming feature.
- **Metrics and audit logging**: Out of scope — Prometheus metrics, structured logging, and CORS handling belong to `cpt-cf-oagw-feature-observability`.
- **UX/Accessibility**: Not applicable — OAGW is a backend API gear with no user interface.
- **Compliance/Privacy**: OAGW does not handle PII directly. Credential isolation via `cred_store` references covers data protection. No additional regulatory compliance beyond standard platform requirements.
