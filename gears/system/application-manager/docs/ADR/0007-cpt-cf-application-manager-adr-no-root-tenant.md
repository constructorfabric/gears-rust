---
status: accepted
date: 2026-10-15
description: The gear uses no root tenant; its operation records are unrestricted, and the PDP decides platform-level permission with no context tenant
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Use No Root Tenant; the PDP Decides Platform-Level Permission


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [No root tenant: unrestricted entities, PDP decides with no context tenant](#no-root-tenant-unrestricted-entities-pdp-decides-with-no-context-tenant)
  - [Resolve the root tenant at startup with a bounded retry](#resolve-the-root-tenant-at-startup-with-a-bounded-retry)
  - [Resolve the root tenant lazily per request](#resolve-the-root-tenant-lazily-per-request)
  - [Root tenant id set in gear configuration](#root-tenant-id-set-in-gear-configuration)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-no-root-tenant`

## Context and Problem Statement

The operation records are platform-wide: they belong to the environment, not to a tenant. Scoping them to the platform root tenant would require the gear to look the root tenant up through the tenant resolver at startup. The tenant directory creates that tenant in its own start phase, so the lookup can run before the tenant exists. That causes first-start races and needs a startup retry loop. Deciding who may act at platform level is the job of the authorization plugin, as in other services; for example, usage-collector gates its platform-global catalog writes through the PDP. Should the gear carry a root tenant at all, or should the PDP alone decide platform-level permission?

## Decision Drivers

* The gear must have no startup dependency on tenant bootstrap.
* Startup failures must surface at deploy time, with no retry (`cpt-cf-application-manager-fr-startup-checks`).
* Permission decisions must have exactly one owner, the PDP (`cpt-cf-application-manager-fr-authorization`).
* A platform-wide gear must not carry a tenant concept (`cpt-cf-application-manager-nfr-platform-scope`).

## Considered Options

* No root tenant: unrestricted entities, PDP decides with no context tenant
* Resolve the root tenant at startup with a bounded retry
* Resolve the root tenant lazily per request
* Root tenant id set in gear configuration

## Decision Outcome

Chosen option: "No root tenant: unrestricted entities, PDP decides with no context tenant", because it removes the tenant-resolver dependency and the start-order problem, and leaves platform-level permission to the one component that owns permission decisions.

The decision has these parts:

* **Unrestricted entities.** The operation records are unrestricted secure-ORM entities with no `owner_tenant_id` and no other tenant column.
* **PDP decision per action.** Every action is a PDP decision with no context tenant and no owner tenant. The decision carries `application_id` where the action concerns one application.
* **Decision handling.** A permit with no constraints allows the action. A deny refuses it. A permit with any constraint fails closed and the request ends as not permitted.
* **Startup.** Startup makes no tenant lookup and has no retry.

### Consequences

* Good, because the gear has no tenant-resolver dependency and no first-start race.
* Good, because startup is simpler: a failure exits at deploy time and is not retried.
* Bad, because policy authors must grant the platform-level permission explicitly. A decision without a context tenant falls back to the caller's own tenant, so a policy written for tenant scope does not match by accident.
* Bad, because a misconfigured policy fails closed: the caller gets not permitted, not an unintended permit.

### Confirmation

* Inspection of the entity scoping declaration: the operation records are declared unrestricted and have no tenant column.
* Tests that PDP decisions are requested with no context tenant.
* A test that a permit carrying constraints is refused as not permitted.

## Pros and Cons of the Options

### No root tenant: unrestricted entities, PDP decides with no context tenant

Store operation records unscoped and ask the PDP for every action without a tenant.

* Good, because startup needs no tenant lookup, so no race and no retry.
* Good, because one component owns permission decisions.
* Good, because the gear matches how other platform-global services use the PDP.
* Bad, because policy authors must grant platform-level permission explicitly.

### Resolve the root tenant at startup with a bounded retry

Look the root tenant up at startup, retry for a bounded time, and scope rows and decisions to it.

* Good, because rows and decisions carry a real tenant.
* Bad, because it depends on the tenant directory's start order and the retry only hides the race.
* Bad, because a slow tenant bootstrap turns into a startup failure after the retry budget.
* Bad, because it puts a tenant concept into a platform-wide gear.

### Resolve the root tenant lazily per request

Look the root tenant up on first use and refuse requests as not ready until it exists.

* Good, because startup does not wait for the tenant directory.
* Bad, because every request path gains a not-ready state and a tenant-resolver dependency.
* Bad, because a missing tenant shows up at run time, not at deploy time.

### Root tenant id set in gear configuration

Configure the root tenant id and use it for scoping.

* Good, because there is no lookup and no start-order dependency.
* Bad, because the configured id can differ from the real root tenant, and nothing checks it at startup.
* Bad, because it still gives a platform-wide gear a tenant concept and a second source of truth for the root tenant.

## More Information

Review and supersession: revisit this decision if per-tenant installs enter scope or if the platform defines a single root tenant that is guaranteed to exist before this gear starts.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-authorization` — every action is a PDP decision with no context tenant; a permit with constraints fails closed.
* `cpt-cf-application-manager-nfr-platform-scope` — operation records are one platform-wide set with no tenant scoping.
* `cpt-cf-application-manager-fr-startup-checks` — startup makes no tenant lookup and has no retry.
