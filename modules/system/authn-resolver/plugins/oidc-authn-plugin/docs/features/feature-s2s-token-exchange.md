# Feature: Service-to-Service Token Exchange

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-featstatus-s2s-token-exchange`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Exchange Client Credentials for SecurityContext](#exchange-client-credentials-for-securitycontext)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [S2S Cache-Key Derivation and Reuse](#s2s-cache-key-derivation-and-reuse)
- [4. States (CDSL)](#4-states-cdsl)
  - [S2S Result Cache Entry State Machine](#s2s-result-cache-entry-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [End-to-End S2S Exchange](#end-to-end-s2s-exchange)
  - [Cache Isolation by Scope and Credential](#cache-isolation-by-scope-and-credential)
  - [Default Subject Type Fallback](#default-subject-type-fallback)
  - [S2S Latency Benchmark](#s2s-latency-benchmark)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature defines OAuth2 client-credentials exchange for background workloads, including endpoint resolution, token acquisition, cache-aware reuse, and mapped `SecurityContext` output.

### 1.2 Purpose

The purpose is to provide authenticated non-user execution contexts through the same validation and mapping pipeline as request-path tokens while minimizing repeated upstream round-trips.

**Requirements**: `cpt-cf-authn-plugin-fr-s2s-exchange`, `cpt-cf-authn-plugin-fr-s2s-caching`, `cpt-cf-authn-plugin-fr-s2s-default-subject-type`, `cpt-cf-authn-plugin-nfr-s2s-latency`

**Principles**: `cpt-cf-authn-plugin-principle-idp-agnostic`, `cpt-cf-authn-plugin-principle-minimalist-interface`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-authn-plugin-actor-background-module` | Requests authenticated context for background execution. |
| `cpt-cf-authn-plugin-actor-idp` | Serves token endpoint for client-credentials grant. |

### 1.4 References

- **PRD**: [../PRD.md](../PRD.md)
- **Design**: [../DESIGN.md](../DESIGN.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-authn-plugin-feature-plugin-bootstrap-config-validation`
  - [x] `p2` - `cpt-cf-authn-plugin-feature-jwt-validation-pipeline`
  - [x] `p2` - `cpt-cf-authn-plugin-feature-claim-mapping-security-context`
  - [x] `p2` - `cpt-cf-authn-plugin-feature-oidc-discovery-jwks-lifecycle`

## 2. Actor Flows (CDSL)

### Exchange Client Credentials for SecurityContext

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-flow-s2s-token-exchange-exchange-client-credentials`

**Actor**: `cpt-cf-authn-plugin-actor-background-module`

**Steps**:
1. [x] - `p2` - Receive `ClientCredentialsRequest` with client identity, secret, and optional scopes. - `inst-s2s-receive-request`
2. [x] - `p2` - Normalize scopes and derive cache identity with credential fingerprint. - `inst-s2s-build-cache-key`
3. [x] - `p2` - **IF** matching cached result exists and is fresh - `inst-s2s-if-cache-hit`
   1. [x] - `p2` - **RETURN** cached `AuthenticationResult` without token re-acquisition. - `inst-s2s-return-cache-hit`
4. [x] - `p2` - Resolve token endpoint from discovery metadata. - `inst-s2s-resolve-token-endpoint`
5. [x] - `p2` - Execute OAuth2 `client_credentials` request to obtain access token. - `inst-s2s-request-token`
6. [x] - `p2` - Validate obtained token through standard JWT pipeline. - `inst-s2s-validate-token`
7. [x] - `p2` - Map claims into `SecurityContext` and apply default subject type if needed. - `inst-s2s-map-context`
8. [x] - `p2` - Store result in bounded cache with TTL min(token-expiry, configured-ttl). - `inst-s2s-store-cache`
9. [x] - `p2` - **RETURN** `AuthenticationResult` to calling module. - `inst-s2s-return-result`

## 3. Processes / Business Logic (CDSL)

### S2S Cache-Key Derivation and Reuse

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-algo-s2s-token-exchange-cache-key-derivation`

**Input**: `client_id`, raw scope input, secret material.

**Output**: Stable cache key and bounded cache policy behavior.

**Steps**:
1. [x] - `p2` - Normalize scope input via trim, dedupe, sort, and stable join. - `inst-s2s-algo-normalize-scopes`
2. [x] - `p2` - Derive non-reversible credential fingerprint from secret input. - `inst-s2s-algo-derive-fingerprint`
3. [x] - `p2` - Build composite key from client ID, normalized scopes, and fingerprint. - `inst-s2s-algo-build-key`
4. [x] - `p2` - Enforce single-flight behavior for concurrent misses on same key. - `inst-s2s-algo-single-flight`
5. [x] - `p2` - Enforce bounded cache size with deterministic eviction. - `inst-s2s-algo-cache-bounds`
6. [x] - `p2` - **RETURN** key and cache policy outcome for exchange flow. - `inst-s2s-algo-return-key`

## 4. States (CDSL)

### S2S Result Cache Entry State Machine

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-state-s2s-token-exchange-result-cache-state`

**States**: `fresh`, `expired`

**Initial State**: `fresh`

**Transitions**:
1. [x] - `p2` - **FROM** `fresh` **TO** `expired` **WHEN** cache TTL or token expiry is reached. - `inst-s2s-state-fresh-to-expired`
2. [x] - `p2` - **FROM** `expired` **TO** `fresh` **WHEN** a successful exchange refreshes the entry. - `inst-s2s-state-expired-to-fresh`

## 5. Definitions of Done

### End-to-End S2S Exchange
- [x] `p2` - **ID**: `cpt-cf-authn-plugin-dod-s2s-token-exchange-end-to-end-flow`
The system **MUST** acquire, validate, and map client-credentials tokens into `AuthenticationResult`.

### Cache Isolation by Scope and Credential
- [x] `p2` - **ID**: `cpt-cf-authn-plugin-dod-s2s-token-exchange-cache-isolation`
The system **MUST** isolate cache entries by normalized scopes and credential fingerprint.

### Default Subject Type Fallback
- [x] `p3` - **ID**: `cpt-cf-authn-plugin-dod-s2s-token-exchange-default-subject-type-fallback`
The system **MUST** apply configured default subject type when claim mapping does not provide it.

### S2S Latency Benchmark
- [x] `p2` - **ID**: `cpt-cf-authn-plugin-dod-s2s-token-exchange-latency-benchmark`
The system **MUST** provide a repeatable benchmark for `exchange_client_credentials` that exercises warm token-cache and cold token-cache paths against `cpt-cf-authn-plugin-nfr-s2s-latency`.

## 6. Acceptance Criteria

- [x] S2S exchange returns mapped `SecurityContext` on successful token acquisition.
- [x] Cache hits avoid repeated token endpoint calls.
- [x] Same scopes in different order map to one cache identity.
- [x] Different credentials or scope sets do not reuse cached result incorrectly.
- [x] Default subject type is applied only when subject type claim is absent.
- [x] S2S latency benchmark coverage verifies p95 warm-cache and cold-cache threshold behavior without depending on repeated token endpoint calls for cache hits.

## 7. Deliberate Omissions

- Request-path middleware token extraction is omitted (covered by JWT validation feature).
- Generic plugin startup validation is omitted (covered by bootstrap feature).
- Cross-cutting retry/breaker hardening details are omitted (covered by reliability feature).
