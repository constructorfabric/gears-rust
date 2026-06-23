# UPSTREAM_REQS — Model Registry

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting Gears](#12-requesting-gears)
- [2. Requirements](#2-requirements)
  - [2.1 LLM Gateway](#21-llm-gateway)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)
  - [LLM Gateway Sources](#llm-gateway-sources)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Model Registry serves as a centralized catalog of AI models. This document consolidates requirements from gears that depend on Model Registry's API to resolve models, check approval status, and obtain provider information for routing.

### 1.2 Requesting Gears

| Gear   | Why it needs this gear |
|--------|-------------------------|
| llm-gateway | Needs to resolve models by canonical ID, check tenant approval status, and obtain provider info and health metrics for routing decisions on every request |

## 2. Requirements

### 2.1 LLM Gateway

#### Resolve Tenant Model

- [ ] `p1` - **ID**: `cpt-cf-model-registry-upreq-get-tenant-model`

The gear **MUST** resolve a model by canonical ID for a given tenant, checking catalog existence, tenant approval status (considering hierarchy), and returning model metadata with provider information.

- **Rationale**: LLM Gateway must resolve the target model and provider on every incoming request before routing to the provider API.
- **Source**: `gears/llm-gateway` ([`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md))

#### Return Model Capabilities and Limits

- [ ] `p1` - **ID**: `cpt-cf-model-registry-upreq-model-capabilities`

The gear **MUST** return model capability flags and limits (context window, max output tokens) alongside model resolution so that the caller can validate request compatibility.

- **Rationale**: LLM Gateway needs to validate that a request is compatible with the model's capabilities before forwarding to the provider.
- **Source**: `gears/llm-gateway` ([`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md))

#### Return Provider Endpoint Information

- [ ] `p1` - **ID**: `cpt-cf-model-registry-upreq-provider-info`

The gear **MUST** return the provider's API endpoint and GTS type for credential injection as part of model resolution.

- **Rationale**: LLM Gateway routes requests through OAGW and needs the provider base URL and GTS type to inject credentials.
- **Source**: `gears/llm-gateway` ([`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md))

#### Return Provider Health Metrics

- [ ] `p3` - **ID**: `cpt-cf-model-registry-upreq-provider-health`

The gear **MUST** return provider health status (healthy, degraded, unhealthy) and optionally latency and error rate metrics for proactive provider selection.

- **Rationale**: LLM Gateway uses health metrics to select providers before making requests, avoiding providers that are degraded or unhealthy.
- **Source**: `gears/llm-gateway` ([`cpt-cf-llm-gateway-adr-circuit-breaking`](../../llm-gateway/docs/ADR/0004-fdd-llmgw-adr-circuit-breaking.md), [`cpt-cf-llm-gateway-fr-provider-fallback-v1`](../../llm-gateway/docs/PRD.md))

#### Tenant Hierarchy Resolution

- [ ] `p1` - **ID**: `cpt-cf-model-registry-upreq-tenant-hierarchy`

The gear **MUST** resolve approval status considering the tenant hierarchy, where child tenants inherit parent approvals and the most specific approval (own > parent > root) takes precedence.

- **Rationale**: LLM Gateway serves requests from tenants at any hierarchy level; approval resolution must be transparent to the caller.
- **Source**: `gears/llm-gateway` ([`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md))

#### Low Latency Resolution

- [ ] `p1` - **ID**: `cpt-cf-model-registry-upreq-low-latency`

The gear **MUST** resolve model lookups within <10ms at P99 latency.

- **Rationale**: LLM Gateway calls model resolution on every request; high latency here directly impacts end-user response times.
- **Source**: `gears/llm-gateway` ([`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md))

#### Specific Error Responses

- [ ] `p1` - **ID**: `cpt-cf-model-registry-upreq-error-responses`

The gear **MUST** return distinct errors for: model not found in catalog, model not approved for tenant, and model deprecated/sunset by provider.

- **Rationale**: LLM Gateway maps these errors to specific HTTP status codes (404, 403, 410) for its callers; a generic error is insufficient.
- **Source**: `gears/llm-gateway` ([`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md))

## 3. Priorities

| Priority | Requirements |
|----------|-------------|
| p1 (critical) | `cpt-cf-model-registry-upreq-get-tenant-model`, `cpt-cf-model-registry-upreq-model-capabilities`, `cpt-cf-model-registry-upreq-provider-info`, `cpt-cf-model-registry-upreq-tenant-hierarchy`, `cpt-cf-model-registry-upreq-low-latency`, `cpt-cf-model-registry-upreq-error-responses` |
| p3 (deferred) | `cpt-cf-model-registry-upreq-provider-health` — depends on Model Registry `fr-health-monitoring` which is scheduled for MR phase 3 |

## 4. Traceability

- **PRD** (when created): [PRD.md](./PRD.md)
- **Design** (when created): [DESIGN.md](./DESIGN.md)

### LLM Gateway Sources

- [`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md)
- [`cpt-cf-llm-gateway-adr-circuit-breaking`](../../llm-gateway/docs/ADR/0004-fdd-llmgw-adr-circuit-breaking.md)
- [`cpt-cf-llm-gateway-fr-provider-fallback-v1`](../../llm-gateway/docs/PRD.md)
