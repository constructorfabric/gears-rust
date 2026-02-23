---
status: accepted
date: 2026-02-24
---
# ADR-0003: Search Capability is Backend-Defined and Routed Per Resource Type

**ID**: `cpt-cf-srr-adr-search-strategy`

## Context and Problem Statement

Simple Resource Registry supports multiple storage backends (plugins) and per-resource-type storage routing. Some resource types require full-text search across content (typically payload fields), while other types do not. There are multiple ways to provide search:

- A storage backend could maintain its own dedicated index and execute search internally.
- Search could be implemented via an external indexing system that indexes content from all backends.

The system needs a clear architectural stance on whether search is a universal requirement (and how it is implemented) or a capability that varies by backend and by resource type.

## Decision Drivers

* Search needs vary by resource type; not all resources require full-text search
* Storage technologies differ: some backends can natively support search, others cannot
* The platform must support heterogeneous backends coexisting at the same time
* The system must remain portable across deployments (edge/cloud/enterprise)
* The main module should not require indexing assumptions about payloads (payload is opaque at the SRR contract level)

## Considered Options

* Require every backend to provide its own dedicated index
* Require a single external index for all content across all backends
* Make search a backend capability, enabled or disabled per resource type via routing

## Decision Outcome

Chosen option: "Make search a backend capability, enabled or disabled per resource type via routing", because it preserves multi-backend flexibility and allows search-heavy resource types to be routed to a search-capable backend while keeping other types on simpler backends.

Search is therefore **not universally available**. It is available only when the resolved backend for a resource type declares `search_support` capability. The main module may expose a search API that returns 501 Not Implemented when the selected backend does not support search.

### Consequences

* Good, because search is optional and per-resource-type — types that do not need search avoid unnecessary indexing cost
* Good, because backends remain free to choose the best search strategy (internal index vs external system)
* Good, because aligns with multi-backend routing — search can be enabled by routing search-heavy types to a search backend
* Bad, because search semantics may differ between backends (ranking, analyzers, query language)
* Bad, because platform operators must manage search-capable backends explicitly (deployment + routing)

### Confirmation

* The search API returns 501 when `search_support` is false for the resolved backend
* At least one search-capable backend can be integrated without changing SRR API contracts
* Per-resource-type routing can enable search for only selected types

## Pros and Cons of the Options

### Require every backend to provide its own dedicated index

* Good, because search is always available
* Bad, because forces indexing complexity onto all backends, including those where it is unnatural or inefficient
* Bad, because increases implementation burden for vendor-provided backends

### Require a single external index for all content across all backends

* Good, because uniform search behavior across resource types
* Good, because decouples indexing from persistence backends
* Bad, because requires a universal indexing pipeline and consistency model
* Bad, because increases operational complexity and is not suitable for all deployments (e.g., edge)

### Make search a backend capability, enabled or disabled per resource type via routing

* Good, because flexible and aligns with plugin architecture
* Good, because avoids universal indexing cost
* Bad, because search behavior is not necessarily uniform across all backends

## Traceability

- **PRD**: [../PRD.md](../PRD.md)
- **DESIGN**: [../DESIGN.md](../DESIGN.md)

This decision directly relates to:

* `cpt-cf-srr-fr-search-api` — search endpoint depends on backend capability
* `cpt-cf-srr-fr-multi-backend-storage` — different backends provide different capabilities
* `cpt-cf-srr-fr-storage-routing` — per-type routing enables selecting a search-capable backend
* `cpt-cf-srr-adr-plugin-storage` — plugin-based architecture enables heterogeneous implementations
