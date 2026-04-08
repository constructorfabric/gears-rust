---
status: accepted
date: 2026-02-18
---

# Delegate Approval Workflow to Generic Approval Service

**ID**: `cpt-cf-model-registry-adr-approval-delegation`

## Context and Problem Statement

Model Registry requires approval workflows for tenant-level model access (pending → approved → revoked). Should Model Registry implement its own approval logic, or delegate to a platform-wide Approval Service?

## Decision Drivers

* `cpt-cf-model-registry-fr-model-approval` — Tenant-level model approval workflow
* `cpt-cf-model-registry-fr-auto-approval` — Automatic approval based on rules
* PRD scope definition: "Approval workflow engine — Generic Approval Service"
* Platform consistency — unified approval UX across modules
* Separation of concerns — Model Registry focuses on model catalog

## Considered Options

* Delegate to Generic Approval Service
* Build approval workflow into Model Registry
* Event Sourcing for approval states

## Decision Outcome

Chosen option: "Delegate to Generic Approval Service", because it provides platform-wide consistency, reuses existing infrastructure, and maintains clear separation of concerns.

### Consequences

* Good, because unified approval UX across the platform
* Good, because approval logic is reusable by other modules
* Good, because Model Registry remains focused on catalog concerns
* Good, because audit trail handled by Approval Service
* Bad, because dependency on external service
* Bad, because event propagation latency for status updates

### Confirmation

* Integration tests verify Model Registry correctly registers approvable resources
* Integration tests verify Model Registry reacts to approval status change events
* End-to-end tests confirm approval workflow functions correctly

## Pros and Cons of the Options

### Delegate to Generic Approval Service

Model Registry integration:
1. Registers discovered models as "approvable resources" with Approval Service
2. Queries approval status from Approval Service when resolving models
3. Reacts to `approval_status_changed` events to invalidate cache

Approval Service responsibilities:
- State machine (pending → approved/rejected, approved → revoked)
- Notifications to stakeholders
- Audit trail for all decisions
- Auto-approval rule evaluation

* Good, because unified approval UX platform-wide
* Good, because reusable approval infrastructure
* Good, because clear separation of concerns
* Good, because audit and notifications handled centrally
* Neutral, because requires event-driven integration
* Bad, because external service dependency
* Bad, because latency on event propagation

### Build Approval Workflow into Model Registry

Full approval implementation within Model Registry module.

* Good, because full control over implementation
* Good, because no external dependencies
* Good, because potentially lower latency
* Bad, because duplicates logic needed by other modules
* Bad, because inconsistent approval UX across platform
* Bad, because Model Registry scope creep

### Event Sourcing for Approval States

Approval state stored as event log, current state derived from events.

* Good, because complete audit trail with time-travel
* Good, because state reconstruction from events
* Bad, because significant implementation complexity
* Bad, because overkill for simple state machine
* Bad, because operational complexity

## More Information

Integration pattern:
```
Model Discovery → Register with Approval Service (status: pending)
                         ↓
Tenant Admin → Approval Service UI → approve/reject
                         ↓
Approval Service → emit event: approval_status_changed
                         ↓
Model Registry → invalidate cache, reflect new status
```

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-model-registry-fr-model-approval` — Approval workflow via Approval Service integration
* `cpt-cf-model-registry-fr-auto-approval` — Auto-approval rules managed by Approval Service
* `cpt-cf-model-registry-principle-approval-delegation` — Establishes delegation pattern
* `cpt-cf-model-registry-constraint-approval-service` — Defines integration boundary
