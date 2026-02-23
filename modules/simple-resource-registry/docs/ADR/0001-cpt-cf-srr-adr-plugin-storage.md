---
status: accepted
date: 2026-02-24
---
# ADR-0001: Plugin-Based Multi-Backend Storage Architecture

**ID**: `cpt-cf-srr-adr-plugin-storage`

## Context and Problem Statement

Simple Resource Registry must work across diverse deployment targets — edge devices (SQLite), cloud infrastructure (PostgreSQL, MariaDB), and enterprise on-premises environments with existing asset stores (CMDBs, configuration databases). The registry needs a strategy for supporting multiple storage backends that can coexist simultaneously, with different resource types routed to different backends. How should the storage layer be structured to support this range of environments while keeping the consumer-facing API stable?

## Decision Drivers

* Must support edge (SQLite), cloud (PostgreSQL, MariaDB), and enterprise (existing vendor stores, including NoSQL storages) without API changes
* Platform vendors must be able to provide their own storage backends to bridge the registry to existing platform components
* Multiple backends must coexist simultaneously — different resource types may live in different stores
* Must align with the established ModKit plugin pattern used across CyberFabric
* Plugin implementations must remain thin — security, authorization, and event emission are centralized in the main module
* Must allow adding new backends without modifying the main module code

## Considered Options

* Single monolithic storage implementation
* Plugin-based architecture with GTS-based discovery
* Abstract storage interface without runtime discovery

## Decision Outcome

Chosen option: "Plugin-based architecture with GTS-based discovery", because it enables vendor-extensible, multi-backend coexistence while aligning with the ModKit plugin pattern already established across CyberFabric. The `ResourceStoragePluginClient` trait provides a minimal contract that keeps plugins thin, while GTS-based discovery and scoped ClientHub registration enable runtime resolution of the correct backend per resource type.

### Consequences

* Good, because vendors can implement custom backends to bridge existing platform storage without forking the module
* Good, because multiple backends coexist — relational DB for transactional workloads, search engines for full-text queries, vendor stores for existing data
* Good, because the same API serves all backends — consumers are unaware of which backend stores their resources
* Good, because aligns with ModKit plugin pattern — consistent with other CyberFabric modules
* Bad, because plugin interface becomes a stability contract — breaking changes require coordination across all plugin implementors
* Bad, because adds indirection (router → plugin resolution → scoped client lookup) compared to direct storage access
* Bad, because each plugin must independently implement idempotency deduplication and other storage-level concerns

### Confirmation

* Default relational DB plugin ships with the module and passes all acceptance criteria
* Plugin interface is versioned with major-version bumps for breaking changes
* At least one alternative plugin (search engine) can be wired without modifying the main module
* Code review verifies that security logic (auth, tenant scoping, GTS type checks) is not duplicated in plugins

## Pros and Cons of the Options

### Single monolithic storage implementation

One storage implementation compiled into the module; swap the entire implementation per deployment.

* Good, because simplest architecture — no plugin discovery, no routing, no trait indirection
* Good, because all storage logic is co-located and easy to debug
* Bad, because cannot support multiple backends simultaneously (e.g., relational DB + search engine)
* Bad, because vendors cannot extend storage without forking the module
* Bad, because swapping storage requires recompilation and deployment, not configuration

### Plugin-based architecture with GTS-based discovery

Trait-based plugin interface (`ResourceStoragePluginClient`) with GTS-based plugin discovery via Types Registry and scoped ClientHub registration. Per-resource-type routing directs operations to the configured backend.

* Good, because supports multi-backend coexistence through per-type routing
* Good, because vendor-extensible — new backends are separate modules registered via GTS
* Good, because aligns with ModKit plugin pattern (scoped clients, GTS instance IDs)
* Good, because plugins are thin — main module handles auth, events, audit centrally
* Bad, because plugin interface is a long-term stability contract
* Bad, because adds routing and discovery overhead per request
* Bad, because each plugin must implement idempotency independently

### Abstract storage interface without runtime discovery

Generic Rust trait for storage, but without GTS-based discovery or ClientHub registration. Backends are wired at compile time via dependency injection.

* Good, because simpler than full plugin architecture — no GTS discovery overhead
* Good, because compile-time safety for backend wiring
* Bad, because cannot add new backends without recompiling the main module
* Bad, because no runtime multi-backend coexistence
* Bad, because does not align with ModKit plugin pattern — inconsistent with platform conventions

## More Information

The plugin architecture follows the standard ModKit plugin pattern documented in `MODKIT_PLUGINS.md`:

- Each plugin registers a GTS instance in Types Registry and a scoped client in ClientHub via `ClientScope::gts_id(&instance_id)`
- The main module discovers plugins via GTS-based lookup and resolves the correct plugin per resource type through a routing configuration
- Plugin GTS instance ID pattern: `gts.x.core.modkit.plugin.v1~x.cf.simple_resource_registry.plugin.v1~<vendor>.<plugin_name>._.plugin.v1`

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-srr-fr-multi-backend-storage` — Enables multiple interchangeable storage backends via the plugin trait
* `cpt-cf-srr-fr-default-storage-backend` — Default relational DB backend is the first plugin implementation
* `cpt-cf-srr-fr-storage-routing` — Per-resource-type routing maps GTS types to plugin instances
* `cpt-cf-srr-fr-search-api` — Search-capable plugins declare `search_support` capability
* `cpt-cf-srr-principle-plugin-isolation` — Plugins handle only persistence; security is centralized
* `cpt-cf-srr-component-storage-router` — Router resolves plugin per resource type via GTS discovery
