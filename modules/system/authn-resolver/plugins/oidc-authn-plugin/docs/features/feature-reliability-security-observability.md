# Feature: Reliability, Security Hardening, and Observability

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-featstatus-reliability-security-observability`
- [x] `p2` - `cpt-cf-authn-plugin-feature-reliability-security-observability`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Handle Degraded Upstream Identity Dependencies](#handle-degraded-upstream-identity-dependencies)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Retry, Timeout, and Circuit-Breaker Control Loop](#retry-timeout-and-circuit-breaker-control-loop)
- [4. States (CDSL)](#4-states-cdsl)
  - [Per-Host Circuit Breaker State Machine](#per-host-circuit-breaker-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Resilience Control Enforcement](#resilience-control-enforcement)
  - [Sensitive Data Non-Disclosure](#sensitive-data-non-disclosure)
  - [Operational Signal Completeness](#operational-signal-completeness)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature defines cross-cutting production controls for timeout enforcement, transient retry policy, per-host circuit-breaker behavior, secure token handling, and telemetry/audit coverage.

### 1.2 Purpose

The purpose is to keep authentication paths resilient under upstream degradation while preserving fail-closed guarantees, protecting sensitive data, and providing actionable operational signals.

**Requirements**: `cpt-cf-authn-plugin-fr-request-timeout`, `cpt-cf-authn-plugin-fr-retry-policy`, `cpt-cf-authn-plugin-fr-circuit-breaker`, `cpt-cf-authn-plugin-nfr-availability`, `cpt-cf-authn-plugin-nfr-security`

**Principles**: `cpt-cf-authn-plugin-principle-fail-closed`, `cpt-cf-authn-plugin-principle-idp-agnostic`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-authn-plugin-actor-platform-admin` | Configures resilience and telemetry guardrails. |
| `cpt-cf-authn-plugin-actor-api-gateway` | Depends on stable request-path outcomes under failures. |

### 1.4 References

- **PRD**: [../PRD.md](../PRD.md)
- **Design**: [../DESIGN.md](../DESIGN.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-authn-plugin-feature-jwt-validation-pipeline`
  - [x] `p2` - `cpt-cf-authn-plugin-feature-oidc-discovery-jwks-lifecycle`
  - [x] `p2` - `cpt-cf-authn-plugin-feature-s2s-token-exchange`

## 2. Actor Flows (CDSL)

### Handle Degraded Upstream Identity Dependencies

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-flow-reliability-security-observability-handle-degraded-upstream`

**Actor**: `cpt-cf-authn-plugin-actor-api-gateway`

**Steps**:
1. [x] - `p2` - Execute outbound identity call with per-attempt request timeout. - `inst-rel-execute-with-timeout`
2. [x] - `p2` - Classify failure as retryable or terminal under retry policy. - `inst-rel-classify-failure`
3. [x] - `p2` - **IF** retryable failure and attempts remain - `inst-rel-if-retryable`
   1. [x] - `p2` - Re-attempt using bounded exponential backoff with optional jitter. - `inst-rel-retry-with-backoff`
4. [x] - `p2` - Record one logical success/failure outcome against host-scoped circuit breaker. - `inst-rel-record-breaker-outcome`
5. [x] - `p2` - **IF** breaker opens for host - `inst-rel-if-breaker-open`
   1. [x] - `p2` - Reject fresh calls requiring that host and continue unaffected calls to other hosts. - `inst-rel-reject-open-host-only`
6. [x] - `p2` - Emit metrics, traces, and structured events for outcome and reason category. - `inst-rel-emit-telemetry`
7. [x] - `p2` - **RETURN** deterministic unavailable/unauthorized path without secret disclosure. - `inst-rel-return-fail-closed`

## 3. Processes / Business Logic (CDSL)

### Retry, Timeout, and Circuit-Breaker Control Loop

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-algo-reliability-security-observability-control-loop`

**Input**: Outbound request intent, host identity, retry policy, breaker policy.

**Output**: Successful response or deterministic fail-closed error.

**Steps**:
1. [x] - `p2` - Start host-scoped breaker guard for current outbound operation. - `inst-rel-algo-start-breaker-guard`
2. [x] - `p2` - Apply per-attempt timeout and execute request. - `inst-rel-algo-timeout-and-call`
3. [x] - `p2` - Retry only retryable failures (connection, 5xx, 429) up to configured bound. - `inst-rel-algo-retry-policy`
4. [x] - `p2` - Treat timeout and non-retryable failures as terminal for current operation. - `inst-rel-algo-terminal-classification`
5. [x] - `p2` - Update breaker counters once per logical operation. - `inst-rel-algo-update-breaker`
6. [x] - `p2` - Emit structured observability payload with reason category and host labels. - `inst-rel-algo-emit-observability`
7. [x] - `p2` - **RETURN** final outcome while preserving no-secret logging guarantees. - `inst-rel-algo-return`

## 4. States (CDSL)

### Per-Host Circuit Breaker State Machine

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-state-reliability-security-observability-host-breaker-state`

**States**: `closed`, `open`, `half-open`

**Initial State**: `closed`

**Transitions**:
1. [x] - `p2` - **FROM** `closed` **TO** `open` **WHEN** failure threshold is reached for a host. - `inst-rel-state-closed-to-open`
2. [x] - `p2` - **FROM** `open` **TO** `half-open` **WHEN** reset timeout elapses. - `inst-rel-state-open-to-half-open`
3. [x] - `p2` - **FROM** `half-open` **TO** `closed` **WHEN** probe request succeeds. - `inst-rel-state-half-open-to-closed`
4. [x] - `p2` - **FROM** `half-open` **TO** `open` **WHEN** probe request fails. - `inst-rel-state-half-open-to-open`

## 5. Definitions of Done

### Resilience Control Enforcement
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-reliability-security-observability-resilience-controls`
The system **MUST** enforce timeout, retry, and breaker controls consistently across discovery, key fetch, and S2S token calls.

### Sensitive Data Non-Disclosure
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-reliability-security-observability-no-secret-disclosure`
The system **MUST** prevent bearer tokens and client secrets from appearing in logs, traces, or emitted metrics payloads.

### Operational Signal Completeness
- [x] `p2` - **ID**: `cpt-cf-authn-plugin-dod-reliability-security-observability-signal-completeness`
The system **MUST** emit enough reliability and security telemetry to diagnose degraded upstream behavior and policy outcomes.

## 6. Acceptance Criteria

- [x] Every outbound identity call applies configured request timeout per attempt.
- [x] Retry policy applies only to retryable classes and respects maximum attempts.
- [x] Circuit breaker state is isolated per host.
- [x] Breaker-open condition blocks only affected host calls.
- [x] Logs/metrics/traces include no raw bearer tokens or client secrets.
- [x] Reliability and security metrics are emitted for both success and error paths.

## 7. Deliberate Omissions

- New authentication capabilities are omitted; this feature hardens existing flows only.
- Authorization policy decisions are omitted (handled downstream).
- Persistent storage/schema changes are omitted (this feature uses in-memory operational state only).
