---
status: accepted
date: 2026-10-20
description: Operation records and request history rows are unrestricted entities with no tenant column, and the PDP decides every gear-level action as a platform-level permission with no context tenant
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Platform-Level Permission and Unrestricted Operation Records

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Platform-level permission with unrestricted records](#platform-level-permission-with-unrestricted-records)
  - [Platform tenant looked up from the tenant directory at startup](#platform-tenant-looked-up-from-the-tenant-directory-at-startup)
  - [Platform tenant looked up per request](#platform-tenant-looked-up-per-request)
  - [Platform tenant id taken from gear configuration](#platform-tenant-id-taken-from-gear-configuration)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-platform-level-access`

## Context and Problem Statement

The gear manages applications installed into the whole environment, so its records belong to the platform, not to a tenant. The platform default for gears is tenant-scoped entities and tenant-scoped PDP decisions; usage-collector already gates its platform-global writes through the PDP. Should the gear scope its records and decisions to a platform tenant, or keep records unrestricted and let the PDP decide platform-level permission?

## Decision Drivers

* `cpt-cf-application-manager-nfr-platform-scope`
* `cpt-cf-application-manager-fr-authorization`
* `cpt-cf-application-manager-fr-startup-checks`

## Considered Options

* Platform-level permission with unrestricted records
* Platform tenant looked up from the tenant directory at startup
* Platform tenant looked up per request
* Platform tenant id taken from gear configuration

## Decision Outcome

Chosen option: "Platform-level permission with unrestricted records", because startup depends only on the types registry and the authz resolver, and platform-level permission stays with the one component that owns permission decisions.

* Operation records and request history rows are unrestricted secure-ORM entities with no tenant column.
* Every gear-level action is a PDP decision with no context tenant, as a platform-level permission; decision handling is in [DESIGN §3.3](../DESIGN.md#33-api-contracts).
* Plugin-level `NotPermitted` is a separate mechanism outside this decision.

### Consequences

* Policy authors grant each of the five actions explicitly at platform level; policy content and its correctness belong to the authz resolver and its plugin.
* The repository applies no tenant filter, and records are reached only through the gear's routes after a permit.

### Confirmation

* Inspection: both entities are unrestricted with no tenant column.
* A table-driven test with a PDP double over permit, deny, `CompileFailed`, constrained permit, `EvaluationFailed`, and timeout, each with its expected outcome, and no context tenant in any decision.

## Pros and Cons of the Options

### Platform-level permission with unrestricted records

* Good, because startup needs no tenant and one component owns permission.
* Bad, because it departs from the tenant-scoped platform default.

### Platform tenant looked up from the tenant directory at startup

* Good, because rows and decisions carry a tenant, as the platform default expects.
* Bad, because startup fails until tenant bootstrap has finished, and the tenant has no business meaning here.

### Platform tenant looked up per request

* Good, because startup does not wait for the tenant directory.
* Bad, because every request gains a not-ready state, and a missing tenant shows up at run time.

### Platform tenant id taken from gear configuration

* Good, because startup needs no lookup.
* Bad, because the configured id can differ from the real tenant, unchecked.
