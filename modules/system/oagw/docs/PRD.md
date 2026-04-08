# PRD — Outbound API Gateway (OAGW)


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Module-Specific Environment Constraints](#31-module-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Core Management](#51-core-management)
  - [5.2 Proxy Execution](#52-proxy-execution)
  - [5.3 Plugin System](#53-plugin-system)
  - [5.4 Streaming](#54-streaming)
  - [5.5 Configuration Hierarchy](#55-configuration-hierarchy)
  - [5.6 Error Codes](#56-error-codes)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Module-Specific NFRs](#61-module-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Proxy HTTP Request](#proxy-http-request)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

<!--
=============================================================================
PRODUCT REQUIREMENTS DOCUMENT (PRD)
=============================================================================
PURPOSE: Define WHAT the system must do and WHY — business requirements,
functional capabilities, and quality attributes.

SCOPE:
  ✓ Business goals and success criteria
  ✓ Actors (users, systems) that interact with this module
  ✓ Functional requirements (WHAT, not HOW)
  ✓ Non-functional requirements (quality attributes, SLOs)
  ✓ Scope boundaries (in/out of scope)
  ✓ Assumptions, dependencies, risks

NOT IN THIS DOCUMENT (see other templates):
  ✗ Stakeholder needs (managed at project/task level by steering committee)
  ✗ Technical architecture, design decisions → DESIGN.md
  ✗ Why a specific technical approach was chosen → ADR/
  ✗ Detailed implementation flows, algorithms → features/

STANDARDS ALIGNMENT:
  - IEEE 830 / ISO/IEC/IEEE 29148:2018 (requirements specification)
  - IEEE 1233 (system requirements)
  - ISO/IEC 15288 / 12207 (requirements definition)

REQUIREMENT LANGUAGE:
  - Use "MUST" or "SHALL" for mandatory requirements (implicit default)
  - Do not use "SHOULD" or "MAY" — use priority p2/p3 instead
  - Be specific and clear; no fluff, bloat, duplication, or emoji
=============================================================================
-->

## 1. Overview

### 1.1 Purpose

The Outbound API Gateway (OAGW) manages all outbound API requests from CyberFabric to external services. It acts as a centralized proxy layer that handles credential injection, rate limiting, header transformation, and security enforcement for every external call made by the platform.

OAGW provides a unified interface for application modules to reach external APIs without managing credentials, connection details, or security policies directly. Modules send requests to OAGW's proxy endpoint, and OAGW resolves the target upstream, injects authentication, applies policies, and forwards the request.

### 1.2 Background / Problem Statement

CyberFabric modules need to communicate with external third-party services (e.g., OpenAI, Stripe, payment gateways). Without a centralized gateway, each module must independently manage credentials, rate limits, error handling, and security policies for outbound calls. This leads to credential sprawl, inconsistent error handling, and no unified observability.

OAGW solves these problems by providing a single outbound proxy layer with pluggable authentication, configurable rate limiting, header transformation, and security policies. All external calls flow through OAGW, ensuring consistent credential isolation, audit trails, and policy enforcement across the platform.

### 1.3 Goals (Business Outcomes)

- Centralize all outbound API credential management — zero credential exposure in application code, logs, or error messages
- Provide a unified proxy interface for external services with consistent error handling and observability
- Enforce rate limiting and security policies (SSRF protection, header validation) to prevent abuse and cost overruns
- Support multi-tenant hierarchical configuration with sharing, inheritance, and enforcement semantics

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Upstream | External service target defined by server endpoints (scheme/host/port), protocol, authentication configuration, default headers, and rate limits |
| Route | API path on an upstream that matches requests by method, path, and query allowlist (HTTP) or service and method (gRPC). Routes map inbound proxy requests to specific upstream behaviors |
| Plugin | Modular processor attached to upstreams or routes. Three types: Auth (credential injection), Guard (validation/policy enforcement), Transform (request/response mutation) |
| Data Plane | Internal service that orchestrates proxy requests: resolves configuration, executes plugin chains, and forwards HTTP calls to external services |
| Control Plane | Internal service that manages configuration data (upstreams, routes, plugins) with repository access |
| Alias | Short identifier used in proxy URLs to reference an upstream. Auto-derived from hostname for hostname-based endpoints (user-provided alias rejected); explicit alias required for IP-based or non-derivable endpoints. Normalized to ASCII lowercase; resolution is case-insensitive |
| Sharing Mode | Configuration visibility setting for hierarchical tenancy: `private` (owner only), `inherit` (descendants can override), `enforce` (descendants cannot override) |
| GTS | Global Type System — the platform's schema and instance registration system used for plugin type identification |

## 2. Actors

> **Note**: Stakeholder needs are managed at project/task level by steering committee. Document **actors** (users, systems) that interact with this module.

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-oagw-actor-platform-operator`

- **Role**: Manages global configuration: upstreams, routes, system-wide plugins, and security policies.
- **Needs**: CRUD operations for upstreams, routes, and plugins; ability to enforce configuration on descendant tenants; visibility into all proxy traffic and errors.

#### Tenant Administrator

**ID**: `cpt-cf-oagw-actor-tenant-admin`

- **Role**: Manages tenant-specific settings: credentials, rate limits, custom plugins, and configuration overrides within allowed sharing policies.
- **Needs**: Override inherited configurations where permitted; manage tenant-scoped credentials; set stricter rate limits for their tenant hierarchy.

#### Application Developer

**ID**: `cpt-cf-oagw-actor-app-developer`

- **Role**: Consumes external APIs via the OAGW proxy endpoint without managing credentials or external service details.
- **Needs**: Simple proxy URL (`/api/oagw/v1/proxy/{alias}/{path}`) with transparent credential injection; clear error responses when requests fail.

### 2.2 System Actors

#### Credential Store

**ID**: `cpt-cf-oagw-actor-cred-store`

- **Role**: Secure storage and retrieval of secrets (API keys, OAuth tokens, passwords) by UUID reference. OAGW never stores credentials directly — it references them via `cred_store`.

#### Types Registry

**ID**: `cpt-cf-oagw-actor-types-registry`

- **Role**: GTS schema and instance registration and validation. OAGW registers its plugin type schemas and upstream/route type definitions in the types registry.

#### Upstream Service

**ID**: `cpt-cf-oagw-actor-upstream-service`

- **Role**: External third-party service (e.g., OpenAI, Stripe) that OAGW proxies requests to. OAGW treats upstream services as opaque HTTP endpoints.

## 3. Operational Concept & Environment

> **Note**: Project-wide runtime, OS, architecture, lifecycle policy, and integration patterns defined in root PRD. Document only module-specific deviations here. **Delete this section if no special constraints.**

### 3.1 Module-Specific Environment Constraints

None. OAGW follows standard CyberFabric ModKit module conventions.

## 4. Scope

### 4.1 In Scope

- CRUD management of upstreams, routes, and plugins via REST API
- HTTP/HTTPS proxy with alias-based upstream resolution and route matching
- Credential injection via auth plugins (API Key, Basic Auth, OAuth2 Client Credentials, Bearer Token)
- Rate limiting at upstream and route levels with configurable strategies
- Header transformation (set/add/remove, hop-by-hop stripping, passthrough control)
- Plugin system with three types: Auth, Guard, Transform (built-in and external)
- Streaming support: HTTP request/response, SSE, WebSocket, WebTransport
- Multi-tenant hierarchical configuration with sharing modes (private/inherit/enforce)
- Alias resolution with shadowing across tenant hierarchy
- Error source distinction (gateway errors vs upstream errors)
- Metrics collection and audit logging

### 4.2 Out of Scope

- DNS resolution and IP pinning rule implementation details
- Plugin versioning and lifecycle management details
- Response caching (client/upstream responsibility)
- Automatic request retries (client responsibility)
- gRPC proxying (planned for phase 4)

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration, e2e) targeting 90%+ code coverage unless otherwise specified. Document verification method only for non-test approaches (analysis, inspection, demonstration).

### 5.1 Core Management

#### Upstream Management

- [ ] `p1` - **ID**: `cpt-cf-oagw-fr-upstream-mgmt`

The system **MUST** provide CRUD operations for upstream configurations. Each upstream defines server endpoints (scheme/host/port), protocol, authentication config, headers, and rate limits. All operations are tenant-scoped.

- **Rationale**: Upstreams are the fundamental configuration unit — every proxy request targets an upstream.
- **Actors**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

#### Route Management

- [ ] `p1` - **ID**: `cpt-cf-oagw-fr-route-mgmt`

The system **MUST** provide CRUD operations for routes. Routes define matching rules (HTTP method, path pattern, query parameter allowlist) that map inbound proxy requests to specific upstreams.

- **Rationale**: Routes control which requests reach which upstream endpoints and with what transformations.
- **Actors**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

#### Enable/Disable Semantics

- [ ] `p1` - **ID**: `cpt-cf-oagw-fr-enable-disable`

The system **MUST** support an `enabled` boolean field (default: `true`) on upstreams and routes. A disabled upstream **MUST** cause all proxy requests to be rejected with `503 Service Unavailable`. A disabled route **MUST** be excluded from route matching. If an ancestor tenant disables an upstream, it **MUST** be disabled for all descendants; descendants **MUST NOT** re-enable an ancestor-disabled resource.

- **Rationale**: Enables temporary maintenance, emergency circuit breaks, and gradual rollouts without deleting configuration.
- **Actors**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

### 5.2 Proxy Execution

#### Request Proxying

- [ ] `p1` - **ID**: `cpt-cf-oagw-fr-request-proxy`

The system **MUST** proxy requests via `{METHOD} /api/oagw/v1/proxy/{alias}[/{path}][?{query}]`. It **MUST** resolve the upstream by alias, match the route by method/path, merge configurations (upstream < route < tenant), execute the plugin chain, and forward the request to the external service. The gateway performs no automatic full client-request retries—i.e., it will not re-issue the original client request as a whole—but connector-level endpoint/connection failover or connection-retry attempts (endpoint-level retries) performed by the upstream connector are permitted.

- **Rationale**: Core value proposition — unified proxy endpoint that handles credential injection, transformation, and forwarding transparently.
- **Actors**: `cpt-cf-oagw-actor-app-developer`

#### Authentication Injection

- [ ] `p1` - **ID**: `cpt-cf-oagw-fr-auth-injection`

The system **MUST** inject credentials into outbound requests via auth plugins. Supported authentication methods: API Key (header/query), HTTP Basic Auth, OAuth2 Client Credentials, and Bearer Token. Credentials **MUST** be retrieved from the credential store at request time by UUID reference.

- **Rationale**: Centralizes credential management so application developers never handle API keys or tokens directly.
- **Actors**: `cpt-cf-oagw-actor-app-developer`, `cpt-cf-oagw-actor-cred-store`

#### Rate Limiting

- [ ] `p1` - **ID**: `cpt-cf-oagw-fr-rate-limiting`

The system **MUST** enforce rate limits at upstream and route levels. Configuration **MUST** include: rate, window, capacity, cost, scope (global/tenant/user/IP), and strategy (reject with 429 + Retry-After, queue, or degrade).

- **Rationale**: Prevents abuse, cost overruns, and protects external service agreements.
- **Actors**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

#### Header Transformation

- [ ] `p1` - **ID**: `cpt-cf-oagw-fr-header-transform`

The system **MUST** transform request and response headers: set, add, and remove operations; passthrough control; automatic stripping of hop-by-hop headers (Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade).

- **Rationale**: Ensures clean outbound requests and prevents header leakage between internal and external networks.
- **Actors**: `cpt-cf-oagw-actor-app-developer`

### 5.3 Plugin System

#### Plugin System

- [ ] `p2` - **ID**: `cpt-cf-oagw-fr-plugin-system`

The system **MUST** provide a plugin system with three plugin types: Auth (`gts.x.core.oagw.auth_plugin.v1~*`) for credential injection, Guard (`gts.x.core.oagw.guard_plugin.v1~*`) for validation/policy enforcement (can reject requests), and Transform (`gts.x.core.oagw.transform_plugin.v1~*`) for request/response mutation. Execution order **MUST** be: Auth -> Guards -> Transform(request) -> Upstream call -> Transform(response/error). Upstream plugins **MUST** execute before route plugins. Plugin definitions **MUST** be immutable after creation; updates are performed by creating a new plugin version and re-binding references.

- **Rationale**: Extensibility for custom authentication schemes, validation rules, and request/response transformations without modifying the gateway core.
- **Actors**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

Circuit breaker is a core gateway resilience capability (configured as core policy), not a plugin.

#### Built-in Plugins

- [ ] `p2` - **ID**: `cpt-cf-oagw-fr-builtin-plugins`

The system **MUST** include the following built-in plugins:

**Auth Plugins**:
- `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.noop.v1` — No authentication
- `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.apikey.v1` — API key injection (header/query)
- `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.basic.v1` — HTTP Basic authentication
- `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred.v1` — OAuth2 client credentials flow
- `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred_basic.v1` — OAuth2 with Basic auth
- `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.bearer.v1` — Bearer token injection

**Guard Plugins**:
- `gts.x.core.oagw.guard_plugin.v1~x.core.oagw.timeout.v1` — Request timeout enforcement
- `gts.x.core.oagw.guard_plugin.v1~x.core.oagw.cors.v1` — CORS preflight validation

**Transform Plugins**:
- `gts.x.core.oagw.transform_plugin.v1~x.core.oagw.logging.v1` — Request/response logging
- `gts.x.core.oagw.transform_plugin.v1~x.core.oagw.metrics.v1` — Prometheus metrics collection
- `gts.x.core.oagw.transform_plugin.v1~x.core.oagw.request_id.v1` — X-Request-ID propagation

- **Rationale**: Covers the most common outbound API authentication and observability patterns out of the box.
- **Actors**: `cpt-cf-oagw-actor-platform-operator`

### 5.4 Streaming

#### Streaming Support

- [ ] `p1` - **ID**: `cpt-cf-oagw-fr-streaming`

The system **MUST** support HTTP request/response proxying and SSE (Server-Sent Events) streaming with proper connection lifecycle handling (open/close/error). The system **MUST** support WebSocket and WebTransport session flows.

- **Rationale**: Many external APIs (e.g., OpenAI chat completions) use SSE for streaming responses; WebSocket/WebTransport needed for bidirectional real-time protocols.
- **Actors**: `cpt-cf-oagw-actor-app-developer`

### 5.5 Configuration Hierarchy

#### Configuration Layering

- [ ] `p2` - **ID**: `cpt-cf-oagw-fr-config-layering`

The system **MUST** merge configurations with the following priority order: Upstream (base) < Route < Tenant (highest priority).

- **Rationale**: Allows fine-grained configuration at each level without duplicating base settings.
- **Actors**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

#### Hierarchical Configuration Override

- [ ] `p2` - **ID**: `cpt-cf-oagw-fr-hierarchical-config`

The system **MUST** support hierarchical configuration override across tenant hierarchies with three sharing modes:

| Mode | Behavior |
|------|----------|
| `private` | Not visible to descendants (default) |
| `inherit` | Visible; descendant can override if specified |
| `enforce` | Visible; descendant cannot override |

Override rules:
- **Auth**: With `sharing: inherit`, descendant with permission can use own credentials
- **Rate limits**: Descendant can only be stricter: `effective = min(ancestor.enforced, descendant)`
- **Plugins**: Descendant's plugins append; enforced plugins cannot be removed

Tags do not have a sharing mode — they always use add-only union semantics:
`effective_tags = union(ancestor_tags..., descendant_tags)`. Descendants can add tags but cannot remove inherited tags.
If upstream creation resolves to an existing upstream definition (binding-style flow), request tags are treated as tenant-local additions for effective discovery; they do not mutate ancestor tags.

**Example — Hierarchical Override**:

```text
Partner Tenant:
  upstream: api.openai.com
  auth: { secret_ref: "cred://partner-openai-key", sharing: "inherit" }
  rate_limit: { rate: 10000/min, sharing: "enforce" }

Leaf Tenant (with permission):
  auth: { secret_ref: "cred://my-own-openai-key" }  <- overrides partner's key
  rate_limit: { rate: 100/min }  <- effective: min(10000, 100) = 100

Leaf Tenant (without permission):
  auth: inherited from partner  <- uses partner's key
```

- **Rationale**: Enables partner/customer hierarchies where partners share upstream access with controlled credential and rate limit policies.
- **Actors**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

#### Alias Resolution and Shadowing

- [ ] `p2` - **ID**: `cpt-cf-oagw-fr-alias-resolution`

The system **MUST** identify upstreams by alias in proxy URLs: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}`. Alias is **enforced** based on endpoint type: hostname-based endpoints always auto-derive alias (user-provided alias is rejected); IP-based or non-derivable endpoints require explicit alias. Derivation rules: single hostname uses hostname (without port for standard ports); multiple hostnames use the longest common domain suffix (≥2 labels), validated against the public suffix list to reject bare public suffixes (e.g., `co.uk`); IP addresses or no common suffix require explicit alias. Aliases are normalized to ASCII lowercase with trailing dots stripped; resolution is case-insensitive. When resolving an alias, the system **MUST** search the tenant hierarchy from descendant to root; the closest match wins (descendant shadows ancestor). Enforced limits from ancestors **MUST** still apply across shadowing.

**Shadowing Resolution Order**:

```text
Request from: subsub-tenant
Alias: "api.openai.com"

Resolution order:
1. subsub-tenant's upstreams  <- wins if found
2. sub-tenant's upstreams
3. root-tenant's upstreams
```

**Alias Examples**:

```json
// Hostname, standard port — alias auto-derived as "api.openai.com"
// User-provided alias is rejected (400 Validation)
{
  "server": { "endpoints": [ { "scheme": "https", "host": "api.openai.com", "port": 443 } ] }
  // alias: "api.openai.com" (auto-derived)
}

// Hostname, non-standard port — alias auto-derived as "api.openai.com:8443"
{
  "server": { "endpoints": [ { "scheme": "https", "host": "api.openai.com", "port": 8443 } ] }
  // alias: "api.openai.com:8443" (auto-derived)
}

// Multi-region with auto-derived alias (common suffix)
{
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "us.vendor.com", "port": 443 },
      { "scheme": "https", "host": "eu.vendor.com", "port": 443 }
    ]
  }
  // alias: "vendor.com" (auto-derived from common suffix)
}

// IP-based endpoints — explicit alias mandatory
{
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "10.0.1.1", "port": 443 },
      { "scheme": "https", "host": "10.0.1.2", "port": 443 }
    ]
  },
  "alias": "my-internal-service"
}
```

**Multi-Endpoint Pooling**:

Multiple endpoints within the same upstream form a load-balance pool. Requests are distributed across endpoints. Endpoints in a pool **MUST** have identical `protocol` (cannot mix HTTP and gRPC), `scheme` (cannot mix https and wss), and `port` (all endpoints must use the same port).

**Enforced Limits Across Shadowing**:

When a descendant shadows an ancestor's alias, enforced limits from the ancestor still apply:

```text
Root: alias "api.openai.com", rate_limit: { sharing: "enforce", rate: 10000 }
Sub:  alias "api.openai.com" (shadows root)

Effective for sub: min(root.enforced:10000, sub:500) = 500
```

- **Rationale**: Provides human-readable proxy URLs while supporting multi-tenant isolation and override semantics.
- **Actors**: `cpt-cf-oagw-actor-app-developer`, `cpt-cf-oagw-actor-platform-operator`

### 5.6 Error Codes

#### Error Codes

- [ ] `p1` - **ID**: `cpt-cf-oagw-fr-error-codes`

The system **MUST** return the following error codes for proxy and management operations:

| HTTP | Error                | Retriable |
|------|----------------------|-----------|
| 400  | ValidationError      | No        |
| 401  | AuthenticationFailed | No        |
| 404  | RouteNotFound        | No        |
| 413  | PayloadTooLarge      | No        |
| 429  | RateLimitExceeded    | Yes       |
| 500  | SecretNotFound       | No        |
| 502  | DownstreamError      | Depends   |
| 503  | CircuitBreakerOpen   | Yes       |
| 504  | Timeout              | Yes       |

- **Rationale**: Consistent, well-defined error codes enable clients to implement correct retry and fallback behavior.
- **Actors**: `cpt-cf-oagw-actor-app-developer`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide NFRs (performance, security, reliability, scalability) defined in root PRD and [guidelines/](../guidelines/). Document only module-specific NFRs here: **exclusions** from defaults or **standalone** requirements.
>
> **Testing strategy**: NFRs verified via automated benchmarks, security scans, and monitoring unless otherwise specified.

### 6.1 Module-Specific NFRs

#### Low Latency

- [ ] `p1` - **ID**: `cpt-cf-oagw-nfr-low-latency`

The system **MUST** add less than 10ms overhead at p95 for proxy requests. Plugin execution timeouts **MUST** be enforced.

- **Threshold**: <10ms added latency at p95 (excluding upstream response time)
- **Rationale**: OAGW is on the hot path for every outbound API call; excessive latency directly impacts end-user experience.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### High Availability

- [ ] `p1` - **ID**: `cpt-cf-oagw-nfr-high-availability`

The system **MUST** maintain 99.9% availability. Circuit breakers **MUST** prevent cascade failures from unhealthy upstreams.

- **Threshold**: 99.9% uptime; circuit breaker trips within 5 failed requests in 30s window
- **Rationale**: OAGW is a critical path component — if it's down, no outbound API calls succeed.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### SSRF Protection

- [ ] `p1` - **ID**: `cpt-cf-oagw-nfr-ssrf-protection`

The system **MUST** validate DNS resolution results, enforce IP pinning rules, strip well-known internal headers, and validate request paths and query parameters against route configuration.

- **Threshold**: Zero SSRF vulnerabilities in security audit
- **Rationale**: OAGW makes HTTP requests to external URLs — it must not be exploitable as an SSRF vector.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### Credential Isolation

- [ ] `p1` - **ID**: `cpt-cf-oagw-nfr-credential-isolation`

Credentials **MUST** never appear in logs, error messages, or API responses. All credential references **MUST** use UUID pointers to the credential store. Credentials **MUST** be tenant-isolated.

- **Threshold**: Zero credential exposure in any log, error, or API output
- **Rationale**: Credential leakage is a critical security risk; defense-in-depth requires isolation at every layer.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### Input Validation

- [ ] `p1` - **ID**: `cpt-cf-oagw-nfr-input-validation`

The system **MUST** validate path, query parameters, headers, and body size for all inbound requests. Invalid requests **MUST** be rejected with `400 Bad Request`.

- **Threshold**: All proxy requests validated before forwarding; 100% of malformed requests rejected
- **Rationale**: Prevents injection attacks and ensures only well-formed requests reach external services.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### Observability

- [ ] `p2` - **ID**: `cpt-cf-oagw-nfr-observability`

The system **MUST** log all proxy requests with correlation IDs and expose Prometheus metrics for request counts, latencies, error rates, and rate limit state.

- **Threshold**: 100% of proxy requests logged with correlation ID; metrics scraped at /metrics endpoint
- **Rationale**: Operators need full visibility into outbound API traffic patterns, errors, and performance.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### Starlark Sandbox

- [ ] `p3` - **ID**: `cpt-cf-oagw-nfr-starlark-sandbox`

Custom Starlark plugins **MUST** run in a sandbox with no network I/O, no file I/O, no imports, and enforced timeout and memory limits.

- **Threshold**: Zero sandbox escapes; plugin execution timeout ≤ 100ms; memory ≤ 10MB per invocation
- **Rationale**: User-defined plugins must not compromise gateway security or stability.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### Multi-tenancy

- [ ] `p1` - **ID**: `cpt-cf-oagw-nfr-multi-tenancy`

All resources (upstreams, routes, plugins) **MUST** be tenant-scoped. Tenant isolation **MUST** be enforced at the data layer.

- **Threshold**: Zero cross-tenant data access
- **Rationale**: CyberFabric is a multi-tenant platform; strict tenant isolation is a fundamental security requirement.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

### 6.2 NFR Exclusions

None. All project-default NFRs apply to this module.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Management REST API

- [ ] `p1` - **ID**: `cpt-cf-oagw-interface-management-api`

- **Type**: REST API (OpenAPI 3.0)
- **Stability**: unstable
- **Description**: CRUD operations for upstreams, routes, and plugins via `/api/oagw/v1/upstreams`, `/api/oagw/v1/routes`, `/api/oagw/v1/plugins` endpoints. Includes `GET /api/oagw/v1/plugins/{id}/source` for retrieving plugin source content.
- **Breaking Change Policy**: Major version bump required (v1 → v2)

#### Proxy REST API

- [ ] `p1` - **ID**: `cpt-cf-oagw-interface-proxy-api`

- **Type**: REST API
- **Stability**: unstable
- **Description**: Proxy endpoint at `{METHOD} /api/oagw/v1/proxy/{alias}[/{path}][?{query}]` that forwards requests to external services with credential injection and transformation.
- **Breaking Change Policy**: Major version bump required (v1 → v2)

#### SDK Client Trait

- [ ] `p1` - **ID**: `cpt-cf-oagw-interface-sdk-client`

- **Type**: Rust trait (`ServiceGatewayClientV1` in `oagw-sdk` crate)
- **Stability**: unstable
- **Description**: Public Rust trait for inter-module communication. Exposes upstream and route management operations and proxy invocation for in-process callers.
- **Breaking Change Policy**: Trait changes require coordinated release of all dependent modules

### 7.2 External Integration Contracts

#### Credential Store Contract

- [ ] `p1` - **ID**: `cpt-cf-oagw-contract-cred-store`

- **Direction**: required from client
- **Protocol/Format**: In-process Rust trait call via `cred_store` SDK
- **Compatibility**: Must match `cred_store` SDK version in workspace

#### Types Registry Contract

- [ ] `p1` - **ID**: `cpt-cf-oagw-contract-types-registry`

- **Direction**: required from client
- **Protocol/Format**: In-process Rust trait call via `types_registry` SDK
- **Compatibility**: Must match `types_registry` SDK version in workspace

## 8. Use Cases

### Proxy HTTP Request

- [ ] `p1` - **ID**: `cpt-cf-oagw-usecase-proxy-request`

**Actor**: `cpt-cf-oagw-actor-app-developer`

**Preconditions**:
- Upstream and route are configured and enabled
- Auth plugin and credentials are set up for the upstream

**Main Flow**:
1. App sends request to `/api/oagw/v1/proxy/{alias}/{path}`
2. System resolves upstream by alias (tenant hierarchy search)
3. System matches route by method/path
4. System merges configs (upstream < route < tenant)
5. System retrieves credentials from credential store and transforms request
6. System executes plugin chain (Auth -> Guard -> Transform)
7. System forwards request to upstream and returns response

**Postconditions**:
- Response from external service returned to caller
- Request logged with correlation ID

**Alternative Flows**:
- **Upstream not found**: Return 404 RouteNotFound
- **Upstream disabled**: Return 503 with gateway error type
- **Auth plugin fails**: Return 401 AuthenticationFailed
- **Rate limit exceeded**: Return 429 with Retry-After header
- **Upstream timeout**: Return 504 Timeout

#### Configure Upstream

- [ ] `p1` - **ID**: `cpt-cf-oagw-usecase-configure-upstream`

**Actor**: `cpt-cf-oagw-actor-platform-operator`

**Preconditions**:
- Actor is authenticated with `gts.x.core.oagw.upstream.v1~:create` permission

**Main Flow**:
1. Operator sends POST to `/api/oagw/v1/upstreams` with server endpoints, protocol, auth config
2. System validates configuration (endpoint format, alias uniqueness, credential reference validity)
3. System persists upstream configuration

**Postconditions**:
- Upstream is created and available for proxy routing

**Alternative Flows**:
- **Validation fails**: Return 400 ValidationError with details
- **Alias conflict**: Return 409 Conflict

#### Configure Route

- [ ] `p1` - **ID**: `cpt-cf-oagw-usecase-configure-route`

**Actor**: `cpt-cf-oagw-actor-platform-operator`

**Preconditions**:
- Target upstream exists
- Actor is authenticated with `gts.x.core.oagw.route.v1~:create` permission

**Main Flow**:
1. Operator sends POST to `/api/oagw/v1/routes` with upstream_id and match rules
2. System validates upstream reference and match rule format
3. System persists route configuration

**Postconditions**:
- Route is created and active for request matching

**Alternative Flows**:
- **Upstream not found**: Return 400 ValidationError
- **Validation fails**: Return 400 ValidationError with details

#### Rate Limit Exceeded

- [ ] `p2` - **ID**: `cpt-cf-oagw-usecase-rate-limit-exceeded`

**Actor**: `cpt-cf-oagw-actor-app-developer`

**Preconditions**:
- Rate limit is configured on upstream or route
- Client has exceeded the configured rate

**Main Flow**:
1. App sends proxy request
2. System evaluates rate limit counter
3. Rate limit is exceeded — system applies configured strategy

**Postconditions**:
- Request handled per strategy (rejected, queued, or degraded)

**Alternative Flows**:
- **Strategy: reject**: Return 429 with Retry-After header
- **Strategy: queue**: Request queued for later execution within bounded capacity
- **Strategy: degrade**: Request processed with reduced functionality

#### SSE Streaming

- [ ] `p1` - **ID**: `cpt-cf-oagw-usecase-sse-streaming`

**Actor**: `cpt-cf-oagw-actor-app-developer`

**Preconditions**:
- Upstream supports SSE responses
- Route is configured for the target path

**Main Flow**:
1. App sends proxy request to SSE endpoint
2. System establishes connection to upstream
3. System forwards SSE events as received to client
4. Connection lifecycle managed (open/close/error)

**Postconditions**:
- All events forwarded; connection cleanly closed

**Alternative Flows**:
- **Upstream closes connection**: System closes client connection and logs event
- **Client disconnects**: System closes upstream connection

## 9. Acceptance Criteria

- [ ] Proxy requests complete with <10ms added latency at p95
- [ ] Zero credential exposure in any log output, error message, or API response
- [ ] 99.9% availability under normal operating conditions
- [ ] Complete audit trail for all proxy requests (correlation ID, timestamps, status)
- [ ] All upstream/route CRUD operations validated and tenant-scoped
- [ ] Rate limiting enforced per configuration; 429 responses include Retry-After header
- [ ] SSE streaming proxies events with correct lifecycle handling

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `types_registry` | GTS schema/instance registration for plugin types and upstream/route type definitions | p1 |
| `cred_store` | Secret material retrieval by UUID reference for auth plugin credential injection | p1 |
| `api_ingress` | REST API hosting via ModKit framework | p1 |
| `modkit-db` | Database persistence for upstream, route, and plugin configurations | p1 |
| `modkit-auth` | Authorization and SecurityContext extraction for all API endpoints | p1 |

## 11. Assumptions

- CyberFabric `cred_store` module is available and supports UUID-based secret retrieval
- ModKit framework provides module lifecycle, dependency injection, and REST API hosting
- Tenant hierarchy is resolved by the platform (tenant-resolver module); OAGW receives tenant_id from SecurityContext
- External upstream services are reachable via HTTP/HTTPS from the CyberFabric deployment environment

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Upstream service outage cascading to OAGW callers | High — all consumers of that upstream blocked | Circuit breaker pattern; configurable timeout and fallback behavior |
| Credential store unavailability | High — proxy requests fail if credentials cannot be retrieved | Cache last-known-good credentials with short TTL; alert on cred_store health |
| Rate limit state loss on restart | Medium — brief window of unlimited requests | Persist rate limit counters; accept brief burst on cold start |
| Plugin execution exceeding timeout | Medium — increased proxy latency | Enforced plugin timeout; circuit-break misbehaving plugins |

## 13. Open Questions

- What is the maximum number of endpoints per upstream pool?
- Should plugin garbage collection be time-based or reference-count-based?
- What is the retention policy for audit logs?

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)
