---
status: proposed
date: 2026-10-24
description: "Send each task's type and queue to the PDP, run all task work on the tenant plane, let platform workers cross tenant barriers only on platform-operated queues, and have workers act as themselves."
---

# Authorize Tasks by Type and Queue, and Run Workers as Themselves

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Base type only, tenant constraints only](#base-type-only-tenant-constraints-only)
  - [Type and queue in the request, platform workers on the platform plane](#type-and-queue-in-the-request-platform-workers-on-the-platform-plane)
  - [Type and queue in the request, tenant plane for all task work, workers act as themselves](#type-and-queue-in-the-request-tenant-plane-for-all-task-work-workers-act-as-themselves)
  - [Same, but workers act as the creator](#same-but-workers-act-as-the-creator)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-authorization-model`

## Context and Problem Statement

The engine stores tasks of many tenants. Workers are often shared platform services with broad rights. If a tenant can put a platform task type into a queue that such workers serve, the workers run tenant input with platform rights (confused deputy).

The platform already provides:

- PDP constraints on properties the PEP declares, checked on create and applied to queries ([authorization DESIGN](../../../../docs/arch/authorization/DESIGN.md)).
- Permissions on a base type that cover all derived types, so a policy can grant one task type or a pattern ([PERMISSION_GTS_TYPE](../../../../docs/arch/authorization/PERMISSION_GTS_TYPE.md)).
- Two planes: tenant work uses the tenant plane, even for system actors with client credentials; the platform plane is only for work without a tenant and never reaches the PDP ([toolkit ADR-0008](../../../../docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md)).
- Tenant barriers, enforced for business data such as tasks ([TENANT_MODEL](../../../../docs/arch/authorization/TENANT_MODEL.md)).
- No way to act on behalf of another subject.

How does the engine describe tasks to the PDP, which identity do workers use, how do they reach tenants behind a barrier, and whose authority do they use when they work on a task?

## Decision Drivers

* A broad `create` grant must not route work to privileged workers.
* Use the platform model as it is; add no new auth primitive.
* Keep tenant barriers meaningful.
* Keep PDP load low at heartbeat rate.

## Considered Options

* Base type only, tenant constraints only
* Type and queue in the request, platform workers on the platform plane
* Type and queue in the request, tenant plane for all task work, workers act as themselves
* Same, but workers act as the creator

## Decision Outcome

Chosen option: "Type and queue in the request, tenant plane for all task work, workers act as themselves", because it closes the confused-deputy path with the platform's existing tools.

How it works:

* **Resource**: one task → `resource.type` is its concrete type, with `owner_tenant_id`, `type_id`, `queue_id`, `owner_id`. Lists, claims, batch heartbeats, and bulk → the base type, with `type_id` and `queue_id` constraints applied in SQL.
* **Planes**: all task work uses the tenant plane. Platform workers and the engine's broker subscription are root-tenant service subjects (client credentials). The platform plane only registers the engine's GTS entities.
* **Barriers**: requests use `barrier_mode: all`. Claims on queues with `operated_by: platform` use `barrier_mode: none`, if the PDP allows it.
* **Acting for a task**: workers act as themselves. The claim returns the task's owner tenant, creator, and owner; downstream calls name the owner tenant as context tenant. The engine never stores or forwards the creator's token.
* **Revocation**: every worker request checks `claim` again (a batch heartbeat once). A denial releases the task without budget, attempt, or blacklist change and returns `403`.
* **Structural guards**: tenant-owned types never enter `operated_by: platform` queues; a queue may limit itself with `accepted_types`.

### Consequences

* Operators can grant `create`, `claim`, and `read` per task type and per queue.
* Workers serving tenants behind a barrier need a platform-operated queue and PDP consent, or must run inside those tenants.
* Downstream gears authorize the worker's own subject for the task's tenant; there is no acting as the creator until AuthN offers it.
* Losing `claim` stops a worker at its next call.

### Confirmation

* Tests check that a subject allowed only type A cannot create type B, and that a worker allowed only queue Q gets no task from another queue.
* Tests check that a tenant-owned type is rejected on an `operated_by: platform` queue.
* Tests check that a platform worker gets no task behind a barrier from other queues.
* Tests check that after `claim` is revoked the next worker call returns `403` and the task is queued again with no budget change.
* Review checks that no task path uses `PlatformSecurityContext`.

## Pros and Cons of the Options

### Base type only, tenant constraints only

* Good, because it is the simplest.
* Bad, because policies cannot tell types or queues apart, so the confused-deputy path stays open.

### Type and queue in the request, platform workers on the platform plane

* Good, because platform workers need no tenant identity.
* Bad, because the platform plane never reaches the PDP, so platform workers would act on tenant data without any check (against toolkit ADR-0008).

### Type and queue in the request, tenant plane for all task work, workers act as themselves

* Good, because it uses only existing platform tools.
* Good, because every action is checked for the task's tenant.
* Bad, because downstream gears see the worker, not the creator.

### Same, but workers act as the creator

* Good, because downstream gears see the creator's rights.
* Bad, because the engine would store tokens that expire and are credentials at rest.
* Bad, because the platform has no token exchange for this today.

## More Information

Durable Execution follows the same rule: "Worker identity does not replace the original execution owner." Reconsider the "act as themselves" part when AuthN offers token exchange.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-resource-path-access` — type and queue constraints.
* `cpt-cf-task-engine-fr-worker-principals` — planes, barriers, acting for a task, revocation.
* `cpt-cf-task-engine-fr-type-trust` — tenant-owned types kept out of platform queues.
* `cpt-cf-task-engine-fr-authz` — PDP request shape.
* `cpt-cf-task-engine-design-authz-model` — DESIGN §3.3 Authorization Model.
