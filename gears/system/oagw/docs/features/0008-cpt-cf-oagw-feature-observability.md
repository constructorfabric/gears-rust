# Feature: Observability & Security Hardening


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
  - [1.6 Configuration Parameters](#16-configuration-parameters)
  - [1.7 Non-Applicable Domains](#17-non-applicable-domains)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Configure CORS Policy](#configure-cors-policy)
  - [Review Prometheus Metrics](#review-prometheus-metrics)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Metrics Collection Pipeline](#metrics-collection-pipeline)
  - [Audit Log Emission](#audit-log-emission)
  - [CORS Handler](#cors-handler)
  - [SSRF Protection Validation](#ssrf-protection-validation)
  - [HTTP Smuggling Prevention](#http-smuggling-prevention)
  - [Proxy Configuration Cache](#proxy-configuration-cache)
  - [Proxy Configuration Change](#proxy-configuration-change)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Prometheus Metrics](#implement-prometheus-metrics)
  - [Implement Structured Audit Logging](#implement-structured-audit-logging)
  - [Implement CORS Handling](#implement-cors-handling)
  - [Implement SSRF Protection](#implement-ssrf-protection)
  - [Implement HTTP Smuggling Prevention](#implement-http-smuggling-prevention)
  - [Implement the Proxy Configuration Cache](#implement-the-proxy-configuration-cache)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-oagw-featstatus-observability-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p2` - `cpt-cf-oagw-feature-observability`

## 1. Feature Context

### 1.1 Overview

Implement Prometheus metrics, structured audit logging, CORS handling, SSRF protection, and, with a database, the proxy configuration cache for operational visibility and security hardening.

### 1.2 Purpose

Operators need full visibility into outbound API traffic patterns, errors, and performance. Security hardening prevents SSRF, HTTP smuggling, and CORS violations. Covers `cpt-cf-oagw-nfr-observability`, `cpt-cf-oagw-nfr-ssrf-protection`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-oagw-actor-platform-operator` | Monitors metrics, reviews audit logs, configures CORS policies |
| `cpt-cf-oagw-actor-app-developer` | Benefits from CORS handling and SSRF protection transparently |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADR**: [ADR/0006-cors.md](../ADR/0006-cors.md), [ADR/0020-proxy-config-cache.md](../ADR/0020-proxy-config-cache.md) (supersedes [ADR/0007-data-plane-caching.md](../ADR/0007-data-plane-caching.md)), [ADR/0013-error-source-distinction.md](../ADR/0013-error-source-distinction.md)
- **Dependencies**: `cpt-cf-oagw-feature-proxy-engine`

### 1.5 Out of Scope

- TLS certificate pinning for upstream connections (future work)
- mTLS support for client certificate authentication with upstreams (future work)
- Centralized logging system deployment and configuration (infrastructure concern)
- Distributed tracing backend deployment (infrastructure concern; feature provides trace context propagation)
- Custom metric definitions beyond the OAGW standard metrics and the 8 proxy configuration cache metrics

### 1.6 Configuration Parameters

| Parameter | Default | Configurable | Notes |
|-----------|---------|-------------|-------|
| Histogram buckets (seconds) | `[0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]` | Yes | `OagwConfig` |
| Proxy configuration cache enabled | `true` | Yes | `proxy_cache.enabled`; no effect without a database |
| Proxy configuration cache entry TTL | 60s | Yes | `proxy_cache.ttl`; 1s to 1h |
| Proxy configuration cache stale lifetime | 15m | Yes | `proxy_cache.max_stale`; 0s (off) to 24h; how long past the TTL an entry answers when its reload fails |
| Wait for a stale entry's reload | 2s from the reload's start | No | Constant |
| Retry window after a failed reload | 5s per key | No | Constant |
| Health report of a stale answer | 30s | No | Constant (`proxy_cache_serving_stale`) |
| Proxy configuration cache capacity | 10,000 entries per section | Yes | `proxy_cache.max_entries`; 1 to 1,000,000 |
| Change log bounds | 128 changes, 32 KiB | No | Constants ([ADR-0020](../ADR/0020-proxy-config-cache.md#the-change-log)) |
| Write's wait for its change log append | 2s | No | Constant; a registry reconcile run waits once, at its end |
| Change log append attempt limit | 1s, 5 attempts | No | Constant |
| Second apply of a write dropped mid-call | 30s after the drop | No | Constant |
| Proxy configuration cache load limit | 30s | No | Constant |
| Change log read limit | 5s | No | Constant |
| Audit log sampling rate | 1/100 | Yes | Per-route configurable |
| CORS max_age | 86400s | No | Hardcoded in handler |

### 1.7 Non-Applicable Domains

- **Usability (UX)**: Not applicable — this feature is backend infrastructure with no user-facing UI. Operators interact via standard Prometheus/logging tooling.
- **Compliance (COMPL)**: Not applicable — no regulatory data processing. Audit logging supports compliance but specific regulatory requirements are governed by organizational policy, not this feature.
- **Data Privacy**: Not applicable — this feature explicitly excludes PII from logs and metrics. No user data is stored or processed beyond operational metadata.

## 2. Actor Flows (CDSL)

### Configure CORS Policy

- [ ] `p2` - **ID**: `cpt-cf-oagw-flow-obs-configure-cors`

**Actor**: `cpt-cf-oagw-actor-platform-operator`

**Success Scenarios**:
- CORS policy is saved on upstream/route and takes effect for subsequent proxy requests
- Preflight OPTIONS requests are handled locally without upstream round-trip

**Error Scenarios**:
- Invalid origin pattern in CORS configuration (rejected with validation error)
- Upstream or route not found (404)
- Insufficient permissions to modify CORS settings (403)

**Steps**:
1. [ ] - `p2` - Operator submits upstream/route update with CORS configuration via Management API - `inst-cors-1`
2. [ ] - `p2` - API: PUT /api/oagw/v1/upstreams/{id} or PUT /api/oagw/v1/routes/{id} (cors field in request body) - `inst-cors-2`
3. [ ] - `p2` - Validate CORS configuration: allowed_origins, allowed_methods - `inst-cors-3`
4. [ ] - `p2` - **IF** validation fails - `inst-cors-4`
   1. [ ] - `p2` - **RETURN** 400 ValidationError with details - `inst-cors-4a`
5. [ ] - `p2` - **ELSE** - `inst-cors-5`
   1. [ ] - `p2` - DB: UPDATE oagw_upstream or oagw_route SET cors config - `inst-cors-5a`
   2. [ ] - `p2` - With a database: evict the proxy configuration cache entries the write affects, and append the change to the cluster change log so every replica evicts them - `inst-cors-5b`
   3. [ ] - `p2` - **RETURN** updated resource with CORS configuration - `inst-cors-5c`

### Review Prometheus Metrics

- [ ] `p2` - **ID**: `cpt-cf-oagw-flow-obs-review-metrics`

**Actor**: `cpt-cf-oagw-actor-platform-operator`

**Success Scenarios**:
- Operator retrieves current Prometheus metrics from the admin `/metrics` endpoint
- Metrics reflect real-time proxy traffic state (counters, gauges, histograms)

**Error Scenarios**:
- Unauthorized access to metrics endpoint (401)

**Steps**:
1. [ ] - `p2` - Operator sends GET /metrics to the admin endpoint - `inst-metrics-1`
2. [ ] - `p2` - Verify request is authenticated and authorized for admin access - `inst-metrics-2`
3. [ ] - `p2` - **IF** unauthorized - `inst-metrics-3`
   1. [ ] - `p2` - **RETURN** 401 Unauthorized - `inst-metrics-3a`
4. [ ] - `p2` - **ELSE** - `inst-metrics-4`
   1. [ ] - `p2` - Collect all registered OAGW metrics from the Prometheus registry - `inst-metrics-4a`
   2. [ ] - `p2` - Serialize metrics in Prometheus exposition format (text/plain) - `inst-metrics-4b`
   3. [ ] - `p2` - **RETURN** 200 with metrics payload - `inst-metrics-4c`

## 3. Processes / Business Logic (CDSL)

### Metrics Collection Pipeline

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-obs-metrics-collection`

**Input**: Proxy request context (host, path, method) and response context (status, duration, error_type)

**Output**: Updated Prometheus metric values

**Steps**:
1. [ ] - `p2` - Extract or generate trace_id from inbound request context (propagate W3C Trace Context `traceparent` header if present; generate new trace_id otherwise) - `inst-mc-0`
2. [ ] - `p2` - Normalize path from route configuration (use route match pattern, not raw path) to control cardinality - `inst-mc-1`
3. [ ] - `p2` - Capture the numeric HTTP response status code for the `http.response.status_code` label (OTel HTTP semconv; aligned with the inbound API Gateway) - `inst-mc-2`
4. [ ] - `p2` - Increment `oagw_requests_in_flight{host}` gauge at request start, under `host` `unknown` until the upstream is resolved, then under the upstream alias - `inst-mc-3`
5. [ ] - `p2` - **TRY** - `inst-mc-4`
   1. [ ] - `p2` - Execute proxy pipeline (auth → guards → transform → upstream call → response transform) - `inst-mc-4a`
6. [ ] - `p2` - **CATCH** any error - `inst-mc-5`
   1. [ ] - `p2` - Increment `oagw_errors_total{host, path, error_type, credential_mode}` counter (gateway errors only; an upstream error response is not an error here; `credential_mode`: `config`, `request_ref`, or `unknown` before the upstream resolves) - `inst-mc-5a`
7. [ ] - `p2` - Decrement `oagw_requests_in_flight{host}` gauge when the response body ends, fails or is dropped; for a gateway error, or a response without a body (101, HEAD, 204, 304), at the response head - `inst-mc-6`
8. [ ] - `p2` - Increment `oagw_requests_total{host, http.request.method, http.route, http.response.status_code, credential_mode}` counter for every request with a response: the upstream's status (4xx/5xx included), or for a gateway error the status the client gets; a denied `proxy:invoke` check, and a granted request the REST handler rejects before the Data Plane runs (proxy path, WebSocket upgrade, body size and body read checks), count under `host` and `http.route` `unknown`. A request the client dropped before its response head counts only in `oagw_errors_total{error_type="Cancelled"}`. A response the REST handler fails to send after the Data Plane returned it (a 101 without its tunnel, a client connection that cannot upgrade) stays counted with the Data Plane's status (label keys per OTel HTTP semconv; method normalized; `credential_mode` per ADR-0022) - `inst-mc-7`
9. [ ] - `p2` - Observe request duration in `oagw_request_duration_seconds{host, path, phase}` histogram using configured buckets (`phase`: `total` from request start to the response head or the gateway error, not to the end of the body; `resolve` for the upstream and route lookup with the proxy configuration cache on or off; `auth`) - `inst-mc-8`
10. [ ] - `p2` - When the response body ends, fails or is dropped, observe `oagw_stream_duration_seconds{host, http.route, outcome}` from request start (`outcome`: `completed` at the end of the body, or once its `Content-Length` went through; `error` for an upstream failure or an idle timeout; `shutdown` for a gateway stop; `cancelled` when the client went away). Every upstream body is relayed as a stream, SSE or not; a response without a body (101, HEAD, 204, 304, `Content-Length: 0`) records none. A WebSocket session records `oagw_websocket_session_duration_seconds{host, outcome}` at teardown instead (`completed` when the tunnel closed, from either side; `error` on an idle timeout or an IO error; `shutdown` for a gateway stop; `cancelled` when the bridge task was dropped or panicked) - `inst-mc-8b`
11. [ ] - `p2` - **IF** rate limit was evaluated - `inst-mc-9`
    1. [ ] - `p2` - Update `oagw_rate_limit_usage_ratio{host, http.route, bucket}` gauge with the token ratio (0.0–1.0) of each bucket checked (`bucket`: `route`, `shadowed_route`, `upstream`, `pool`; the shadowed ancestor routes of one route share the `shadowed_route` series, written once per request with the highest ratio among them) - `inst-mc-9a`
    2. [ ] - `p2` - **IF** rate limit exceeded, increment `oagw_rate_limit_exceeded_total{host, http.route, bucket}` counter for the rejecting bucket - `inst-mc-9b`
12. [x] - `p2` - **IF** circuit breaker state changed - `inst-mc-10`
    1. [x] - `p2` - Move one unit of `resilience.circuit_breaker.state{resilience.name, resilience.state}` (up-down counter, `closed` / `open` / `half_open`) from the old state to the new one; `resilience.name` is `{alias}/{upstream_id}`, with `/{owner_id}` when the effective config is another upstream's (aliases alone repeat across tenants) - `inst-mc-10a`
    2. [x] - `p2` - Increment `resilience.circuit_breaker.transitions{resilience.name, resilience.previous_state, resilience.state}` - `inst-mc-10b`
    3. [x] - `p2` - Every call and refusal increments `resilience.circuit_breaker.calls{resilience.name, resilience.outcome}` (`success`, `failure`, `ignored`, `rejected`) - `inst-mc-10c`
13. [ ] - `p2` - **IF** multi-endpoint upstream - `inst-mc-11`
    1. [ ] - `p2` - Increment `oagw_routing_endpoint_selected{upstream_id, endpoint_host, selection_method}` counter - `inst-mc-11a`
    2. [ ] - `p2` - **IF** X-OAGW-Target-Host header was used, increment `oagw_routing_target_host_used{upstream_id, endpoint_host}` counter - `inst-mc-11b`
14. [ ] - `p2` - Update `oagw_upstream_available{host, endpoint}` gauge based on response outcome (0=down, 1=up) - `inst-mc-12`
15. [ ] - `p2` - Update `oagw_upstream_connections{host, state}` gauge with current connection pool state - `inst-mc-13`
16. [ ] - `p2` - **RETURN** void (metrics updated in-place via Prometheus registry) - `inst-mc-14`

### Audit Log Emission

**Planned for [upstream credentials](../ADR/0021-upstream-credentials.md#errors-and-observability).** Audit create, replace, delete, suspend and resume, including repeated suspends and registry changes. Record actor/source, tenant, credential UUID, action, UTC time and result. Keep secret values, references, pins and auth headers out of audit data.

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-obs-audit-logging`

**Input**: Completed proxy request context (request metadata, response metadata, error if any)

**Output**: Structured JSON log line emitted to stdout

**Steps**:
1. [ ] - `p2` - Extract log fields: timestamp, request_id, trace_id, tenant_id, principal_id, host, path, method, status, duration_ms, request_size, response_size - `inst-al-1`
2. [ ] - `p2` - **IF** request failed, add error_type and error_message fields - `inst-al-2`
3. [ ] - `p2` - Determine log level based on outcome - `inst-al-3`
   1. [ ] - `p2` - **IF** status 2xx/3xx → INFO - `inst-al-3a`
   2. [ ] - `p2` - **IF** rate limit exceeded or circuit breaker open → WARN - `inst-al-3b`
   3. [ ] - `p2` - **IF** upstream failure, timeout, or auth failure → ERROR - `inst-al-3c`
4. [ ] - `p2` - Scrub fields: never include request/response bodies, query parameters, or non-allowlisted headers - `inst-al-4`
5. [ ] - `p2` - Scrub secrets: never include API keys, tokens, credentials, or secret_ref values - `inst-al-5`
6. [ ] - `p2` - **IF** high-frequency route (sampling enabled) - `inst-al-6`
   1. [ ] - `p2` - Apply sampling rate (e.g., 1/100) to decide whether to emit this log entry - `inst-al-6a`
   2. [ ] - `p2` - **IF** sampled out, skip emission - `inst-al-6b`
7. [ ] - `p2` - Serialize log entry as structured JSON - `inst-al-7`
8. [ ] - `p2` - Emit to stdout - `inst-al-8`
9. [ ] - `p2` - **RETURN** void - `inst-al-9`

### CORS Handler

Preflight returns a permissive 204 at the handler level (no upstream resolution). Origin validation happens on actual requests after upstream resolution. See [ADR: CORS](../ADR/0006-cors.md).

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-obs-cors-handler`

**Input**: Inbound HTTP request (preflight or actual), upstream/route CORS configuration (actual requests only)

**Output**: CORS preflight response (for OPTIONS) or CORS headers added to proxy response

**Steps**:

**Preflight path** (handler level, no upstream resolution):
1. [ ] - `p2` - **IF** request is CORS preflight (OPTIONS + Origin + Access-Control-Request-Method) - `inst-cors-h-1`
   1. [ ] - `p2` - Return permissive 204 echoing the request's origin, method, and headers - `inst-cors-h-1a`
   2. [ ] - `p2` - **RETURN** 204 No Content with permissive CORS headers (no upstream resolution, no tenant context required) - `inst-cors-h-1b`

**Actual request path** (after upstream resolution in Data Plane service):
2. [ ] - `p2` - **IF** no CORS configuration on upstream or route - `inst-cors-h-2`
   1. [ ] - `p2` - Skip CORS processing; CORS is disabled by default (secure default) - `inst-cors-h-2a`
   2. [ ] - `p2` - **RETURN** request unchanged - `inst-cors-h-2b`
3. [ ] - `p2` - Extract `Origin` header from inbound request - `inst-cors-h-3`
4. [ ] - `p2` - **IF** Origin header absent - `inst-cors-h-4`
   1. [ ] - `p2` - Skip CORS processing (not a cross-origin request) - `inst-cors-h-4a`
   2. [ ] - `p2` - **RETURN** request unchanged - `inst-cors-h-4b`
5. [ ] - `p2` - Match Origin against configured `allowed_origins` - `inst-cors-h-5`
6. [ ] - `p2` - **IF** Origin not in allowed list - `inst-cors-h-6`
   1. [ ] - `p2` - **RETURN** 403 with CORS origin not allowed error (request not forwarded to upstream) - `inst-cors-h-6a`
7. [ ] - `p2` - Forward request to upstream - `inst-cors-h-7`
8. [ ] - `p2` - Add `Access-Control-Allow-Origin` to response headers after upstream call - `inst-cors-h-8`
   1. [ ] - `p2` - Add `Access-Control-Expose-Headers` if configured - `inst-cors-h-8a`
   2. [ ] - `p2` - **RETURN** response with CORS headers - `inst-cors-h-8b`

### SSRF Protection Validation

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-obs-ssrf-protection`

**Input**: Upstream endpoints (scheme, host, port) on write and on proxy; the addresses an endpoint host resolves to on connect; inbound request headers, path and query

**Output**: Validation result (pass or reject with error)

The SSRF policy is the gear option `ssrf_policy` (on by default; `enabled: false` skips steps 3 and 4). DESIGN lists the denied ranges and how IPv4 and IPv6 CIDRs match each address family ([Security Considerations](../DESIGN.md#security-considerations)).

**Steps**:
1. [x] - `p1` - Validate the endpoint scheme on upstream create, update and registry reconcile, and again in the proxy before the auth plugin runs, for upstreams stored earlier - `inst-ssrf-1`
2. [x] - `p1` - **IF** the scheme is `http` and the gear option `allow_http_upstream` is off (the default: HTTPS-only) - `inst-ssrf-2`
   1. [x] - `p1` - **RETURN** reject with 400 `invalid_argument`, field `server.endpoints.scheme`, reason `HTTP_UPSTREAM_FORBIDDEN` - `inst-ssrf-2a`
3. [x] - `p1` - On write, check every endpoint host: a denied hostname (`localhost`, `localhost.localdomain`, `extra_deny_hostnames`; not in `allow_hostnames`) or an IP address in a denied range (built-in list, `extra_deny_cidrs`; not in `allow_cidrs`) is rejected with 400: "endpoint[i] IP address '…' is blocked by SSRF protection: <reason>" or "endpoint[i] hostname '…' is blocked by SSRF protection (matches '…')". The hostname check runs again on every alias resolution - `inst-ssrf-3`
4. [x] - `p1` - On connect, check every address the endpoint host resolves to, on every resolution (DNS rebinding); IP literals are checked the same way - `inst-ssrf-4`
   1. [x] - `p1` - **IF** the address is in a denied range and not in `allow_cidrs`; an `allow_hostnames` entry lifts the deny for its hostname's addresses, except loopback, unspecified, link-local, multicast, broadcast and cloud metadata addresses - `inst-ssrf-4a`
      1. [x] - `p1` - Skip the address and log the reason; **IF** every address is refused, **RETURN** an upstream connection error without connecting (503 for a single endpoint, 502 for a pool) - `inst-ssrf-4a1`
5. [x] - `p1` - Strip well-known internal headers from outbound request - `inst-ssrf-5`
   1. [x] - `p1` - Remove: `Forwarded`, `Via`, `X-Forwarded*`, `X-Envoy-*`, `X-ProxyUser-Ip`, `*Client-IP`, `X-Real-IP`, `X-Originating-IP`, `X-Remote-IP`, `X-Remote-Addr`, `CF-Connecting-IP`, `X-Original-Forwarded-For`, URL/host override (`X-Original-URL`, `X-Original-URI`, `X-Rewrite-URL`, `X-Original-Host`, `X-Host`) and method override (`X-HTTP-Method-Override`, `X-HTTP-Method`, `X-Method-Override`) headers (unless explicitly allowed: the upstream's `passthrough_allowlist` names the header; `passthrough: all` does not count) - `inst-ssrf-5a`
6. [x] - `p1` - Validate request path against route configuration: resolve literal `.` and `..` segments before route matching, and reject with 400 a segment that is a dot-segment once percent-decoded (`%2e%2e`, `..%2f`, `..\\`, `..;`, double encoding, non-UTF-8 escapes) - `inst-ssrf-6`
7. [x] - `p1` - Validate query parameters against the route's `query_allowlist`: a parameter it does not name is rejected with 400, reason `QUERY_NOT_ALLOWED` - `inst-ssrf-7`
8. [x] - `p1` - **RETURN** pass (request is safe to forward) - `inst-ssrf-8`

### HTTP Smuggling Prevention

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-obs-http-smuggling`

**Input**: Inbound HTTP request (headers, body stream)

**Output**: Validation result (pass or reject with error)

**Steps**:
1. [ ] - `p2` - Scan all header lines for bare CR (`\r` without `\n`) or bare LF (`\n` without preceding `\r`); reject with 400 ValidationError: "Malformed header line (bare CR or LF)" if found - `inst-hsmug-1`
2. [ ] - `p2` - Extract Content-Length and Transfer-Encoding headers from inbound request - `inst-hsmug-2`
3. [ ] - `p2` - **IF** both Content-Length and Transfer-Encoding are present - `inst-hsmug-3`
   1. [ ] - `p2` - **RETURN** reject with 400 ValidationError: "Ambiguous framing: request contains both Content-Length and Transfer-Encoding" - `inst-hsmug-3a`
4. [ ] - `p2` - **IF** Transfer-Encoding is present - `inst-hsmug-4`
   1. [ ] - `p2` - **IF** Transfer-Encoding value (case-insensitive, trimmed) is not exactly `chunked` - `inst-hsmug-4a`
      1. [ ] - `p2` - **RETURN** reject with 400 ValidationError: "Unsupported Transfer-Encoding value; only 'chunked' is allowed" - `inst-hsmug-4a1`
5. [ ] - `p2` - **IF** Content-Length is present - `inst-hsmug-5`
   1. [ ] - `p2` - Parse Content-Length value as a non-negative integer (no leading zeros, no whitespace, no sign) - `inst-hsmug-5a`
   2. [ ] - `p2` - **IF** parse fails - `inst-hsmug-5b`
      1. [ ] - `p2` - **RETURN** reject with 400 ValidationError: "Content-Length is not a valid non-negative integer" - `inst-hsmug-5b1`
   3. [ ] - `p2` - After body is fully received, verify actual body size matches declared Content-Length - `inst-hsmug-5c`
   4. [ ] - `p2` - **IF** mismatch - `inst-hsmug-5d`
      1. [ ] - `p2` - **RETURN** reject with 400 ValidationError: "Body size does not match Content-Length" - `inst-hsmug-5d1`
6. [ ] - `p2` - **RETURN** pass (request framing is unambiguous and well-formed) - `inst-hsmug-6`

### Proxy Configuration Cache

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-obs-proxy-config-cache`

With a database and `proxy_cache.enabled: true` ([ADR: Proxy Configuration Cache](../ADR/0020-proxy-config-cache.md)). Without a database, the in-memory repositories are read directly.

**Input**: Alias section: normalized alias + tenant chain. Route section: the tenant and ID of each upstream whose routes are needed (the selected upstream and its merge chain)

**Output**: The raw repository answer without tags: every upstream with the alias in the chain (disabled ones included; empty for "not found"), or each upstream's enabled HTTP routes in full. Upstream selection, the SSRF check, route selection and the merge run per request on this output.

**Eviction policy**: An entry older than `proxy_cache.ttl` is stale: it is reloaded on use and kept to answer if the reload fails, until it is older than `proxy_cache.ttl` plus `proxy_cache.max_stale`, when it is removed. Emptying the whole cache marks every entry stale (with `max_stale: 0s` it removes them). When a section holds `proxy_cache.max_entries`, the least recently used entry is evicted. A configuration change evicts the entries it affects (`cpt-cf-oagw-algo-obs-proxy-cache-change`).

**Steps**:
1. [ ] - `p2` - Look up the key in its section - `inst-cache-1`
2. [ ] - `p2` - **IF** an entry is present, younger than the TTL and not marked stale - `inst-cache-2`
   1. [ ] - `p2` - **RETURN** the cached value (no database query) - `inst-cache-2a`
3. [ ] - `p2` - **IF** a stale entry is present and a reload of it failed less than 5s ago - `inst-cache-st-1`
   1. [ ] - `p2` - **RETURN** the stale entry (no database query); count it in `oagw_proxy_cache_stale_served_total` (`retry_window`) - `inst-cache-st-1a`
4. [ ] - `p2` - **IF** a load for the key is in flight - `inst-cache-3`
   1. [ ] - `p2` - **RETURN** that load's result, error included, or, with a stale entry, as in steps 6 and 7 - `inst-cache-3a`
5. [ ] - `p2` - DB: one SELECT for the key (alias: the upstreams with the alias across the chain; route: the enabled HTTP routes of every missing upstream at once) - `inst-cache-4`
6. [ ] - `p2` - **IF** the request has a stale entry and the load has not answered 2s after it started - `inst-cache-st-2`
   1. [ ] - `p2` - **RETURN** the stale entry (`slow`); the load runs on and is stored as below - `inst-cache-st-2a`
7. [ ] - `p2` - **IF** the load fails - `inst-cache-5`
   1. [ ] - `p2` - **RETURN** the stale entry to every waiting request that has one (`error`), keep it, and answer it without a reload for 5s; log one warning, at most one per section every 30s - `inst-cache-st-3`
   2. [ ] - `p2` - **RETURN** the error to every other request waiting on the load; nothing is stored - `inst-cache-5a`
8. [ ] - `p2` - **IF** no configuration change detached the load while it ran - `inst-cache-6`
   1. [ ] - `p2` - Store the entry and index it by the upstream or route IDs it holds; evict the least recently used entry when the section is full - `inst-cache-6a`
9. [ ] - `p2` - **ELSE** count the load in `oagw_proxy_cache_loads_discarded_total`; do not store it - `inst-cache-7`
10. [ ] - `p2` - **RETURN** the loaded value - `inst-cache-8`

### Proxy Configuration Change

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-obs-proxy-cache-change`

**Input**: A configuration write (upstream or route create, update, delete) from the Management API, the SDK or the registry reconcile, on a database

**Output**: Every replica's cache holds no entry the write made stale, or, when a replica cannot know what it missed, an empty cache

**Steps**:
1. [ ] - `p2` - Build the change from the repository call's arguments - `inst-cache-chg-1`
2. [ ] - `p2` - Run the write; when it ends (succeeded, failed or dropped mid-call), evict the entries the change affects and detach the in-flight loads of each section it touches; if dropped mid-call, apply and publish the change once more 30s later, in case the statement commits late - `inst-cache-chg-2`
3. [ ] - `p2` - Append the change to the cluster change log (compare-and-swap, up to 5 attempts of at most 1s each; a newer-format log is left alone) and wait at most 2s; a registry reconcile run waits once, at its end - `inst-cache-chg-3`
4. [ ] - `p2` - **IF** the append fails, times out, meets a newer-format log, or the publisher has stopped - `inst-cache-chg-4`
   1. [ ] - `p2` - Log WARN naming the change; once its fate is known, count it once in `oagw_proxy_cache_publish_failures_total{reason}` (`append_failed`, `newer_log`, `publisher_stopped`; a timed-out wait is not counted); the write still succeeds, and other replicas see the change within the TTL - `inst-cache-chg-4a`
5. [ ] - `p2` - On every other replica, the watcher reads the log and applies, in order, each newer change another replica made - `inst-cache-chg-5`
6. [ ] - `p2` - **IF** the log has a gap, a new ID or cannot be read, or the watch reports `Lagged`, `Reset`, `Deleted` or `Expired` - `inst-cache-chg-6`
   1. [ ] - `p2` - Empty the whole cache. After a gap, a new ID, `Lagged` or `Reset`, follow the log from its current end; after an unreadable log (or failed read), `Deleted` or `Expired`, keep no baseline, so the next log read empties the cache once more and becomes the baseline - `inst-cache-chg-6a`

## 4. States (CDSL)

Not applicable — this feature implements pipeline-integrated cross-cutting concerns (metrics, logging, CORS validation, SSRF protection, caching) that do not introduce new entity lifecycle states. Circuit breaker state is owned by `cpt-cf-oagw-feature-rate-limiting`.

## 5. Definitions of Done

### Implement Prometheus Metrics

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-obs-prometheus-metrics`

The system **MUST** expose these OAGW Prometheus metrics at the admin `/metrics` endpoint:
- Counters: `oagw_requests_total`, `oagw_errors_total`, `oagw_rate_limit_exceeded_total`, `oagw_rate_limit_buckets_evicted_total`, `resilience.circuit_breaker.transitions`, `resilience.circuit_breaker.calls`, `oagw_routing_target_host_used`, `oagw_routing_endpoint_selected`, `oagw_registry_provisioning_failures_total{kind, reason}`, `oagw_registry_provisioning_runs_total{outcome}`, `oagw_credentials_withheld_total{host, outcome}`
- Histograms: `oagw_request_duration_seconds` with buckets `[0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]`, `oagw_stream_duration_seconds{host, http.route, outcome}`, `oagw_websocket_session_duration_seconds{host, outcome}`
- Gauges: `oagw_requests_in_flight`, `resilience.circuit_breaker.state` (up-down counter), `oagw_rate_limit_usage_ratio`, `oagw_rate_limit_buckets`, `oagw_upstream_available`, `oagw_upstream_connections`, `oagw_active_websocket_sessions{host}`

`oagw_requests_total` counts every proxy request with a response under its status: the upstream's status, or for a gateway error (401, 403, 429, 502, ...) the status the client gets, so a 5xx-rate query covers gateway 5xx too. `oagw_errors_total` counts the gateway errors again by `error_type`, plus `Cancelled` for a request the client dropped before its response head; such a request has no status and is not in `oagw_requests_total`. Rejections the REST handler makes after `proxy:invoke` is granted and before the Data Plane runs (proxy path, WebSocket upgrade, body size and body read checks) are counted the same way, under `host` and `http.route` `unknown`; a granted request dropped before the Data Plane runs (the client gone mid-upload, the handler dropped by the request timeout) counts as `Cancelled`. Their `phase="total"` is the `proxy:invoke` check, as for a proxied request: the client's upload is not included. A response the REST handler fails to send after the Data Plane returned it (a 101 without its tunnel, a client connection that cannot upgrade) stays counted with the Data Plane's status.

`oagw_request_duration_seconds` records the phases `total` (request start to the response head, or to the gateway error; the body is not included), `resolve` (the upstream and route lookup, with the proxy configuration cache on or off, so the two can be compared) and `auth`. `oagw_stream_duration_seconds` records request start to the end of the response body, by `outcome`: `completed` (the body ended, or its `Content-Length` went through: hyper's HTTP/1.1 server drops such a body there without reading its end), `error` (upstream failure, idle timeout), `shutdown` (gateway stop, so a rolling deploy is not an error spike) or `cancelled` (the client went away); a response without a body (101, HEAD, 204, 304, `Content-Length: 0`) records none. `oagw_websocket_session_duration_seconds` records a session's lifetime by the same outcomes: `completed` when the tunnel closed, from either side (the relay does not parse frames, and after a client-initiated Close the server closes TCP first), `error` on an idle timeout or an IO error, `shutdown` on a gateway stop, `cancelled` when the bridge task was dropped or panicked.

`oagw_requests_in_flight` counts a request from the start of the Data Plane pipeline until its response body ends, fails or is dropped, SSE and other streamed bodies included; under `host` `unknown` until the upstream is resolved.

`oagw_rate_limit_usage_ratio` and `oagw_rate_limit_exceeded_total` carry a `bucket` label: `route`, `shadowed_route`, `upstream` or `pool`, one series per bucket checked; the shadowed ancestor routes of one route share the `shadowed_route` series, which gets the highest ratio among them once per request (1.0 when one rejects), so an ancestor near its limit is not hidden by the root.

With a database, the system **MUST** also expose the 8 proxy configuration cache metrics ([ADR: Proxy Configuration Cache](../ADR/0020-proxy-config-cache.md#health-and-metrics)):
- Counters: `oagw_proxy_cache_lookups_total{map, result}`, `oagw_proxy_cache_evictions_total{map, reason}`, `oagw_proxy_cache_changes_total{source}`, `oagw_proxy_cache_flushes_total{cause}`, `oagw_proxy_cache_loads_discarded_total{map}`, `oagw_proxy_cache_stale_served_total{map, cause}`, `oagw_proxy_cache_publish_failures_total{reason}`
- Gauges: `oagw_proxy_cache_entries{map}`

Cardinality **MUST** be controlled: no tenant labels; the circuit breaker's `resilience.name` is the one per-upstream label (one set of series per upstream with an enabled breaker, kept until restart), which a deployment with many tenant upstreams drops or aggregates in the collector; `http.route` is the normalized route match pattern from route config; `http.request.method` is normalized to a standard verb or `_OTHER`; `http.response.status_code` is the numeric upstream status (OTel HTTP semconv) — status-class queries (e.g. `5xx` rate) are expressed at query time over the numeric code. Label-key vocabulary matches the inbound API Gateway so both gateways share dashboards.

**Implements**:
- `cpt-cf-oagw-algo-obs-metrics-collection`
- `cpt-cf-oagw-flow-obs-review-metrics`

**Touches**:
- API: `GET /metrics`
- Entities: proxy pipeline instrumentation points

### Implement Structured Audit Logging

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-obs-audit-logging`

The system **MUST** emit structured JSON logs to stdout for all proxy requests. Log entries **MUST** include: `timestamp`, `level`, `event`, `request_id`, `trace_id`, `tenant_id`, `principal_id`, `host`, `path`, `method`, `status`, `duration_ms`, `request_size`, `response_size`. Failed requests **MUST** additionally include `error_type` and `error_message`.

The `trace_id` field **MUST** be propagated from inbound W3C Trace Context (`traceparent` header) when present, or generated as a new identifier otherwise, to support distributed tracing correlation.

Log levels **MUST** follow: INFO (success), WARN (rate limit/circuit breaker), ERROR (failures/timeouts).

The system **MUST NOT** log request/response bodies, query parameters, non-allowlisted headers, API keys, tokens, credentials, or secret_ref values.

The system **MUST** support high-frequency sampling to prevent excessive log volume on high-traffic routes.

Log retention is an infrastructure concern (centralized logging system). This feature assumes a minimum 30-day retention policy for audit logs; operators configure retention in their logging infrastructure.

**Implements**:
- `cpt-cf-oagw-algo-obs-audit-logging`

**Touches**:
- Entities: proxy pipeline post-processing

### Implement CORS Handling

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-obs-cors-handling`

The system **MUST** provide built-in CORS handling configurable per upstream and per route. Preflight OPTIONS requests **MUST** be handled locally without upstream round-trip. CORS **MUST** be disabled by default (secure default); only configured origins are allowed.

Configuration **MUST** support: `allowed_origins`, `allowed_methods`. Origin matching **MUST** reject unrecognized origins with no CORS headers in the response.

**Implements**:
- `cpt-cf-oagw-algo-obs-cors-handler`
- `cpt-cf-oagw-flow-obs-configure-cors`

**Touches**:
- API: `PUT /api/oagw/v1/upstreams/{id}`, `PUT /api/oagw/v1/routes/{id}`
- DB: `oagw_upstream`, `oagw_route` (cors config field)
- Entities: `Upstream`, `Route`

### Implement SSRF Protection

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-obs-ssrf-protection`

The system **MUST** enforce SSRF protection on all outbound requests:
- Scheme allowlist: HTTPS-only unless `allow_http_upstream` is on (`cpt-cf-oagw-constraint-https-only`), checked on write and in the proxy
- Write-time check: an endpoint whose hostname is denied or whose IP address is in a denied range is rejected with 400 unless `allow_hostnames` / `allow_cidrs` exempts it
- Connect-time check: every resolved address is checked on every resolution, and a refused one is never connected to; `allow_hostnames` never exempts loopback, link-local or metadata addresses
- Header stripping: remove forwarding and override headers (`Forwarded`, `X-Forwarded-*`, `X-Real-IP` and the rest of `inst-ssrf-5a`) received from the client unless the upstream's `passthrough_allowlist` names them
- Path validation: reject path traversal sequences (`../`, `..\\`, encoded variants)
- Query validation: reject query parameters the route's `query_allowlist` does not name

**Implements**:
- `cpt-cf-oagw-algo-obs-ssrf-protection`

**Touches**:
- Entities: proxy pipeline pre-forward validation

### Implement HTTP Smuggling Prevention

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-obs-http-smuggling`

The system **MUST** enforce strict HTTP parsing to prevent smuggling attacks:
- Reject requests containing bare CR or LF characters in headers
- Validate Content-Length / Transfer-Encoding combinations: reject requests with both CL and TE headers (ambiguous)
- Reject unsupported Transfer-Encoding values (only `chunked` supported)
- Enforce Content-Length as valid integer matching actual body size

**Implements**:
- `cpt-cf-oagw-algo-obs-http-smuggling`

**Touches**:
- Entities: proxy pipeline request validation

### Implement the Proxy Configuration Cache

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-obs-proxy-config-cache`

With a database, the system **MUST** serve the proxy lookup from a per-replica in-memory cache ([ADR: Proxy Configuration Cache](../ADR/0020-proxy-config-cache.md)):
- Two sections: upstream candidates by alias and tenant chain, and enabled HTTP routes by upstream; both hold raw repository answers, so results equal those of an uncached lookup
- A hit runs no database query; concurrent misses on one key run one load; errors are never stored
- Each entry expires after `proxy_cache.ttl`; a full section evicts its least recently used entry
- Every configuration write evicts only the entries it affects, on the writing replica at once and on every other replica through the cluster change log; a replica that cannot know what it missed empties its cache
- The database stays the only source of truth: the cluster gear holds only the change log

A failed or slow change log append **MUST NOT** fail the write: it is logged at WARN and each change not appended is counted once. A change log watch that is not up **MUST NOT** fail proxying: health is `Degraded` (`proxy_cache_watch_connecting` while connecting or reconnecting, `proxy_cache_watch_down` once down) and freshness falls back to the TTL. A newer-format change log, which the replica's appends skip, reports `Degraded` (`proxy_cache_newer_log`). A cluster still starting, or an `oagw` profile whose backend is degraded, **MUST NOT** fail proxying either: health is `Degraded`; only a cluster misconfiguration is `Unhealthy`. A database outage **MUST NOT** stop proxying for cached configuration within `proxy_cache.max_stale` past its TTL: stale entries answer, and health is `Degraded` (`proxy_cache_serving_stale`) for 30 s after each stale answer. A slow cluster readiness check **MUST NOT** hold up the health check: OAGW waits at most 250 ms, reports the last result for up to 10 s, then `Degraded` (`cluster_readiness_slow`). The SDK's lookup with tags and `proxy_cache.enabled: false` read the database every time.

**Implements**:
- `cpt-cf-oagw-algo-obs-proxy-config-cache`
- `cpt-cf-oagw-algo-obs-proxy-cache-change`

**Touches**:
- Entities: `ControlPlaneService`, `UpstreamRepository`, `RouteRepository`, the cluster cache of the `oagw` profile

## 6. Acceptance Criteria

- [ ] The OAGW Prometheus metrics of `cpt-cf-oagw-dod-obs-prometheus-metrics` are exposed at `/metrics` in Prometheus exposition format, plus the 8 `oagw_proxy_cache_*` metrics with a database
- [ ] Every proxy request with a response is counted in `oagw_requests_total` with its status, gateway errors and the REST handler's own rejections included
- [ ] `oagw_requests_in_flight` holds a request from its start until its response body ends, fails or is dropped
- [ ] `oagw_request_duration_seconds{phase="total"}` ends at the response head; `oagw_stream_duration_seconds` ends with the body and carries its `outcome`
- [ ] Each rate-limit bucket has its own `oagw_rate_limit_usage_ratio` and `oagw_rate_limit_exceeded_total` series (`bucket` label)
- [ ] `oagw_request_duration_seconds` histogram uses the specified 12-bucket configuration, and records `phase="resolve"` for the upstream and route lookup
- [ ] No metric label includes tenant_id (cardinality control)
- [ ] Metric paths are normalized from route config, not raw request paths
- [ ] Structured JSON audit logs are emitted to stdout for every proxy request
- [ ] Audit logs never contain request/response bodies, query parameters, API keys, tokens, or credentials
- [ ] Log levels follow policy: INFO for success, WARN for rate limit/circuit breaker, ERROR for failures
- [ ] High-frequency route sampling reduces log volume without losing visibility
- [ ] CORS preflight OPTIONS requests return permissive 204 at handler level without upstream resolution (see [ADR: CORS](../ADR/0006-cors.md))
- [ ] CORS is disabled by default; only explicitly configured origins are allowed
- [ ] Unrecognized origins on actual requests are rejected with 403 before reaching the upstream
- [x] HTTPS-only scheme enforcement rejects `http` endpoints on write and in the proxy unless `allow_http_upstream` is on
- [x] An endpoint IP address in a denied range is rejected on write with 400 naming the range (unless `allow_cidrs` exempts it)
- [x] Resolved addresses in a denied range are refused on connect (unless explicitly allowed)
- [x] Forwarding and override headers from the client are stripped from outbound requests
- [x] Path traversal sequences are rejected
- [ ] Requests with ambiguous CL/TE combinations are rejected
- [ ] Bare CR/LF characters in headers are rejected
- [ ] With a database, a repeated proxy lookup is served from the proxy configuration cache with no database query, and returns what an uncached lookup returns
- [ ] A configuration write evicts only the cache entries it affects, on the writing replica before the write answers and on every other replica once the change is delivered through the cluster change log
- [ ] A replica that misses changes (gap, new or unreadable log, `Lagged`, `Reset`) empties its cache; a lost change is corrected within `proxy_cache.ttl`
- [ ] With the database unavailable, cached lookups resolve, past their TTL for up to `proxy_cache.max_stale` from their stale entries (a failed key is reloaded again only after 5s), and a miss with nothing cached returns the error without caching it
- [ ] With the database slow, a request with a stale entry answers it 2s after the entry's reload started
- [ ] Stale answers are counted in `oagw_proxy_cache_stale_served_total` by `cause`, and health is `Degraded` with `proxy_cache_serving_stale` for 30s after one
- [ ] When the change log append fails, the write succeeds and each change not appended is logged and counted once by `reason`; health is `Degraded` with `proxy_cache_watch_connecting`, `proxy_cache_watch_down` or `proxy_cache_newer_log` in the matching state, and with a cluster still starting or a degraded `oagw` profile; a cluster readiness check slower than 250 ms leaves the last result, then `cluster_readiness_slow`
- [ ] At capacity, a section evicts its least recently used entry
- [ ] Audit log entries include `trace_id` field for distributed tracing correlation
- [ ] W3C Trace Context (`traceparent` header) is propagated from inbound requests when present

