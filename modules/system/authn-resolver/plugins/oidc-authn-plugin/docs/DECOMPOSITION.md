# Decomposition: OIDC AuthN Resolver Plugin

**Overall implementation status:**
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Plugin Bootstrap, Registration, and Config Validation - HIGH](#21-plugin-bootstrap-registration-and-config-validation---high)
  - [2.2 JWT Validation Pipeline - HIGH](#22-jwt-validation-pipeline---high)
  - [2.3 Claim Mapping and SecurityContext Construction - HIGH](#23-claim-mapping-and-securitycontext-construction---high)
  - [2.4 OIDC Discovery and JWKS Lifecycle - HIGH](#24-oidc-discovery-and-jwks-lifecycle---high)
  - [2.5 Service-to-Service Token Exchange - MEDIUM](#25-service-to-service-token-exchange---medium)
  - [2.6 Reliability, Security Hardening, and Observability - MEDIUM](#26-reliability-security-hardening-and-observability---medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

This DECOMPOSITION breaks the OIDC AuthN Resolver Plugin into six features that map cleanly to implementation boundaries: bootstrap/configuration, JWT validation, claim mapping, OIDC/JWKS integration, S2S token exchange, and cross-cutting reliability/security/observability controls.

**Decomposition strategy**:

- Foundation-first delivery: bootstrap/config validation and endpoint discovery are implemented before request-path authentication features.
- Pipeline-oriented slicing: JWT validation and claim mapping are separated to keep scopes cohesive and testable.
- Cross-cutting concerns isolated: retry/circuit-breaker/metrics/security hardening are grouped to avoid coupling functional correctness with operational hardening.
- Incremental rollout: high-priority request-path capabilities (`p1`) are delivered first, then medium-priority S2S and operational maturity features (`p2`).

**Coverage note**: All listed PRD FR/NFR IDs, DESIGN principles, constraints, core components, and sequence IDs are assigned to at least one feature entry in this artifact.

## 2. Entries

### 2.1 [Plugin Bootstrap, Registration, and Config Validation](./features/feature-plugin-bootstrap-config-validation.md) - HIGH

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-feature-plugin-bootstrap-config-validation`

- **Purpose**: Initialize the plugin as a discoverable runtime component, validate all critical configuration up front, and ensure fail-fast startup behavior before any authentication requests are served.

- **Depends On**: None

- **Scope**:
  - Module wiring and startup initialization
  - GTS identity construction and ClientHub registration
  - Plugin metadata (vendor key, priority, display name)
  - Startup validation for trusted issuers, algorithm policy, claim mappings, and timeout/retry/circuit-breaker boundaries
  - Configuration normalization and deterministic precedence rules

- **Out of scope**:
  - Request-time JWT signature verification
  - OIDC discovery/JWKS fetch logic
  - S2S token acquisition flows

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-authn-plugin-fr-clienthub-registration`

- **Design Principles Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-principle-minimalist-interface`
  - [x] `p2` - `cpt-cf-authn-plugin-principle-fail-closed`

- **Design Constraints Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-gts-identity`
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-clienthub`
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-versioning`

- **Domain Model Entities**:
  - `AuthNResolverPluginSpecV1` instance metadata
  - Plugin startup configuration (`jwt`, `http_client`, `retry_policy`, `circuit_breaker`, `s2s_oauth`)

- **Design Components**:
  - [x] `p2` - `cpt-cf-authn-plugin-component-config-validation`

- **API**:
  - In-process plugin registration contract only (`AuthNResolverPluginClient`)

- **Sequences**:
  - None

- **Data**:
  - Runtime config and plugin registration metadata (no persistent storage)

- **Phases**: Single-phase implementation.

---

### 2.2 [JWT Validation Pipeline](./features/feature-jwt-validation-pipeline.md) - HIGH

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-feature-jwt-validation-pipeline`

- **Purpose**: Implement deterministic JWT-first authentication for inbound bearer tokens with issuer/audience/expiry/signature checks and fail-closed behavior.

- **Depends On**:
  - `cpt-cf-authn-plugin-feature-plugin-bootstrap-config-validation`
  - `cpt-cf-authn-plugin-feature-oidc-discovery-jwks-lifecycle`

- **Scope**:
  - Token-type detection and non-JWT rejection
  - Trusted issuer enforcement with ordered matching semantics
  - Signature verification with supported algorithms policy
  - Key-rotation handling via unknown-`kid` refresh path
  - `exp` and optional `aud` validation
  - AuthN middleware request-path integration contract

- **Out of scope**:
  - Claim-to-`SecurityContext` mapping details
  - S2S credential exchange flow
  - Observability dashboards and policy-level authorization logic

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-authn-plugin-fr-jwt-validation`
  - [x] `p1` - `cpt-cf-authn-plugin-fr-non-jwt-rejection`
  - [x] `p1` - `cpt-cf-authn-plugin-fr-trusted-issuers`
  - [x] `p1` - `cpt-cf-authn-plugin-fr-key-rotation`
  - [x] `p2` - `cpt-cf-authn-plugin-fr-audience-validation`
  - [x] `p1` - `cpt-cf-authn-plugin-nfr-jwt-latency`
  - [x] `p1` - `cpt-cf-authn-plugin-nfr-fail-closed`

- **Design Principles Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-principle-jwt-first`
  - [x] `p2` - `cpt-cf-authn-plugin-principle-fail-closed`

- **Design Constraints Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-no-opaque-tokens`
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-oidc-standards`

- **Domain Model Entities**:
  - `AuthenticationResult`
  - `AuthNResolverError`
  - `JwtClaims`

- **Design Components**:
  - [x] `p2` - `cpt-cf-authn-plugin-component-token-validator`

- **API**:
  - `authenticate(bearer_token) -> Result<AuthenticationResult, AuthNResolverError>`

- **Sequences**:
  - `cpt-cf-authn-plugin-seq-middleware-flow`
  - `cpt-cf-authn-plugin-seq-jwt-validation`

- **Data**:
  - JWT header/payload claims (request-scoped only)

- **Phases**: Single-phase implementation.

---

### 2.3 [Claim Mapping and SecurityContext Construction](./features/feature-claim-mapping-security-context.md) - HIGH

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-feature-claim-mapping-security-context`

- **Purpose**: Map validated token claims into platform `SecurityContext` with strict tenant isolation guarantees and first-party/third-party scope semantics.

- **Depends On**:
  - `cpt-cf-authn-plugin-feature-jwt-validation-pipeline`

- **Scope**:
  - Configurable claim mapping (`subject_id`, `subject_tenant_id`, `subject_type`, `token_scopes`)
  - UUID parsing and required-claim enforcement
  - First-party vs third-party app detection via client identity claims
  - Scope override semantics for first-party clients
  - `SecurityContext` construction and error mapping

- **Out of scope**:
  - JWT cryptographic validation and issuer trust matching
  - OIDC endpoint/network interactions
  - Policy evaluation and authorization decisions

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-authn-plugin-fr-claim-mapping`
  - [x] `p1` - `cpt-cf-authn-plugin-fr-tenant-claim`
  - [x] `p2` - `cpt-cf-authn-plugin-fr-first-party-detection`
  - [x] `p1` - `cpt-cf-authn-plugin-nfr-tenant-isolation`

- **Design Principles Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-principle-claim-tenant-isolation`
  - [x] `p2` - `cpt-cf-authn-plugin-principle-minimalist-interface`

- **Design Constraints Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-security-context`
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-no-authz`

- **Domain Model Entities**:
  - `SecurityContext`
  - `AuthenticationResult`
  - Claim-mapping configuration model

- **Design Components**:
  - [x] `p2` - `cpt-cf-authn-plugin-component-claim-mapper`

- **API**:
  - Claim mapping path inside `authenticate(...)` and `exchange_client_credentials(...)`

- **Sequences**:
  - `cpt-cf-authn-plugin-seq-app-detection`

- **Data**:
  - Claim map and `SecurityContext` fields (`subject_id`, `subject_tenant_id`, `subject_type`, `token_scopes`, `bearer_token`)

- **Phases**: Single-phase implementation.

---

### 2.4 [OIDC Discovery and JWKS Lifecycle](./features/feature-oidc-discovery-jwks-lifecycle.md) - HIGH

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-feature-oidc-discovery-jwks-lifecycle`

- **Purpose**: Provide standards-based endpoint discovery and JWKS key lifecycle management with bounded caching and refresh behavior for high-throughput local JWT validation.

- **Depends On**:
  - `cpt-cf-authn-plugin-feature-plugin-bootstrap-config-validation`

- **Scope**:
  - OIDC discovery fetch and cache
  - `jwks_uri` resolution from discovery documents
  - JWKS cache lifecycle (TTL, stale window, capacity bounds)
  - Forced refresh behavior on unknown `kid`
  - Endpoint resolution contract consumed by validation and S2S features

- **Out of scope**:
  - Token claim mapping and `SecurityContext` assembly
  - Plugin registration and priority semantics
  - Authorization policy concerns

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-authn-plugin-fr-oidc-discovery`
  - [x] `p1` - `cpt-cf-authn-plugin-fr-jwks-caching`
  - [x] `p1` - `cpt-cf-authn-plugin-nfr-availability`

- **Design Principles Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-principle-idp-agnostic`
  - [x] `p2` - `cpt-cf-authn-plugin-principle-jwt-first`

- **Design Constraints Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-oidc-standards`

- **Domain Model Entities**:
  - OIDC discovery document (`issuer`, `jwks_uri`, `token_endpoint`)
  - JWKS key-set cache entries

- **Design Components**:
  - [x] `p2` - `cpt-cf-authn-plugin-component-oidc-discovery`

- **API**:
  - Internal endpoint resolution consumed by validation and S2S operations

- **Sequences**:
  - `cpt-cf-authn-plugin-seq-jwt-validation`

- **Data**:
  - In-memory discovery/JWKS caches only (no persistent tables)

- **Phases**: Single-phase implementation.

---

### 2.5 [Service-to-Service Token Exchange](./features/feature-s2s-token-exchange.md) - MEDIUM

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-feature-s2s-token-exchange`

- **Purpose**: Enable authenticated background-module execution via OAuth2 client-credentials exchange and cache-aware `SecurityContext` production.

- **Depends On**:
  - `cpt-cf-authn-plugin-feature-plugin-bootstrap-config-validation`
  - `cpt-cf-authn-plugin-feature-jwt-validation-pipeline`
  - `cpt-cf-authn-plugin-feature-claim-mapping-security-context`
  - `cpt-cf-authn-plugin-feature-oidc-discovery-jwks-lifecycle`

- **Scope**:
  - OAuth2 `client_credentials` grant flow
  - Token endpoint resolution through OIDC discovery
  - S2S cache key normalization and bounded cache behavior
  - Reuse of JWT validation + claim mapping pipeline for obtained tokens
  - Default subject-type fallback for S2S tokens

- **Out of scope**:
  - End-user request-path middleware integration
  - Plugin bootstrap and registration mechanics
  - Independent authorization policy logic

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-authn-plugin-fr-s2s-exchange`
  - [x] `p2` - `cpt-cf-authn-plugin-fr-s2s-caching`
  - [x] `p3` - `cpt-cf-authn-plugin-fr-s2s-default-subject-type`
  - [x] `p2` - `cpt-cf-authn-plugin-nfr-s2s-latency`

- **Design Principles Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-principle-idp-agnostic`
  - [x] `p2` - `cpt-cf-authn-plugin-principle-minimalist-interface`

- **Design Constraints Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-oidc-standards`
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-no-authz`

- **Domain Model Entities**:
  - `ClientCredentialsRequest`
  - `AuthenticationResult`
  - S2S cache entries

- **Design Components**:
  - [x] `p2` - `cpt-cf-authn-plugin-component-token-client-s2s`

- **API**:
  - `exchange_client_credentials(request) -> Result<AuthenticationResult, AuthNResolverError>`

- **Sequences**:
  - `cpt-cf-authn-plugin-seq-s2s-exchange`

- **Data**:
  - In-memory S2S token/result cache only

- **Phases**: Single-phase implementation.

---

### 2.6 [Reliability, Security Hardening, and Observability](./features/feature-reliability-security-observability.md) - MEDIUM

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-feature-reliability-security-observability`

- **Purpose**: Provide production-grade operational resilience and auditability through timeout/retry/circuit-breaker controls, secure token handling, and metrics/log instrumentation.

- **Depends On**:
  - `cpt-cf-authn-plugin-feature-jwt-validation-pipeline`
  - `cpt-cf-authn-plugin-feature-oidc-discovery-jwks-lifecycle`
  - `cpt-cf-authn-plugin-feature-s2s-token-exchange`

- **Scope**:
  - Outbound HTTP timeout enforcement per attempt
  - Retry policy (transient failures, exponential backoff, jitter, retry-after handling)
  - Per-host circuit-breaker state machine and host isolation
  - Security controls for token/secret non-disclosure in logs and cache keys
  - Structured metrics, failure taxonomy, and readiness for load/chaos validation

- **Out of scope**:
  - New functional authentication capabilities
  - Authorization policy and access-decision behavior
  - Persistent storage or schema changes

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-authn-plugin-fr-request-timeout`
  - [x] `p1` - `cpt-cf-authn-plugin-fr-retry-policy`
  - [x] `p1` - `cpt-cf-authn-plugin-fr-circuit-breaker`
  - [x] `p1` - `cpt-cf-authn-plugin-nfr-availability`
  - [x] `p1` - `cpt-cf-authn-plugin-nfr-security`

- **Design Principles Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-principle-fail-closed`
  - [x] `p2` - `cpt-cf-authn-plugin-principle-idp-agnostic`

- **Design Constraints Covered**:
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-vendor-licensing`
  - [x] `p2` - `cpt-cf-authn-plugin-constraint-legacy-integration`

- **Domain Model Entities**:
  - Retry policy configuration
  - Circuit-breaker per-host state
  - Metrics and audit-log event envelopes

- **Design Components**:
  - [x] `p2` - `cpt-cf-authn-plugin-component-http-client`
  - [x] `p2` - `cpt-cf-authn-plugin-component-circuit-breaker`

- **API**:
  - No new external API; cross-cutting behavior attached to existing authenticate and S2S flows

- **Sequences**:
  - `cpt-cf-authn-plugin-seq-jwt-validation`
  - `cpt-cf-authn-plugin-seq-s2s-exchange`

- **Data**:
  - In-memory breaker state and metrics streams (no DB tables)

- **Phases**: Single-phase implementation.

---

## 3. Feature Dependencies

```text
cpt-cf-authn-plugin-feature-plugin-bootstrap-config-validation
    ↓
    ├─→ cpt-cf-authn-plugin-feature-oidc-discovery-jwks-lifecycle
    │       ↓
    │       ├─→ cpt-cf-authn-plugin-feature-jwt-validation-pipeline
    │       │       ↓
    │       │       └─→ cpt-cf-authn-plugin-feature-claim-mapping-security-context
    │       │
    │       └─→ cpt-cf-authn-plugin-feature-s2s-token-exchange
    │               ↘
    └────────────────→ cpt-cf-authn-plugin-feature-reliability-security-observability
```

**Dependency rationale**:

- `cpt-cf-authn-plugin-feature-plugin-bootstrap-config-validation` is foundational: runtime registration and strict startup validation must precede all request-path behavior.
- `cpt-cf-authn-plugin-feature-oidc-discovery-jwks-lifecycle` is a shared prerequisite for both JWT validation and S2S endpoint/token flows.
- `cpt-cf-authn-plugin-feature-jwt-validation-pipeline` must complete before claim mapping and before full S2S end-to-end reuse of the validation pipeline.
- `cpt-cf-authn-plugin-feature-claim-mapping-security-context` depends on validated claims output from JWT/S2S tokens.
- `cpt-cf-authn-plugin-feature-s2s-token-exchange` depends on discovery, validation, and mapping components to avoid duplicated logic.
- `cpt-cf-authn-plugin-feature-reliability-security-observability` spans all runtime network paths and is sequenced after core functional paths exist so instrumentation and resilience controls can be verified against real flows.
