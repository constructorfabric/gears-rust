---
status: accepted
date: 2026-02-28
---

# Use RFC 9457 Problem Details as REST Wire Format

**ID**: `cpt-cf-errors-adr-rfc9457-wire-format`

## Context and Problem Statement

The canonical error model needs a REST wire format — a JSON structure that every error response uses. The format must accommodate the platform's structured context fields while being recognizable to external consumers. Should the platform invent a custom JSON structure or adopt an existing standard?

## Decision Drivers

* Standards alignment — prefer established formats that external consumers already know
* Extensibility — the format must support platform-specific fields (GTS type, structured context, trace ID) alongside standard fields
* Tooling ecosystem — existing libraries, validators, and documentation for the format
* Transport specificity — the REST format is for HTTP only; gRPC will use its own format (future work)

## Considered Options

* **Option A**: RFC 9457 Problem Details for HTTP APIs
* **Option B**: Custom JSON error envelope
* **Option C**: JSON:API error format

## Decision Outcome

Chosen option: **Option A — RFC 9457 Problem Details**, because it is an IETF standard specifically designed for HTTP API error responses, explicitly permits extension members for platform-specific fields, and is widely supported by API tooling.

### Consequences

* The error library must define a `Problem` struct with the five standard RFC 9457 members (`type`, `title`, `status`, `detail`, `instance`) plus CyberFabric extension members (`trace_id`, `context`, `debug`)
* All REST error responses must use `Content-Type: application/problem+json` — middleware must set this header
* The `type` field carries a GTS URI (not a dereferenceable URL), which deviates from RFC 9457's intent but is valid per spec — this must be documented as a known deviation
* gRPC and SSE transports will need their own wire format mappings in the future — RFC 9457 only covers REST
* Round-trip deserialization (SDK side) must parse the `Problem` JSON and reconstruct `CanonicalError` from the `type` field — the `TryFrom<Problem>` implementation is required
* Extension members (`context`, `debug`, `trace_id`) must be defined and versioned alongside the core Problem structure

### Confirmation

The PoC implementation produces valid `application/problem+json` responses with all five standard members plus `trace_id` and `context` extensions.

## Pros and Cons of the Options

### Option A: RFC 9457 Problem Details

Use the IETF standard for HTTP API error responses.

* Good, because IETF standard (RFC 9457, successor to RFC 7807)
* Good, because explicit extension mechanism for custom fields
* Good, because `application/problem+json` media type enables content negotiation
* Good, because widely supported by API gateways, documentation tools, and client libraries
* Neutral, because `type` field is intended for dereferenceable URIs — GTS identifiers are opaque but still valid per spec
* Bad, because HTTP-specific — does not cover gRPC or event-driven transports

### Option B: Custom JSON Error Envelope

Define a platform-specific JSON structure (e.g., `{ "error": { "code": ..., "message": ..., "details": ... } }`).

* Good, because full control over every field
* Good, because transport-agnostic (same JSON for REST and SSE)
* Bad, because no industry recognition — consumers must learn a bespoke format
* Bad, because no standard media type — cannot use content negotiation
* Bad, because reinvents what RFC 9457 already provides

### Option C: JSON:API Error Format

Use the JSON:API error object specification.

* Good, because well-documented open standard
* Bad, because JSON:API is a full API specification — adopting just the error format creates inconsistency (CyberFabric does not use JSON:API for success responses)
* Bad, because JSON:API error objects have a different structure (`errors` array, `source` pointer) that does not align with the canonical error model's single-error-per-response pattern

## More Information

RFC 9457 wire format with CyberFabric extensions:

| Field | RFC 9457 | CyberFabric Extension |
|-------|----------|----------------------|
| `type` | Standard (§3.1.1) | Carries GTS type URI |
| `title` | Standard (§3.1.2) | Static per category |
| `status` | Standard (§3.1.3) | HTTP status from category mapping |
| `detail` | Standard (§3.1.4) | Human-readable per-occurrence message |
| `instance` | Standard (§3.1.5) | Request URI path |
| `trace_id` | Extension (§3.2) | W3C trace ID for correlation |
| `context` | Extension (§3.2) | Category-specific structured details |
| `debug` | Extension (§3.2) | Optional diagnostic info (debug mode only) |

See [DESIGN.md](../DESIGN.md) § RFC 9457 Problem Wire Format.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements:

* `cpt-cf-errors-fr-standard-adoption` — Selects RFC 9457 as the industry standard for REST error responses
* `cpt-cf-errors-fr-structured-context` — RFC 9457 extension members carry the structured context payload
* `cpt-cf-errors-fr-mandatory-trace-id` — `trace_id` extension member in every Problem response
