---
description: "Requirements that Model Registry, Task Engine, Orders Workflow, and the BSS gears place on the Approval Service gear."
---

# UPSTREAM_REQS — Approval Service

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting Gears](#12-requesting-gears)
- [2. Requirements](#2-requirements)
  - [2.1 Model Registry](#21-model-registry)
  - [2.2 Task Engine](#22-task-engine)
  - [2.3 Orders Workflow](#23-orders-workflow)
  - [2.4 Pricing and Products](#24-pricing-and-products)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)
  - [Model Registry Sources](#model-registry-sources)
  - [Task Engine Sources](#task-engine-sources)
  - [BSS Sources](#bss-sources)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

A centralized approval service is needed to manage approval decisions for gear operations on shared and business resources (AI models, tasks, orders, etc.). Multiple gears currently implement ad-hoc approval logic independently, leading to inconsistent behavior and duplicated code.

### 1.2 Requesting Gears

| Gear | Why it needs this gear |
|------|------------------------|
| model-registry | Needs tenant-scoped approval checks before serving AI models; requires approval lifecycle management, bulk operations, auto-approval rules, tenant hierarchy support, and approval-filtered listing |
| task-engine | Does not implement approvals itself; a task waits for an approval through an event wait on this gear's request events |
| bss/orders-workflow | Obtains the approval-requirement verdict for submitted orders, opens approval requests per gate, escalates stalled gates, and reflects decisions into Orders Lifecycle (§9.2 expectations contract) |
| bss/orders-changes | Same approval path as acquisitions, through Orders Workflow, evaluated against the delta TCV; no separate asks |
| bss/pricing, bss/products | Embed `cf-gears-bss-approval` today; listed asks are preconditions for a future replacement of the library |
| bss/contracts | Contract signature approval is out of scope until this gear exists; no asks yet |

## 2. Requirements

### 2.1 Model Registry

#### Register Approvable Resource

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-register-resource`

The future gear **MUST** allow registering a resource as approvable within the approval workflow, associating it with a tenant context and resource-specific metadata.

- **Rationale**: Model Registry needs to onboard new AI models into the approval workflow so tenants can control which models are available.

#### Query Approval Status

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-query-status`

The future gear **MUST** provide a synchronous check of the current approval status for a resource in a tenant context, resolving status through the tenant hierarchy.

- **Rationale**: Model Registry must reject requests for unapproved models at query time; status resolution must consider tenant hierarchy (child inherits parent approvals).

#### Batch Approval Status

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-batch-status`

The future gear **MUST** return the approval status of a bounded list of resources for one tenant context in a single call.

- **Rationale**: Model Registry lists exactly the models that would resolve, with OData filtering and paging; one call per model does not scale.
- **Source**: `cpt-cf-model-registry-fr-list-available-models`

#### Approval Status Changed Notification

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-status-changed-event`

The future gear **MUST** emit a notification when an approval status changes so that dependent gears can invalidate caches.

- **Rationale**: Model Registry caches approval decisions for performance; stale cache entries cause incorrect model serving.

#### Bulk Approval

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-bulk-approve`

The future gear **MUST** support approving multiple resources for a tenant in a single atomic operation, emitting individual status change notifications per resource.

- **Rationale**: Tenant onboarding requires approving a predefined set of models; doing this one-by-one is error-prone and slow. Per-resource notifications are required for cache invalidation.

#### Auto-Approval Rules

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-auto-approval`

The future gear **MUST** support resource-type-specific auto-approval rules evaluated against resource metadata, with tenant-scoped rule hierarchy where child tenants can only restrict (not expand) parent rules.

- **Rationale**: Manual approval of every model per tenant does not scale; auto-approval rules allow tenants to define policies while maintaining platform-level safety constraints.

#### Tenant Hierarchy Support

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-tenant-hierarchy`

The future gear **MUST** support additive inheritance of approvals through the tenant tree, where child tenants see parent approvals plus their own, with "restrict only" semantics (child cannot approve what parent blocked).

- **Rationale**: Multi-level tenant hierarchies require consistent approval propagation; without hierarchy support, each tenant must manually replicate parent approvals.

#### Approval Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-lifecycle`

The future gear **MUST** support a full approval lifecycle with states (pending, approved, rejected, revoked) and valid transitions between them, including re-approval of rejected and revoked resources.

- **Rationale**: Model Registry requires granular control over model availability; simple approve/reject is insufficient for operational scenarios like temporary revocation.

#### Decisions Through the Requesting Gear

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-on-behalf-decision`

The future gear **MUST** let a requesting gear record and revoke decisions on behalf of the administrator who acts in that gear, recording both identities.

- **Rationale**: From its P2, Model Registry keeps its create and update operations, which may set the approval state, but routes those state changes to this gear.
- **Source**: `cpt-cf-model-registry-fr-model-approval`; Model Registry SDK `update_model` (P2 routing)

#### Audit Trail

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-audit-trail`

The future gear **MUST** record all approval decisions with actor, timestamp, tenant context, previous and new status.

- **Rationale**: Compliance and operational debugging require a complete history of who approved/rejected/revoked which resources and when.

#### Stakeholder Notifications

- [ ] `p3` - **ID**: `cpt-cf-approval-service-upreq-stakeholder-notifications`

The future gear **MUST** notify relevant stakeholders on approval status changes (approvers on new pending requests, requesters on decisions, affected users on revocations).

- **Rationale**: Without notifications, pending approvals can go unnoticed and revocations can surprise users.

### 2.2 Task Engine

#### Request Events Usable as Event Waits

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-request-event-wait`

The future gear **MUST** publish request events that a Task Engine event wait can match: a GTS event type, a subject type the caller can be granted `consume` on, the request ID as subject ID, and publication in the tenant of the waiting task. Every way a request closes **MUST** produce an event.

- **Rationale**: Task Engine has no approval wait kind; a task arms an event wait on its approval request. Events match only tasks in the event's own tenant, and a request that closes silently leaves a task waiting until its timeout.
- **Source**: `cpt-cf-task-engine-fr-wait-event` (§4.3 Future Scope: approval wait kind not planned)

#### Read Request Outcome by ID

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-request-read`

The future gear **MUST** let the requester and the gear that opened a request read its current outcome by request ID.

- **Rationale**: Task Engine stores only the event header; the resumed worker reads the outcome from the source with its own permissions.
- **Source**: `cpt-cf-task-engine-fr-wait-event` (header only)

### 2.3 Orders Workflow

Orders Workflow's §9.2 expectations contract (`cpt-cf-bss-orders-workflow-contract-owf-approval-contract`) is registered here. Orders Changes uses the same path through Orders Workflow, with the delta TCV as context.

#### Approval-Requirement Verdict

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-requirement-verdict`

The future gear **MUST** answer whether an order version requires approval, keyed on order ID and order version, cacheable per version, and fail closed when it cannot answer. A newer answer **MUST** be distinguishable from an older one.

- **Rationale**: Workflow reflects `submitted → pending_approval` or `submitted → approved` and treats a disagreeing later answer as stale.
- **Source**: contract items (b), (f); `cpt-cf-bss-orders-workflow-fr-owf-approval-request`

#### Approval Request with Context and Correlation

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-approval-request`

The future gear **MUST** accept an approval request per gate carrying order context (including the TCV figure), a gate identifier, and the process correlation ID, and **MUST** be idempotent: a retry opens no second request and notifies no approver twice.

- **Rationale**: Long-running workflow retries must not open duplicate gates.
- **Source**: contract items (a), (e); `cpt-cf-bss-orders-workflow-fr-owf-approval-idempotency`

#### Decision Outcome

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-decision-outcome`

The future gear **MUST** deliver the outcome of a request (approved or rejected) with the decision reasons and its deciding authority, echoing the correlation ID, as an event.

- **Rationale**: Workflow reflects the outcome into Orders Lifecycle, which records every verdict with its deciding authority.
- **Source**: contract item (d); `cpt-cf-bss-orders-workflow-fr-owf-approval-decision`; Orders Lifecycle D-73

#### Cancel Gates of a Superseded Version

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-gate-cancel`

The future gear **MUST** let the requesting gear close the open requests of an order version that an amendment superseded.

- **Rationale**: On `OrderAmended`, Workflow cancels the prior version's gates and opens new ones.
- **Source**: `cpt-cf-bss-orders-workflow-fr-owf-approval-request`

#### Multi-Party Gates

- [ ] `p1` - **ID**: `cpt-cf-approval-service-upreq-multi-party-gates`

The future gear **MUST** support parallel approval by several parties, all of which must approve before the order is approved.

- **Rationale**: Commercial acquisitions need financial, legal, or partner sign-off.
- **Source**: contract item (b); Orders Workflow AC 1

#### Staged Gates

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-staged-gates`

The future gear **MUST** support sequential approval, where a party decides only after the previous party approved, as defined by its routing configuration.

- **Rationale**: Some gates, such as legal after finance, must be ordered.
- **Source**: `cpt-cf-bss-orders-workflow-fr-owf-approval-request`

#### Threshold Evaluation

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-threshold-evaluation`

The future gear **MUST** evaluate approval requirements and approver routing against request context, such as the TCV.

- **Rationale**: Whether and by whom an order is approved depends on its value.
- **Source**: contract item (b)

#### Escalation Command

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-escalation`

The future gear **MUST** accept an escalation command for an open request and route it to the configured escalation path, without owning escalation timers or auto-rejecting the request.

- **Rationale**: Workflow owns the durable timers (default 72 hours); this gear owns who is escalated to.
- **Source**: contract item (c); `cpt-cf-bss-orders-workflow-fr-owf-approval-escalation`

Orders Workflow marks its contract `p1`, but its multi-party, escalation, and inbox paths stay inert behind a stand-in until this gear exists (Orders Workflow §15), so staged gates, thresholds, and escalation are requested at `p2`.

### 2.4 Pricing and Products

#### BSS Approval Library Parity

- [ ] `p3` - **ID**: `cpt-cf-approval-service-upreq-bss-library-parity`

Before it can replace `cf-gears-bss-approval`, the future gear **MUST** support multi-item approval units, exclusion of the submitter and every item author, quorum fixed at submission, re-vote after business-content drift, and a feed for the BSS Approvals Inbox (`ApprovalSourceV1`).

- **Rationale**: Pricing and Products decide approval units inside their own transactions today; replacement is future scope.
- **Source**: `gears/bss/libs/approval/README.md`; `cpt-cf-bss-approvals-fr-one-inbox`

## 3. Priorities

| Priority | Requirements |
|----------|-------------|
| p1 (critical) | `cpt-cf-approval-service-upreq-register-resource`, `cpt-cf-approval-service-upreq-query-status`, `cpt-cf-approval-service-upreq-batch-status`, `cpt-cf-approval-service-upreq-status-changed-event`, `cpt-cf-approval-service-upreq-tenant-hierarchy`, `cpt-cf-approval-service-upreq-lifecycle`, `cpt-cf-approval-service-upreq-requirement-verdict`, `cpt-cf-approval-service-upreq-approval-request`, `cpt-cf-approval-service-upreq-decision-outcome`, `cpt-cf-approval-service-upreq-gate-cancel`, `cpt-cf-approval-service-upreq-multi-party-gates` |
| p2 (important) | `cpt-cf-approval-service-upreq-bulk-approve`, `cpt-cf-approval-service-upreq-auto-approval`, `cpt-cf-approval-service-upreq-on-behalf-decision`, `cpt-cf-approval-service-upreq-audit-trail`, `cpt-cf-approval-service-upreq-request-event-wait`, `cpt-cf-approval-service-upreq-request-read`, `cpt-cf-approval-service-upreq-staged-gates`, `cpt-cf-approval-service-upreq-threshold-evaluation`, `cpt-cf-approval-service-upreq-escalation` |
| p3 (nice-to-have) | `cpt-cf-approval-service-upreq-stakeholder-notifications`, `cpt-cf-approval-service-upreq-bss-library-parity` |

## 4. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **Design**: [DESIGN.md](./DESIGN.md)

### Model Registry Sources

- [Model Registry PRD](../../model-registry/docs/PRD.md) — §5.2 Model Approval, §5.3 Resolution for Evaluation, §13 Open Questions

### Task Engine Sources

- [Task Engine PRD](../../task-engine/docs/PRD.md) — §4.3 Future Scope, §5.4 Event Wait Condition

### BSS Sources

- [Orders Workflow PRD](../../bss/orders-workflow/docs/PRD.md) — §6.2 Approval Execution, §9.2 Approval Request/Decision Contract, §15 Open Questions
- [Orders Changes PRD](../../bss/orders-changes/docs/PRD.md) — Generic Approval Service actor
- [Orders Lifecycle DECISIONS](../../bss/orders-lifecycle/docs/DECISIONS.md) — D-73, D-166, Q-09
- [BSS approval library](../../bss/libs/approval/README.md) and [BSS Approvals Inbox PRD](../../bss/approvals/docs/PRD.md)
- [Contracts PRD](../../bss/contracts/docs/PRD.md) — contract signature approval deferred
