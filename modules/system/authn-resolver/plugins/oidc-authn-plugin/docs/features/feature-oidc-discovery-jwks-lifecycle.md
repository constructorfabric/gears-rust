# Feature: OIDC Discovery and JWKS Lifecycle

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-featstatus-oidc-discovery-jwks-lifecycle`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Resolve OIDC Metadata and Signing Keys](#resolve-oidc-metadata-and-signing-keys)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Discovery and Key Cache Lifecycle](#discovery-and-key-cache-lifecycle)
- [4. States (CDSL)](#4-states-cdsl)
  - [Discovery/JWKS Cache Entry State Machine](#discoveryjwks-cache-entry-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Discovery Endpoint Auto-Resolution](#discovery-endpoint-auto-resolution)
  - [JWKS Rotation and Refresh Behavior](#jwks-rotation-and-refresh-behavior)
  - [Availability-Oriented Stale Handling](#availability-oriented-stale-handling)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This feature defines endpoint discovery and signing-key lifecycle behavior used by authentication and S2S flows, including metadata caching, key caching, forced refresh on key misses, and bounded stale windows.

### 1.2 Purpose

The purpose is to provide standards-based dynamic endpoint resolution and resilient key management that keeps request-path JWT verification local and low-latency while tolerating temporary IdP outages.

**Requirements**: `cpt-cf-authn-plugin-fr-oidc-discovery`, `cpt-cf-authn-plugin-fr-jwks-caching`, `cpt-cf-authn-plugin-nfr-availability`

**Principles**: `cpt-cf-authn-plugin-principle-idp-agnostic`, `cpt-cf-authn-plugin-principle-jwt-first`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-authn-plugin-actor-idp` | Provides discovery document and key material. |
| `cpt-cf-authn-plugin-actor-api-gateway` | Indirectly depends on this feature for request-path validation reliability. |

### 1.4 References

- **PRD**: [../PRD.md](../PRD.md)
- **Design**: [../DESIGN.md](../DESIGN.md)
- **Dependencies**:
  - [x] `p1` - `cpt-cf-authn-plugin-feature-plugin-bootstrap-config-validation`

## 2. Actor Flows (CDSL)

### Resolve OIDC Metadata and Signing Keys

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-flow-oidc-discovery-jwks-lifecycle-resolve-metadata-keys`

**Actor**: `cpt-cf-authn-plugin-actor-idp`

**Steps**:
1. [x] - `p1` - Accept issuer input selected by trusted issuer policy. - `inst-discovery-accept-issuer`
2. [x] - `p1` - Read cached discovery metadata when entry is fresh. - `inst-discovery-read-cache`
3. [x] - `p1` - **IF** metadata is missing or stale - `inst-discovery-if-cache-miss`
   1. [x] - `p1` - Fetch discovery document and cache endpoint fields with configured TTL. - `inst-discovery-fetch-store`
4. [x] - `p1` - Resolve `jwks_uri` from current discovery state. - `inst-discovery-resolve-jwks-uri`
5. [x] - `p1` - Read JWKS key set from cache when valid. - `inst-discovery-read-jwks-cache`
6. [x] - `p1` - **IF** key set is missing, stale, or forced refresh is requested - `inst-discovery-if-jwks-refresh`
   1. [x] - `p1` - Fetch JWKS and update key cache with fresh and stale bounds. - `inst-discovery-fetch-jwks-store`
7. [x] - `p1` - **RETURN** resolved metadata and key set handles. - `inst-discovery-return-resolved`

## 3. Processes / Business Logic (CDSL)

### Discovery and Key Cache Lifecycle

- [x] `p1` - **ID**: `cpt-cf-authn-plugin-algo-oidc-discovery-jwks-lifecycle-cache-management`

**Input**: Issuer identity, cache state, refresh reason (`cold start`, `expired`, `unknown kid`, `scheduled miss`).

**Output**: Effective discovery metadata and key set state.

**Steps**:
1. [x] - `p1` - Keep discovery cache and JWKS cache independently bounded by TTL and max entries. - `inst-discovery-algo-bounded-caches`
2. [x] - `p1` - Evict least-recently-used entries when capacity limits are reached. - `inst-discovery-algo-lru-eviction`
3. [x] - `p1` - Enforce minimum interval between forced unknown-`kid` refreshes. - `inst-discovery-algo-refresh-throttle`
4. [x] - `p1` - Serve stale keys only inside configured stale window during upstream outage. - `inst-discovery-algo-stale-window`
5. [x] - `p1` - **RETURN** unavailable error when stale window is exhausted and fresh fetch fails. - `inst-discovery-algo-return-outage-failure`

## 4. States (CDSL)

### Discovery/JWKS Cache Entry State Machine

- [x] `p2` - **ID**: `cpt-cf-authn-plugin-state-oidc-discovery-jwks-lifecycle-cache-entry-state`

**States**: `fresh`, `stale-usable`, `expired-unusable`

**Initial State**: `fresh`

**Transitions**:
1. [x] - `p1` - **FROM** `fresh` **TO** `stale-usable` **WHEN** fresh TTL expires but stale window is still open. - `inst-discovery-state-fresh-to-stale`
2. [x] - `p1` - **FROM** `stale-usable` **TO** `expired-unusable` **WHEN** stale window expires. - `inst-discovery-state-stale-to-expired`
3. [x] - `p1` - **FROM** `stale-usable` **TO** `fresh` **WHEN** a successful refresh updates the entry. - `inst-discovery-state-stale-to-fresh`

## 5. Definitions of Done

### Discovery Endpoint Auto-Resolution
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-oidc-discovery-jwks-lifecycle-endpoint-auto-resolution`
The system **MUST** resolve and cache endpoint metadata from standard discovery documents.

### JWKS Rotation and Refresh Behavior
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-oidc-discovery-jwks-lifecycle-jwks-rotation-refresh`
The system **MUST** support unknown-`kid` forced refresh with bounded refresh frequency.

### Availability-Oriented Stale Handling
- [x] `p1` - **ID**: `cpt-cf-authn-plugin-dod-oidc-discovery-jwks-lifecycle-stale-availability`
The system **MUST** allow bounded stale key usage and fail closed when stale limits are exceeded.

## 6. Acceptance Criteria

- [x] Discovery document fields are cached and reused until TTL expiry.
- [x] JWKS cache supports explicit refresh on unknown `kid`.
- [x] Cache capacity and eviction behavior are deterministic.
- [x] Stale-while-revalidate behavior works inside configured stale TTL.
- [x] Expired-unusable cache state causes deterministic unavailable error.

## 7. Deliberate Omissions

- JWT claim validation semantics are omitted (covered by JWT validation feature).
- Claim-to-context mapping semantics are omitted (covered by claim mapping feature).
- S2S grant request payload logic is omitted (covered by S2S feature).
