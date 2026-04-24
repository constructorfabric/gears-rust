---
status: accepted
date: 2026-03-30
decision-makers: Virtuozzo
---

# ADR-0001: Separate IdP Integration Contract from AuthN Resolver


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Separate contracts (IdP integration plugin + AuthN Resolver plugin)](#option-1-separate-contracts-idp-integration-plugin--authn-resolver-plugin)
  - [Option 2: Single unified IdP contract](#option-2-single-unified-idp-contract)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-account-management-adr-idp-contract-separation`

## Context and Problem Statement

AM needs to interact with Identity Providers for two distinct categories of operations: (1) hot-path token validation on every API request, and (2) infrequent administrative operations such as tenant provisioning/deprovisioning, user provisioning/deprovisioning, and impersonation. Both categories target the same underlying IdP but have fundamentally different performance profiles, failure tolerance, and deployment requirements. Should AM define one unified contract covering both, or separate contracts for each concern?

The Cyber Fabric platform already provides the AuthN Resolver as a gateway + plugin architecture for hot-path authentication — plugins implement `AuthNResolverPluginClient`, are discovered via GTS types-registry, and are selected at runtime through `ClientHub`. The question is whether the administrative IdP operations should be folded into the same plugin contract or separated into a dedicated plugin with its own trait and GTS schema.

## Decision Drivers

* **Hot-path latency budget**: AuthN Resolver validates tokens on every API request with a microsecond budget; it must be stateless and always available. Any coupling to transactional admin logic risks degrading this critical path.
* **Admin operation characteristics**: Tenant provisioning (e.g., creating a Keycloak realm), user provisioning, deprovisioning, and impersonation are infrequent operations with different latency tolerance (retries, transactions) and different protocols (SCIM, admin REST API vs OIDC token validation).
* **CyberFabric plugin pattern alignment**: The platform uses a gateway + plugin pattern for extensible integrations (AuthN Resolver, Tenant Resolver, AuthZ Resolver). The IdP integration contract should follow the same pattern — a plugin trait discovered via GTS, registered in `ClientHub`, and selected by the gateway at runtime.
* **Independent evolution**: Authentication standards (OIDC) and admin APIs (SCIM, vendor REST) evolve on different timelines. Coupling them in one contract forces synchronized changes.
* **Optional deployment**: Not all deployments require user management through AM. Some may use external provisioning while still requiring token validation.

## Considered Options

1. **Separate contracts** — IdP integration plugin (`IdpProviderPluginClient`) for admin operations, AuthN Resolver plugin (`AuthNResolverPluginClient`) for token validation. Both follow the CyberFabric gateway + plugin pattern with independent GTS schemas and ClientHub registration.
2. **Single unified IdP contract** — one plugin contract covering both token validation and admin operations.

## Decision Outcome

Chosen option: **Separate contracts**, because the two categories have fundamentally different performance profiles, protocols, and deployment requirements. Both contracts follow the CyberFabric plugin pattern (GTS-registered, ClientHub-discovered, vendor-replaceable) but as independent plugins. Merging them would couple the hot-path authentication with transactional admin logic, creating unnecessary risk on every API request.

The IdP integration contract is implemented as a CyberFabric plugin, analogous to `AuthNResolverPluginClient`:

- **SDK trait**: `IdpProviderPluginClient` — defines `check_availability`, `provision_tenant`, `deprovision_tenant`, `create_user`, `delete_user`, `list_users`, `issue_impersonation_token`, `supports_impersonation`. `check_availability` is the side-effect-free provider readiness probe used by bootstrap retry/backoff and returns only available / unavailable / provider error outcomes under the configured timeout. Mutating operations must not silently no-op; unsupported mutating capabilities return explicit `idp_unsupported_operation` failures. Capability probes such as `supports_impersonation` may default to `false`, and read helpers may use explicit empty results only where the trait contract documents that behavior.
- **GTS schema**: A dedicated GTS schema (e.g., `gts.x.core.am.idp_provider.v1~`) registers the plugin spec. Vendor-specific implementations derive from this schema.
- **Discovery**: The accounts gateway module discovers the active IdP provider plugin via GTS types-registry and resolves it through `ClientHub`, the same way AuthN Resolver discovers its plugins.
- **Deployment**: The platform ships a default IdP provider plugin. Vendors substitute their own implementation (e.g., Keycloak-specific realm provisioning) behind the same trait.

### Consequences

* The IdP integration contract (`IdpProviderPluginClient`) and AuthN Resolver contract (`AuthNResolverPluginClient`) are independent plugins — no compile-time or runtime coupling. Both may target the same IdP instance but reference a shared configuration block rather than being merged into one interface.
* Deployments that only need authentication (no AM-managed user provisioning) can implement only the AuthN Resolver plugin without the IdP provider plugin.
* Deployments whose IdP supports standard OpenID Connect can reuse the platform-shipped OIDC AuthN Resolver plugin out of the box for authentication and only implement the `IdpProviderPluginClient` for vendor-specific admin operations (user provisioning, realm setup, impersonation). This significantly reduces integration effort — the vendor writes only the admin part, not the entire IdP integration surface.
* Each plugin can evolve independently — changes to admin API protocols (e.g., migrating from vendor REST to SCIM) do not require changes to the token validation plugin.
* Implementors must provide two separate plugin implementations when both capabilities are needed, which adds a small amount of integration complexity.
* The plugin follows established CyberFabric patterns: GTS-typed, ClientHub-registered, vendor-replaceable.

### Confirmation

* DESIGN validates that the two plugin contracts have no compile-time or runtime coupling.
* At least two conforming IdP provider implementations pass the plugin contract test suite independently (per PRD success criteria).
* IdP provider plugin is discovered via GTS types-registry and resolved through `ClientHub`, consistent with AuthN Resolver plugin discovery.

## Pros and Cons of the Options

### Option 1: Separate contracts (IdP integration plugin + AuthN Resolver plugin)

* Good, because hot-path token validation remains decoupled from admin operation failures and latency.
* Good, because each plugin can evolve independently with different protocol requirements.
* Good, because deployments can selectively enable user management without affecting authentication.
* Good, because deployments whose IdP supports standard OIDC can reuse the platform-shipped AuthN plugin and only implement the vendor-specific admin plugin — reducing integration effort to just the provisioning part.
* Good, because both follow the established CyberFabric plugin pattern — consistent with AuthN Resolver, Tenant Resolver, and AuthZ Resolver.
* Neutral, because shared configuration between the two plugins requires a small coordination mechanism.
* Bad, because implementors must provide two separate plugin implementations when targeting the same IdP.

### Option 2: Single unified IdP contract

* Good, because a single contract simplifies the integration surface — one implementation per IdP.
* Good, because shared state (connection pools, configuration) is implicit.
* Bad, because hot-path token validation becomes coupled to transactional admin logic, risking latency degradation on every API request.
* Bad, because changes to admin protocols force changes to the authentication contract.
* Bad, because deployments that don't need user management still carry the admin contract surface.
* Bad, because it deviates from the CyberFabric pattern of focused, single-responsibility plugins.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements:

* `cpt-cf-account-management-fr-idp-tenant-provision` — IdP provider plugin handles tenant provisioning as an admin operation, separate from hot-path auth.
* `cpt-cf-account-management-fr-idp-tenant-deprovision` — IdP provider plugin handles tenant deprovisioning during hard deletion.
* `cpt-cf-account-management-fr-idp-user-provision` — IdP provider plugin handles user provisioning as an admin operation.
* Open-question follow-up — if managed-tenant impersonation is adopted for a future baseline, token issuance remains an admin operation routed through the IdP provider plugin rather than through the AuthN Resolver path.
* `cpt-cf-account-management-nfr-context-validation-latency` — Tenant context validation (p95 <= 5ms) depends on AuthN Resolver remaining decoupled from admin operations.
* `cpt-cf-account-management-contract-idp-provider` — Defines the IdP integration contract as a separate pluggable plugin interface.
