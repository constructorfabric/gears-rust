# Upstream Requirements — Approval Service

## Overview

This document consolidates requirements from downstream modules that depend on Approval Service. It serves as input for Approval Service PRD and DESIGN.

---

## Model Registry Requirements

**Source**: `modules/model-registry/docs/` (PRD, DESIGN, ADR-0002)

**Priority**: P1 (core integration)

### Required Operations

#### 1. Register Approvable Resource

Register a resource as approvable within the approval workflow system.

**Input**:
| Field | Type | Description |
|-------|------|-------------|
| resource_type | string | Resource type identifier in GTS format (e.g., `gts.x.genai.model.model.v1~`) |
| resource_id | UUID | Unique resource identifier |
| tenant_id | UUID | Tenant context for the approval |
| metadata | object | Resource-specific metadata for display/filtering |

**Behavior**:
- Creates approval record with initial status `pending`
- Associates resource with tenant context
- Stores metadata for UI display and rule evaluation

**Source**: [`cpt-cf-model-registry-fr-model-approval`](../../model-registry/docs/PRD.md)

---

#### 2. Query Approval Status

Query the current approval status for a resource in tenant context.

**Input**:
| Field | Type | Description |
|-------|------|-------------|
| resource_type | string | Resource type identifier |
| resource_id | UUID | Unique resource identifier |
| tenant_id | UUID | Tenant context (resolves with hierarchy) |

**Output**:
| Field | Type | Description |
|-------|------|-------------|
| status | enum | `pending`, `approved`, `rejected`, `revoked` |
| decided_at | timestamp | When decision was made (null if pending) |
| decided_by | UUID | Actor who made decision (null if pending) |
| inherited_from | UUID | Parent tenant ID if inherited (null if own) |

**Behavior**:
- Resolves status considering tenant hierarchy (child inherits parent approvals)
- Returns most specific approval (own tenant > parent > root)
- Indicates if status is inherited vs own

**Source**: [`cpt-cf-model-registry-fr-model-approval`](../../model-registry/docs/PRD.md), [`cpt-cf-model-registry-principle-additive-inheritance`](../../model-registry/docs/DESIGN.md)

---

#### 3. Bulk Approval (P2)

Approve multiple

**Input**:
| Field | Type | Description |
|-------|------|-------------|
| resource_type | string | Resource type identifier |
| resource_ids | string[] | List of resource identifiers |
| tenant_id | UUID | Tenant context |
| actor_id | UUID | Actor performing approval |

**Behavior**:
- Atomic operation: all succeed or all fail
- Validates actor has permission for all resources
- Emits individual `approval_status_changed` event per resource (required for per-model cache invalidation in Model Registry)

**Source**: [`cpt-cf-model-registry-fr-bulk-operations`](../../model-registry/docs/PRD.md)

---

#### 4. Register Criteria Schema (P2)

Register resource-specific criteria schema for auto-approval rules.

**Input**:
| Field | Type | Description |
|-------|------|-------------|
| resource_type | string | Resource type identifier |
| schema | object | JSON Schema defining available criteria fields |

**Model Registry Criteria Schema**:
```json
{
  "type": "object",
  "properties": {
    "provider_slug": { "type": "string" },
    "provider_gts_type": { "type": "string" },
    "lifecycle_status": {
      "type": "string",
      "enum": ["production", "preview", "experimental", "deprecated", "sunset"]
    },
    "capabilities": {
      "type": "object",
      "properties": {
        "text_input": { "type": "boolean" },
        "image_input": { "type": "boolean" },
        "text_output": { "type": "boolean" },
        "image_output": { "type": "boolean" },
        "embeddings": { "type": "boolean" },
        "function_calling": { "type": "boolean" },
        "streaming": { "type": "boolean" }
      }
    },
    "managed": { "type": "boolean" }
  }
}
```

**Source**: [`cpt-cf-model-registry-fr-auto-approval`](../../model-registry/docs/PRD.md)

---

#### 5. Create Auto-Approval Rule (P2)

Create an automatic approval rule for a resource type.

**Input**:
| Field | Type | Description |
|-------|------|-------------|
| resource_type | string | Resource type identifier |
| tenant_id | UUID | Tenant scope (root = platform-wide) |
| criteria | object | Criteria matching the registered schema |
| action | enum | `allow` or `block` |
| priority | integer | Rule evaluation order (lower = higher priority) |

