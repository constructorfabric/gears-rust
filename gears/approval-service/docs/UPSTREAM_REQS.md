# UPSTREAM_REQS — Approval Service

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting Gears](#12-requesting-gears)
- [2. Requirements](#2-requirements)
  - [2.1 Model Registry](#21-model-registry)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)
  - [Model Registry Sources](#model-registry-sources)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

A centralized approval service is needed to manage tenant-level approval decisions for shared resources (AI models, serverless functions, etc.). Multiple gears currently implement ad-hoc approval logic independently, leading to inconsistent behavior and duplicated code.

### 1.2 Requesting Gears

| Gear   | Why it needs this gear |
|--------|-------------------------|
| model-registry | Needs tenant-scoped approval checks before serving AI models; requires approval lifecycle management, bulk operations, auto-approval rules, and tenant hierarchy support |

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

#### Audit Trail

- [ ] `p2` - **ID**: `cpt-cf-approval-service-upreq-audit-trail`

The future gear **MUST** record all approval decisions with actor, timestamp, tenant context, previous and new status.

- **Rationale**: Compliance and operational debugging require a complete history of who approved/rejected/revoked which resources and when.

#### Stakeholder Notifications

- [ ] `p3` - **ID**: `cpt-cf-approval-service-upreq-stakeholder-notifications`

The future gear **MUST** notify relevant stakeholders on approval status changes (approvers on new pending requests, requesters on decisions, affected users on revocations).

- **Rationale**: Without notifications, pending approvals can go unnoticed and revocations can surprise users.

## 3. Priorities

| Priority | Requirements |
|----------|-------------|
| p1 (critical) | `cpt-cf-approval-service-upreq-register-resource`, `cpt-cf-approval-service-upreq-query-status`, `cpt-cf-approval-service-upreq-status-changed-event`, `cpt-cf-approval-service-upreq-tenant-hierarchy`, `cpt-cf-approval-service-upreq-lifecycle` |
| p2 (important) | `cpt-cf-approval-service-upreq-bulk-approve`, `cpt-cf-approval-service-upreq-auto-approval`, `cpt-cf-approval-service-upreq-audit-trail` |
| p3 (nice-to-have) | `cpt-cf-approval-service-upreq-stakeholder-notifications` |

## 4. Traceability

- **PRD** (when created): [PRD.md](./PRD.md)
- **Design** (when created): [DESIGN.md](./DESIGN.md)
