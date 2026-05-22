# Feature: Plugin Bootstrap, Registration, and Config Validation

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-featstatus-plugin-bootstrap-config-validation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Plugin Startup and Registration](#plugin-startup-and-registration)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Configuration Validation Pipeline](#configuration-validation-pipeline)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Startup Validation Completeness](#startup-validation-completeness)
  - [Deterministic Registration Contract](#deterministic-registration-contract)
  - [Fail-Closed Startup Behavior](#fail-closed-startup-behavior)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature defines how the OIDC AuthN plugin starts, validates configuration, registers itself for gateway resolution, and fails fast when required runtime prerequisites are missing or inconsistent.

### 1.2 Purpose

The purpose is to ensure deterministic plugin activation before serving authentication traffic, so request-path and S2S flows start from a known-safe baseline.

**Requirements**: `cpt-cf-authn-plugin-fr-clienthub-registration`

**Principles**: `cpt-cf-authn-plugin-principle-minimalist-interface`, `cpt-cf-authn-plugin-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-authn-plugin-actor-platform-admin` | Provides startup configuration and expects immediate validation feedback. |
| `cpt-cf-authn-plugin-actor-api-gateway` | Resolves and invokes the plugin after successful registration. |

### 1.4 References

- **PRD**: [../PRD.md](../PRD.md)
- **Design**: [../DESIGN.md](../DESIGN.md)
- **Dependencies**: None

## 2. Actor Flows (CDSL)

### Plugin Startup and Registration

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-flow-plugin-bootstrap-config-validation-startup-registration`

**Actor**: `cpt-cf-authn-plugin-actor-platform-admin`

**Steps**:
1. [x] - `p1` - Load plugin configuration and normalize values with deterministic precedence rules. - `inst-bootstrap-load-config`
2. [x] - `p1` - Validate trusted issuers, algorithm policy, claim mappings, timeout/retry, and breaker boundaries. - `inst-bootstrap-validate-config`
3. [x] - `p1` - **IF** any validation fails - `inst-bootstrap-if-invalid`
   1. [x] - `p1` - **RETURN** startup failure with structured error and prevent registration. - `inst-bootstrap-return-invalid`
4. [x] - `p1` - Build plugin identity metadata (`vendor key`, `priority`, `display name`). - `inst-bootstrap-build-metadata`
5. [x] - `p1` - Register plugin client in ClientHub under the AuthN resolver scope. - `inst-bootstrap-register-client`
6. [x] - `p1` - **RETURN** ready state for gateway resolution. - `inst-bootstrap-return-ready`

## 3. Processes / Business Logic (CDSL)

### Configuration Validation Pipeline

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-algo-plugin-bootstrap-config-validation-pipeline`

**Input**: Startup configuration (`jwt`, `http_client`, `retry_policy`, `circuit_breaker`, `s2s_oauth`).

**Output**: Validated runtime configuration or fail-fast startup error.

**Steps**:
1. [x] - `p1` - Validate required sections and required keys are present. - `inst-bootstrap-validate-required`
2. [x] - `p1` - Validate issuer rules and compile any configured issuer patterns. - `inst-bootstrap-validate-issuer-rules`
3. [x] - `p1` - Validate security invariants (`alg none` disallowed, tenant claim mapping present). - `inst-bootstrap-validate-security-invariants`
4. [x] - `p1` - Validate runtime guardrails (timeouts, retry bounds, breaker thresholds, cache limits). - `inst-bootstrap-validate-runtime-guardrails`
5. [x] - `p1` - **RETURN** normalized validated config for plugin runtime. - `inst-bootstrap-return-validated`

## 4. States (CDSL)

Not applicable because this feature defines startup gating, not a persistent entity lifecycle.

## 5. Definitions of Done

### Startup Validation Completeness
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-plugin-bootstrap-config-validation-startup-validation`
The system **MUST** fail plugin initialization when required config is missing or violates safety constraints.

### Deterministic Registration Contract
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-plugin-bootstrap-config-validation-deterministic-registration`
The system **MUST** register exactly one plugin client instance with deterministic metadata and scope identity.

### Fail-Closed Startup Behavior
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-plugin-bootstrap-config-validation-fail-closed-startup`
The system **MUST** prevent any authenticate or S2S execution path when startup validation has not completed successfully.

## 6. Acceptance Criteria

- [x] Invalid issuer or algorithm configuration blocks plugin startup.
- [x] Missing tenant-claim mapping blocks plugin startup.
- [x] Successful startup registers plugin client once with configured metadata.
- [x] Gateway can resolve plugin only after successful registration.
- [x] No request-path auth operation is allowed before ready state.

## 7. Deliberate Omissions

- Request-time JWT verification logic is omitted (covered by JWT validation feature).
- OIDC discovery and JWKS refresh logic is omitted (covered by discovery/JWKS feature).
- S2S token acquisition flow is omitted (covered by S2S feature).
