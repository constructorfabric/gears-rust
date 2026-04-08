---
status: superseded
date: 2026-02-09
decision-makers: OAGW Team
---

# Component Architecture — Single Module with Trait-Based Service Isolation


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Internal Services](#internal-services)
  - [Module Structure](#module-structure)
  - [Internal Communication](#internal-communication)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Multi-crate architecture](#multi-crate-architecture)
  - [Single module with internal trait-based service isolation](#single-module-with-internal-trait-based-service-isolation)
  - [Monolithic single-service design](#monolithic-single-service-design)
- [More Information](#more-information)
- [Related ADRs](#related-adrs)
- [References](#references)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-component-architecture`

**Superseded By**: Single-module implementation with internal trait-based service isolation (2026-02-17). The original multi-crate design was simplified during implementation to a single `oagw` crate with DDD-Light layering (`domain/infra/api`). The CP/DP separation is preserved as internal domain traits, not separate crates.

## Context and Problem Statement

OAGW is being designed as a greenfield project without existing code. We need to establish the architectural foundation for how components are organized and how separation of concerns is achieved.

OAGW needs a component architecture that separates concerns between configuration management (Control Plane) and request processing (Data Plane) while remaining practical to implement within CyberFabric's modular monolith architecture. The question is whether to use separate crates or a single module with internal isolation.

**Key Requirements**:

- Clear separation of concerns between configuration management and request execution
- Testability of each concern in isolation
- Minimize latency through efficient communication patterns

## Decision Drivers

* Clear separation between configuration management and request processing
* Practical implementation within CyberFabric's modular monolith
* Testability: Each service should be testable in isolation via trait mocking
* Performance: Direct in-process function calls with zero serialization overhead
* Maintainability: Clear boundaries and responsibilities via domain trait interfaces
* Simplicity: Avoid multi-crate coordination overhead when not needed
* Support for both single-exec and potential microservice deployment modes

## Considered Options

* Multi-crate architecture with separate `oagw-cp` and `oagw-dp` crates
* Single module with internal trait-based service isolation
* Monolithic single-service design (no CP/DP separation)

## Decision Outcome

Chosen option: "Single module with internal trait-based service isolation", because it provides clean separation via Rust traits without the overhead of multi-crate dependency management.

The architecture uses two domain traits within a single `oagw` crate:

### Internal Services

**1. Control Plane (`ControlPlaneService` trait)**

- **Location**: Trait in `domain/services/mod.rs`, impl in `domain/services/management.rs`
- **Responsibility**: Manage configuration data
- **Functions**:
    - CRUD operations for upstreams and routes
    - Alias resolution for proxy requests
    - Tenant-scoped repository access
- **Dependencies**: `UpstreamRepository`, `RouteRepository` (domain traits)
- **Endpoints**: `/api/oagw/v1/{upstreams,routes,plugins}/*`

**2. Data Plane (`DataPlaneService` trait)**

- **Location**: Trait in `domain/services/mod.rs`, impl in `infra/proxy/service.rs`
- **Responsibility**: Orchestrate proxy requests to external services
- **Functions**:
    - Call Control Plane for config resolution (upstream, route)
    - Execute auth plugins (credential injection via `AuthPluginRegistry`)
    - Build and send HTTP requests to upstream services
    - Strip sensitive/hop-by-hop headers from responses
- **Dependencies**: `ControlPlaneService`, `AuthPluginRegistry`, `CredentialRepository`, `reqwest::Client`
- **Endpoints**: `/api/oagw/v1/proxy/*`

### Module Structure

```text
modules/system/oagw/
├── oagw-sdk/              # Public API: ServiceGatewayClientV1 trait, models, errors
└── oagw/                  # Single module crate
    └── src/
        ├── api/rest/      # Transport layer (handlers, routes, DTOs)
        ├── domain/        # Business logic (traits, models, errors)
        │   └── services/  # ControlPlaneService + DataPlaneService
        └── infra/         # Infrastructure (proxy, storage, plugins)
```

### Internal Communication

All services communicate via in-process trait method calls. There is no inter-service RPC or serialization:

- REST handlers call `ControlPlaneService` or `DataPlaneService` directly
- `DataPlaneServiceImpl` holds an `Arc<dyn ControlPlaneService>` for config resolution
- Services are wired together during ModKit module initialization in `module.rs`

### Consequences

#### Positive

* Good, because trait-based isolation enables independent testing of CP and DP (e.g., `MockControlPlaneService`)
* Good, because single crate simplifies build, dependency management, and deployment
* Good, because DDD-Light layering keeps domain logic separate from infrastructure, enforced by dylint linters
* Good, because migration to separate crates remains possible if needed
* Good, because zero overhead — direct Rust function calls, no serialization or RPC

#### Negative

* Bad, because CP and DP share the same compilation unit (tighter coupling than separate crates)
* Bad, because no independent scaling — CP and DP cannot be scaled separately (acceptable for current workload)
* Neutral, because both services must be deployed together (acceptable for modular monolith)

#### Risks

* If CP and DP need independent scaling, extraction into separate crates would require refactoring. Mitigated by clean trait boundaries — the domain traits already define the split points.

### Confirmation

Code review confirms: `ControlPlaneService` and `DataPlaneService` traits exist in `domain/`, implementations in `infra/`, and REST handlers in `api/`. No direct cross-service calls bypass trait boundaries.

## Pros and Cons of the Options

### Multi-crate architecture

Three separate library crates (`oagw`, `oagw-cp`, `oagw-dp`) with shared `oagw-types` crate and modkit wiring for both single-executable and microservice deployment.

* Good, because maximum compile-time isolation between CP and DP
* Good, because enables independent deployment in microservice mode
* Bad, because increases dependency management complexity (workspace, version alignment)
* Bad, because shared types crate introduces coupling anyway
* Bad, because slower iteration during development (cross-crate builds)
* Bad, because microservice mode not needed for current scale

**Not adopted**: The single-module approach provides the same testability and separation of concerns with less complexity. The trait boundaries are preserved, making future extraction straightforward if needed.

### Single module with internal trait-based service isolation

Single `oagw` crate with `ControlPlaneService` and `DataPlaneService` traits.

* Good, because clean separation via Rust trait system
* Good, because single crate simplifies build and deployment
* Good, because easy to refactor to multi-crate later if needed
* Bad, because no compile-time enforcement of service boundaries (only convention)

### Monolithic single-service design

No CP/DP separation; single crate with no CP/DP trait distinction — all logic in handlers.

* Good, because simplest implementation
* Bad, because mixes configuration management with request processing concerns
* Bad, because harder to test proxy orchestration independently
* Bad, because no clear boundary between config management and request execution
* Bad, because no path to independent scaling

**Rejected**: Trait-based separation provides essential testability and maintainability.

## More Information

The CP/DP separation mirrors industry patterns (Envoy, Kong, Istio) where the control plane manages configuration and the data plane handles traffic. In OAGW's case, both run in-process but the trait boundary enables future separation.

Related architectural patterns:
- CyberFabric ModKit module conventions (single crate per module)
- DDD-Light layering (`domain/infra/api`)

## Related ADRs

- [ADR: Request Routing](./0002-request-routing.md) - How requests flow through handlers to services
- [ADR: Control Plane Caching](./0007-data-plane-caching.md) - Multi-layer cache strategy
- [ADR: State Management](./0008-state-management.md) - State distribution patterns

## References

- Modkit framework documentation (module lifecycle and dependency injection)
- CyberFabric module patterns: `tenant_resolver`, `types_registry`

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-upstream-mgmt` — Control Plane handles upstream CRUD
* `cpt-cf-oagw-fr-route-mgmt` — Control Plane handles route CRUD
* `cpt-cf-oagw-fr-request-proxy` — Data Plane orchestrates proxy execution
* `cpt-cf-oagw-fr-plugin-system` — Plugin execution split: definitions in CP, execution in DP
