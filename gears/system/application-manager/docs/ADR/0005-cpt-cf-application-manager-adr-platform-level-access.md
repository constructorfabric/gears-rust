---
status: accepted
date: 2026-10-20
description: Operation records and audit ledger rows are unrestricted entities with no tenant column, and the PDP decides every gear-level action as a platform-level permission with no context tenant
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
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-platform-level-access`

## Context and Problem Statement

The gear manages applications installed into the whole environment, so its records belong to the platform, not to a tenant. The platform default for gears is tenant-scoped entities and tenant-scoped PDP decisions. The gear must decide how it fits that default. Other platform-global services already use the PDP for this: usage-collector gates its platform-global catalog writes through it. Should the gear scope its records and decisions to a tenant for the platform, or keep its records unrestricted and let the PDP decide platform-level permission?

## Decision Drivers

* Startup should depend only on services that the host starts before the gear, so a first start of the platform succeeds without waiting for data that other services create.
* Startup failures must surface at deploy time, with no retry (`cpt-cf-application-manager-fr-startup-checks`).
* Permission decisions must have exactly one owner, the PDP (`cpt-cf-application-manager-fr-authorization`).
* A platform-wide gear must keep its records as one platform-wide set (`cpt-cf-application-manager-nfr-platform-scope`).

## Considered Options

* Platform-level permission with unrestricted records
* Platform tenant looked up from the tenant directory at startup
* Platform tenant looked up per request
* Platform tenant id taken from gear configuration

## Decision Outcome

Chosen option: "Platform-level permission with unrestricted records", because startup depends only on the types registry and the authz resolver, which the host starts before the gear, and it leaves platform-level permission to the one component that owns permission decisions.

The decision has these parts:

* **Unrestricted records.** Operation records and audit ledger rows are unrestricted secure-ORM entities with no `owner_tenant_id` and no other tenant column.
* **PDP decision per action.** Every gear-level action (seed, uninstall, read_schema, read_operation) is a PDP decision requested with no context tenant and no owner tenant. It is a platform-level permission.
* **Decision content.** The decision for seed and uninstall carries `application_id` when the id is within the limits.
* **Decision handling.** A permit with no constraints allows the action. A deny or `CompileFailed` refuses it as not permitted. A permit with any constraint fails closed as not permitted. `EvaluationFailed`, or no decision within `pdp_call_timeout`, means access control cannot answer, and the request ends as temporarily unavailable.
* **Startup.** Startup depends only on the types registry and the authz resolver (`deps = [types_registry, authz_resolver]`).

Decision scope: this ADR covers the gear-level PDP decisions and the scoping of operation records and audit ledger rows. Plugin-level permission, where a section plugin refuses a call with `NotPermitted`, is a separate mechanism, set in [ADR 0002](./0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md) for the `schema` call and in [DESIGN §3.3](../DESIGN.md#33-api-contracts).

### Consequences

* A startup failure comes only from the gear's own checks and exits at deploy time without a retry.
* With no context tenant, the PDP decides by its own rules, so each of the four actions needs an explicit platform-level grant from policy authors.
* The content of a policy and its correctness belong to the authz resolver and its plugin. The gear enforces the decision it receives: a deny, `CompileFailed` or a permit with constraints gives not permitted, and `EvaluationFailed` or a PDP timeout gives temporarily unavailable.
* Operation records are reached only after a permit, and the gear's repository code applies no tenant filter.
* If per-tenant installs enter scope, the entity scoping and every decision must be redefined.

### Confirmation

* Inspection of the entity declarations: operation records and audit ledger rows are unrestricted and have no tenant column.
* Tests that PDP decisions are requested with no context tenant.
* A table-driven test with a PDP double over a permit, a deny, `CompileFailed`, a permit with constraints, `EvaluationFailed` and a timeout shows the expected outcome of each: the action proceeds, not permitted for the next three, and temporarily unavailable for the last two.
* Tests that a refused decision reaches no operation record.

## Pros and Cons of the Options

### Platform-level permission with unrestricted records

Store operation records and audit ledger rows unscoped, and ask the PDP for every action without a tenant.

* Good, because startup depends only on the types registry and the authz resolver, so it needs no retry.
* Good, because one component owns permission decisions.
* Good, because it matches how other platform-global services use the PDP.
* Bad, because policy authors must grant platform-level permission explicitly.
* Bad, because it departs from the platform default of tenant-scoped entities, so it needs this record.

### Platform tenant looked up from the tenant directory at startup

Look the platform tenant up at startup, fail the start at once with no retry when the lookup fails, and scope rows and decisions to it.

* Good, because rows and decisions carry a real tenant, as the platform default expects.
* Good, because a missing tenant shows up at deploy time.
* Bad, because startup depends on the tenant directory being ready at deploy, so a first start before tenant bootstrap has finished fails.
* Bad, because records and decisions carry a tenant with no business meaning here.

### Platform tenant looked up per request

Look the platform tenant up on first use and refuse requests as not ready until it exists.

* Good, because startup does not wait for the tenant directory.
* Bad, because every request path gains a not-ready state and a tenant-resolver dependency.
* Bad, because a missing tenant shows up at run time, not at deploy time.

### Platform tenant id taken from gear configuration

Configure the platform tenant id and use it for scoping.

* Good, because startup needs no lookup and has no start-order dependency.
* Bad, because the configured id can differ from the real tenant, and nothing checks it at startup.
* Bad, because records and decisions carry a tenant with no business meaning here, and the configured id is a second source of truth for that tenant.

## More Information

Revisit this decision if per-tenant installs enter scope. Related decisions:

* [ADR 0003](./0003-cpt-cf-application-manager-adr-async-operations.md): the gear-level PDP decision is part of the bounded synchronous phase.
* [ADR 0004](./0004-cpt-cf-application-manager-adr-audit-ledger.md): audit ledger rows are among the unrestricted records.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-application-manager-fr-authorization` — every action is a PDP decision with no context tenant; a permit with constraints fails closed.
* `cpt-cf-application-manager-nfr-platform-scope` — operation records and audit ledger rows are one platform-wide set with no tenant scoping.
* `cpt-cf-application-manager-fr-startup-checks` — startup depends only on the types registry and the authz resolver, and a failure exits with no retry.
* `cpt-cf-application-manager-component-operation-repository` — the repository holds unrestricted entities and applies no tenant filter.
* `cpt-cf-application-manager-nfr-authorized-before-plugin` — the gear-level PDP decision comes before any section plugin call.
* `cpt-cf-application-manager-constraint-platform-auth` — every gear-level decision goes through PolicyEnforcer to the PDP, with no context tenant.
* `cpt-cf-application-manager-constraint-secure-orm` — operation records and audit ledger rows are unrestricted secure-ORM entities with no tenant column.
* `cpt-cf-application-manager-principle-minimal-state` — operation records and audit ledger rows carry no tenant, only the fields the gear needs.
