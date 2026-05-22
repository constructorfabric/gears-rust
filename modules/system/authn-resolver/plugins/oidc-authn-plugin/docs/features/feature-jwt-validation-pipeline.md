# Feature: JWT Validation Pipeline

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-featstatus-jwt-validation-pipeline`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Authenticate Bearer Token](#authenticate-bearer-token)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [JWT Verification and Guardrail Enforcement](#jwt-verification-and-guardrail-enforcement)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Non-JWT Rejection](#non-jwt-rejection)
  - [Issuer and Signature Enforcement](#issuer-and-signature-enforcement)
  - [Key Rotation Handling](#key-rotation-handling)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature defines the request-path JWT validation pipeline for bearer tokens, including token format checks, signature and claims verification, issuer trust enforcement, and fail-closed rejection behavior.

### 1.2 Purpose

The purpose is to establish deterministic JWT-first authentication with low-latency local verification and strict rejection semantics for malformed, untrusted, or expired tokens.

**Requirements**: `cpt-cf-authn-plugin-fr-jwt-validation`, `cpt-cf-authn-plugin-fr-non-jwt-rejection`, `cpt-cf-authn-plugin-fr-trusted-issuers`, `cpt-cf-authn-plugin-fr-key-rotation`, `cpt-cf-authn-plugin-fr-audience-validation`, `cpt-cf-authn-plugin-nfr-jwt-latency`, `cpt-cf-authn-plugin-nfr-fail-closed`

**Principles**: `cpt-cf-authn-plugin-principle-jwt-first`, `cpt-cf-authn-plugin-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-authn-plugin-actor-api-gateway` | Delegates bearer token authentication to plugin runtime. |
| `cpt-cf-authn-plugin-actor-idp` | Provides issuer metadata and signing keys consumed by validation. |

### 1.4 References

- **PRD**: [../PRD.md](../PRD.md)
- **Design**: [../DESIGN.md](../DESIGN.md)
- **Dependencies**:
  - [x] `p1` - `cpt-cf-authn-plugin-feature-plugin-bootstrap-config-validation`
  - [x] `p1` - `cpt-cf-authn-plugin-feature-oidc-discovery-jwks-lifecycle`

## 2. Actor Flows (CDSL)

### Authenticate Bearer Token

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-flow-jwt-validation-pipeline-authenticate-bearer-token`

**Actor**: `cpt-cf-authn-plugin-actor-api-gateway`

**Steps**:
1. [x] - `p1` - Receive bearer token and check JWT structure (three base64url segments). - `inst-jwt-accept-and-parse`
2. [x] - `p1` - **IF** token is not JWT format - `inst-jwt-if-non-jwt`
   1. [x] - `p1` - **RETURN** unauthorized unsupported token format. - `inst-jwt-return-non-jwt`
3. [x] - `p1` - Decode header/payload for `alg`, `kid`, `iss`, `exp`, and optional `aud`. - `inst-jwt-decode-unverified`
4. [x] - `p1` - Match issuer against trusted issuer policy in configured order. - `inst-jwt-match-trusted-issuer`
5. [x] - `p1` - Resolve key material and verify signature using allowed algorithms. - `inst-jwt-verify-signature`
6. [x] - `p1` - **IF** unknown `kid` is encountered - `inst-jwt-if-unknown-kid`
   1. [x] - `p1` - Trigger forced key refresh and re-attempt key lookup once. - `inst-jwt-refresh-on-unknown-kid`
7. [x] - `p1` - Validate expiration and configured audience requirements. - `inst-jwt-validate-exp-aud`
8. [x] - `p1` - **RETURN** validated claims for downstream mapping into `SecurityContext`. - `inst-jwt-return-validated-claims`

## 3. Processes / Business Logic (CDSL)

### JWT Verification and Guardrail Enforcement

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-algo-jwt-validation-pipeline-verification-guardrails`

**Input**: Bearer token, trusted issuer policy, algorithm policy, audience policy, key set cache state.

**Output**: Verified claims set or rejection error.

**Steps**:
1. [x] - `p1` - Reject prohibited algorithm modes before signature verification. - `inst-jwt-guardrail-reject-none`
2. [x] - `p1` - Enforce deterministic issuer trust resolution and discovery-base derivation. - `inst-jwt-guardrail-issuer-resolution`
3. [x] - `p1` - Verify signature using issuer-resolved key set and selected `kid`. - `inst-jwt-guardrail-signature`
4. [x] - `p1` - Validate temporal claims with configured skew tolerance. - `inst-jwt-guardrail-temporal`
5. [x] - `p1` - Validate audience claim when required by configuration. - `inst-jwt-guardrail-audience`
6. [x] - `p1` - **RETURN** verified claims for claim mapper stage. - `inst-jwt-guardrail-return-claims`

## 4. States (CDSL)

Not applicable because this feature executes request-scoped validation without a plugin-owned persistent entity lifecycle.

## 5. Definitions of Done

### Non-JWT Rejection
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-jwt-validation-pipeline-non-jwt-rejection`
The system **MUST** reject non-JWT bearer tokens before any downstream mapping or authorization path.

### Issuer and Signature Enforcement
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-jwt-validation-pipeline-issuer-signature-enforcement`
The system **MUST** enforce trusted issuer policy and cryptographic signature verification for every token.

### Key Rotation Handling
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-jwt-validation-pipeline-key-rotation-handling`
The system **MUST** support unknown-`kid` key refresh once and reject when key is still unavailable.

## 6. Acceptance Criteria

- [x] Non-JWT tokens return unauthorized unsupported format.
- [x] Untrusted issuer tokens are rejected deterministically.
- [x] Invalid signature tokens are rejected.
- [x] Unknown `kid` triggers refresh path and either succeeds or rejects with key-not-found.
- [x] `exp` and optional `aud` checks are enforced per configuration.

## 7. Deliberate Omissions

- Claim-to-`SecurityContext` mapping semantics are omitted (covered by claim mapping feature).
- S2S client credential acquisition is omitted (covered by S2S feature).
- Cross-cutting retry/breaker observability hardening is omitted (covered by reliability feature).
