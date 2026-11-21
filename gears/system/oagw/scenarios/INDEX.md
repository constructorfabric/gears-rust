# OAGW Scenario Guide

Practical companion to [DESIGN.md](../docs/DESIGN.md). Organized in two sections:

1. **How To** — what you CAN do with OAGW, organized by integration journey then by feature
2. **Guardrails** — what is rejected, guarded, or explicitly unsupported

Rules for all scenarios:
- Use Management API to create upstreams/routes/plugins unless scenario explicitly tests invalid config.
- Use Proxy API to invoke.
- Assert both:
  - Response semantics (status, headers, body/stream behavior)
  - Side effects (metrics/audit logs, stored config, plugin lifecycle)

Legend (used in checks):
- `ESrc`: `X-OAGW-Error-Source` header.
- `PD`: RFC 9457 Problem Details (`application/problem+json`).

---

## 1. How To (Common Integration Scenarios)

> **Flow reference**: [Proxy Operations](flows/proxy-operations.md)

### Integration journey

To proxy requests to an external service through OAGW, follow these steps in order. Each step links to the feature section with full scenario coverage.

#### Step 1: Register the upstream

Create an upstream pointing to your external service.

- [Create minimal HTTP upstream](management-api/upstreams/positive-2.1-create-minimal-http-upstream.md) — `POST /upstreams` with host, scheme, port (omitted: 80 for `http`, 443 otherwise). Auto-generates alias from hostname.
- For non-standard ports: [Alias auto-generation](management-api/upstreams/positive-2.2-alias-auto-generation-non-standard-port.md)
- For multiple endpoints: [Multi-endpoint load balancing](management-api/upstreams/positive-2.10-multi-endpoint-load-balancing-distributes-requests.md)
- See → [Upstream management](#upstream-management)

#### Step 2: Define routes

Create routes that map inbound method + path to outbound upstream calls.

- [Create HTTP route with method + path](management-api/routes/positive-3.1-create-http-route-method-path.md) — `POST /routes` with methods, path, upstream_id
- Configure path suffix mode: [append](management-api/routes/positive-3.5-path-suffix-mode-append.md) for REST prefixes
- For gRPC: [Create gRPC route by service+method](management-api/routes/positive-3.8-create-grpc-route-service-method.md)
- See → [Route configuration](#route-configuration)

#### Step 3: Attach authentication

Configure how OAGW authenticates to the external service.

- [API key injection](proxy-api/authentication/positive-9.2-api-key-injection.md) — header-based key injection from `cred_store`
- [OAuth2 client credentials](proxy-api/authentication/positive-9.5-oauth2-client-credentials.md) — automatic token fetch + cache + refresh
- [AWS SigV4](proxy-api/authentication/positive-9.11-aws-sigv4-signed-request.md) — Signature Version 4 with IAM access keys (Bedrock, S3, other AWS APIs)
- [Noop (public APIs)](proxy-api/authentication/positive-9.1-noop-auth-plugin-forwards-credential-injection.md) — no credential injection
- See → [Injecting authentication](#injecting-authentication)

#### Step 4: Invoke via proxy

Call your external service through OAGW's proxy endpoint: `{METHOD} /api/oagw/v1/proxy/{alias}/{path_suffix}`

- [Plain HTTP passthrough](protocols/http/positive-12.1-plain-http-request-response-passthrough.md)
- [SSE streaming](protocols/sse/positive-13.1-sse-stream-forwarded-buffering.md)
- [WebSocket upgrade](protocols/websocket/positive-14.1-websocket-upgrade-proxied.md)
- [gRPC unary](protocols/grpc/positive-15.1-grpc-unary-request-proxied.md)
- See → [E2E protocol examples](#e2e-protocol-examples)

#### Optional: Rate limits, plugins, transforms

- [Token bucket rate limiting](rate-limiting/positive-18.1-token-bucket-sustained-burst.md) — see [Rate limiting](#rate-limiting)
- [Request path rewrite](plugins/transforms/positive-11.1-request-path-rewrite-transform.md) — see [Transform plugins](#transform-plugins)
- [Built-in CORS handling](plugins/guards/positive-10.2-built-cors-handling.md) — see [Guard plugins](#guard-plugins)
- [Tags for discovery](management-api/upstreams/positive-2.9-tags-support-discovery-filtering.md) — see [Upstream management](#upstream-management)

---

### Upstream management

> **Flow references**: [Management Operations](flows/management-operations.md) | [Cache Invalidation](flows/cache-invalidation.md)

#### Create a minimal HTTP upstream (single endpoint)
- **Scenario**: [positive-2.1-create-minimal-http-upstream.md](management-api/upstreams/positive-2.1-create-minimal-http-upstream.md)
- **Mechanism**: `POST /upstreams` with server endpoints and protocol. Returns `201` with auto-generated alias and `enabled=true` default. Alias follows hostname rules for standard ports. An omitted port is the scheme's default (80 for `http`, 443 otherwise) and is stored and returned.

#### Alias auto-generation for non-standard port
- **Scenario**: [positive-2.2-alias-auto-generation-non-standard-port.md](management-api/upstreams/positive-2.2-alias-auto-generation-non-standard-port.md)
- **Mechanism**: Endpoint `host=api.example.com, port=8443` yields alias `api.example.com:8443`. Prevents collisions and ambiguous routing.

#### Update upstream configuration
- **Scenario**: [positive-2.5-update-upstream.md](management-api/upstreams/positive-2.5-update-upstream.md)
- **Mechanism**: `PUT /upstreams/{id}` updates mutable fields. Versioning/immutability rules respected. Triggers cache invalidation across CP/DP layers.

#### Delete upstream with route cascade
- **Scenario**: [positive-2.7-delete-upstream-cascades-routes.md](management-api/upstreams/positive-2.7-delete-upstream-cascades-routes.md)
- **Mechanism**: Deleting upstream removes dependent routes (or rejects with clear error if cascade not implemented). Guardrail: subsequent proxy requests return `404 UPSTREAM_NOT_FOUND` or `ROUTE_NOT_FOUND`.

#### Tags support discovery and filtering
- **Scenario**: [positive-2.9-tags-support-discovery-filtering.md](management-api/upstreams/positive-2.9-tags-support-discovery-filtering.md)
- **Mechanism**: Create upstream with `tags` array. Listing/filtering returns expected upstreams by tag. Filtering is not implemented yet: `$filter`, `$orderby` or any other unsupported list parameter returns `400 UNSUPPORTED_QUERY_PARAM`.

#### Multi-endpoint load balancing distributes requests
- **Scenario**: [positive-2.10-multi-endpoint-load-balancing-distributes-requests.md](management-api/upstreams/positive-2.10-multi-endpoint-load-balancing-distributes-requests.md)
- **Mechanism**: Multiple endpoints in one upstream form a pool. Requests distributed round-robin. All endpoints must share same protocol/scheme/port.

#### Re-enable upstream restores proxy traffic
- **Scenario**: [positive-2.11-re-enable-upstream-restores-proxy-traffic.md](management-api/upstreams/positive-2.11-re-enable-upstream-restores-proxy-traffic.md)
- **Mechanism**: `PUT /upstreams/{id}` with `enabled=true` after maintenance. Subsequent proxy requests succeed.

#### List upstreams includes disabled resources
- **Scenario**: [positive-2.12-list-upstreams-includes-disabled-resources.md](management-api/upstreams/positive-2.12-list-upstreams-includes-disabled-resources.md)
- **Mechanism**: `GET /upstreams` returns both enabled and disabled upstreams with `enabled` field. Filtering to active only (`$filter=enabled eq true`) is planned; today it returns `400 UNSUPPORTED_QUERY_PARAM`.

#### CRUD endpoint update invalidates load balancer
- **Scenario**: [positive-2.13-crud-endpoint-update-invalidates-load-balancer.md](management-api/upstreams/positive-2.13-crud-endpoint-update-invalidates-load-balancer.md)
- **Mechanism**: `PUT /upstreams/{id}` with new endpoints invalidates cached LoadBalancer. Next proxy request lazily rebuilds with updated endpoints.

---

### Route configuration

#### Create HTTP route with method + path
- **Scenario**: [positive-3.1-create-http-route-method-path.md](management-api/routes/positive-3.1-create-http-route-method-path.md)
- **Mechanism**: `POST /routes` with `match.http.methods` and `match.http.path`. Returns `201`. Route is associated to the target upstream.

#### Path suffix mode = append (prefix routes for REST resources)
- **Scenario**: [positive-3.5-path-suffix-mode-append.md](management-api/routes/positive-3.5-path-suffix-mode-append.md)
- **Mechanism**: Suffix appended to outbound path. Outbound path normalized (no `//` surprises).

#### Route priority resolves ambiguities
- **Scenario**: [positive-3.6-route-priority-resolves-ambiguities.md](management-api/routes/positive-3.6-route-priority-resolves-ambiguities.md)
- **Mechanism**: With two candidate routes, higher `priority` wins. Deterministic selection.

#### Create gRPC route by service+method
- **Scenario**: [positive-3.8-create-grpc-route-service-method.md](management-api/routes/positive-3.8-create-grpc-route-service-method.md)
- **Mechanism**: `match.grpc.service` + `match.grpc.method` routes to HTTP/2 `:path` `/Service/Method`.

#### Re-enable route restores proxy traffic
- **Scenario**: [positive-3.9-re-enable-route-restores-proxy-traffic.md](management-api/routes/positive-3.9-re-enable-route-restores-proxy-traffic.md)
- **Mechanism**: Route with `enabled=false` is skipped in matching. `PUT /routes/{id}` with `enabled=true` restores matching.

#### List routes includes disabled resources
- **Scenario**: [positive-3.10-list-routes-includes-disabled-resources.md](management-api/routes/positive-3.10-list-routes-includes-disabled-resources.md)
- **Mechanism**: `GET /routes` returns both enabled and disabled routes with `enabled` field.

---

### Plugin management

#### Create custom Starlark guard plugin
- **Scenario**: [positive-4.1-create-custom-starlark-guard-plugin.md](management-api/plugins/positive-4.1-create-custom-starlark-guard-plugin.md)
- **Mechanism**: `POST /plugins` returns `201`. Plugin addressable by anonymous GTS id. Source retrievable via `GET /plugins/{id}/source`.

#### Delete plugin succeeds only when unreferenced
- **Scenario**: [positive-4.3-delete-plugin-succeeds-only-unreferenced.md](management-api/plugins/positive-4.3-delete-plugin-succeeds-only-unreferenced.md)
- **Mechanism**: Referenced plugin deletion returns `409 plugin.in_use`. Unlinked plugin deletion returns `204`.

#### Plugin resolution supports builtin named ids and custom UUID ids
- **Scenario**: [positive-4.5-plugin-resolution-supports-builtin-named-ids-custom-uuid.md](management-api/plugins/positive-4.5-plugin-resolution-supports-builtin-named-ids-custom-uuid.md)
- **Mechanism**: Builtin plugin id works without DB row. A custom UUID id, a catalog-only id or an unknown id is rejected on write (`400 NOT_IMPLEMENTED`); a binding stored before that check fails at proxy time with 401 (auth), 500 (guard) or is skipped (transform); #4985 tracks `503 PLUGIN_NOT_FOUND`.

#### Plugin sharing modes merge correctly across tenant hierarchy
- **Scenario**: [positive-4.6-plugin-sharing-modes-merge-correctly-across-tenant-hierarchy.md](management-api/plugins/positive-4.6-plugin-sharing-modes-merge-correctly-across-tenant-hierarchy.md)
- **Mechanism**: `sharing=inherit` → chain is parent + child. `sharing=enforce` → no child at any depth, and no route (`private` included), can remove an enforced plugin or replace its config with the same `plugin_ref`. `sharing=private` → child does not see parent plugins. An ancestor route's enforced plugins apply at its owner's level: a child enforcing the same `plugin_ref` on its own upstream does not replace the route's config, whether the child's route shadows the parent's or the parent's is matched (`400 REQUIRED_HEADER_MISSING`); the closest-to-root enforcing level wins.

#### Plugin usage tracking and GC eligibility
- **Scenario**: [positive-4.7-plugin-usage-tracking-gc-eligibility-behavior.md](management-api/plugins/positive-4.7-plugin-usage-tracking-gc-eligibility-behavior.md)
- **Mechanism**: `last_used_at` updated on proxy invoke. Unlinked plugins get `gc_eligible_at` set. GC job deletes when `gc_eligible_at < now`.

---

### Injecting authentication

#### Noop auth plugin (public upstreams)
- **Scenario**: [positive-9.1-noop-auth-plugin-forwards-credential-injection.md](proxy-api/authentication/positive-9.1-noop-auth-plugin-forwards-credential-injection.md)
- **Mechanism**: No auth headers added. Use for public APIs.

#### API key injection (header + optional prefix)
- **Scenario**: [positive-9.2-api-key-injection.md](proxy-api/authentication/positive-9.2-api-key-injection.md)
- **Mechanism**: Configure `auth.type` = apikey with header name, prefix, and `secret_ref`. Secret retrieved from `cred_store` at request time. Common for OpenAI, Anthropic, etc.

#### Basic auth injection
- **Scenario**: [positive-9.3-basic-auth-injection.md](proxy-api/authentication/positive-9.3-basic-auth-injection.md)
- **Mechanism**: Correct `Authorization: Basic ...` formatting from secret.

#### Bearer token passthrough/injection
- **Scenario**: [positive-9.4-bearer-token-passthrough-injection.md](proxy-api/authentication/positive-9.4-bearer-token-passthrough-injection.md)
- **Mechanism**: `Authorization: Bearer ...` set from static secret. For service tokens.

#### OAuth2 client credentials (body-based)
- **Scenario**: [positive-9.5-oauth2-client-credentials.md](proxy-api/authentication/positive-9.5-oauth2-client-credentials.md)
- **Mechanism**: Token fetched via OAuth2 flow, cached. On upstream `401`, plugin refreshes token. Plugin does not retry the original request.

#### OAuth2 client credentials (basic-auth variant)
- **Scenario**: [positive-9.6-oauth2-client-credentials.md](proxy-api/authentication/positive-9.6-oauth2-client-credentials.md)
- **Mechanism**: Some token endpoints require `client_id/client_secret` via basic auth. Token request uses correct client authentication.

#### AWS SigV4 signed request
- **Scenario**: [positive-9.11-aws-sigv4-signed-request.md](proxy-api/authentication/positive-9.11-aws-sigv4-signed-request.md)
- **Mechanism**: `aws_sigv4` signs in the signing phase ([ADR-0023](../docs/ADR/0023-request-signing.md)), on the request as sent: `Authorization`, `X-Amz-Date`, `X-Amz-Content-Sha256` and, with `session_token_ref`, `X-Amz-Security-Token`. Signs `host`, every `x-amz-*`, `content-type`, `content-md5`. The query goes out in SigV4 encoding, exactly as signed. A wrong secret reaches the upstream and its `403` is passed through. A missing `region`, `service` or key reference is a `400` naming the key on write.

#### AWS SigV4: client signing headers
- **Scenario**: [positive-9.13-aws-sigv4-client-amz-headers.md](proxy-api/authentication/positive-9.13-aws-sigv4-client-amz-headers.md)
- **Mechanism**: A client's `X-Amz-Date`, `X-Amz-Content-Sha256` and `X-Amz-Security-Token` are replaced by the plugin's; other `x-amz-*` headers are signed as sent. Without `session_token_ref`, a client `X-Amz-Security-Token` cannot be removed by the signer and the request is refused (`400 VALIDATION` on field `x-amz-security-token`); `headers.request.remove` drops it.

#### Per-request secret references (`X-OAGW-Secret-Ref`)
- **Scenario**: [positive-31.1-inject-requested-secret.md](request-secret-refs/positive-31.1-inject-requested-secret.md)
- **Mechanism**: API-key auth with `credential_mode` = `request_ref`. The caller names a CredStore secret per request; OAGW reads it as the caller and injects it when its owner and type pass the upstream's guards. See [Per-request secret references](#per-request-secret-references).

#### Hierarchical auth sharing modes
- **Scenario**: [positive-9.8-hierarchical-auth-sharing-modes-behave-specified.md](proxy-api/authentication/positive-9.8-hierarchical-auth-sharing-modes-behave-specified.md)
- **Mechanism**: `auth.sharing=inherit` + child provides auth → child override. `inherit` + child omits → parent auth. `enforce` → child cannot override. `private` → the parent's credential is never sent for a child, also when the child has no upstream of its own; child must provide own.

---

### Request transforms

#### Inbound → outbound path + query mapping for HTTP
- **Scenario**: [positive-7.1-inbound-outbound-path-query-mapping-http.md](proxy-api/request-transforms/positive-7.1-inbound-outbound-path-query-mapping-http.md)
- **Mechanism**: Outbound path = `route.match.http.path` (+ suffix if enabled). Only allowlisted query params forwarded.

#### Hop-by-hop headers stripped
- **Scenario**: [positive-7.2-hop-hop-headers-stripped.md](proxy-api/request-transforms/positive-7.2-hop-hop-headers-stripped.md)
- **Mechanism**: `Connection`, `Upgrade`, `Transfer-Encoding`, `TE`, etc. do not reach upstream unless explicitly allowed by protocol handler. Under `passthrough: all`, client forwarding and override headers (`Forwarded`, `Via`, `X-Forwarded-*`, `X-Real-IP`, ...) are not forwarded unless the allowlist names them. With an auth plugin, repeated and non-UTF-8 client headers reach the upstream unchanged; only the plugin's header is replaced. A transform plugin rewrites only the header names it changes; the credential and other lines keep their bytes and sensitive flag, request and response.

#### Host header replaced by upstream host
- **Scenario**: [positive-7.3-host-header-replaced-upstream-host.md](proxy-api/request-transforms/positive-7.3-host-header-replaced-upstream-host.md)
- **Mechanism**: Upstream sees correct `Host` header. Prevents host header injection. The port is added unless it is the scheme's default (`http` 80, others 443), so `http:443` and `https:80` keep it.

#### Upstream headers config applies simple transformations
- **Scenario**: [positive-7.5-upstream-headers-config-applies-simple-transformations.md](proxy-api/request-transforms/positive-7.5-upstream-headers-config-applies-simple-transformations.md)
- **Mechanism**: `upstream.headers.request.set` adds/overwrites headers. Header removal rules apply. Invalid header names/values rejected with `400 PD`.

#### Request correlation headers propagate end-to-end
- **Scenario**: [positive-7.6-request-correlation-headers-propagate-end-end.md](proxy-api/request-transforms/positive-7.6-request-correlation-headers-propagate-end-end.md)
- **Mechanism**: Client `X-Request-ID` forwarded to upstream and included in response. If absent, gateway generates one.

---

### Alias resolution and shadowing

#### Alias resolves by walking tenant hierarchy (shadowing)
- **Scenario**: [positive-6.1-alias-resolves-walking-tenant-hierarchy.md](proxy-api/alias-resolution/positive-6.1-alias-resolves-walking-tenant-hierarchy.md)
- **Mechanism**: Child upstream with same alias overrides parent for routing. Closest tenant wins.

#### Upstream sharing via ancestor hierarchy
- **Scenario**: [positive-5.3-upstream-sharing-ancestor-hierarchy.md](proxy-api/authz/positive-5.3-upstream-sharing-ancestor-hierarchy.md)
- **Mechanism**: Ancestor upstream visible to descendant when sharing mode permits. Only its shared fields apply; its `private` configuration (upstream and routes) is neither applied nor returned.

---

### Custom header routing (X-OAGW-Target-Host)

#### Single-endpoint upstream without X-OAGW-Target-Host header
- **Scenario**: [positive-1.1-single-endpoint-no-header.md](proxy-api/custom-header-routing/positive-1.1-single-endpoint-no-header.md)
- **Mechanism**: Single-endpoint upstreams route successfully without the custom header. Backward compatibility preserved.

#### Single-endpoint upstream with valid X-OAGW-Target-Host header
- **Scenario**: [positive-1.2-single-endpoint-with-header.md](proxy-api/custom-header-routing/positive-1.2-single-endpoint-with-header.md)
- **Mechanism**: Header value must match the single endpoint. Header stripped before forwarding.

#### Multi-endpoint explicit alias without header uses round-robin
- **Scenario**: [positive-2.1-multi-endpoint-explicit-alias-no-header.md](proxy-api/custom-header-routing/positive-2.1-multi-endpoint-explicit-alias-no-header.md)
- **Mechanism**: Requests distribute across endpoints via round-robin. No header required for explicit alias.

#### Multi-endpoint explicit alias with header bypasses load balancing
- **Scenario**: [positive-2.2-multi-endpoint-explicit-alias-with-header.md](proxy-api/custom-header-routing/positive-2.2-multi-endpoint-explicit-alias-with-header.md)
- **Mechanism**: Header value selects specific endpoint. Round-robin bypassed.

#### Multi-endpoint common suffix alias with header succeeds
- **Scenario**: [positive-3.1-multi-endpoint-common-suffix-with-header.md](proxy-api/custom-header-routing/positive-3.1-multi-endpoint-common-suffix-with-header.md)
- **Mechanism**: Header disambiguates endpoints with common suffix. Core use case for custom header routing.

#### Case-insensitive X-OAGW-Target-Host matching
- **Scenario**: [positive-3.2-case-insensitive-matching.md](proxy-api/custom-header-routing/positive-3.2-case-insensitive-matching.md)
- **Mechanism**: Mixed case header values match endpoints. DNS convention followed.

#### X-OAGW-Target-Host bypasses round-robin load balancing
- **Scenario**: [positive-3.3-load-balancing-bypass.md](proxy-api/custom-header-routing/positive-3.3-load-balancing-bypass.md)
- **Mechanism**: Header consistently routes to same endpoint. Load balancing state preserved for requests without header.

---

### Rate limiting

#### Token bucket sustained + burst
- **Scenario**: [positive-18.1-token-bucket-sustained-burst.md](rate-limiting/positive-18.1-token-bucket-sustained-burst.md)
- **Mechanism**: Burst allows short spike. Sustained rate enforced. `429` includes `Retry-After` and `X-RateLimit-*` headers.

#### Rate limit response headers can be disabled
- **Scenario**: [positive-18.1.1-rate-limit-response-headers-can-be-disabled.md](rate-limiting/positive-18.1.1-rate-limit-response-headers-can-be-disabled.md)
- **Mechanism**: With `rate_limit.response_headers=false`, success responses omit `X-RateLimit-*`. Error responses still include `Retry-After`.

#### Rate limit scope variants
- **Scenario**: [positive-18.3-rate-limit-scope-variants.md](rate-limiting/positive-18.3-rate-limit-scope-variants.md)
- **Mechanism**: One scenario each for `scope=global`, `tenant`, `user`, `ip`, `route`. `ip` keys on the client address the API gateway resolved (`ClientIp` extension; the peer address unless `trusted_proxy_hops` ≥ 1), `unknown` without it. `route` is one bucket per matched route, on upstream limits too.

#### Weighted cost per route
- **Scenario**: [positive-18.4-weighted-cost-per-route.md](rate-limiting/positive-18.4-weighted-cost-per-route.md)
- **Mechanism**: Route with `cost=10` consumes 10 tokens. Expensive endpoints consume more budget.

#### Hierarchical min() merge for descendant overrides
- **Scenario**: [positive-18.6-hierarchical-min-merge-descendant-overrides.md](rate-limiting/positive-18.6-hierarchical-min-merge-descendant-overrides.md)
- **Mechanism**: Parent `enforce 1000/min`, child `500/min` → effective `500/min`, still `enforce`. Parent caps child and every tenant below it. Route limits are checked on route buckets next to the upstream's (`min(upstream, route)`) and are not merged into it, so calling routes in turn does not refill the upstream bucket. The enforced limit of an ancestor's route that the matched route shadows is checked on that route's own bucket too.

#### Budget modes (allocated/shared/unlimited)
- **Scenario**: [positive-18.7-budget-modes-behave-specified.md](rate-limiting/positive-18.7-budget-modes-behave-specified.md)
- **Mechanism**: `allocated` rejects when the sum of direct allocations exceeds `total * overcommit_ratio`; a parent's change of budget or window is re-checked. `shared` adds one pool bucket of `total` per the owner's window that every tenant drawing on it shares, next to each tenant's own bucket. An upstream whose effective `cost` exceeds the pool's `total` is rejected on write. `unlimited` skips validation.

#### Half-open probes close the circuit on recovery
- **Scenario**: [positive-18.10-half-open-probe-closes-circuit.md](rate-limiting/positive-18.10-half-open-probe-closes-circuit.md)
- **Mechanism**: After `timeout_seconds` an open circuit lets `success_threshold` probes through. All succeed → closed, and failures count from 0 again. One fails → open for another `timeout_seconds`.

#### A changed breaker or a deleted upstream resets the circuit
- **Scenario**: [positive-18.11-config-change-or-delete-resets-circuit.md](rate-limiting/positive-18.11-config-change-or-delete-resets-circuit.md)
- **Mechanism**: A changed effective `circuit_breaker`, alias or endpoint list starts a closed breaker on the next request, on every replica; a write that changes or removes `circuit_breaker`, and a delete, discard the breaker on the replica that handles it, and idle breakers are evicted elsewhere. A removed and re-added breaker starts closed. An unchanged write keeps an open circuit open.

#### A changed or removed limit applies at once
- **Scenario**: *No separate file. Covered by the tests `changing_a_rate_limit_drops_the_exhausted_bucket` and `writes_drop_the_buckets_they_make_stale`.*
- **Mechanism**: The Control Plane and the Data Plane share one limiter. A write that changes or removes an upstream or route rate limit drops its buckets on every write path (REST, SDK, registry), so a caller exhausted under the old limit is admitted under the new one. A write that keeps the limit keeps the buckets. A bucket reached with another config without such a write (a merged limit that differs by tenant chain, a write on another replica) keeps what it consumed (`token_bucket_config_change_keeps_what_was_consumed`).

---

### Configuration storage

> **Decision reference**: [ADR-0018: Optional Persistence](../docs/ADR/0018-optional-persistence.md)

#### Registry instances provisioned at startup
- **Scenario**: [positive-19.1-registry-instances-provisioned-at-startup.md](configuration-storage/positive-19.1-registry-instances-provisioned-at-startup.md)
- **Mechanism**: Upstream and route instances in the types registry (for example its `entities` configuration) become stored rows at startup, under their instance UUIDs, in the root tenant unless the instance names one. They are listed and proxy like API-created resources.

---

### HTTP basics

#### Plain HTTP request/response passthrough
- **Scenario**: [positive-12.1-plain-http-request-response-passthrough.md](protocols/http/positive-12.1-plain-http-request-response-passthrough.md)
- **Mechanism**: Status, headers, body forwarded. Gateway adds rate-limit headers if enabled.

#### HTTP version negotiation (HTTP/2 attempt + fallback)
- **Scenario**: [positive-12.4-http-version-negotiation.md](protocols/http/positive-12.4-http-version-negotiation.md)
- **Mechanism**: First call attempts HTTP/2 via ALPN. On failure, falls back to HTTP/1.1. Subsequent calls use cached protocol decision.

---

### Streaming (SSE)

#### SSE stream forwarded without buffering
- **Scenario**: [positive-13.1-sse-stream-forwarded-buffering.md](protocols/sse/positive-13.1-sse-stream-forwarded-buffering.md)
- **Mechanism**: Response `Content-Type: text/event-stream`. Events arrive incrementally. Rate limit headers in initial headers.

#### Client disconnect aborts upstream stream
- **Scenario**: [positive-13.2-client-disconnect-aborts-upstream-stream.md](protocols/sse/positive-13.2-client-disconnect-aborts-upstream-stream.md)
- **Mechanism**: Disconnect client mid-stream. Upstream stream closed. No leaked in-flight metrics.

#### SSE stream ends when the gear stops
- **Scenario**: [positive-13.3-sse-stream-ends-on-gear-stop.md](protocols/sse/positive-13.3-sse-stream-ends-on-gear-stop.md)
- **Mechanism**: Server shutdown or gear stop ends open SSE streams (and sends Close 1001 on WebSocket relays) before the HTTP drain, also a stream opened before the first start or one that starts waiting after the stop was signalled. Non-SSE bodies are not cut: they finish within the HTTP drain deadline. A later start lets new streams run.

---

### WebSocket

#### WebSocket upgrade is proxied
- **Scenario**: [positive-14.1-websocket-upgrade-proxied.md](protocols/websocket/positive-14.1-websocket-upgrade-proxied.md)
- **Mechanism**: `101 Switching Protocols` handshake forwarded. Required WS headers forwarded/validated.

#### Auth injected during handshake (not per-message)
- **Scenario**: [positive-14.2-auth-injected-during-handshake.md](protocols/websocket/positive-14.2-auth-injected-during-handshake.md)
- **Mechanism**: Upstream sees auth header on upgrade request. Subsequent WS frames forwarded unchanged.

#### Frame relayed intact when split across multiple reads
- **Scenario**: [positive-14.6-frame-relayed-intact-across-split-reads.md](protocols/websocket/positive-14.6-frame-relayed-intact-across-split-reads.md)
- **Mechanism**: Header and payload sent as separate writes. Upstream receives byte-identical frame — relay never needs a whole frame at once.

#### Fragmented message of any size relayed without a cap
- **Scenario**: [positive-14.7-fragmented-message-relayed-without-size-limit.md](protocols/websocket/positive-14.7-fragmented-message-relayed-without-size-limit.md)
- **Mechanism**: Large multi-fragment message relayed in full. No Close 1009 — there is no per-frame or per-message size limit to configure.

---

### gRPC

#### gRPC unary request proxied (native)
- **Scenario**: [positive-15.1-grpc-unary-request-proxied.md](protocols/grpc/positive-15.1-grpc-unary-request-proxied.md)
- **Mechanism**: `content-type: application/grpc*` detection routes to gRPC handler. Metadata headers preserved.

#### gRPC server streaming proxied
- **Scenario**: [positive-15.2-grpc-server-streaming-proxied.md](protocols/grpc/positive-15.2-grpc-server-streaming-proxied.md)
- **Mechanism**: Stream forwarded without buffering. Backpressure respects HTTP/2 flow control.

#### gRPC JSON transcoding for HTTP clients
- **Scenario**: [positive-15.3-grpc-json-transcoding-http-clients.md](protocols/grpc/positive-15.3-grpc-json-transcoding-http-clients.md)
- **Mechanism**: HTTP JSON request converted to gRPC protobuf. Server streaming returned as `application/x-ndjson`.

---

### WebTransport

#### WT session establishment + auth
- **Scenario**: [positive-17.1-wt-session-establishment-auth.md](protocols/webtransport/positive-17.1-wt-session-establishment-auth.md)
- **Mechanism**: Upstream scheme `wt` accepted. Future-facing — if not implemented yet, `502 ProtocolError` with `ESrc=gateway`.

---

### CORS

#### Built-in CORS handling (preflight + actual request)
- **Scenario**: [positive-10.2-built-cors-handling.md](plugins/guards/positive-10.2-built-cors-handling.md)
- **Mechanism**: OPTIONS preflight returns permissive `204` at handler level (no upstream resolution). Origin enforcement happens on actual requests after upstream resolution — disallowed origins are rejected with `403` before reaching the upstream. Allowed origins receive CORS response headers (`Access-Control-Allow-Origin`, `Vary: Origin`, etc.).

#### Hierarchical merge/union for CORS allowed origins
- **Scenario**: *Covered within hierarchical configuration scenarios.*
- **Mechanism**: `inherit` merges origins by union. `enforce` forbids child adding origins.

---

### Guard plugins

> **Flow reference**: [Plugin Execution](flows/plugin-execution.md)

*Note: CORS origin validation is covered in [CORS](#cors) above. Timeout and Starlark guard rejection scenarios are in [Guardrails → Guard rejections](#guard-rejections).*

---

### Transform plugins

#### Request path rewrite transform
- **Scenario**: [positive-11.1-request-path-rewrite-transform.md](plugins/transforms/positive-11.1-request-path-rewrite-transform.md)
- **Mechanism**: Outbound `path` is rewritten. Transform emits audit-safe logs (no secrets).

#### Query mutation transform
- **Scenario**: [positive-11.2-query-mutation-transform.md](plugins/transforms/positive-11.2-query-mutation-transform.md)
- **Mechanism**: New query param added. Internal params removed. Query allowlist rules remain enforced before transform.

#### Header mutation transform
- **Scenario**: [positive-11.3-header-mutation-transform.md](plugins/transforms/positive-11.3-header-mutation-transform.md)
- **Mechanism**: `headers.set/add/remove` changes reflected upstream. Hop-by-hop headers remain stripped even if plugin tries to set them.

#### Response JSON redaction transform
- **Scenario**: [positive-11.4-response-json-redaction-transform.md](plugins/transforms/positive-11.4-response-json-redaction-transform.md)
- **Mechanism**: Target fields replaced with placeholder. Non-JSON response triggers defined behavior (reject or no-op).

#### on_error transform handles gateway errors
- **Scenario**: [positive-11.5-error-transform-handles-gateway-errors.md](plugins/transforms/positive-11.5-error-transform-handles-gateway-errors.md)
- **Mechanism**: `on_error` runs only for gateway-originated errors of the upstream exchange (connection failure, timeout, protocol error, body limit, response guard rejection). A WebSocket upgrade does not run it: a failed upgrade returns its gateway error without `on_error`. It gets the error type, the status and the detail, and cannot change the response. An upstream 4xx/5xx response goes through `on_response`, not `on_error`, and reaches the client as the upstream sent it (part B).

#### Plugin ordering and layering (upstream before route)
- **Scenario**: [positive-11.6-plugin-ordering-layering.md](plugins/transforms/positive-11.6-plugin-ordering-layering.md)
- **Mechanism**: Upstream plugins run before route plugins. Auth before guards, before transforms.

#### Plugin control flow (next, reject, respond)
- **Scenario**: [positive-11.7-plugin-control-flow.md](plugins/transforms/positive-11.7-plugin-control-flow.md)
- **Mechanism**: `ctx.reject` stops chain and returns gateway error. `ctx.respond` returns custom success response without calling upstream.

---

### Streaming body handling

#### Streaming request bodies are not buffered
- **Scenario**: [positive-8.3-streaming-request-bodies-not-buffered.md](proxy-api/body-validation/positive-8.3-streaming-request-bodies-not-buffered.md)
- **Mechanism**: For endpoints supporting streaming body, memory does not grow with body size.

---

### Observability

#### Metrics endpoint is auth-protected
- **Scenario**: *Section 22.1 — no separate file. Verified as part of management auth scenarios.*
- **Mechanism**: Unauthenticated `GET /metrics` rejected. Non-admin rejected.

#### Request metrics count every request
- **Scenario**: *Section 22.2 — no separate file.*
- **Mechanism**: `oagw_requests_total` increments once per request with a response, with correct labels (`host`, `http.request.method`, `http.route`, `http.response.status_code`): the upstream's status, 4xx/5xx included, or for a gateway error (401, 403, 429, 502, ...) the status the client gets. Histogram `oagw_request_duration_seconds` records `phase=total` (request start to the response head), `resolve`, `auth` and, for an auth plugin that signs, `sign`. Verified by Rust unit tests: the e2e suite has no metrics collector.

#### Error metrics increment on gateway errors
- **Scenario**: *Section 22.3 — no separate file.*
- **Mechanism**: Induce gateway error; `oagw_errors_total{error_type=...}` increments, and `oagw_requests_total` counts the request with the error's status. A request the REST handler rejects after the `proxy:invoke` grant (bad proxy path 400, body over the limit 413, failed body read) counts the same way under `host="unknown"`. A request the client drops before its response head counts only as `error_type="Cancelled"`.

#### Cardinality rules (no tenant labels)
- **Scenario**: *Section 22.4 — no separate file.*
- **Mechanism**: Metrics output does not include `tenant_id` label. The `http.route` label uses the configured route match pattern, not the dynamic request path suffix.

#### Status label follows OTel HTTP semantic conventions
- **Scenario**: *Section 22.5 — no separate file.*
- **Mechanism**: Metrics export uses the numeric `http.response.status_code` attribute (OTel HTTP semconv), matching the inbound API Gateway. Status-class queries (e.g. 5xx error rate) are expressed at query time via regex on the numeric code.

#### In-flight and stream duration last until the body ends
- **Scenario**: *Section 22.6 — no separate file.*
- **Mechanism**: `oagw_requests_in_flight` rises at the start of the Data Plane pipeline (under `host="unknown"` until the upstream is resolved) and falls when the response body ends, fails or is dropped, SSE streams included; a gateway error falls at once. `oagw_stream_duration_seconds{outcome}` is recorded then: `completed` (EOF, or the declared `Content-Length` relayed, as hyper drops such a body without reading its end), `error` (upstream cut, idle timeout), `shutdown` (gateway stop) or `cancelled` (client went away). `oagw_websocket_session_duration_seconds` carries the same `outcome`: `completed` when the tunnel closed, from either side (the relay does not parse frames, and after a client-initiated Close the server closes TCP first), `error` on an idle timeout or an IO error, `shutdown` on a gateway stop.

#### Rate-limit metrics per bucket
- **Scenario**: *Section 22.7 — no separate file.*
- **Mechanism**: `oagw_rate_limit_usage_ratio` and `oagw_rate_limit_exceeded_total` carry `bucket` (`route`, `shadowed_route`, `upstream`, `pool`): each bucket checked writes its own series, so a wider bucket does not overwrite the route's ratio. The shadowed ancestor routes of one route share the `shadowed_route` series, written once per request with the highest ratio among them.

#### Proxy requests produce structured audit log
- **Scenario**: *Section 23.1 — no separate file.*
- **Mechanism**: Log includes request_id, tenant_id, principal_id, host, path, status, duration.

#### Sensitive data not logged
- **Scenario**: *Section 23.2 — no separate file.*
- **Mechanism**: No request bodies, query params, auth headers, or secret values logged.

#### Config change operations are logged
- **Scenario**: *Section 23.3 — no separate file.*
- **Mechanism**: Upstream/route/plugin create/update/delete yields audit log entry.

---

### OData query support

> **Planned.** None of the scenarios below is implemented yet. Today the list endpoints accept `limit` and `offset` (routes also `upstream_id`) and refuse any other parameter with `400 UNSUPPORTED_QUERY_PARAM`; see [positive-2.9-tags-support-discovery-filtering.md](management-api/upstreams/positive-2.9-tags-support-discovery-filtering.md).

#### $select projects fields for upstream list
- **Scenario**: *Section 25.1 — no separate file.*
- **Mechanism**: `GET /upstreams?$select=id,alias` returns only those fields.

#### $select validation
- **Scenario**: *Section 25.2 — no separate file.*
- **Mechanism**: Too long, too many fields, or duplicate fields returns `400 PD`.

#### $filter on alias
- **Scenario**: *Section 25.3 — no separate file.*
- **Mechanism**: `GET /upstreams?$filter=alias eq 'api.openai.com'` returns only matches.

#### Pagination ($top/$skip) stable ordering
- **Scenario**: *Section 25.4 — no separate file.*
- **Mechanism**: Paging produces consistent sets.

#### $select works for routes and plugins lists
- **Scenario**: *Section 25.5 — no separate file.*
- **Mechanism**: `GET /routes?$select=id,upstream_id,match` and `GET /plugins?$select=id,plugin_type,name` return only those fields.

#### $orderby is applied and validated
- **Scenario**: *Section 25.6 — no separate file.*
- **Mechanism**: `created_at desc` ordering changes item order. Invalid `$orderby` yields `400 PD`.

#### $top max and $skip bounds enforced
- **Scenario**: *Section 25.7 — no separate file.*
- **Mechanism**: `$top` above max clamps or rejects. Negative `$skip` rejected.

---

### E2E protocol examples

Full integration walkthroughs — each demonstrates the complete journey (upstream → route → auth → invoke → response).

#### HTTP request/response
- **Scenario**: [positive-example-01-http-request-response.md](protocols/http/examples/positive-example-01-http-request-response.md)
- **Mechanism**: End-to-end HTTP proxy call with upstream creation, route setup, auth injection, and response passthrough.

#### SSE streaming
- **Scenario**: [positive-example-02-sse-streaming.md](protocols/sse/examples/positive-example-02-sse-streaming.md)
- **Mechanism**: End-to-end SSE stream with `text/event-stream` forwarding without buffering.

#### WebSocket upgrade
- **Scenario**: [positive-example-03-websocket-upgrade.md](protocols/websocket/examples/positive-example-03-websocket-upgrade.md)
- **Mechanism**: End-to-end WebSocket with `101 Switching Protocols` handshake proxying.

#### gRPC unary proxy
- **Scenario**: [positive-example-04-grpc-unary-proxy.md](protocols/grpc/examples/positive-example-04-grpc-unary-proxy.md)
- **Mechanism**: End-to-end gRPC unary call with service+method routing and metadata preservation.

---

## 2. Guardrails, Edge Cases & Rejections

### Authentication & authorization

#### Missing Bearer token on management endpoints → 401
- **Scenario**: [negative-1.1-all-management-endpoints-require-bearer-auth.md](management-api/auth/negative-1.1-all-management-endpoints-require-bearer-auth.md)
- **What happens**: All management endpoints require `Authorization: Bearer` header. Missing or invalid token returns `401` `PD` with stable `type`.

#### Permission gates for upstream/route/plugin CRUD → 403
- **Scenario**: [negative-1.2-permission-gates-upstream-route-plugin-crud.md](management-api/auth/negative-1.2-permission-gates-upstream-route-plugin-crud.md)
- **What happens**: Missing permission for resource type returns `403`. With correct permission, same call succeeds. Each operation asks for its own action (`create`, `read`, `override`, `delete`); a grant on part of the tenant is refused with `AUTHZ_SCOPE_NOT_SUPPORTED`.

#### Tenant scoping in management APIs → cross-tenant blocked
- **Scenario**: [negative-1.3-tenant-scoping-management-apis.md](management-api/auth/negative-1.3-tenant-scoping-management-apis.md)
- **What happens**: Tenant A cannot `GET /upstreams/{id}` created by tenant B. Listing returns only tenant-visible resources.

#### Per-request secret reference rejected → 400/403/503
- **Scenarios**: [Per-request secret references](#per-request-secret-references)
- **What happens**: A malformed header, a header on an upstream that does not accept it, or no header without a default is `400` on `x-oagw-secret-ref`. A secret that is missing, invisible, owned above the auth owner, or of a disallowed type is `403`. A header rule or transform that sets the auth header is a gateway misconfiguration and is `500`. A CredStore outage is `503`. OAGW never falls back to another key.

#### Proxy invoke permission required → 403
- **Scenario**: [negative-5.1-proxy-invoke-permission-required.md](proxy-api/authz/negative-5.1-proxy-invoke-permission-required.md)
- **What happens**: Missing `gts.cf.core.oagw.proxy.v1~:invoke` returns `403`, before the body is read or validated.

#### Proxy cannot access upstream not owned or shared → 403/404
- **Scenario**: [negative-5.2-proxy-cannot-access-upstream-not-owned-shared.md](proxy-api/authz/negative-5.2-proxy-cannot-access-upstream-not-owned-shared.md)
- **What happens**: Tenant A invoking Tenant B private upstream returns `403` or `404` (must not leak existence).

---

### Input validation

#### Method allowlist enforcement → 404
- **Scenario**: [negative-3.2-method-allowlist-enforcement.md](management-api/routes/negative-3.2-method-allowlist-enforcement.md)
- **What happens**: Route allows `POST`; invoking `GET` returns `404 ROUTE_NOT_FOUND` as a gateway error.

#### Query allowlist enforcement → 400
- **Scenario**: [negative-3.3-query-allowlist-enforcement.md](management-api/routes/negative-3.3-query-allowlist-enforcement.md)
- **What happens**: Unknown query param rejected with `400 PD`, `ESrc=gateway`.

#### Path suffix mode = disabled → rejected
- **Scenario**: [negative-3.4-path-suffix-mode-disabled.md](management-api/routes/negative-3.4-path-suffix-mode-disabled.md)
- **What happens**: Supplying any suffix returns a gateway validation error.

#### Unsupported list parameters → 400
- **Scenario**: [positive-2.9-tags-support-discovery-filtering.md](management-api/upstreams/positive-2.9-tags-support-discovery-filtering.md)
- **What happens**: `GET /upstreams?$filter=...` (or `$orderby`, `tags`, any key other than `limit`, `offset` and, for routes, `upstream_id`) returns `400 PD` with one `UNSUPPORTED_QUERY_PARAM` violation per key instead of an unfiltered list.

#### Write-time validation → 400
- **Scenario**: [negative-2.15-write-validation-rejected-400.md](management-api/upstreams/negative-2.15-write-validation-rejected-400.md)
- **What happens**: Port 0, a tag outside `^[a-z0-9_-]+$`, an alias starting or ending with `.`, `-` or `:`, an empty or relative HTTP route path, an empty method list, a rate limit value of 0, a circuit breaker `failure_threshold`, `timeout_seconds` or `success_threshold` of 0 or above its maximum (1000, 86400, 100), a rate limit `cost` above the bucket capacity or above the `budget.total` of the shared pool the upstream draws on, a plugin id that is not implemented, or an `http` endpoint while `allow_http_upstream` is off returns `400 PD` (`invalid_argument`). Nothing is stored.

#### Stored field limits → 400
- **Scenario**: [negative-2.14-field-limits-rejected-400.md](management-api/upstreams/negative-2.14-field-limits-rejected-400.md)
- **What happens**: More than 64 tags, a tag over 128 bytes or with a control character, a route path over 2048 bytes, a gRPC service or method name over 256 bytes, more than 64 query parameters or one over 128 bytes, or a protocol, auth type, or plugin reference over 256 bytes or with a control character returns `400 PD` (`invalid_argument`). Nothing is stored.

#### Well-known header validation errors → 400
- **Scenario**: [negative-7.4-well-known-header-validation-errors-400.md](proxy-api/request-transforms/negative-7.4-well-known-header-validation-errors-400.md)
- **What happens**: Invalid `Content-Length` or mismatch with actual body returns `400 PD`.

#### Maximum body size limit enforced → 413
- **Scenario**: [negative-8.1-maximum-body-size-limit-enforced.md](proxy-api/body-validation/negative-8.1-maximum-body-size-limit-enforced.md)
- **What happens**: The smaller of OAGW's `max_body_size_bytes` (default and maximum 100 MiB) and the api-gateway's `body_limit_bytes` (default 16 MiB) applies. Over OAGW's limit → `413 PD` (`PAYLOAD_TOO_LARGE`), `ESrc=gateway`, rejected early. Over the api-gateway's limit → `413` `about:blank` PD from the outer layer, no `ESrc`.

#### Transfer-Encoding support limited to chunked → 400
- **Scenario**: [negative-8.2-transfer-encoding-support-limited-chunked.md](proxy-api/body-validation/negative-8.2-transfer-encoding-support-limited-chunked.md)
- **What happens**: Unsupported transfer-encoding rejected with `400 PD`.

---

### Routing rejections

#### Explicit alias required for IP-based endpoints → 400
- **Scenario**: [negative-2.3-explicit-alias-required-ip-based-endpoints.md](management-api/upstreams/negative-2.3-explicit-alias-required-ip-based-endpoints.md)
- **What happens**: Create upstream with `host=10.0.0.1` and no alias is rejected (`400 PD`).

#### Multi-endpoint upstream pool compatibility rules → 400
- **Scenario**: [negative-2.4-multi-endpoint-upstream-pool-compatibility-rules.md](management-api/upstreams/negative-2.4-multi-endpoint-upstream-pool-compatibility-rules.md)
- **What happens**: Mismatched `scheme`, `port`, or `protocol` in pool fails with `400`.

#### Disable upstream blocks proxy traffic → 503
- **Scenario**: [negative-2.6-disable-upstream-blocks-proxy-traffic.md](management-api/upstreams/negative-2.6-disable-upstream-blocks-proxy-traffic.md)
- **What happens**: With `enabled=false`, proxy returns `503 PD` (`service_unavailable`, reason `UPSTREAM_DISABLED`), `Retry-After: 300`, `ESrc=gateway`. Disabled in ancestor → disabled for descendants too.

#### Alias uniqueness enforced per tenant → conflict
- **Scenario**: [negative-2.8-alias-uniqueness-enforced-per-tenant.md](management-api/upstreams/negative-2.8-alias-uniqueness-enforced-per-tenant.md)
- **What happens**: Two upstreams with same alias in same tenant rejected (conflict). Same alias in different tenants succeeds.

#### All backends unreachable → 502 or 504
- **Scenario**: [negative-2.10-all-backends-unreachable-returns-502.md](management-api/upstreams/negative-2.10-all-backends-unreachable-returns-502.md)
- **What happens**: Multi-endpoint upstream with all endpoints unreachable returns `502 Bad Gateway` or `504 Gateway Timeout`. The selected endpoint's connection attempt fails and the error is returned immediately (OAGW does not retry).

#### Enforced ancestor limits still apply when alias shadowed
- **Scenario**: [negative-6.2-enforced-ancestor-limits-still-apply-alias-shadowed.md](proxy-api/alias-resolution/negative-6.2-enforced-ancestor-limits-still-apply-alias-shadowed.md)
- **What happens**: Parent `rate_limit.sharing=enforce` still constrains child effective limit even when alias is shadowed, and down a chain: top `enforce` → middle `inherit` (stricter) → leaf `private` keeps the middle's limit (`429`). A child's route shadowing the parent's route for the same path keeps the parent route's enforced guard (`400 REQUIRED_HEADER_MISSING`) and enforced limit, checked on the parent route's own bucket (`429` with the parent route's `X-RateLimit-Limit`).

#### Private ancestor configuration never reaches a descendant
- **Scenario**: [negative-6.6-private-ancestor-configuration-never-reaches-descendant.md](proxy-api/alias-resolution/negative-6.6-private-ancestor-configuration-never-reaches-descendant.md)
- **What happens**: An ancestor's `private` auth, rate limit, plugins and route settings are not applied for a descendant, whether it resolves to the ancestor's upstream or binds the alias itself: no credential reaches the upstream, no `429`, no guard `400`. The descendant's requests do not reset the ancestor's private limit either (parent, child, parent under `scope=global`: `200`, `200`, `429`).

#### Alias not found → stable 404
- **Scenario**: [negative-6.4-alias-not-found-returns-stable-404.md](proxy-api/alias-resolution/negative-6.4-alias-not-found-returns-stable-404.md)
- **What happens**: Unknown alias returns `404 PD`, `ESrc=gateway`, `type` = `...upstream.not_found...`.

#### Known alias without a matching route → 404
- **Scenario**: [negative-6.5-known-alias-without-matching-route-returns-404.md](proxy-api/alias-resolution/negative-6.5-known-alias-without-matching-route-returns-404.md)
- **What happens**: A known alias with no route for the method and path returns `404 PD`, `ESrc=gateway`, `type` = `...not_found...`. `resource_type` is the route schema, where 6.4 names the upstream schema.

#### Disable route blocks proxy traffic → 404/503
- **Scenario**: [negative-3.7-disable-route-blocks-proxy-traffic.md](management-api/routes/negative-3.7-disable-route-blocks-proxy-traffic.md)
- **What happens**: With `route.enabled=false`, request returns `404 ROUTE_NOT_FOUND` or `503`, `ESrc=gateway`.

#### Route prefix matches whole segments (no match inside a segment) → 404
- **Scenario**: [negative-3.12-route-prefix-matches-whole-segments.md](management-api/routes/negative-3.12-route-prefix-matches-whole-segments.md)
- **What happens**: `/v1/chat` does not match `/v1/chatty` (`404`); matching uses the normalized path; route `/` matches the bare alias; a prefix no request could match (or not starting with `/`) is rejected on write; a route's trailing `/` is not forwarded before `.json`; an encoded `/` or `\` that would select another route (`/v1%2Fadmin` past `/v1/admin`) returns `400 INVALID_PROXY_PATH`.

#### Overlapping route → 409
- **Scenario**: [negative-3.11-overlapping-route-rejected-409.md](management-api/routes/negative-3.11-overlapping-route-rejected-409.md)
- **What happens**: An enabled route on the same upstream with the same path, the same priority and a shared method returns `409 PD` (`already_exists`, `resource_type` = route schema). Another priority does not overlap.

---

### Security protections (SSRF, headers, injection)

#### SSRF prevention by strict upstream host selection
- **Scenario**: *Section 26.1 — no separate file.*
- **What happens**: Client cannot override upstream destination via inbound `Host` header. Absolute-form URLs in request line rejected or normalized.

#### Encoded dot-segments (path traversal) → 400
- **Scenario**: [negative-7.7-encoded-dot-segments-rejected-400.md](proxy-api/request-transforms/negative-7.7-encoded-dot-segments-rejected-400.md)
- **What happens**: A path segment that decodes to `.` or `..`, a `;` parameter, a non-UTF-8 escape or a stray `%` returns `400 INVALID_PROXY_PATH`, `ESrc=gateway`. So does an encoded `/` or `\` that would select another route (3.12 Scenario F).

#### Header injection protections
- **Scenario**: *Section 26.2 — no separate file.*
- **What happens**: Newline characters in header values rejected. Prevents response/request splitting.

#### Protocol mismatch errors are explicit
- **Scenario**: *Section 26.3 — no separate file.*
- **What happens**: Using HTTP route against `protocol=grpc` upstream fails with `502 ProtocolError` (`PD`).

#### Non-public IPv6 address refused at write and at connect → 400
- **Scenario**: [negative-26.4-non-public-ipv6-address-refused.md](proxy-api/ssrf/negative-26.4-non-public-ipv6-address-refused.md)
- **What happens**: An IP endpoint in a non-public IPv6 range (IANA special-purpose blocks not globally reachable, `fec0::/10`, `ff00::/8`, anything else outside `2000::/3`) is refused on write with `400` naming the range. A hostname resolving into one is never connected to (Rust tests only).

#### IPv6 deny CIDR does not refuse an IPv4 destination → allowed
- **Scenario**: [positive-26.5-ipv6-deny-cidr-does-not-refuse-ipv4.md](proxy-api/ssrf/positive-26.5-ipv6-deny-cidr-does-not-refuse-ipv4.md)
- **What happens**: `extra_deny_cidrs: ["::/3"]` does not refuse `8.8.8.8` or `::ffff:8.8.8.8`; `allow_cidrs: ["::/0"]` does not admit `10.0.0.1`. CIDRs match addresses of their own family only (Rust tests only).

---

### Plugin enforcement

#### Plugin immutability (no update) → 404/405
- **Scenario**: [negative-4.2-plugin-immutability.md](management-api/plugins/negative-4.2-plugin-immutability.md)
- **What happens**: `PUT /plugins/{id}` is not available (404/405). Ensures reproducibility and auditability.

#### Plugin type enforcement → 400
- **Scenario**: [negative-4.4-plugin-type-enforcement.md](management-api/plugins/negative-4.4-plugin-type-enforcement.md)
- **What happens**: Attaching `plugin.guard` where auth is expected fails at config validation (`400`).

#### Secret access control via cred_store → 401
- **Scenario**: [negative-9.7-secret-access-control-cred-store.md](proxy-api/authentication/negative-9.7-secret-access-control-cred-store.md)
- **What happens**: A missing secret is rejected when the upstream is written (`400 failed_precondition` naming the key). At proxy time a secret `cred_store` denies, or one deleted after the write, returns `401 PD` (`unauthenticated`, reason `AUTH_CREDENTIAL_NOT_FOUND`), for every auth plugin. A credential store outage is `503` (`CREDENTIAL_STORE_UNAVAILABLE`), an identity provider outage `503` (`IDENTITY_PROVIDER_UNAVAILABLE`, 9.11), an invalid identity provider response `502`, a plugin misconfiguration `500`. The response contains neither the reference nor the key; the server log names the key.

#### OAuth2 identity provider unavailable → 503
- **Scenario**: [negative-9.11-oauth2-identity-provider-unavailable.md](proxy-api/authentication/negative-9.11-oauth2-identity-provider-unavailable.md)
- **What happens**: The OAuth2 token endpoint is unreachable, times out or answers `408`, `429` or `5xx`: `503` `service_unavailable`, reason `IDENTITY_PROVIDER_UNAVAILABLE`, `Retry-After: 5`. The upstream is not contacted. A credential store outage keeps `CREDENTIAL_STORE_UNAVAILABLE`.

#### AWS SigV4 with a streamed request body → 400
- **Scenario**: [negative-9.12-aws-sigv4-streamed-body.md](proxy-api/authentication/negative-9.12-aws-sigv4-streamed-body.md)
- **What happens**: A body streamed by an in-process SDK caller cannot be hashed: `400 VALIDATION` on field `body`, naming `auth.config.unsigned_payload`, nothing sent. With `unsigned_payload: true` it is signed as `UNSIGNED-PAYLOAD`. REST callers are always buffered.

#### AWS SigV4 for S3 on a path the gateway rewrote → 400
- **Scenario**: [negative-9.14-aws-sigv4-s3-rewritten-path.md](proxy-api/authentication/negative-9.14-aws-sigv4-s3-rewritten-path.md)
- **What happens**: With `uri_encoding: single` (the S3 signing names), a client path with `//`, `.`/`..` segments or a trailing `/` the gateway removed would sign and address another object key: `400 VALIDATION` on field `path`, nothing sent. A trailing `/` is compared with the URL as built: one the route path gives back is not a rewrite, one it adds (route `/dir/`, request `/dir`) is; `double` encoding signs the normalized path.

#### Descendant override permissions enforced → blocked
- **Scenario**: [negative-9.9-descendant-override-permissions-enforced.md](proxy-api/authentication/negative-9.9-descendant-override-permissions-enforced.md)
- **What happens**: Without `oagw:upstream:override_auth`, child cannot override inherited auth. Same for `override_rate` and `add_plugins`. An auth or CORS override that any ancestor enforces is refused (`400`), even when the closest ancestor shares it with `inherit`. A rate limit is refused only under an enforcing closest ancestor: a leaf allocating from an `inherit` parent's allocated budget under an enforcing grand-ancestor is accepted (`201`) and still held to the grand-ancestor's limit (`429`).

#### Starlark sandbox restrictions → blocked
- **Scenario**: [negative-11.8-starlark-sandbox-restrictions.md](plugins/transforms/negative-11.8-starlark-sandbox-restrictions.md)
- **What happens**: Network I/O fails. File I/O fails. Infinite loop times out. Large allocation blocked.

---

### Rate limiting rejections

#### Sliding window strictness → no boundary bursts
- **Scenario**: [negative-18.2-sliding-window-strictness.md](rate-limiting/negative-18.2-sliding-window-strictness.md)
- **What happens**: Requests across a window boundary do not allow 2x burst.

#### Circuit opens after consecutive failures → 503
- **Scenario**: [negative-18.9-circuit-opens-after-consecutive-failures.md](rate-limiting/negative-18.9-circuit-opens-after-consecutive-failures.md)
- **What happens**: After `failure_threshold` failures in a row (upstream `502`/`503`/`504`, connect, TLS or DNS errors, a timeout the upstream had its full budget for), requests fail fast with `503 PD` (`service_unavailable`, reason `REQUEST_NOT_SENT`), `ESrc=gateway` and `Retry-After` = time left until a probe, without reaching the upstream. Per replica, shared by every tenant on the upstream: SSRF refusals, gateway errors, client cancellations and timeouts after a slow CredStore read do not count.

#### Spoofed forwarding headers → same IP bucket
- **Scenario**: [negative-18.8-spoofed-forwarding-headers-keep-the-ip-bucket.md](rate-limiting/negative-18.8-spoofed-forwarding-headers-keep-the-ip-bucket.md)
- **What happens**: A new `X-Forwarded-For` or `X-Real-IP` value does not open a fresh `scope: ip` bucket: OAGW keys on the address the API gateway resolved, so the request is `429`.

#### Strategy variants when limit exceeded → 429/503
- **Scenario**: [negative-18.5-strategy-variants-limit-exceeded.md](rate-limiting/negative-18.5-strategy-variants-limit-exceeded.md)
- **What happens**: `strategy=reject` returns `429`. `strategy=queue` delays then succeeds or times out with `503 queue.timeout`. `strategy=degrade` uses configured fallback.

---

### Custom header routing rejections

#### Missing X-OAGW-Target-Host for common suffix alias → 400
- **Scenarios**:
  - [negative-6.3-multi-endpoint-common-suffix-alias-requires-host-header.md](proxy-api/alias-resolution/negative-6.3-multi-endpoint-common-suffix-alias-requires-host-header.md)
  - [negative-1.1-missing-header-common-suffix.md](proxy-api/custom-header-routing/negative-1.1-missing-header-common-suffix.md)
- **What happens**: Missing header for common suffix alias returns `400 PD` with list of valid endpoint hosts.

#### Invalid X-OAGW-Target-Host format (port) → 400
- **Scenario**: [negative-1.2-invalid-format-with-port.md](proxy-api/custom-header-routing/negative-1.2-invalid-format-with-port.md)
- **What happens**: Header with port number rejected. Port is defined in upstream config, not header.

#### Invalid X-OAGW-Target-Host format (path) → 400
- **Scenario**: [negative-1.3-invalid-format-with-path.md](proxy-api/custom-header-routing/negative-1.3-invalid-format-with-path.md)
- **What happens**: Header with path component rejected.

#### Invalid X-OAGW-Target-Host format (special chars) → 400
- **Scenario**: [negative-1.4-invalid-format-special-chars.md](proxy-api/custom-header-routing/negative-1.4-invalid-format-special-chars.md)
- **What happens**: Header with query params or special characters rejected.

#### Unknown X-OAGW-Target-Host not in endpoint list → 400
- **Scenario**: [negative-2.1-unknown-host.md](proxy-api/custom-header-routing/negative-2.1-unknown-host.md)
- **What happens**: Header value not matching any endpoint rejected. Allowlist validation prevents SSRF.

#### IP address when hostname expected → 400
- **Scenario**: [negative-2.2-ip-address-when-hostname-expected.md](proxy-api/custom-header-routing/negative-2.2-ip-address-when-hostname-expected.md)
- **What happens**: IP address when endpoints use hostnames treated as unknown host.

---

### Guard rejections

#### Timeout guard plugin enforces request timeout → 504
- **Scenario**: [negative-10.1-timeout-guard-plugin-enforces-request-timeout.md](plugins/guards/negative-10.1-timeout-guard-plugin-enforces-request-timeout.md)
- **What happens**: Request exceeding timeout returns `504` gateway timeout (`PD`, `ESrc=gateway`).

#### CORS credentials + wildcard rejected by config validation → 400
- **Scenario**: [negative-10.3-cors-credentials-wildcard-rejected-config-validation.md](plugins/guards/negative-10.3-cors-credentials-wildcard-rejected-config-validation.md)
- **What happens**: `allow_credentials=true` + `allowed_origins=['*']` rejected (`400`).

#### Custom Starlark guard rejects based on headers/body
- **Scenario**: [negative-10.4-custom-starlark-guard-rejects-based-headers-body.md](plugins/guards/negative-10.4-custom-starlark-guard-rejects-based-headers-body.md)
- **What happens**: Missing required header rejected with plugin-defined status. Body-too-large rejected with `413`. Guard only runs `on_request`.

---

### Protocol errors

#### Upstream error passthrough with ESrc=upstream
- **Scenario**: [negative-12.2-upstream-error-passthrough-esrc-upstream.md](protocols/http/negative-12.2-upstream-error-passthrough-esrc-upstream.md)
- **What happens**: Upstream returns `500` with JSON body. OAGW keeps body intact, sets `ESrc=upstream`. The upstream's own `Retry-After` and `WWW-Authenticate` pass through unchanged.

#### Gateway error uses PD + ESrc=gateway
- **Scenario**: [negative-12.3-gateway-error-uses-pd-esrc-gateway.md](protocols/http/negative-12.3-gateway-error-uses-pd-esrc-gateway.md)
- **What happens**: Route not found / validation error → `PD`, `Content-Type=application/problem+json`, `ESrc=gateway`.

#### No automatic retries for upstream failures
- **Scenario**: [negative-12.6-no-automatic-retries-upstream-failures.md](protocols/http/negative-12.6-no-automatic-retries-upstream-failures.md)
- **What happens**: Upstream transient `5xx` or connection close → OAGW returns error once. Upstream sees single request attempt.

#### Transport failure before sending → REQUEST_NOT_SENT
- **Scenario**: [negative-12.8-transport-failure-before-sending-request-not-sent.md](protocols/http/negative-12.8-transport-failure-before-sending-request-not-sent.md)
- **What happens**: Connection refused or TLS handshake failure → `502 PD` (`service_unavailable`, reason `REQUEST_NOT_SENT`), `Retry-After: 5`, `ESrc=gateway`. DNS, SSRF and no-route failures are `503`, connect timeouts `504`, with the same reason. Any method can be retried.

#### Transport failure after sending → REQUEST_MAY_HAVE_BEEN_SENT
- **Scenario**: [negative-12.9-transport-failure-after-sending-request-may-have-been-sent.md](protocols/http/negative-12.9-transport-failure-after-sending-request-may-have-been-sent.md)
- **What happens**: Upstream reads the request and closes without answering → `502 PD` (reason `REQUEST_MAY_HAVE_BEEN_SENT`), `ESrc=gateway`. Read/write errors, HTTP/2 and protocol errors are `502`, read and request timeouts `504`, with the same reason. Retry only idempotent requests.

#### Scheme/protocol mismatches fail explicitly
- **Scenario**: [negative-12.7-scheme-protocol-mismatches-fail-explicitly.md](protocols/http/negative-12.7-scheme-protocol-mismatches-fail-explicitly.md)
- **What happens**: `protocol=http` with `scheme=grpc` rejected at config validation. `protocol=grpc` with `scheme=https` yields `502 ProtocolError`.

#### gRPC status mapping and error source
- **Scenario**: [negative-15.4-grpc-status-mapping-error-source.md](protocols/grpc/negative-15.4-grpc-status-mapping-error-source.md)
- **What happens**: gRPC `RESOURCE_EXHAUSTED` maps to rate limit error. Upstream gRPC failures marked `ESrc=upstream`.

#### SSE idle timeout ends the stream with an error
- **Scenario**: [negative-13.4-sse-idle-timeout-ends-stream-with-error.md](protocols/sse/negative-13.4-sse-idle-timeout-ends-stream-with-error.md)
- **What happens**: Upstream silent past `streaming_idle_timeout_secs` → body aborted after the events sent (no terminating chunk), nothing injected; SDK gets `StreamingError::IdleTimeout`.

#### Upstream failure mid-stream breaks the SSE stream
- **Scenario**: [negative-13.5-upstream-failure-mid-stream-breaks-stream.md](protocols/sse/negative-13.5-upstream-failure-mid-stream-breaks-stream.md)
- **What happens**: Upstream drops the connection after the head → caller gets the events sent, then a broken body; no JSON after the head; SDK gets `StreamingError::Stream`.

---

### Connection limits

#### WebSocket rate limit applies to connection establishment → 429
- **Scenario**: [negative-14.3-rate-limit-applies-connection-establishment.md](protocols/websocket/negative-14.3-rate-limit-applies-connection-establishment.md)
- **What happens**: Exceeding rate limit rejects upgrade with `429` (`PD`, `ESrc=gateway`).

#### WS connection idle timeout enforced
- **Scenario**: [negative-14.4-ws-connection-idle-timeout-enforced.md](protocols/websocket/negative-14.4-ws-connection-idle-timeout-enforced.md)
- **What happens**: Idle connection gets Close 1001 (Going Away) after the configured timeout, then EOF. A bare EOF fails.

#### Stalled caller does not block upstream's graceful close
- **Scenario**: [negative-14.5-stalled-caller-does-not-block-upstream-close.md](protocols/websocket/negative-14.5-stalled-caller-does-not-block-upstream-close.md)
- **What happens**: Caller stops reading; upstream still gets Close 1001 and a half-close within the close timeout. Each side's announcement has its own grace budget.

---

### Error handling invariants

#### Gateway errors always use RFC 9457 PD
- **Scenario**: *Section 24.1 — no separate file.*
- **What happens**: For each gateway error class (400/401/403/404/409/413/429/5xx), body is `PD`.

#### ESrc header set for both gateway and upstream failures
- **Scenario**: *Section 24.2 — no separate file.*
- **What happens**: Gateway error → `ESrc=gateway`. Upstream error → `ESrc=upstream`.

#### Retry-After present for retriable gateway errors
- **Scenario**: *Section 24.3 — no separate file.*
- **What happens**: `429`, `503 link unavailable`, `503 circuit breaker open`, `504` timeouts include `Retry-After`.

#### Gateway 502/503/504 from the upstream exchange carry a dispatch reason
- **Scenario**: [12.8](protocols/http/negative-12.8-transport-failure-before-sending-request-not-sent.md), [12.9](protocols/http/negative-12.9-transport-failure-after-sending-request-may-have-been-sent.md).
- **What happens**: `context.reason` is `REQUEST_NOT_SENT` or `REQUEST_MAY_HAVE_BEEN_SENT`, set from the engine error kind; a failed resend after a first attempt the upstream may have processed is `REQUEST_MAY_HAVE_BEEN_SENT`. A disabled upstream's `503` carries `UPSTREAM_DISABLED`, a credential store outage `CREDENTIAL_STORE_UNAVAILABLE`, an OAuth2 identity provider outage `IDENTITY_PROVIDER_UNAVAILABLE`.

#### Stream aborted classified distinctly
- **Scenario**: *Section 24.4 — no separate file.*
- **What happens**: Upstream read/write failure before the response head → `502 StreamAborted`. After the head no status can follow: the body is aborted instead (13.5).

#### Every documented error type has at least one reproducer
- **Scenario**: *Section 24.5 — no separate file.*
- **What happens**: `ValidationError`, `RouteNotFound`, `AuthenticationFailed`, `PayloadTooLarge`, `RateLimitExceeded`, `SecretNotFound` (401), `UpstreamDisabled`, `CredentialStoreUnavailable`, `IdpUnavailable`, `ProtocolError`, `DownstreamError`, `StreamAborted`, `LinkUnavailable`, `CircuitBreakerOpen`, `ConnectionTimeout`, `RequestTimeout`, `IdleTimeout`, `PluginNotFound`, `PluginInUse` — each has at least one reproducer scenario.

---

### Concurrency & per-endpoint circuit scope (future-facing)

Upstream circuit breakers are implemented: [18.9](rate-limiting/negative-18.9-circuit-opens-after-consecutive-failures.md), [18.10](rate-limiting/positive-18.10-half-open-probe-closes-circuit.md), [18.11](rate-limiting/positive-18.11-config-change-or-delete-resets-circuit.md).

#### Concurrency limit rejects when max in-flight reached → 503
- *No scenario file (future-facing).*
- **What happens**: When `max_concurrent` exceeded, return `503 concurrency_limit.exceeded` with `Retry-After`.

#### Queue strategy buffers requests up to max depth
- *No scenario file (future-facing).*
- **What happens**: FIFO order. `queue.full` when depth exceeded. `queue.timeout` when wait exceeds config. Memory limit enforced.

#### Streaming requests hold permits until completion
- *No scenario file (future-facing).*
- **What happens**: SSE and WS hold concurrency permit until closed.

#### Per-endpoint circuit scope
- *No scenario file (future-facing).*
- **What happens**: With `scope=per_endpoint`, failures isolate to single endpoint. Today one breaker covers all endpoints of an upstream.

---

### ID & cross-surface consistency

#### IDs are anonymous GTS identifiers on API surface
- **Scenario**: *Section 27.1 — no separate file.*
- **What happens**: `GET /upstreams/{id}` accepts `gts.cf.core.oagw.upstream.v1~{uuid}`. Same for routes/plugins.

---

### Configuration storage

#### Registry-managed resources are read-only → 400
- **Scenario**: [negative-19.2-registry-managed-resources-read-only.md](configuration-storage/negative-19.2-registry-managed-resources-read-only.md)
- **What happens**: `PUT` or `DELETE` of an upstream or route provisioned from the types registry returns `400 PD` (`failed_precondition`, violation type `REGISTRY_MANAGED`, subject `managed_by`) and changes nothing. A route created through the API on a registry upstream is allowed, and can be deleted.

---

## Appendix: Coverage Matrix

For E2E coverage, ensure at least one scenario exists for each cell:

### Protocol × scheme
- HTTP:
  - `https`
- Streaming:
  - SSE over `https`
  - WebSocket over `wss`
  - WebTransport over `wt` (feature or explicit-not-supported behavior)
- gRPC:
  - `grpc` (and HTTP/2 multiplexing on shared ingress port)

### Outbound auth plugin × protocol
- `noop`: HTTP
- `apikey`: HTTP, WebSocket, SSE, gRPC
- `basic`: HTTP
- `bearer`: HTTP
- `oauth2.client_cred`: HTTP
- `oauth2.client_cred_basic`: HTTP
- `aws_sigv4`: HTTP, WebSocket handshake (signed with an empty payload), SSE responses

### Plugin chain phases
- Guard-only (`on_request`)
- Transform (`on_request`)
- Transform (`on_response`)
- Transform (`on_error`)

### Error source
- Gateway-generated (validation/rate-limit/etc)
- Upstream passthrough (4xx/5xx)

## Named upstream credentials (planned)

Phase 1 specifications for [ADR-0021](../docs/ADR/0021-upstream-credentials.md). Use the [shared fixture](upstream-credentials/README.md#shared-setup); these cases describe future behavior, not implemented endpoints.

- [Select accounts on one route](upstream-credentials/positive-30.1-select-several-accounts.md)
- [Reject empty selector](upstream-credentials/negative-30.2-empty-selector.md)
- [Reject duplicate selector](upstream-credentials/negative-30.3-duplicate-selector.md)
- [Reject comma selector](upstream-credentials/negative-30.4-comma-selector.md)
- [Reject mixed case selector](upstream-credentials/negative-30.5-mixed-case-selector.md)
- [Reject reserved default](upstream-credentials/negative-30.6-reserved-default.md)
- [Reject malformed selector](upstream-credentials/negative-30.7-malformed-selector.md)
- [Hide absent and inaccessible items](upstream-credentials/negative-30.8-hide-unavailable-items.md)
- [Deny an operation before exposing items](upstream-credentials/negative-30.9-operation-permission.md)
- [Refuse a private shadow or recreated record](upstream-credentials/negative-30.10-pin-mismatch.md)
- [Reject an exhausted credential before CredStore](upstream-credentials/negative-30.11-independent-and-shared-limits.md)
- [Suspend during an outage with CRUD disabled](upstream-credentials/positive-30.12-suspend-without-crud.md)
- [Keep a newer suspension](upstream-credentials/negative-30.13-resume-suspend-race.md)
- [Reject invalid registry ownership](upstream-credentials/negative-30.14-registry-ownership-conflict.md)
- [Ignore delayed cleanup of retired counters](upstream-credentials/positive-30.15-generation-cleanup.md)
- [Use live credential state with cached parent policies](upstream-credentials/positive-30.16-live-state-cached-parents.md)
- [Fail a live read without stale credential fallback](upstream-credentials/negative-30.17-live-read-outage.md)
- [Keep attempt debits on a secret failure](upstream-credentials/negative-30.18-credstore-outage.md)
- [Reject incompatible parent auth](upstream-credentials/negative-30.19-incompatible-parent.md)
- [Handle preflight without capacity](upstream-credentials/positive-30.20-cors-preflight.md)
- [Keep approvals on replace](upstream-credentials/positive-30.21-preserve-endpoint-approval.md)
- [Read a complete credential resource](upstream-credentials/positive-30.22-resource-and-list.md)
- [Keep wrong-account forwarding out of rollout](upstream-credentials/positive-30.23-rollout-and-audit.md)

## Per-request secret references

Specifications for [ADR-0022](../docs/ADR/0022-request-secret-refs.md). Use the [shared fixture](request-secret-refs/README.md#shared-setup).

- [Inject the requested secret](request-secret-refs/positive-31.1-inject-requested-secret.md)
- [Use the configured default without a header](request-secret-refs/positive-31.2-default-secret.md)
- [Use an ancestor's request_ref auth](request-secret-refs/positive-31.3-inherited-mode-and-owner-chain.md)
- [Reject the header on an upstream that does not accept it](request-secret-refs/negative-31.4-header-not-allowed.md)
- [Require a reference when there is no default](request-secret-refs/negative-31.5-secret-ref-required.md)
- [Reject an empty, malformed, repeated or comma-separated header](request-secret-refs/negative-31.6-invalid-header.md)
- [Answer missing and invisible secrets the same](request-secret-refs/negative-31.7-secret-not-accessible.md)
- [Refuse a secret owned above the auth owner](request-secret-refs/negative-31.8-owner-outside-chain.md)
- [Refuse a secret of a type the upstream does not allow](request-secret-refs/negative-31.9-type-not-allowed.md)
- [Refuse a request whose auth header a rule or transform set](request-secret-refs/negative-31.10-auth-header-changed.md)
- [Keep the debit on a CredStore outage](request-secret-refs/negative-31.11-credstore-outage.md)
- [Reject an invalid credential mode on write](request-secret-refs/negative-31.12-invalid-mode-config.md)
