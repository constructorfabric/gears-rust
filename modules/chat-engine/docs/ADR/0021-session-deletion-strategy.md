Created:  2026-02-06 by Constructor Tech
Updated:  2026-03-10 by Constructor Tech
# ADR-0021: Session Deletion Strategy (Soft Delete as Default)


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Immediate hard delete](#option-1-immediate-hard-delete)
  - [Option 2: Soft delete with automatic hard delete (chosen)](#option-2-soft-delete-with-automatic-hard-delete-chosen)
  - [Option 3: External archival system](#option-3-external-archival-system)
  - [Option 4: Soft delete only (no auto cleanup)](#option-4-soft-delete-only-no-auto-cleanup)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-06

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-session-deletion-strategy`

## Context and Problem Statement

Chat Engine needs a deletion strategy that balances user safety (protection from accidental data loss), storage costs (minimizing database footprint for deleted data), compliance (GDPR/CCPA right-to-erasure), query performance as data grows, and the ability to restore accidentally deleted sessions. How should session deletion be implemented to satisfy these competing concerns?

## Decision Drivers

* User safety: protect users from accidental, irreversible data loss
* Storage costs: minimize database storage for logically deleted data
* Compliance: support GDPR/CCPA right-to-erasure and data minimization principles
* Performance: maintain query performance as soft-deleted data accumulates
* Recovery: enable restoration of accidentally deleted sessions within a grace period
* Auditability: emit webhook events for all lifecycle state transitions

## Considered Options

* **Option 1: Immediate hard delete** — DELETE row permanently on user request; no recovery possible
* **Option 2: Soft delete with automatic hard delete** — set lifecycle_state=soft_deleted with configurable retention period; background job hard-deletes after grace period expires
* **Option 3: External archival system** — move deleted sessions to external archive (S3, Glacier) for long-term storage
* **Option 4: Soft delete only (no auto cleanup)** — keep soft-deleted sessions indefinitely; never hard-delete

## Decision Outcome

Chosen option: **Option 2 (Soft delete with automatic hard delete)**, because it provides a recovery window for accidental deletions, satisfies compliance requirements via automatic cleanup, and keeps storage bounded — all without requiring external dependencies.

Key design choices:

1. **Dual Deletion Model**: SessionDeleteRequest accepts a `deletion_type` parameter (default: `soft`). Soft delete sets `lifecycle_state=soft_deleted` and is recoverable. Hard delete physically removes from database and is permanent.

2. **Retention Policies**: Configurable per session type via `RetentionPolicy`. Background job enforces `soft_delete_retention_days` daily, hard-deleting sessions past their grace period. Optional archival tier for inactive sessions.

3. **Lifecycle States** (4 states): `active → archived → soft_deleted → hard_deleted`. Messages inherit state from session (cascade). Webhook notifications emitted for all transitions.

4. **Recovery Window**: Sessions are restorable until `scheduled_hard_delete_at`. Explicit restore via `POST /sessions/:id/restore`. Clear error messages after grace period expires.

### Consequences

* Good, because users are protected from accidental data loss via recovery window
* Good, because compliance-friendly with grace period followed by automatic cleanup
* Good, because retention policies are flexible per session type
* Good, because lifecycle state transitions are auditable via webhook events
* Good, because follows industry standard behavior (Gmail, Slack, Google Drive)
* Bad, because requires a background cleanup job for hard-delete enforcement
* Bad, because slightly more storage than immediate deletion (<5% overhead typical)
* Bad, because more complex implementation than simple DELETE (lifecycle state management)
* Bad, because queries must filter on lifecycle_state to exclude soft-deleted sessions

### Confirmation

Confirmed via design review and alignment with DESIGN.md session lifecycle implementation. Verified when:

- Soft delete sets lifecycle_state and scheduled_hard_delete_at correctly
- Background job hard-deletes sessions past their retention period
- Restore operation succeeds within grace period and fails after
- All lifecycle transitions emit appropriate webhook events
- Queries exclude soft-deleted sessions by default

## Pros and Cons of the Options

### Option 1: Immediate hard delete

DELETE row permanently on user request; no recovery possible.

* Good, because simplest implementation (single DELETE statement)
* Good, because lowest storage cost (no soft-deleted rows)
* Bad, because no recovery from accidental deletion
* Bad, because higher support burden (users requesting data recovery)
* Bad, because potential compliance issues (premature deletion before legal hold)
* Bad, because uncommon pattern — users expect trash/recycle bin behavior

### Option 2: Soft delete with automatic hard delete (chosen)

Set lifecycle_state=soft_deleted with configurable retention; background job enforces cleanup.

* Good, because recovery window protects against accidental deletion
* Good, because automatic cleanup satisfies data minimization requirements
* Good, because flexible retention per session type
* Bad, because requires background job infrastructure
* Bad, because queries must account for lifecycle_state filtering

### Option 3: External archival system

Move deleted sessions to external archive service (S3, Glacier).

* Good, because separates hot and cold storage tiers
* Bad, because adds external dependency and failure modes
* Bad, because slower recovery (must retrieve from archive)
* Bad, because higher implementation complexity

### Option 4: Soft delete only (no auto cleanup)

Keep soft-deleted sessions indefinitely; never hard-delete.

* Good, because simplest soft-delete implementation (no background job)
* Bad, because violates data minimization (GDPR requires eventual deletion)
* Bad, because unbounded storage growth over time

## Related Design Elements

**Requirements**:
* `cpt-cf-chat-engine-fr-delete-session` — Soft delete as default deletion mechanism
* `cpt-cf-chat-engine-nfr-data-integrity` — Lifecycle state transitions maintain referential integrity

**Design Elements**:
* `cpt-cf-chat-engine-dbtable-sessions` — lifecycle_state, deleted_at, scheduled_hard_delete_at columns

**Related ADRs**:
* `cpt-cf-chat-engine-adr-message-tree-structure` — Messages inherit lifecycle state from parent session (cascade)
* `cpt-cf-chat-engine-adr-webhook-event-types` — Webhook events for lifecycle transitions (session.soft_deleted, session.hard_deleted, session.restored)
