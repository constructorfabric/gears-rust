---
status: accepted
date: 2026-10-25
description: "Send each task's type and queue to the PDP, run all task work on the tenant plane, let platform workers cross tenant barriers only on platform-operated queues, have workers act as themselves while creator checks at creation prove the creator's own rights, and publish events under a dedicated, narrowly granted subject."
---

# Authorize Tasks by Type and Queue, Run Workers as Themselves, and Check Creator Authority at Creation

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
  - [Same, plus creator checks at creation](#same-plus-creator-checks-at-creation)
  - [Same, but workers act as the creator](#same-but-workers-act-as-the-creator)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-authorization-model`

## Context and Problem Statement

The engine stores tasks of many tenants. Workers are often shared platform services with broad rights. If a tenant can put a platform task type into a queue that such workers serve, the workers run tenant input with platform rights (confused deputy).

Limiting a worker to the task's tenant is not enough. Inside one tenant, the worker may still reach resources the task's creator cannot: a creator could name another user's document in the task input, and a worker with tenant-wide rights would act on it.

The engine also publishes lifecycle events for every tenant, including tenants behind a barrier. Someone has to be the producer of those events, with rights that are wide enough to publish and no wider.

The platform already provides:

- PDP constraints on properties the PEP declares, checked on create and applied to queries ([authorization DESIGN](../../../../docs/arch/authorization/DESIGN.md)).
- Permissions on a base type that cover all derived types, so a policy can grant one task type or a pattern ([PERMISSION_GTS_TYPE](../../../../docs/arch/authorization/PERMISSION_GTS_TYPE.md)).
- Two planes: tenant work uses the tenant plane, even for system actors with client credentials; the platform plane is only for work without a tenant and never reaches the PDP ([toolkit ADR-0008](../../../../docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md)).
- Tenant barriers, enforced for business data such as tasks ([TENANT_MODEL](../../../../docs/arch/authorization/TENANT_MODEL.md)).
- Event Broker publish checks: `produce` on the event type and subject type, and `produce` for the event's tenant (Event Broker DESIGN §3.3).
- No way to act on behalf of another subject.

How does the engine describe tasks to the PDP, which identity do workers use, how do they reach tenants behind a barrier, whose authority bounds what they do for a task, and who publishes the engine's events?

## Decision Drivers

* A broad `create` grant must not route work to privileged workers.
* A worker must not do, on a creator's behalf, what the creator itself may not do.
* Use the platform model as it is; add no new auth primitive.
* Keep tenant barriers meaningful.
* Keep PDP load low at heartbeat rate.
* Every identity the engine uses holds only the grants its job needs.

## Considered Options

* Base type only, tenant constraints only
* Type and queue in the request, platform workers on the platform plane
* Type and queue in the request, tenant plane for all task work, workers act as themselves
* Same, plus creator checks at creation
* Same, but workers act as the creator

## Decision Outcome

Chosen option: "Same, plus creator checks at creation" — type and queue in the request, tenant plane for all task work, workers act as themselves, and the creator's own rights on referenced resources are proven at creation. It closes the confused-deputy paths, across tenants and inside one, with the platform's existing tools.

How it works:

* **Resource**: one task → `resource.type` is its concrete type, with `owner_tenant_id`, `type_id`, `queue_id`, `owner_id`. Lists, claims, batch heartbeats, and bulk → the base type, with `type_id` and `queue_id` constraints applied in SQL.
* **Planes**: all task work uses the tenant plane. Platform workers, the engine's publishing subject, and its consuming subject are root-tenant service subjects (client credentials). The platform plane only registers the engine's GTS entities.
* **Barriers**: requests use `barrier_mode: respect` (`BarrierMode::Respect`, the platform default). A claim whose queues are all `operated_by: platform` uses `barrier_mode: ignore` (`BarrierMode::Ignore`); the PDP decides whether the caller may use it.
* **Acting for a task**: workers act as themselves. The claim returns the task's owner tenant, creator, and owner; downstream calls name the owner tenant as context tenant. The engine never stores or forwards the creator's token.
* **Creator authority**: each task type declares `authority.mode`.
  * `creator_verified` (default): the type lists creator checks — where a resource is referenced (resource references by type pattern, or a required string input field) and the action the work performs on it. At creation, before the create transaction, the engine asks the PDP with the **creator's own** security context (the create request carries it, so no impersonation is needed) whether each action is allowed on each referenced resource in the task's tenant. It accepts only results it can evaluate completely itself: an unconstrained allow, or constraints on the properties it sent (`owner_tenant_id`, `id`) that are decidable in memory or with the Tenant Resolver. A denial, an unreachable PDP, or a result only the resource's owning gear could evaluate rejects the create and stores nothing. Checked references are stored and published as `verified`; others as `verified = false`. Workers act downstream only on verified resources; the SDK makes that the default path.
  * `worker`: the work uses only the worker's authority. Allowed only for platform-owned types — work on no creator-chosen resource, or **gear-mediated creation**, where the owning gear authorizes the user with its own PEP and creates the task as its own service subject.
* **Event publishing**: a dedicated publishing subject publishes every event, never the request's caller. Each event names the task's owner tenant. The subject holds `produce` on the engine's topic, its event types, and the task and queue subject types, for every tenant that can own tasks (including tenants behind a barrier), and nothing else. The consuming subject for event waits is separate and holds only `consume`. A refused publish changes no task state and pages operators.
* **Event waits**: the engine reads waitable topics with platform rights, so setting or arming an event wait runs the Event Broker's subscribe checks with the caller's own context (`consume` on the topic, the event type, and the condition's always-known subject type), applies the returned constraints to every candidate event, refuses constraints it cannot apply, and caps the wait's lifetime (`event_waits.max_wait_secs`) because the checks cannot be repeated (DESIGN §3.2 *Event wait authorization*).
* **Revocation**: every worker request checks `claim` again (a batch heartbeat once). A denial releases the task without budget, attempt, or blacklist change and returns `403`.
* **Structural guards**: tenant-owned types never enter `operated_by: platform` queues and never use `authority.mode = worker`; a queue may limit itself with `accepted_types`.

### Consequences

* Operators can grant `create`, `claim`, and `read` per task type and per queue.
* Workers serving tenants behind a barrier need a platform-operated queue and PDP consent, or must run inside those tenants.
* Downstream gears authorize the worker's own subject for the task's tenant; there is no acting as the creator until AuthN offers it. Creator checks bound what the worker may do for the task to what the creator was allowed to do at creation.
* Creator checks run once, at creation: the engine holds no creator credential later, so a right revoked afterwards is not seen by existing tasks. Accepted; types that need a short window keep their lifetime timeout short.
* A PDP result that only the resource's owning gear can evaluate makes creation fail closed. Such work uses gear-mediated creation, which puts the decision where the data is.
* Creation of a checked type costs one PDP call per checked reference, run concurrently and bounded by `trust_limits.max_creator_checks` and `max_resource_refs_per_task`; unchecked types cost nothing extra.
* Losing `claim` stops a worker at its next call.
* Publishing into tenants behind a barrier depends on the broker's tenant check allowing the publishing subject there by policy; the open question is tracked in DESIGN §3.2 Event Producer.

### Confirmation

* Tests check that a subject allowed only type A cannot create type B, and that a worker allowed only queue Q gets no task from another queue.
* Tests check that a tenant-owned type is rejected on an `operated_by: platform` queue, and that a tenant-owned type declaring `authority.mode = worker` cannot be created.
* Tests check that a platform worker gets no task behind a barrier from other queues.
* Tests check that a creator without the checked action on a referenced resource gets `403` and no task is stored, even when the worker has the right; that a PDP result with constraints the engine cannot evaluate is refused; and that unchecked references are stored and published with `verified = false`.
* Tests check that every published event carries the task's owner tenant and is produced by the publishing subject, including for a tenant behind a barrier, and that with the publishing grant removed task operations still succeed, the history stays complete, and an alert fires.
* Tests check that after `claim` is revoked the next worker call returns `403` and the task is queued again with no budget change.
* Tests check that an event wait is refused without `consume` on its topic, event type, or subject type; that a wait set under a constrained `consume` grant never matches an event outside the constraint; and that a grant with constraints the engine cannot evaluate refuses the wait.
* Review checks that no task path uses `PlatformSecurityContext`, and that the publishing and consuming subjects are never used to authorize a task operation.

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
* Bad, because downstream gears see the worker, not the creator, so inside a tenant a creator can steer a worker to resources the creator cannot reach.

### Same, plus creator checks at creation

* Good, because it closes the confused-deputy path inside a tenant with the platform's own PDP and the creator's own context, with no new auth primitive and no stored credential.
* Good, because types and the gears that register them say exactly which resources the work touches and with which action.
* Good, because results the engine cannot evaluate fail closed, and gear-mediated creation gives such work a correct path.
* Neutral, because it adds PDP calls at creation, only for types that declare checks.
* Bad, because rights revoked after creation are not re-checked.
* Bad, because the engine cannot see downstream calls, so "act only on verified resources" is a worker obligation that the SDK supports but cannot enforce.

### Same, but workers act as the creator

* Good, because downstream gears see the creator's rights.
* Bad, because the engine would store tokens that expire and are credentials at rest.
* Bad, because the platform has no token exchange for this today.

## More Information

Durable Execution follows the same rule: "Worker identity does not replace the original execution owner." Reconsider the "act as themselves" part when AuthN offers token exchange; creator checks would then become a first line in front of delegated calls rather than the only proof of the creator's rights.

See DESIGN §3.3 Authorization Model and Creator authority, §3.2 Event Producer (publishing identity), and §4.6 Task Creation Admission (step 0).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-resource-path-access` — type and queue constraints.
* `cpt-cf-task-engine-fr-worker-principals` — planes, barriers, acting for a task, creator authority, revocation.
* `cpt-cf-task-engine-fr-resource-refs` — `verified` references.
* `cpt-cf-task-engine-fr-type-trust` — tenant-owned types kept out of platform queues and out of `worker` mode.
* `cpt-cf-task-engine-fr-event-publishing` — the publishing subject and its grants.
* `cpt-cf-task-engine-fr-authz` — PDP request shape.
* `cpt-cf-task-engine-design-authz-model` — DESIGN §3.3 Authorization Model.
* `cpt-cf-task-engine-design-creator-authority` — DESIGN §3.3 Creator authority.