**Behavior**:
- Validates criteria against registered schema
- Enforces "restrict only" — child tenant cannot allow what parent blocked
- Rules evaluated in priority order; first match wins

**Rule Hierarchy** (enforced by Approval Service):
1. Platform ceiling rules (root tenant, highest priority)
2. Tenant-specific rules (can only restrict, not expand)
3. Default: pending (no auto-approval)

**Source**: [`cpt-cf-model-registry-fr-auto-approval`](../../model-registry/docs/PRD.md)

---

### Required Events

#### approval_status_changed

Event emitted when approval status changes.

**Payload**:
| Field | Type | Description |
|-------|------|-------------|
| resource_type | string | Resource type identifier |
| resource_id | UUID | Resource identifier |
| tenant_id | UUID | Tenant context |
| old_status | enum | Previous status |
| new_status | enum | New status |
| actor_id | UUID | Actor who triggered change |
| timestamp | timestamp | When change occurred |

**Consumer**: Model Registry invalidates cache on receipt.

**Source**: [`cpt-cf-model-registry-seq-model-approval`](../../model-registry/docs/DESIGN.md)

---

### Required State Machine

```
                    ┌─────────┐
                    │ pending │
                    └────┬────┘
                         │
              ┌──────────┼──────────┐
              │          │          │
              ▼          │          ▼
        ┌──────────┐     │    ┌──────────┐
        │ approved │     │    │ rejected │
        └────┬─────┘     │    └──────────┘
             │           │
             ▼           │
        ┌──────────┐     │
        │ revoked  │─────┘ (can re-approve)
        └──────────┘
```

**Transitions**:
| From | To | Trigger |
|------|-----|---------|
| pending | approved | Admin approval or auto-approval rule match |
| pending | rejected | Admin rejection |
| approved | revoked | Admin revocation |
| revoked | approved | Admin re-approval |
| rejected | approved | Admin re-approval |

**Source**: [PRD State Machine](../../model-registry/docs/PRD.md) (section 3.1, ModelApproval — includes `rejected → approved` and `revoked → approved` transitions), [`cpt-cf-model-registry-adr-approval-delegation`](../../model-registry/docs/ADR/0002-cpt-cf-model-registry-adr-approval-delegation.md) (covers core transitions)

---

### Required Behaviors

#### Tenant Hierarchy Support

- Approvals inherit down the tenant tree (additive inheritance)
- Child tenant sees: parent's approvals + own approvals
- Resolution order: tenant → parent → ... → root (first match wins)
- Child can revoke inherited approval for their scope
- Child cannot approve what parent blocked ("restrict only")

**Source**: [`cpt-cf-model-registry-adr-tenant-inheritance`](../../model-registry/docs/ADR/0004-cpt-cf-model-registry-adr-tenant-inheritance.md)

---

#### Audit Trail

All approval decisions must be recorded with:
- Actor (who made the decision)
- Timestamp (when decision was made)
- Tenant context
- Previous and new status
- Reason/comment (optional)

**Source**: [`cpt-cf-model-registry-adr-approval-delegation`](../../model-registry/docs/ADR/0002-cpt-cf-model-registry-adr-approval-delegation.md)

---

#### Notifications

Notify stakeholders on status changes:
- Pending → notify approvers
- Approved/Rejected → notify requester
- Revoked → notify affected users

**Source**: [`cpt-cf-model-registry-adr-approval-delegation`](../../model-registry/docs/ADR/0002-cpt-cf-model-registry-adr-approval-delegation.md)

---

## Traceability

### Model Registry Sources

- [ ] [`cpt-cf-model-registry-fr-model-approval`](../../model-registry/docs/PRD.md) — register resource, query status
- [ ] [`cpt-cf-model-registry-fr-bulk-operations`](../../model-registry/docs/PRD.md) — bulk approval
- [ ] [`cpt-cf-model-registry-fr-auto-approval`](../../model-registry/docs/PRD.md) — criteria schema, auto-approval rules
- [ ] [`cpt-cf-model-registry-seq-model-approval`](../../model-registry/docs/DESIGN.md) — status change events
- [ ] [`cpt-cf-model-registry-adr-approval-delegation`](../../model-registry/docs/ADR/0002-cpt-cf-model-registry-adr-approval-delegation.md) — state machine, audit, notifications
- [ ] [`cpt-cf-model-registry-adr-tenant-inheritance`](../../model-registry/docs/ADR/0004-cpt-cf-model-registry-adr-tenant-inheritance.md) — tenant hierarchy support
