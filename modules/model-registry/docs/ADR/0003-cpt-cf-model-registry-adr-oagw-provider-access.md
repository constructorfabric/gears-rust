---
status: accepted
date: 2026-02-18
---

# Route All Provider API Calls Through Outbound API Gateway

**ID**: `cpt-cf-model-registry-adr-oagw-provider-access`

## Context and Problem Statement

Model Registry needs to call Provider APIs for model discovery. These calls require authentication credentials and must comply with security policies. How should Model Registry access external Provider APIs?

## Decision Drivers

* `cpt-cf-model-registry-fr-model-discovery` — Fetch models from provider APIs
* `cpt-cf-model-registry-constraint-no-credentials` — No credential storage in Model Registry
* Security — centralized outbound URL policy enforcement
* Reliability — circuit breaking for external calls
* Platform architecture — OAGW is the standard outbound gateway

## Considered Options

* All calls through Outbound API Gateway (OAGW)
* Direct calls with credentials stored in Model Registry
* Sidecar proxy per provider type

## Decision Outcome

Chosen option: "All calls through OAGW", because it aligns with platform security architecture, centralizes credential management, and provides circuit breaking out of the box.

### Consequences

* Good, because credentials are never stored in Model Registry
* Good, because centralized security policy enforcement
* Good, because circuit breaking prevents cascade failures
* Good, because consistent with platform architecture
* Bad, because dependency on OAGW availability
* Bad, because additional network hop adds latency

### Confirmation

* Code review verifies no direct HTTP calls to provider APIs
* Code review verifies no credential storage in Model Registry
* Integration tests verify discovery works through OAGW

## Pros and Cons of the Options

### All Calls Through OAGW

Model Registry → OAGW → Provider API

OAGW responsibilities:
- Credential injection (API keys, OAuth tokens)
- Outbound URL policy enforcement (block internal networks, require HTTPS)
- Circuit breaking for failing providers
- Request/response logging (metadata only)

* Good, because centralized credential management
* Good, because unified security policy enforcement
* Good, because circuit breaking prevents cascade failures
* Good, because aligns with platform architecture
* Neutral, because requires OAGW configuration per provider
* Bad, because OAGW dependency
* Bad, because additional network hop

### Direct Calls with Credentials in Model Registry

Model Registry stores credentials and calls providers directly.

* Good, because lower latency (no intermediate hop)
* Good, because independent of OAGW
* Bad, because credential sprawl across services
* Bad, because duplicated security policy implementation
* Bad, because no centralized circuit breaking
* Bad, because violates platform security architecture

### Sidecar Proxy per Provider Type

Dedicated proxy sidecar for each provider type.

* Good, because provider-specific handling isolated
* Good, because credentials isolated per provider
* Bad, because operational complexity (many sidecars)
* Bad, because resource overhead
* Bad, because inconsistent with platform patterns

## More Information

Discovery flow:
```
Model Registry
    │
    ├── GET /providers/{id}/discover
    │
    ▼
OAGW (inject credentials, enforce policy)
    │
    ▼
Provider API (e.g., OpenAI /models, Azure /deployments)
    │
    ▼
OAGW (circuit breaking, logging)
    │
    ▼
Model Registry (process response, update catalog)
```

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-model-registry-fr-model-discovery` — Discovery via OAGW integration
* `cpt-cf-model-registry-constraint-no-credentials` — Credentials handled by OAGW
* `cpt-cf-model-registry-constraint-oagw-dependency` — Establishes OAGW as mandatory
* `cpt-cf-model-registry-interface-provider-apis` — Defines provider API access pattern
