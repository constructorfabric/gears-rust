---
status: accepted
date: 2026-10-27
description: Seed, uninstall, schema reads, and operation reads run on the platform plane and are granted by allowed callers in configuration; history reads and the application list are tenant-free PDP decisions; all records are unrestricted with no tenant column
---
<!-- Created: 2026-10-05 by Constructor Tech -->
# Allowed Callers for Seeding Actions, Platform Access Control for Operator Reads, Unrestricted Records

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Allowed callers for seeding actions, PDP for operator reads](#allowed-callers-for-seeding-actions-pdp-for-operator-reads)
  - [Tenant-free PDP decisions for every action](#tenant-free-pdp-decisions-for-every-action)
  - [Allowed callers for every action](#allowed-callers-for-every-action)
  - [A platform tenant from the tenant directory or from configuration](#a-platform-tenant-from-the-tenant-directory-or-from-configuration)

<!-- /toc -->

**ID**: `cpt-cf-application-manager-adr-platform-level-access`

## Context and Problem Statement

The gear manages applications installed into the whole environment, so its records belong to the platform, not to a tenant. Seeding callers are deploy jobs and other platform workloads, while operators read history and the application list with their own login. The platform separates tenant-scoped work, which uses a tenant `SecurityContext` and the PDP, from platform work, which uses a `PlatformSecurityContext` that the PDP never evaluates ([two-plane auth ADR](../../../../../docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md)). Which plane does each action use, who grants it, and how are the records scoped?

## Decision Drivers

* `cpt-cf-application-manager-fr-authorization`, `cpt-cf-application-manager-fr-authentication`
* `cpt-cf-application-manager-nfr-platform-scope`, `cpt-cf-application-manager-nfr-authorized-before-plugin`
* `cpt-cf-application-manager-fr-list-applications`, `cpt-cf-application-manager-fr-history-read`
* [Two-plane auth ADR](../../../../../docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md)

## Considered Options

* Allowed callers for seeding actions, PDP for operator reads
* Tenant-free PDP decisions for every action
* Allowed callers for every action
* A platform tenant from the tenant directory or from configuration

## Decision Outcome

Chosen option: "Allowed callers for seeding actions, PDP for operator reads", because seeding is platform work done by known workloads, it then needs no tenant and no PDP, and operators keep the access model they use for every other read.

* Seed, uninstall, the discovery reads, and the operation read are platform-plane routes. The platform validates the caller's workload credential and gives the gear its platform identity.
* The `allowed_callers` setting lists platform identities, each with its actions from `seed`, `uninstall`, `read_schema`, and `read_operation`. A Kubernetes service account matches by namespace and service-account name, ignoring the pod; a SPIFFE identity matches by trust domain and name, ignoring the version. Any other identity kind never matches.
* `init` rejects an entry with no action or an unknown action, and a duplicate identity. An empty list starts and allows nobody.
* The check is in memory and cannot be unavailable. A caller not allowed for the action is refused as not permitted before any plugin call.
* The history read and the application list are tenant-plane routes, decided by the PDP as platform-level permissions with no context tenant; decision handling is in [DESIGN §3.3](../DESIGN.md#33-api-contracts).
* Operation records, history rows, and application records are unrestricted secure-ORM entities with no tenant column.
* Section plugins receive the caller's platform identity; plugin-level `NotPermitted` is a separate mechanism outside this decision.

### Consequences

* Grants for seeding live in versioned configuration and change with a restart; the PDP holds only the two operator-read grants.
* Seeding, discovery, and operation reads do not depend on the authz resolver, and never end as unavailable for access control.
* An operator who seeds by hand needs a platform credential of an allowed identity.
* While platforms move from service-account tokens to SPIFFE, the allowed callers list each caller in both forms.
* A plugin's own permission check is its own rule on the platform identity, since the PDP never evaluates it.
* The repository applies no tenant filter, and records are reached only through the gear's routes after a permit.

### Confirmation

* A table-driven test over identity kinds and actions: a match, a wrong namespace or trust domain, a different pod or version still matching, an action not listed, and an identity kind that never matches.
* No plugin call for a caller not allowed for the action, and no PDP call on a platform-plane route.
* A PDP double for the history read and the application list over permit, deny, `CompileFailed`, constrained permit, `EvaluationFailed`, and timeout, each with its expected outcome and no context tenant.
* Inspection: the three entities are unrestricted with no tenant column.

## Pros and Cons of the Options

### Allowed callers for seeding actions, PDP for operator reads

* Good, because each action uses the plane that fits it, and seeding needs no tenant, no PDP, and no synthetic context.
* Good, because the list of workloads that may seed is short and reviewed with the deployment.
* Bad, because grants live in two places, and changing a seeding grant needs a restart.

### Tenant-free PDP decisions for every action

* Good, because one component owns every grant, and grants change without a restart.
* Bad, because seeding callers must hold a tenant-plane token although the work has no tenant, and every seed depends on the authz resolver.

### Allowed callers for every action

* Good, because the gear has no PDP dependency at all.
* Bad, because operators would need platform credentials to read history or list applications, and operator access would bypass the platform's access policy.

### A platform tenant from the tenant directory or from configuration

* Good, because rows and decisions carry a tenant, as the platform default expects.
* Bad, because the tenant has no business meaning here, and a lookup ties startup to tenant bootstrap while a configured id can differ from the real tenant, unchecked.
