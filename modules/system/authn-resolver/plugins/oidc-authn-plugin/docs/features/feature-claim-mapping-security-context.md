# Feature: Claim Mapping and SecurityContext Construction

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-featstatus-claim-mapping-security-context`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Build SecurityContext from Validated Claims](#build-securitycontext-from-validated-claims)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Claim Extraction and Scope Normalization](#claim-extraction-and-scope-normalization)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Required Identity Claim Enforcement](#required-identity-claim-enforcement)
  - [First-Party Scope Override Semantics](#first-party-scope-override-semantics)
  - [SecurityContext Build Consistency](#securitycontext-build-consistency)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature defines how validated JWT claims are mapped into the platform `SecurityContext`, including required identity fields, tenant isolation enforcement, and first-party versus third-party scope behavior.

### 1.2 Purpose

The purpose is to produce a consistent, policy-ready identity context from token claims while ensuring strict tenant claim requirements and predictable scope semantics across request and S2S paths.

**Requirements**: `cpt-cf-authn-plugin-fr-claim-mapping`, `cpt-cf-authn-plugin-fr-tenant-claim`, `cpt-cf-authn-plugin-fr-first-party-detection`, `cpt-cf-authn-plugin-nfr-tenant-isolation`

**Principles**: `cpt-cf-authn-plugin-principle-claim-tenant-isolation`, `cpt-cf-authn-plugin-principle-minimalist-interface`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-authn-plugin-actor-api-gateway` | Receives mapped `SecurityContext` on request-path authenticate calls. |
| `cpt-cf-authn-plugin-actor-background-module` | Receives mapped `SecurityContext` on S2S exchanges. |
| `cpt-cf-authn-plugin-actor-platform-admin` | Configures claim mapping and first-party client list. |

### 1.4 References

- **PRD**: [../PRD.md](../PRD.md)
- **Design**: [../DESIGN.md](../DESIGN.md)
- **Dependencies**:
  - [x] `p1` - `cpt-cf-authn-plugin-feature-jwt-validation-pipeline`

## 2. Actor Flows (CDSL)

### Build SecurityContext from Validated Claims

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-flow-claim-mapping-security-context-build-context-from-claims`

**Actor**: `cpt-cf-authn-plugin-actor-api-gateway`

**Steps**:
1. [x] - `p1` - Receive verified claims from validation pipeline. - `inst-claim-map-receive-claims`
2. [x] - `p1` - Resolve claim mapping keys for subject, tenant, type, and scopes. - `inst-claim-map-resolve-keys`
3. [x] - `p1` - Parse required subject and tenant identifiers as UUID values. - `inst-claim-map-parse-required-ids`
4. [x] - `p1` - **IF** required claim is missing or malformed - `inst-claim-map-if-missing-required`
   1. [x] - `p1` - **RETURN** unauthorized with deterministic claim-specific reason. - `inst-claim-map-return-required-error`
5. [x] - `p1` - Resolve application identity from token client claims. - `inst-claim-map-resolve-client-identity`
6. [x] - `p1` - **IF** caller is first-party client - `inst-claim-map-if-first-party`
   1. [x] - `p1` - Set token scopes to unrestricted marker set. - `inst-claim-map-set-first-party-scopes`
7. [x] - `p1` - **ELSE** use normalized token scopes from mapped scope claim. - `inst-claim-map-set-third-party-scopes`
8. [x] - `p1` - Build and return `SecurityContext` for downstream authorization. - `inst-claim-map-return-security-context`

## 3. Processes / Business Logic (CDSL)

### Claim Extraction and Scope Normalization

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-algo-claim-mapping-security-context-extract-normalize`

**Input**: Verified claims map and claim-mapping configuration.

**Output**: Structured `SecurityContext` fields (`subject_id`, `subject_tenant_id`, `subject_type`, `token_scopes`, `bearer_token`).

**Steps**:
1. [x] - `p1` - Read claim names from configuration with defaults for subject and scopes. - `inst-claim-algo-read-config`
2. [x] - `p1` - Extract and validate required values for identity and tenant binding. - `inst-claim-algo-extract-required`
3. [x] - `p1` - Extract optional subject-type value when claim is present. - `inst-claim-algo-extract-subject-type`
4. [x] - `p1` - Normalize scope claim format into stable string-vector representation. - `inst-claim-algo-normalize-scopes`
5. [x] - `p1` - Apply first-party override semantics when client identity is allowlisted. - `inst-claim-algo-apply-first-party-override`
6. [x] - `p1` - **RETURN** completed context fields for builder assembly. - `inst-claim-algo-return-fields`

## 4. States (CDSL)

Not applicable because this feature transforms request-scoped claims into a context object without storing lifecycle state.

## 5. Definitions of Done

### Required Identity Claim Enforcement
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-claim-mapping-security-context-required-claims`
The system **MUST** reject tokens when required identity or tenant claims are missing or non-UUID.

### First-Party Scope Override Semantics
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-claim-mapping-security-context-first-party-scope-override`
The system **MUST** apply unrestricted scopes only for configured first-party clients.

### SecurityContext Build Consistency
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-claim-mapping-security-context-build-consistency`
The system **MUST** produce consistent `SecurityContext` fields for both request and S2S flows.

## 6. Acceptance Criteria

- [x] Required `subject_id` and `subject_tenant_id` claims are validated as UUID.
- [x] Missing or invalid tenant claim returns deterministic unauthorized error.
- [x] Scope mapping produces normalized scope sets for third-party clients.
- [x] First-party client detection overrides scopes as designed.
- [x] `SecurityContext` output is complete and policy-ready.

## 7. Deliberate Omissions

- Cryptographic JWT signature and issuer validation are omitted (covered by JWT pipeline feature).
- OIDC discovery and key retrieval concerns are omitted (covered by discovery/JWKS feature).
- Retry/breaker instrumentation and security-hardening telemetry are omitted (covered by reliability feature).
