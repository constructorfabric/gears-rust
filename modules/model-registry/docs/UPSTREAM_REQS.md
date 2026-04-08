# Upstream Requirements — Model Registry

## Overview

This document consolidates requirements from downstream modules that depend on Model Registry. It serves as validation that Model Registry API meets consumer needs.

---

## LLM Gateway Requirements

**Source**: `modules/llm-gateway/docs/` (PRD, DESIGN, ADR-0004)

**Priority**: P1 (core integration)

### Required Operations

#### 1. Get Tenant Model

Resolve model by canonical ID for a tenant, checking availability and approval status.

**Input**:
| Field | Type | Description |
|-------|------|-------------|
| ctx | SecurityContext | Tenant context for resolution |
| model_id | string | Canonical model ID (`provider_slug::model_id`) |

**Output**:
| Field | Type | Description |
|-------|------|-------------|
| model | ModelInfo | Model metadata and capabilities |
| provider | ProviderInfo | Provider endpoint and configuration |
| health | ProviderHealth | Health metrics for routing decisions |
| approval_status | enum | `approved` (or error if not) |

**Behavior**:
- Check model exists in catalog
- Check tenant approval status (considering hierarchy)
- Return provider health metrics for routing
- Error if model not found, not approved, or deprecated

**Source**: [`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md)

---

### Required Response Data

#### ModelInfo

| Field | Required | Description |
|-------|----------|-------------|
| canonical_id | yes | `provider_slug::provider_model_id` |
| name | yes | Display name |
| capabilities | yes | Capability flags for validation |
| limits | yes | context_window, max_output_tokens |
| lifecycle_status | yes | production, preview, deprecated, sunset |

**Source**: [`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md)

---

#### ProviderInfo

| Field | Required | Description |
|-------|----------|-------------|
| slug | yes | Provider identifier |
| base_url | yes | Provider API endpoint (for OAGW routing) |
| gts_type | yes | GTS type for credential injection |

**Source**: [`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md)

---

#### ProviderHealth (P2)

| Field | Required | Description |
|-------|----------|-------------|
| status | yes | healthy, degraded, unhealthy |
| latency_p50_ms | no | Discovery latency P50 |
| latency_p99_ms | no | Discovery latency P99 |
| error_rate | no | Error rate over time window |

**Usage**: LLM Gateway uses health metrics for proactive provider selection before making requests.

**Source**: [`cpt-cf-llm-gateway-adr-circuit-breaking`](../../llm-gateway/docs/ADR/0004-fdd-llmgw-adr-circuit-breaking.md)

---

### Required Errors

| Error | When | LLM Gateway Response |
|-------|------|---------------------|
| `model_not_found` | Model not in catalog | 404 model_not_found |
| `model_not_approved` | Model not approved for tenant | 403 model_not_approved |
| `model_deprecated` | Model sunset by provider | 410 model_deprecated |

**Source**: [`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md)

---

### Required Behaviors

#### Tenant Hierarchy Resolution

- Resolve approval status considering tenant hierarchy
- Child tenant inherits parent approvals
- Return most specific approval (own > parent > root)

**Source**: [`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md)

---

#### Low Latency

- get_tenant_model must meet <10ms P99 latency
- LLM Gateway calls this on every request
- Cache-first resolution required

**Source**: [`cpt-cf-model-registry-nfr-performance`](./PRD.md)

---

## Traceability

### LLM Gateway Sources

- [ ] [`cpt-cf-llm-gateway-seq-provider-resolution-v1`](../../llm-gateway/docs/DESIGN.md) — model resolution sequence
- [ ] [`cpt-cf-llm-gateway-adr-circuit-breaking`](../../llm-gateway/docs/ADR/0004-fdd-llmgw-adr-circuit-breaking.md) — health-based routing
- [ ] [`cpt-cf-llm-gateway-fr-provider-fallback-v1`](../../llm-gateway/docs/PRD.md) — fallback on failure
